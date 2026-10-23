//! jwarc's `WarcReader`: a WARC or ARC file, gzip-compressed member by member or not, read
//! record by record, each header by jwarc's WARC machine in its strict form, which turns an ARC
//! header into WARC fields, each block as long as its `Content-Length` says, and the trailer
//! after it skipped.

use super::headers::{Headers, NotSole};
use super::ragel::{Flow, Unsigned, ERROR};
use super::tables::{WARC, WARC_FIRST_FINAL, WARC_START};
use super::text::TextBuffer;
use crate::compress::{gzip, Decoding, Limited, Undecoded};
use crate::input::{Input, Pages};
use crate::java;
use crate::java::time::{civil, format_instant};

/// A record: its protocol, WARC or ARC, its header fields and its block.
#[derive(Debug)]
pub(super) struct Record<'a> {
    pub(super) headers: Headers,
    pub(super) block: &'a [u8],
}

/// Why the reader stops where jwarc's would throw, failing Tika's whole parse, after the
/// records it read before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Stop {
    /// An exception jwarc throws, which ends the parse.
    Thrown(String),
    /// A record whose block jwarc would read past the end of the file, which Tika's parse of
    /// it would record.
    CutBlock,
    /// A block length jwarc's `LengthedBody` mishandles.
    NegativeLength,
}

impl From<NotSole> for Stop {
    fn from(not_sole: NotSole) -> Self {
        Self::Thrown(format!("IllegalArgumentException: {}", not_sole.0))
    }
}

/// `WarcReader`'s decompression of `input`, whose pages are `pages`: gzip members inflated one
/// after another to a spool, or the bytes as they are, in place; or what it throws.
pub(super) fn decompress<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<Input<'a>, String> {
    if input.len() < 4 {
        if input.is_empty() {
            return Ok(Input::Borrowed(&[]));
        }
        return Err("EOFException".to_owned());
    }
    if input.starts_with(&[0x1F, 0x8B]) {
        return gunzip(input);
    }
    let magic = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
    if magic == 0xFD2F_B528 || magic == 0x184D_2A5D {
        return Err("IOException: ZStandard decoder not found".to_owned());
    }
    Ok(Input::Within(input, pages))
}

const FHCRC: u8 = 2;
const FEXTRA: u8 = 4;
const FNAME: u8 = 8;
const FCOMMENT: u8 = 16;

/// `GunzipChannel`: each member's header read, its deflate data inflated and its size checked
/// against the trailer's, the CRC left unchecked, until the input ends at a member boundary;
/// what the members inflate into spooled, up to the decode limit of `input`.
fn gunzip(input: &[u8]) -> Result<Input<'static>, String> {
    let thrown = |what: &str| what.to_owned();
    let unspooled = |undecoded: Undecoded| format!("IOException: {undecoded}");
    let mut out = Decoding::spooled(input.len(), gzip::declared_size(input));
    let mut chunk = vec![0; 64 * 1024];
    let mut at = 0;
    while at < input.len() {
        let header = input
            .get(at..at + 10)
            .ok_or_else(|| thrown("EOFException: partial gzip header"))?;
        if header[..2] != [0x1F, 0x8B] {
            return Err(thrown("ZipException: not in gzip format"));
        }
        if header[2] != 8 {
            return Err(thrown("ZipException: unsupported compression method"));
        }
        let flags = header[3];
        at += 10;
        if flags & FEXTRA != 0 {
            let length = input
                .get(at..at + 2)
                .ok_or_else(|| thrown("EOFException: reading gzip extra"))?;
            let length = i16::from_le_bytes([length[0], length[1]]);
            let length = usize::try_from(length)
                .map_err(|_| thrown("IllegalArgumentException: a negative gzip extra length"))?;
            at += 2;
            if input.len() - at < length {
                return Err(thrown("EOFException: reading gzip extra"));
            }
            at += length;
        }
        for (flag, what) in [(FNAME, "name"), (FCOMMENT, "comment")] {
            if flags & flag != 0 {
                let end = input[at..]
                    .iter()
                    .position(|&byte| byte == 0)
                    .ok_or_else(|| format!("EOFException: reading gzip {what}"))?;
                at += end + 1;
            }
        }
        if flags & FHCRC != 0 {
            if input.len() - at < 2 {
                return Err(thrown("EOFException: reading gzip header crc"));
            }
            at += 2;
        }
        let member = &input[at..];
        let start = out.written();
        let mut inflater = flate2::Decompress::new(false);
        loop {
            let (read, written) = (inflater.total_in(), inflater.total_out());
            let offset = usize::try_from(read).expect("the input fits in memory");
            let status = inflater
                .decompress(&member[offset..], &mut chunk, flate2::FlushDecompress::None)
                .map_err(|e| format!("ZipException: {e}"))?;
            let produced = usize::try_from(inflater.total_out() - written)
                .expect("at most a chunk is produced");
            out.write(&chunk[..produced]).map_err(unspooled)?;
            if out.is_cut() {
                return Err(unspooled(Undecoded::TooLarge));
            }
            if status == flate2::Status::StreamEnd {
                break;
            }
            if inflater.total_in() == read && inflater.total_out() == written {
                return Err(thrown("EOFException: unexpected end of gzip stream"));
            }
        }
        at += usize::try_from(inflater.total_in()).expect("the input fits in memory");
        let trailer = input
            .get(at..at + 8)
            .ok_or_else(|| thrown("EOFException: reading gzip trailer"))?;
        let size = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
        if size != (out.written() - start) as u32 {
            return Err(thrown("ZipException: gzip uncompressed size mismatch"));
        }
        at += 8;
    }
    match out.finish().map_err(unspooled)? {
        Limited::Whole(data) => Ok(data),
        Limited::Cut(_) => Err(unspooled(Undecoded::TooLarge)),
    }
}

/// `WarcReader.next`: the record at `at`, which moves past it and its trailer, or `None` at the
/// end of the data.
pub(super) fn next<'a>(data: &'a [u8], at: &mut usize) -> Result<Option<Record<'a>>, Stop> {
    if *at == data.len() {
        return Ok(None);
    }
    let (headers, protocol, header_length) = parse_header(&data[*at..])?;
    let block_start = *at + header_length;
    let length = match headers.sole("Content-Length")? {
        Some(text) => java::parse_long(text).ok_or_else(|| {
            Stop::Thrown(format!(
                "NumberFormatException: For input string: \"{text}\""
            ))
        })?,
        None => 0,
    };
    let length = usize::try_from(length).map_err(|_| Stop::NegativeLength)?;
    let block = data
        .get(block_start..)
        .and_then(|rest| rest.get(..length))
        .ok_or(Stop::CutBlock)?;
    headers.sole("WARC-Type")?;
    *at = block_start + length + trailer_length(&data[block_start + length..], protocol);
    Ok(Some(Record { headers, block }))
}

/// The protocol a record's header names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Warc,
    Arc,
}

/// `consumeTrailer`: the line feed that ends an ARC record, the two CRLFs that end a WARC
/// record, or else the run of line breaks there is.
fn trailer_length(rest: &[u8], protocol: Protocol) -> usize {
    match protocol {
        Protocol::Arc => usize::from(rest.first() == Some(&b'\n')),
        Protocol::Warc if rest.len() < 4 => 0,
        Protocol::Warc if rest.starts_with(b"\r\n\r\n") => 4,
        Protocol::Warc => rest
            .iter()
            .take_while(|&&byte| byte == b'\r' || byte == b'\n')
            .count(),
    }
}

/// The header at the start of `data`, by jwarc's strict WARC machine: its fields, its protocol
/// and its length.
fn parse_header(data: &[u8]) -> Result<(Headers, Protocol, usize), Stop> {
    let mut state = Header {
        buffer: TextBuffer::default(),
        name: None,
        headers: Headers::default(),
        protocol: Protocol::Warc,
    };
    let mut cs = WARC_START;
    let end = WARC
        .exec(&mut cs, &Unsigned(data), 0, false, |action, p| {
            Ok::<_, ()>(state.act(action, data[p]))
        })
        .expect("the WARC actions never fail");
    if cs >= WARC_FIRST_FINAL {
        return Ok((state.headers, state.protocol, end));
    }
    if cs == ERROR {
        return Err(Stop::Thrown(format!(
            "ParsingException: invalid WARC record at position {end}"
        )));
    }
    Err(Stop::Thrown("EOFException".to_owned()))
}

/// The state `WarcParser`'s actions keep.
struct Header {
    buffer: TextBuffer,
    name: Option<String>,
    headers: Headers,
    protocol: Protocol,
}

impl Header {
    /// `setHeader`.
    fn set(&mut self, name: &str, value: String) {
        self.headers.set(name, value);
    }

    /// Runs action `action` of `WarcParser` on `byte`, the key it fires on.
    fn act(&mut self, action: u16, byte: u8) -> Flow {
        match action {
            0 => self.buffer.push(byte),
            1 | 2 => {}
            3 => self.buffer.end_text(),
            4 => self.buffer.fold(),
            5 => {
                self.name = Some(java::ascii(self.buffer.pushed()));
                self.buffer.restart();
            }
            6 => {
                let value = java::utf8(self.buffer.text());
                let name = self.name.clone().expect("a field value follows its name");
                self.headers.add(name, value);
                self.buffer.restart();
                self.buffer.forget_text();
            }
            7 => {
                let url = java::latin1(self.buffer.pushed());
                if let Some(file_name) = url.strip_prefix("filedesc://") {
                    self.set("WARC-Type", "warcinfo".to_owned());
                    self.set("WARC-Filename", file_name.to_owned());
                    self.set("Content-Type", "text/plain".to_owned());
                } else if url.starts_with("dns:") {
                    self.set("WARC-Type", "response".to_owned());
                    self.set("Content-Type", "text/dns".to_owned());
                    self.set("WARC-Target-URI", url);
                } else {
                    self.set("WARC-Type", "response".to_owned());
                    self.set(
                        "Content-Type",
                        "application/http;msgtype=response".to_owned(),
                    );
                    self.set("WARC-Target-URI", url);
                }
                self.buffer.restart();
            }
            8 => {
                let address = java::ascii(self.buffer.pushed());
                self.set("WARC-IP-Address", address);
                self.buffer.restart();
            }
            9 => {
                if let Some(date) = arc_date(self.buffer.pushed()) {
                    self.set("WARC-Date", date);
                }
                self.buffer.restart();
            }
            10 => {
                if self.buffer.pushed() == b"alexa/dat" {
                    self.set("Content-Type", "alexa/dat".to_owned());
                }
                self.buffer.restart();
            }
            11 => {
                let length = java::ascii(self.buffer.pushed());
                self.set("Content-Length", length);
                self.buffer.restart();
            }
            12 => self.buffer.restart(),
            13 => self.protocol = Protocol::Arc,
            14 | 15 => return Flow::Break,
            _ => unreachable!("the WARC machine has no action {action}"),
        }
        Flow::Continue
    }
}

/// The `WARC-Date` an ARC date, of 8 to 28 digits, stands for: padded with zeros or cut to 14
/// digits, read as `yyyyMMddHHmmss` by `LocalDateTime.parse` in its smart resolver style, in
/// UTC, as `Instant.toString` writes it; `None` where that parse throws.
fn arc_date(digits: &[u8]) -> Option<String> {
    let mut digits = digits.to_vec();
    digits.resize(14, b'0');
    let field = |range: std::ops::Range<usize>| -> i64 {
        digits[range]
            .iter()
            .fold(0, |value, digit| value * 10 + i64::from(digit - b'0'))
    };
    let (year, month, day) = (field(0..4), field(4..6), field(6..8));
    let (hour, minute, second) = (field(8..10), field(10..12), field(12..14));
    if year == 0
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let last_day = match month {
        2 if civil::is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    let days = civil::days_from_civil(year, month, day.min(last_day));
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    Some(format_instant(seconds * 1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The records of decompressed `data`, in order, and where the reader stops if it stops
    /// short of the end.
    fn records(data: &[u8]) -> (Vec<Record<'_>>, Option<Stop>) {
        let mut records = Vec::new();
        let mut at = 0;
        loop {
            match next(data, &mut at) {
                Ok(Some(record)) => records.push(record),
                Ok(None) => return (records, None),
                Err(stop) => return (records, Some(stop)),
            }
        }
    }

    #[test]
    fn reads_warc_and_arc_headers_as_jwarc_does() {
        let data = b"WARC/1.0\r\nWARC-Type: response\r\nContent-Length: 3\r\nX: a\r\n b\r\n\r\nabc\r\n\r\n\
                     http://e.com/ 1.2.3.4 20080430204825 text/html 2\nhi\n";
        let (records, stop) = records(data);
        assert_eq!(stop, None);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].block, b"abc");
        assert_eq!(records[0].headers.first("x"), Some("a b"));
        let arc = &records[1].headers;
        assert_eq!(arc.first("WARC-Type"), Some("response"));
        assert_eq!(arc.first("WARC-Target-URI"), Some("http://e.com/"));
        assert_eq!(arc.first("WARC-Date"), Some("2008-04-30T20:48:25Z"));
        assert_eq!(
            arc.first("Content-Type"),
            Some("application/http;msgtype=response")
        );
        assert_eq!(records[1].block, b"hi");
    }

    #[test]
    fn stops_where_jwarc_throws() {
        let (_, stop) = records(b"WARC/1.0\r\nContent-Length: 9\r\n\r\nabc");
        assert_eq!(stop, Some(Stop::CutBlock));
        let (_, stop) = records(b"WARC/1.0\r\nBad Name: x\r\n\r\n");
        assert!(matches!(stop, Some(Stop::Thrown(_))));
        let (_, stop) = records(b"WARC/1.0\r\nContent-Length: 1\r\ncontent-length: 2\r\n\r\n");
        assert!(matches!(stop, Some(Stop::Thrown(_))));
        assert!(decompress(&[0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 0, 1], Pages::IN_MEMORY).is_err());
    }

    #[test]
    fn reads_arc_dates_as_java_time_does() {
        assert_eq!(
            arc_date(b"20080430").as_deref(),
            Some("2008-04-30T00:00:00Z")
        );
        assert_eq!(
            arc_date(b"20080231000000").as_deref(),
            Some("2008-02-29T00:00:00Z")
        );
        assert_eq!(arc_date(b"20081301000000"), None);
    }
}
