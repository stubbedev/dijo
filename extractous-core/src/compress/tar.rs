//! TAR archives as commons-compress's `TarArchiveInputStream` reads them with its defaults:
//! 512-byte records, names decoded as UTF-8 with `?` for what is malformed, numbers that must be
//! octal or base-256, POSIX, GNU and star headers, PAX extended headers, local and global, and
//! GNU long names. Sparse files are not read: the entries that are or may be sparse are
//! [`Unread::Sparse`].

use std::collections::HashMap;
use std::ops::Range;

use super::parse_long;
use super::zip::decode_name;

const RECORD: usize = 512;
const NAME: Range<usize> = 0..100;
const MODE: Range<usize> = 100..108;
const UID: Range<usize> = 108..116;
const GID: Range<usize> = 116..124;
const SIZE: Range<usize> = 124..136;
const MTIME: Range<usize> = 136..148;
const CHECKSUM: Range<usize> = 148..156;
const LINK_FLAG: usize = 156;
const MAGIC: Range<usize> = 257..263;
const VERSION: Range<usize> = 263..265;
const USER_NAME: Range<usize> = 265..297;
const GROUP_NAME: Range<usize> = 297..329;
const DEV_MAJOR: Range<usize> = 329..337;
const DEV_MINOR: Range<usize> = 337..345;
const PREFIX: Range<usize> = 345..500;
const XSTAR_PREFIX: Range<usize> = 345..476;
const XSTAR_ATIME: Range<usize> = 476..488;
const XSTAR_CTIME: Range<usize> = 488..500;
const XSTAR_MAGIC: Range<usize> = 508..512;
const GNU_ATIME: Range<usize> = 345..357;
const GNU_CTIME: Range<usize> = 357..369;
const GNU_SPARSE: usize = 386;
const GNU_IS_EXTENDED: usize = 482;
const GNU_REAL_SIZE: Range<usize> = 483..495;
const XSTAR_MULTIVOLUME: usize = 464;

const LF_CHR: u8 = b'3';
const LF_BLK: u8 = b'4';
const LF_DIR: u8 = b'5';
const LF_LONG_LINK: u8 = b'K';
const LF_LONG_NAME: u8 = b'L';
const LF_MULTIVOLUME: u8 = b'M';
const LF_OLD_GNU_SPARSE: u8 = b'S';
const LF_PAX: [u8; 2] = *b"xX";
const LF_PAX_GLOBAL: u8 = b'g';

/// `Instant`'s range of epoch seconds.
const INSTANT_SECONDS: std::ops::RangeInclusive<i128> =
    -31_557_014_167_219_200..=31_556_889_864_403_199;

/// Why an archive is not read natively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unread {
    /// commons-compress throws, for this reason.
    Throws(String),
    /// An entry is or may be sparse, which the port does not read.
    Sparse,
    /// A PAX number holds characters beyond ASCII, which Java's `Long.parseLong` may read as
    /// digits.
    NonAsciiNumber,
}

fn throws(reason: impl Into<String>) -> Unread {
    Unread::Throws(reason.into())
}

type PaxHeaders = HashMap<String, String>;

/// `TarArchiveInputStream.matches`: the magic and version of a POSIX, AIX, GNU or Ant header.
pub(crate) fn matches(header: &[u8]) -> bool {
    let (Some(magic), Some(version)) = (header.get(MAGIC), header.get(VERSION)) else {
        return false;
    };
    (magic == b"ustar\0" && matches!(version, b"00" | b"0 " | b"\0\0"))
        || (magic == b"ustar " && matches!(version, b" \0" | b"0\0"))
}

/// `TarUtils.parseOctal`: leading spaces and all trailing NULs and spaces ignored, a field
/// starting with NUL zero.
fn parse_octal(field: &[u8]) -> Result<i64, Unread> {
    if field[0] == 0 {
        return Ok(0);
    }
    let start = field.iter().take_while(|&&b| b == b' ').count();
    let mut end = field.len();
    while start < end && matches!(field[end - 1], 0 | b' ') {
        end -= 1;
    }
    let mut result: i64 = 0;
    for &digit in &field[start..end] {
        if !(b'0'..=b'7').contains(&digit) {
            return Err(throws("a number in a header is not octal"));
        }
        result = result.wrapping_shl(3).wrapping_add(i64::from(digit - b'0'));
    }
    Ok(result)
}

/// `TarUtils.parseOctalOrBinary`: octal, or base-256 when the first byte's high bit is set.
fn parse_octal_or_binary(field: &[u8]) -> Result<i64, Unread> {
    if field[0] & 0x80 == 0 {
        return parse_octal(field);
    }
    let negative = field[0] == 0xFF;
    if field.len() < 9 {
        let mut value: i64 = 0;
        for &b in &field[1..] {
            value = value.wrapping_shl(8).wrapping_add(i64::from(b));
        }
        if negative {
            value -= 1;
            value ^= (1i64 << ((field.len() - 1) * 8)) - 1;
        }
        return Ok(if negative { -value } else { value });
    }
    let mut value: i128 = if field[1] & 0x80 != 0 { -1 } else { 0 };
    for &b in &field[1..] {
        value = (value << 8) | i128::from(b);
    }
    if negative {
        value = !(value - 1);
    }
    let bits = 128
        - if value < 0 {
            (!value).leading_zeros()
        } else {
            value.leading_zeros()
        };
    if bits > 63 {
        return Err(throws("a base-256 number in a header exceeds a long"));
    }
    let value = value as i64;
    Ok(if negative { -value } else { value })
}

/// `TarUtils.verifyCheckSum`: the stored sum equals the unsigned or the signed byte sum with
/// the checksum field read as spaces.
fn checksum_ok(header: &[u8]) -> Result<bool, Unread> {
    let stored = parse_octal(&header[CHECKSUM])?;
    let (mut unsigned, mut signed) = (0i64, 0i64);
    for (i, &b) in header.iter().enumerate() {
        let b = if CHECKSUM.contains(&i) { b' ' } else { b };
        unsigned += i64::from(b);
        signed += i64::from(b as i8);
    }
    Ok(stored == unsigned || stored == signed)
}

/// `TarUtils.parseName`: the field up to its first NUL, decoded.
fn parse_name(field: &[u8]) -> String {
    let length = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    decode_name(&field[..length])
}

/// `TarArchiveEntry.isXstar`.
fn is_xstar(global: &PaxHeaders, header: &[u8]) -> bool {
    if &header[XSTAR_MAGIC] == b"tar\0" {
        return true;
    }
    if let Some(kind) = global.get("SCHILY.archtype") {
        return kind == "xustar" || kind == "exustar";
    }
    let invalid_prefix = header[XSTAR_PREFIX.end - 1] != 0
        && (header[LINK_FLAG] != LF_MULTIVOLUME
            || (header[XSTAR_MULTIVOLUME] & 0x80 == 0 && header[XSTAR_MULTIVOLUME + 11] != b' '));
    let invalid_time = |field: &[u8]| {
        field[0] & 0x80 == 0
            && (field[..11].iter().any(|b| !(b'0'..=b'7').contains(b))
                || !matches!(field[11], b' ' | 0))
    };
    !(invalid_prefix || invalid_time(&header[XSTAR_ATIME]) || invalid_time(&header[XSTAR_CTIME]))
}

/// `TarArchiveEntry.normalizeFileName` without the absolute path kept: on Windows without its
/// drive letter and with its separators turned into slashes, then without its leading slashes.
fn normalize(name: &str) -> String {
    let mut name = name;
    let windows;
    if cfg!(windows) {
        let bytes = name.as_bytes();
        if name.chars().count() > 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            name = &name[2..];
        }
        windows = name.replace('\\', "/");
        name = &windows;
    }
    name.trim_start_matches('/').to_owned()
}

/// A header as `TarArchiveEntry` parses it, with what its PAX headers and GNU long name set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) name: String,
    pub(crate) user_name: String,
    pub(crate) group_name: String,
    pub(crate) size: i64,
    /// The modification time as `FileTime.toMillis` gives it.
    pub(crate) modified_millis: i64,
    pub(crate) checksum_ok: bool,
    pub(crate) link_flag: u8,
    /// An old GNU header's flag that sparse headers follow it.
    extended: bool,
    /// Whether the entry is an old GNU sparse file, which the port does not read.
    pub(crate) sparse: bool,
}

impl Header {
    /// `TarArchiveEntry(globalPaxHeaders, headerBuf, encoding, false)`.
    pub(crate) fn parse(header: &[u8], global: &PaxHeaders) -> Result<Header, Unread> {
        let mut name = parse_name(&header[NAME]);
        for field in [MODE, UID, GID] {
            parse_octal_or_binary(&header[field])?;
        }
        let size = parse_octal_or_binary(&header[SIZE])?;
        if size < 0 {
            return Err(throws("broken archive, entry with negative size"));
        }
        let mtime = parse_octal_or_binary(&header[MTIME])?;
        let checksum_ok = checksum_ok(header)?;
        let link_flag = header[LINK_FLAG];
        let user_name = parse_name(&header[USER_NAME]);
        let group_name = parse_name(&header[GROUP_NAME]);
        if matches!(link_flag, LF_CHR | LF_BLK) {
            parse_octal_or_binary(&header[DEV_MAJOR])?;
            parse_octal_or_binary(&header[DEV_MINOR])?;
        }
        let mut extended = false;
        let magic = &header[MAGIC];
        if magic == b"ustar " {
            parse_octal_or_binary(&header[GNU_ATIME])?;
            parse_octal_or_binary(&header[GNU_CTIME])?;
            for sparse in 0..4 {
                let at = GNU_SPARSE + sparse * 24;
                if parse_octal_or_binary(&header[at..at + 12])? < 0
                    || parse_octal_or_binary(&header[at + 12..at + 24])? < 0
                {
                    return Err(throws("Corrupted TAR archive, sparse entry is invalid"));
                }
            }
            extended = header[GNU_IS_EXTENDED] == 1;
            parse_octal(&header[GNU_REAL_SIZE])?;
        } else if magic == b"ustar\0" && is_xstar(global, header) {
            let prefix = parse_name(&header[XSTAR_PREFIX]);
            parse_octal_or_binary(&header[XSTAR_ATIME])?;
            parse_octal_or_binary(&header[XSTAR_CTIME])?;
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        } else {
            let prefix = parse_name(&header[PREFIX]);
            if link_flag == LF_DIR && !name.ends_with('/') {
                name.push('/');
            }
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        Ok(Header {
            name,
            user_name,
            group_name,
            size,
            modified_millis: mtime.saturating_mul(1000),
            checksum_ok,
            link_flag,
            extended,
            sparse: false,
        })
    }

    fn is_pax(&self) -> bool {
        LF_PAX.contains(&self.link_flag)
    }

    fn is_global_pax(&self) -> bool {
        self.link_flag == LF_PAX_GLOBAL
    }

    pub(crate) fn is_directory(&self) -> bool {
        self.link_flag == LF_DIR
            || (!self.is_pax() && !self.is_global_pax() && self.name.ends_with('/'))
    }

    /// `processPaxHeader` of each header, sparse files aside.
    fn apply(&mut self, headers: &PaxHeaders) -> Result<(), Unread> {
        for (key, value) in headers {
            match key.as_str() {
                "path" => self.name = normalize(value),
                "gid" | "uid" => {
                    parse_java_long(value)?;
                }
                "gname" => self.group_name.clone_from(value),
                "uname" => self.user_name.clone_from(value),
                "size" => {
                    let size = parse_java_long(value)?;
                    if size < 0 {
                        return Err(throws("Corrupted TAR archive. Entry size is negative"));
                    }
                    self.size = size;
                }
                "mtime" => self.modified_millis = decimal_seconds_millis(value)?,
                "atime" | "ctime" | "LIBARCHIVE.creationtime" => {
                    decimal_seconds_millis(value)?;
                }
                "SCHILY.devminor" | "SCHILY.devmajor" => {
                    let number = parse_java_long(value)?;
                    if i32::try_from(number).is_err() {
                        return Err(throws(format!(
                            "Unable to parse int from string value: {value}"
                        )));
                    }
                    if number < 0 {
                        return Err(throws("Corrupted TAR archive. Device number is negative"));
                    }
                }
                "SCHILY.filetype" if value == "sparse" => return Err(Unread::Sparse),
                key if key.starts_with("GNU.sparse.") => return Err(Unread::Sparse),
                _ => {}
            }
        }
        Ok(())
    }
}

/// `Long.parseLong` of a PAX value.
fn parse_java_long(value: &str) -> Result<i64, Unread> {
    if !value.is_ascii() {
        return Err(Unread::NonAsciiNumber);
    }
    parse_long(value, 10)
        .ok_or_else(|| throws(format!("Unable to parse long from string value: {value}")))
}

/// `parseInstantFromDecimalSeconds` and `FileTime.toMillis`: the milliseconds of a PAX time,
/// at most 19 digits before and after the point, within `Instant`'s range.
fn decimal_seconds_millis(value: &str) -> Result<i64, Unread> {
    let invalid = || {
        throws(format!(
            "Corrupted PAX header. Time field value is invalid '{value}'"
        ))
    };
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };
    let digits =
        |part: &str| (1..=19).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || fraction.is_some_and(|fraction| !digits(fraction)) {
        return Err(invalid());
    }
    let sign: i128 = if negative { -1 } else { 1 };
    let seconds = sign * whole.parse::<i128>().map_err(|_| invalid())?;
    let seconds = i128::from(seconds as i64);
    let nanos = fraction.map_or(0, |fraction| {
        let nine: String = fraction
            .chars()
            .chain(std::iter::repeat('0'))
            .take(9)
            .collect();
        sign * nine.parse::<i128>().expect("nine digits")
    });
    let epoch_seconds = seconds + nanos.div_euclid(1_000_000_000);
    let nano = nanos.rem_euclid(1_000_000_000);
    if !INSTANT_SECONDS.contains(&epoch_seconds) {
        return Err(invalid());
    }
    let millis = epoch_seconds * 1000;
    if millis > i128::from(i64::MAX) || millis < i128::from(i64::MIN) {
        return Ok(if epoch_seconds < 0 {
            i64::MIN
        } else {
            i64::MAX
        });
    }
    Ok((millis + nano / 1_000_000) as i64)
}

/// A file of the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) header: Header,
    /// Where its data lies in the archive.
    data: Range<usize>,
}

/// A `TarArchiveInputStream` over the whole input.
pub(crate) struct TarStream<'a> {
    data: &'a [u8],
    position: usize,
    at_eof: bool,
    current: Option<Header>,
    /// How much of the current entry's data has been read.
    entry_offset: usize,
    entry_size: i64,
    global: PaxHeaders,
}

impl<'a> TarStream<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        TarStream {
            data,
            position: 0,
            at_eof: false,
            current: None,
            entry_offset: 0,
            entry_size: 0,
            global: PaxHeaders::new(),
        }
    }

    /// `getNextEntry`: `None` at the end of the archive.
    pub(crate) fn next_entry(&mut self) -> Result<Option<Entry>, Unread> {
        if !self.next()? {
            return Ok(None);
        }
        let header = self.current.clone().expect("an entry was read");
        let size = usize::try_from(header.size).map_err(|_| throws("an entry is too large"))?;
        let start = self.position;
        Ok(Some(Entry {
            header,
            data: start..start.saturating_add(size),
        }))
    }

    /// The data of `entry`, which must be the entry last read, as reading it to its end gives
    /// it, failing where the archive cuts it short.
    pub(crate) fn content(&self, entry: &Entry) -> Result<&'a [u8], Unread> {
        self.data
            .get(entry.data.clone())
            .ok_or_else(|| throws("Truncated TAR archive"))
    }

    /// `getNextTarEntry`: whether an entry was read into `current`.
    fn next(&mut self) -> Result<bool, Unread> {
        if self.at_eof {
            return Ok(false);
        }
        if self.current.is_some() {
            self.skip_rest()?;
            self.skip_record_padding()?;
        }
        let Some(record) = self.record() else {
            self.current = None;
            return Ok(false);
        };
        let header = Header::parse(record, &self.global)?;
        self.entry_offset = 0;
        self.entry_size = header.size;
        let long_link = header.link_flag == LF_LONG_LINK;
        let long_name = header.link_flag == LF_LONG_NAME;
        self.current = Some(header);
        if long_link && self.long_name_data()?.is_none() {
            return Ok(false);
        }
        if long_name {
            let Some(data) = self.long_name_data()? else {
                return Ok(false);
            };
            let current = self
                .current
                .as_mut()
                .expect("an entry follows the long name");
            let name = decode_name(&data);
            current.name = normalize(&name);
            if current.is_directory() && !name.ends_with('/') {
                current.name = normalize(&format!("{name}/"));
            }
        }
        let current = self.current.as_ref().expect("an entry was read");
        if current.is_global_pax() {
            self.global = self.pax_headers_of_current()?;
            if !self.next()? {
                return Err(throws("Error detected parsing the pax header"));
            }
        }
        let current = self.current.as_ref().expect("an entry was read");
        if current.is_pax() {
            let headers = self.pax_headers_of_current()?;
            if !self.next()? {
                return Err(throws(
                    "premature end of tar archive. Didn't find any entry after PAX header.",
                ));
            }
            self.current_mut().apply(&headers)?;
        } else if !self.global.is_empty() {
            let global = self.global.clone();
            self.current_mut().apply(&global)?;
        }
        let current = self.current_mut();
        if current.link_flag == LF_OLD_GNU_SPARSE {
            current.sparse = true;
            if current.extended {
                if self.record().is_none() {
                    return Err(throws(
                        "premature end of tar archive. Didn't find extended_header after header \
                         with extended flag.",
                    ));
                }
                return Err(Unread::Sparse);
            }
        }
        self.entry_size = self.current_mut().size;
        Ok(true)
    }

    fn current_mut(&mut self) -> &mut Header {
        self.current.as_mut().expect("an entry was read")
    }

    /// `readRecord` and `getRecord`: the next record, `None` at an all-zero record or the end
    /// of the input, after which the stream reads nothing more.
    fn record(&mut self) -> Option<&'a [u8]> {
        let record = self.data.get(self.position..self.position + RECORD);
        self.position = (self.position + RECORD).min(self.data.len());
        match record {
            Some(record) if record.iter().any(|&b| b != 0) => Some(record),
            _ => {
                self.at_eof = true;
                None
            }
        }
    }

    /// What reading the rest of the current entry's data takes: none of a directory's.
    fn skip_rest(&mut self) -> Result<(), Unread> {
        let current = self.current.as_ref().expect("an entry was read");
        if current.is_directory() {
            return Ok(());
        }
        let size = usize::try_from(current.size).map_err(|_| throws("an entry is too large"))?;
        let rest = size - self.entry_offset.min(size);
        if self.data.len() - self.position < rest {
            return Err(throws("Truncated TAR archive"));
        }
        self.position += rest;
        self.entry_offset += rest;
        Ok(())
    }

    /// `skipRecordPadding`: the rest of the last record of a file's data.
    fn skip_record_padding(&mut self) -> Result<(), Unread> {
        let current = self.current.as_ref().expect("an entry was read");
        let size = usize::try_from(self.entry_size).map_err(|_| throws("an entry is too large"))?;
        if current.is_directory() || size == 0 || size % RECORD == 0 {
            return Ok(());
        }
        let padding = RECORD - size % RECORD;
        if self.data.len() - self.position < padding {
            return Err(throws("Truncated TAR archive"));
        }
        self.position += padding;
        Ok(())
    }

    /// The current entry's data read to its end, as `read` gives it.
    fn read_all(&mut self) -> Result<&'a [u8], Unread> {
        let current = self.current.as_ref().expect("an entry was read");
        let size = usize::try_from(current.size).map_err(|_| throws("an entry is too large"))?;
        if current.is_directory() {
            return Ok(&[]);
        }
        let rest = size - self.entry_offset.min(size);
        let data = self
            .data
            .get(self.position..self.position + rest)
            .ok_or_else(|| throws("Truncated TAR archive"))?;
        self.position += rest;
        self.entry_offset += rest;
        Ok(data)
    }

    /// `getLongNameData`: the long name read, then the entry it names, without its trailing
    /// NULs; `None` when no entry follows.
    fn long_name_data(&mut self) -> Result<Option<Vec<u8>>, Unread> {
        let data = self.read_all()?.to_vec();
        if !self.next()? {
            return Ok(None);
        }
        let length = data.len() - data.iter().rev().take_while(|&&b| b == 0).count();
        Ok(Some(data[..length].to_vec()))
    }

    /// `TarUtils.parsePaxHeaders` over the current entry's data, on top of the global headers.
    fn pax_headers_of_current(&mut self) -> Result<PaxHeaders, Unread> {
        let header_size = self.entry_size;
        let data = self.read_all()?;
        parse_pax_headers(data, &self.global, header_size)
    }
}

/// `TarUtils.parsePaxHeaders`: `length keyword=value\n` records, an empty value removing the
/// keyword, read until the data ends.
fn parse_pax_headers(
    data: &[u8],
    global: &PaxHeaders,
    header_size: i64,
) -> Result<PaxHeaders, Unread> {
    let mut headers = global.clone();
    let mut bytes = data.iter().copied();
    let mut total_read: i32 = 0;
    let mut sparse_offset: Option<i64> = None;
    loop {
        let mut length: i32 = 0;
        let mut read: i32 = 0;
        let mut ch: Option<u8>;
        loop {
            ch = bytes.next();
            let Some(c) = ch else { break };
            read = read.wrapping_add(1);
            total_read = total_read.wrapping_add(1);
            if c == b'\n' {
                break;
            }
            if c == b' ' {
                let mut keyword = Vec::new();
                loop {
                    ch = bytes.next();
                    let Some(c) = ch else { break };
                    read = read.wrapping_add(1);
                    total_read = total_read.wrapping_add(1);
                    if total_read < 0 || (header_size >= 0 && i64::from(total_read) >= header_size)
                    {
                        break;
                    }
                    if c == b'=' {
                        let keyword = String::from_utf8_lossy(&keyword).into_owned();
                        let rest_length = length.wrapping_sub(read);
                        if rest_length <= 1 {
                            headers.remove(&keyword);
                        } else if header_size >= 0
                            && i64::from(rest_length) > header_size - i64::from(total_read)
                        {
                            return Err(throws("a PAX header value exceeds the header record"));
                        } else {
                            let rest_length = rest_length as usize;
                            let rest: Vec<u8> = bytes.by_ref().take(rest_length).collect();
                            if rest.len() != rest_length {
                                return Err(throws("Failed to read Paxheader"));
                            }
                            total_read = total_read.wrapping_add(rest_length as i32);
                            if rest[rest_length - 1] != b'\n' {
                                return Err(throws(
                                    "Failed to read Paxheader.Value should end with a newline",
                                ));
                            }
                            let value =
                                String::from_utf8_lossy(&rest[..rest_length - 1]).into_owned();
                            check_pax_sparse_struct(&keyword, &value, &mut sparse_offset)?;
                            headers.insert(keyword, value);
                        }
                        break;
                    }
                    keyword.push(c);
                }
                break;
            }
            if !c.is_ascii_digit() {
                return Err(throws(
                    "Failed to read Paxheader. Encountered a non-number while reading length",
                ));
            }
            length = length.wrapping_mul(10).wrapping_add(i32::from(c - b'0'));
        }
        if ch.is_none() {
            return Ok(headers);
        }
    }
}

/// The checks `parsePaxHeaders` makes of the GNU sparse 0.0 offsets and sizes.
fn check_pax_sparse_struct(
    keyword: &str,
    value: &str,
    offset: &mut Option<i64>,
) -> Result<(), Unread> {
    match keyword {
        "GNU.sparse.offset" => {
            let parsed = parse_java_long(value)?;
            if parsed < 0 {
                return Err(throws("GNU.sparse.offset contains negative value"));
            }
            *offset = Some(parsed);
        }
        "GNU.sparse.numbytes" => {
            if offset.is_none() {
                return Err(throws(
                    "GNU.sparse.offset is expected before GNU.sparse.numbytes shows up.",
                ));
            }
            if parse_java_long(value)? < 0 {
                return Err(throws("GNU.sparse.numbytes contains negative value"));
            }
            *offset = None;
        }
        _ => {}
    }
    Ok(())
}

/// The COMPRESS-117 check of `ArchiveStreamFactory.detect`: `header`, a first record, read as an
/// archive of its own whose first entry, past at most ten sound directories, is a sound file
/// with ASCII names, or is a sound directory.
pub(crate) fn is_tar_by_entries(header: &[u8]) -> bool {
    let mut stream = TarStream::new(header);
    let Ok(mut entry) = stream.next_entry() else {
        return false;
    };
    let mut count = 0;
    while let Some(found) = &entry {
        if !(found.header.is_directory() && found.header.checksum_ok && count < 10) {
            break;
        }
        count += 1;
        let Ok(next) = stream.next_entry() else {
            return false;
        };
        entry = next;
    }
    let ascii = |name: &str| name.chars().all(|c| (' '..'\u{80}').contains(&c));
    entry.is_some_and(|entry| {
        entry.header.checksum_ok
            && !entry.header.is_directory()
            && ascii(&entry.header.group_name)
            && ascii(&entry.header.name)
            && ascii(&entry.header.user_name)
    }) || count > 0
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A header of `name`, `flag` and `size` with the POSIX magic, a modification time of
    /// 1700000000 and a sound checksum.
    pub(crate) fn header(name: &[u8], flag: u8, size: usize) -> Vec<u8> {
        let mut header = vec![0u8; RECORD];
        header[..name.len()].copy_from_slice(name);
        header[MODE].copy_from_slice(b"0000644\0");
        header[UID].copy_from_slice(b"0000000\0");
        header[GID].copy_from_slice(b"0000000\0");
        header[SIZE].copy_from_slice(format!("{size:011o}\0").as_bytes());
        header[MTIME].copy_from_slice(format!("{:011o}\0", 1_700_000_000).as_bytes());
        header[LINK_FLAG] = flag;
        header[MAGIC].copy_from_slice(b"ustar\0");
        header[VERSION].copy_from_slice(b"00");
        header[CHECKSUM].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[CHECKSUM].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        header
    }

    /// An archive of `entries`, each a name, a type flag and the data, ended by two zero
    /// records.
    pub(crate) fn tar(entries: &[(&[u8], u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, flag, data) in entries {
            out.extend_from_slice(&header(name, *flag, data.len()));
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(RECORD) * RECORD, 0);
        }
        out.resize(out.len() + 2 * RECORD, 0);
        out
    }

    /// PAX records of `headers`.
    pub(crate) fn pax(headers: &[(&str, &str)]) -> Vec<u8> {
        let mut out = String::new();
        for (key, value) in headers {
            let body = format!(" {key}={value}\n");
            let mut length = body.len() + 1;
            while format!("{length}{body}").len() != length {
                length += 1;
            }
            out.push_str(&format!("{length}{body}"));
        }
        out.into_bytes()
    }

    fn entries(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, Unread> {
        let mut stream = TarStream::new(data);
        let mut found = Vec::new();
        while let Some(entry) = stream.next_entry()? {
            let content = if entry.header.is_directory() {
                Vec::new()
            } else {
                stream.content(&entry)?.to_vec()
            };
            found.push((entry.header.name.clone(), content));
        }
        Ok(found)
    }

    #[test]
    fn reads_pax_paths_and_gnu_long_names() {
        let long = "d/".repeat(60) + "long.txt";
        let data = tar(&[
            (b"dir/", b'5', b""),
            (
                b"././@PaxHeader",
                b'x',
                &pax(&[("path", "/abs/pax.txt"), ("mtime", "1.5")]),
            ),
            (b"short", b'0', b"pax data"),
            (b"././@LongLink", b'L', format!("{long}\0").as_bytes()),
            (b"cut", b'0', b"long data"),
        ]);
        assert_eq!(
            entries(&data).unwrap(),
            [
                ("dir/".to_owned(), Vec::new()),
                ("abs/pax.txt".to_owned(), b"pax data".to_vec()),
                (long, b"long data".to_vec()),
            ]
        );
    }

    #[test]
    fn applies_global_headers_over_local_ones() {
        let data = tar(&[
            (b"global", b'g', &pax(&[("path", "global.txt")])),
            (b"local", b'x', &pax(&[("path", "local.txt")])),
            (b"file", b'0', b"data"),
        ]);
        assert_eq!(
            entries(&data).unwrap(),
            [("global.txt".to_owned(), b"data".to_vec())]
        );
    }

    #[test]
    fn reads_the_next_header_from_a_directory_s_data() {
        let mut data = header(b"dir/", b'5', RECORD);
        data.extend_from_slice(&tar(&[(b"inside.txt", b'0', b"hidden")]));
        assert_eq!(
            entries(&data).unwrap(),
            [
                ("dir/".to_owned(), Vec::new()),
                ("inside.txt".to_owned(), b"hidden".to_vec())
            ]
        );
    }

    #[test]
    fn fails_where_commons_compress_fails() {
        let data = tar(&[(b"a.txt", b'0', b"content"), (b"b.txt", b'0', b"more")]);
        assert!(entries(&data[..RECORD + 3]).is_err());
        assert!(entries(&data[..3 * RECORD + 2]).is_err());
        assert_eq!(entries(&data[..2 * RECORD + 100]).unwrap().len(), 1);
        let mut bad = data.clone();
        bad[SIZE.start] = b'9';
        assert!(entries(&bad).is_err());
        let bad_time = tar(&[
            (b"pax", b'x', &pax(&[("mtime", "12a")])),
            (b"file", b'0', b"data"),
        ]);
        assert!(entries(&bad_time).is_err());
        let sparse = tar(&[(b"sparse", b'S', b"")]);
        let entry = TarStream::new(&sparse).next_entry().unwrap().unwrap();
        assert!(entry.header.sparse);
    }

    #[test]
    fn tells_a_tar_by_its_first_entry_as_commons_compress_does() {
        let mut old = header(b"file.txt", b'0', 0);
        old[MAGIC].fill(0);
        old[VERSION].fill(0);
        let sum: u32 = old
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if CHECKSUM.contains(&i) {
                    32
                } else {
                    u32::from(b)
                }
            })
            .sum();
        old[CHECKSUM].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        assert!(is_tar_by_entries(&old));
        let mut latin1 = old.clone();
        latin1[0] = 0xE9;
        let sum: u32 = latin1
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if CHECKSUM.contains(&i) {
                    32
                } else {
                    u32::from(b)
                }
            })
            .sum();
        latin1[CHECKSUM].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        assert!(is_tar_by_entries(&latin1));
        assert!(is_tar_by_entries(&header(b"dir/", b'5', 512)));
        assert!(!is_tar_by_entries(&[0u8; RECORD]));
        assert!(!is_tar_by_entries(&header(b"pax", b'x', 10)));
    }
}
