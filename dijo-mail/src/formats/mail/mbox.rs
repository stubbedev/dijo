//! Mailboxes in the mbox format, as Tika's `MboxParser` reads them: the input read line by
//! line; each message, from a `From ` line to the next, the `From ` line left out, handed to
//! the embedded document extractor as `message/rfc822`, its lines ending in a line feed, with
//! metadata from the `From ` line and the message's header lines.
//!
//! Where Tika slips, the port does not (#103):
//!
//! - every message's header lines are read; Tika reads the first message's alone, as it never
//!   starts reading a header again;
//! - a continuation line joins the header line it continues; Tika joins it to the oldest line
//!   waiting, or to the text `null` when none is;
//! - a message's bytes are handed on as they are, and its header lines decoded in the charset
//!   it declares, else as UTF-8, else as windows-1252; Tika reads the whole mailbox as
//!   windows-1252, so the bytes that charset leaves undefined reach the message as `?`;
//! - a message cut at 50,000,000 bytes, the lines up to the next `From ` line dropped, is
//!   reported in a warning; Tika cuts it silently.

use std::io::{self, BufWriter, Write};
use std::ops::Range;

use super::address::{flatten, parse_address_list, Address};
use super::date::parse_date;
use super::handler::set_name_and_email;
use super::structured::ContentType;
use super::Unported;
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::auto_detect_reader::line_ranges;
use crate::formats::embedded::{Wrapping, EXCEPTION_WARNING};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Difference, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Paging, Spool};
use crate::java::to_lower_case;
use crate::text::charset::Charset;

const RECORD_DIVIDER: &[u8] = b"From ";
const MAIL_MAX_SIZE: usize = 50_000_000;
const CHARSET: &str = "windows-1252";
const MESSAGE: &str = "message/rfc822";
const HEADER_PREFIX: &str = "mbox:";

/// A message of the mailbox: its lines, which the extractor is handed each ending in a line
/// feed, the metadata it is handed with them, and whether it was cut at the size limit.
struct Message<'a> {
    lines: Lines<'a>,
    /// How many bytes the extractor is handed.
    size: usize,
    metadata: TikaMetadata,
    cut: bool,
    /// The first header the port does not read as Tika does, which its metadata leaves out.
    unported: Option<Unported>,
}

/// The lines of a message in the mailbox.
enum Lines<'a> {
    /// Lines that each end in a line feed alone, with it: the bytes the extractor is handed, as
    /// they are.
    Ended(&'a [u8]),
    /// Lines from the first's start to the last's end, of which one ends otherwise, which the
    /// extractor is handed each ending in a line feed.
    Spanned(&'a [u8]),
}

impl<'a> Message<'a> {
    /// The bytes the extractor is handed: the mailbox's own, read in place, where each line
    /// ends in a line feed alone, else the lines spooled, each ending in one.
    fn bytes(&self, pages: Pages<'a>) -> Result<Input<'a>, Failure> {
        let span = match self.lines {
            Lines::Ended(bytes) => return Ok(Input::Within(bytes, pages)),
            Lines::Spanned(span) => span,
        };
        let unspooled = |error: io::Error| {
            Failure::Extraction(Error::IoError(format!(
                "spooling a message of the mailbox fails: {error}"
            )))
        };
        let mut spool = Spool::new(Some(self.size as u64));
        let mut out = BufWriter::new(&mut spool);
        for range in line_ranges(span) {
            out.write_all(&span[range]).map_err(unspooled)?;
            out.write_all(b"\n").map_err(unspooled)?;
        }
        out.flush().map_err(unspooled)?;
        drop(out);
        spool.finish().map_err(unspooled)
    }
}

/// The text of a line of a mailbox: in `charset` where it is given and decodes, else UTF-8
/// where the line is, else windows-1252.
fn decode_line(line: &[u8], charset: Option<Charset>) -> String {
    if let Some(text) = charset.and_then(|charset| charset.decode(line)) {
        return text;
    }
    match std::str::from_utf8(line) {
        Ok(text) => text.to_owned(),
        Err(_) => Charset::named(CHARSET)
            .decode(line)
            .expect("the JDK's windows-1252 decodes natively"),
    }
}

/// `line` without the characters up to a space at either end, as Java's `trim`.
fn trim_bytes(line: &[u8]) -> &[u8] {
    let start = line.iter().position(|&b| b > b' ').unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|&b| b > b' ')
        .map_or(start, |end| end + 1);
    &line[start..end]
}

/// The charset the header lines of a message declare in its `Content-Type`, if the JDK knows
/// it.
fn declared_charset(headers: &[Vec<u8>]) -> Option<Charset> {
    headers.iter().find_map(|line| {
        let text = String::from_utf8_lossy(line);
        let (name, value) = header(&text)?;
        if !name.eq_ignore_ascii_case("content-type") {
            return None;
        }
        Charset::for_name(ContentType::parse(value).parameter("charset")?)
    })
}

/// What `MboxParser.parse` hands the extractor, a message at a time, or `None` for an empty
/// input, on whose missing first line it fails.
fn messages(input: &[u8]) -> Option<Messages<'_, impl Iterator<Item = Range<usize>> + '_>> {
    let mut lines = line_ranges(input);
    let current = lines.next()?;
    Some(Messages {
        input,
        lines,
        current: Some(current),
    })
}

/// The messages of a mailbox, read from its lines as they are asked for.
struct Messages<'a, I: Iterator<Item = Range<usize>>> {
    input: &'a [u8],
    /// Where the lines lie in the input.
    lines: I,
    /// Where the line reading goes on from lies.
    current: Option<Range<usize>>,
}

impl<'a, I: Iterator<Item = Range<usize>>> Messages<'a, I> {
    /// The next line and where it lies, if any.
    fn next_line(&mut self) -> Option<(Range<usize>, &'a [u8])> {
        let range = self.lines.next()?;
        Some((range.clone(), &self.input[range]))
    }
}

impl<'a, I: Iterator<Item = Range<usize>>> Iterator for Messages<'a, I> {
    type Item = Message<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let input = self.input;
        loop {
            let current = &input[self.current.clone()?];
            if !current.starts_with(RECORD_DIVIDER) {
                self.current = self.lines.next();
                continue;
            }
            let mut metadata = TikaMetadata::new();
            metadata.add_prefixed(
                HEADER_PREFIX,
                "from",
                decode_line(&current[RECORD_DIVIDER.len()..], None),
            );
            metadata.set("Content-Type", MESSAGE);
            metadata.set("tk:content-type-parser-override", MESSAGE);
            let Some((mut range, mut line)) = self.next_line() else {
                self.current = None;
                return None;
            };
            // A `From ` line right after another starts the next message: Tika takes it for
            // the first line of this one.
            if line.starts_with(RECORD_DIVIDER) {
                self.current = Some(range);
                continue;
            }
            let start = range.start;
            let mut in_header = true;
            let mut headers: Vec<Vec<u8>> = Vec::new();
            let mut size = 0;
            let (next, cut) = loop {
                if in_header && line.iter().all(|&byte| byte <= b' ') {
                    in_header = false;
                }
                if in_header {
                    match headers.last_mut() {
                        Some(last) if matches!(line.first(), Some(b' ' | b'\t')) => {
                            last.push(b' ');
                            last.extend_from_slice(trim_bytes(line));
                        }
                        // A continuation of no header continues nothing.
                        None if matches!(line.first(), Some(b' ' | b'\t')) => {}
                        _ => headers.push(line.to_vec()),
                    }
                }
                size += line.len() + 1;
                match self.next_line() {
                    Some((next_range, next)) if !next.starts_with(RECORD_DIVIDER) => {
                        if size >= MAIL_MAX_SIZE {
                            break (
                                self.lines
                                    .find(|line| input[line.clone()].starts_with(RECORD_DIVIDER)),
                                true,
                            );
                        }
                        (range, line) = (next_range, next);
                    }
                    next => break (next.map(|(range, _)| range), false),
                }
            };
            let end = range.end;
            let span = &input[start..end];
            let lines = if !span.contains(&b'\r') && input.get(end) == Some(&b'\n') {
                Lines::Ended(&input[start..=end])
            } else {
                Lines::Spanned(span)
            };
            self.current = next;
            let charset = declared_charset(&headers);
            let mut unported = None;
            for header in &headers {
                if let Err(header_unported) = save(&mut metadata, &decode_line(header, charset)) {
                    unported.get_or_insert(header_unported);
                }
            }
            return Some(Message {
                lines,
                size,
                metadata,
                cut,
                unported,
            });
        }
    }
}

/// `saveHeaderInMetadata`, the sender's name and email address taken from its first mailbox.
fn save(metadata: &mut TikaMetadata, line: &str) -> Result<(), Unported> {
    let Some((tag, content)) = header(line) else {
        return Ok(());
    };
    let tag = to_lower_case(tag);
    match tag.as_str() {
        "from" => {
            metadata.set("dc:creator", content);
            let mailboxes = flatten(parse_address_list(content)?);
            if let Some(Address::Mailbox(mailbox)) = mailboxes.first() {
                set_name_and_email(mailbox, metadata);
            }
        }
        "to" | "cc" | "bcc" => {
            if let Some(address) = bracketed_address(content) {
                metadata.add("message:recipient-address", address);
            } else if content.contains('@') {
                metadata.add("message:recipient-address", content);
            }
            metadata.add(&format!("message:{tag}"), content);
        }
        "subject" => metadata.add("dc:subject", content),
        "date" => {
            if let Some(date) = parse_date(content)? {
                metadata.set("dcterms:created", date.format());
            }
        }
        "message-id" => metadata.set("dc:identifier", content),
        "in-reply-to" => metadata.set("dc:relation", content),
        "content-type" => {
            metadata.set("Content-Type", content);
            metadata.set("dc:format", content);
        }
        _ => metadata.add_prefixed(HEADER_PREFIX, &tag, content),
    }
    Ok(())
}

/// The groups of `([^ ]+):[ \t]*(.*)` matching all of `line`: the name up to the last colon
/// before the first space, the value after the spaces and tabs that follow it.
fn header(line: &str) -> Option<(&str, &str)> {
    let run = line.find(' ').unwrap_or(line.len());
    let colon = line[..run].rfind(':').filter(|&colon| colon > 0)?;
    Some((
        &line[..colon],
        line[colon + 1..].trim_start_matches([' ', '\t']),
    ))
}

/// The group of the first match of `<(.*@.*)>`: from the first `<` to the last `>`, when an
/// `@` lies between them.
fn bracketed_address(content: &str) -> Option<&str> {
    let open = content.find('<')?;
    let close = content.rfind('>').filter(|&close| close > open)?;
    let inner = &content[open + 1..close];
    inner.contains('@').then_some(inner)
}

/// Tika's `MboxParser`.
pub struct MboxParser;

impl Parser for MboxParser {
    fn name(&self) -> &'static str {
        "dijo::MboxParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.mbox.MboxParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/mbox"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    /// Reads the mailbox's messages once to decide whether it extracts them, and again, a
    /// message at a time, as it writes them.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let paging = Paging::new(pages);
        let mut unported = None;
        for message in messages(input).into_iter().flatten() {
            unported = unported.or(message.unported);
            paging.read(message.size);
        }
        paging.release();
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set("Content-Type", "application/mbox");
            metadata.set("Content-Encoding", CHARSET);
            let messages = messages(input).ok_or_else(|| {
                Failure::Malformed("The mailbox has no first line to read".to_owned())
            })?;
            for (index, message) in messages.enumerate() {
                paging.read(message.size);
                if message.cut {
                    xhtml.metadata_mut().add(
                        EXCEPTION_WARNING,
                        format!(
                            "Message {} of the mailbox was cut at {MAIL_MAX_SIZE} bytes",
                            index + 1
                        ),
                    );
                }
                xhtml.parse_embedded_input(
                    &message.bytes(pages)?,
                    &message.metadata,
                    Wrapping::PackageEntry,
                )?;
            }
            Ok(())
        });
        match unported {
            None => Ok(reading),
            Some(unported) => Err(Decline::Differs {
                reading,
                difference: Difference::new(format!(
                    "{}, so the native reading leaves the header it is in out of the message's \
                     metadata",
                    unported.reason()
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn reads_header_lines_as_the_patterns_do() {
        assert_eq!(header("Subject: Re: hi"), Some(("Subject", "Re: hi")));
        assert_eq!(header("a:b:c d"), Some(("a:b", "c d")));
        assert_eq!(header("X-Tab:\t \tvalue"), Some(("X-Tab", "value")));
        assert_eq!(header(":value"), None);
        assert_eq!(header("no colon here: x"), None);
        assert_eq!(
            bracketed_address("Ada <ada@example.org>, Bob <bob@x>"),
            Some("ada@example.org>, Bob <bob@x")
        );
        assert_eq!(bracketed_address("<nobody> ada@example.org"), None);
    }

    #[test]
    fn reads_every_message_as_it_is() {
        let mailbox = b"From ada@example.org Mon Jan  1 00:00:00 2024\n\
From: Ada Lovelace <ada@example.org>\n\
Subject: First\n\
\t continued\n\
X-Other: x\n\
\n\
Body \x81.\n\
From nobody Mon Jan  1 00:00:00 2024\n\
From bob@example.org Tue Jan  2 00:00:00 2024\n\
From: \"Bob @ home\" <bob@example.org>\n\
Subject: Caf\xe9\n\
Content-Type: text/plain; charset=iso-8859-1\n\
Date: Tue, 2 Jan 2024 10:00:00 +0100\n\
\n\
Second.\n\
From carol@example.org Wed Jan  3 00:00:00 2024\n\
Subject: Caf\xc3\xa9\n\
\n\
Third.\n";
        let messages: Vec<Message> = messages(mailbox).unwrap().collect();
        assert!(messages.iter().all(|message| message.unported.is_none()));
        assert_eq!(messages.len(), 3);
        let get = |index: usize, name: &str| {
            messages[index]
                .metadata
                .get(name)
                .map(str::to_owned)
                .unwrap_or_default()
        };
        assert_eq!(get(0, "dc:subject"), "First continued");
        assert_eq!(get(0, "mbox:x-other"), "x");
        assert_eq!(get(0, "message:from-name"), "Ada Lovelace");
        assert_eq!(
            get(0, "mbox:from"),
            "ada@example.org Mon Jan  1 00:00:00 2024"
        );
        let bytes = messages[0].bytes(Pages::IN_MEMORY).unwrap();
        assert!(bytes.ends_with(b"\nBody \x81.\n"));
        assert!(!bytes.starts_with(RECORD_DIVIDER));
        assert_eq!(get(1, "dc:subject"), "Caf\u{e9}");
        assert_eq!(get(1, "message:from-name"), "Bob @ home");
        assert_eq!(get(1, "message:from-email"), "bob@example.org");
        assert_eq!(get(1, "dcterms:created"), "2024-01-02T09:00:00Z");
        assert_eq!(
            get(1, "mbox:from"),
            "bob@example.org Tue Jan  2 00:00:00 2024"
        );
        assert_eq!(get(2, "dc:subject"), "Caf\u{e9}");
        assert!(messages.iter().all(|message| !message.cut));
    }
}
