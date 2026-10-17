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

use super::address::{flatten, parse_address_list, Address};
use super::date::parse_date;
use super::handler::set_name_and_email;
use super::structured::ContentType;
use super::Unported;
use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::line_ranges;
use crate::formats::embedded::{Wrapping, EXCEPTION_WARNING};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
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
    lines: Vec<&'a [u8]>,
    metadata: TikaMetadata,
    cut: bool,
}

impl Message<'_> {
    /// The bytes the extractor is handed.
    fn bytes(&self) -> Vec<u8> {
        let length = self.lines.iter().map(|line| line.len() + 1).sum();
        let mut bytes = Vec::with_capacity(length);
        for line in &self.lines {
            bytes.extend_from_slice(line);
            bytes.push(b'\n');
        }
        bytes
    }

    /// How many bytes of the mailbox the message takes, near enough.
    fn size(&self) -> usize {
        self.lines.iter().map(|line| line.len() + 1).sum()
    }
}

/// The text of a line of a mailbox: in `charset` where it is given and decodes, else UTF-8
/// where the line is, else windows-1252.
fn decode_line(line: &[u8], charset: Option<Charset>) -> String {
    if let Some(text) = charset.and_then(|charset| charset.decode_exactly(line)) {
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
fn messages(input: &[u8]) -> Option<Messages<'_, impl Iterator<Item = &[u8]>>> {
    let mut lines = line_ranges(input).map(|range| &input[range]);
    let current = lines.next()?;
    Some(Messages {
        lines,
        current: Some(current),
    })
}

/// The messages of a mailbox, read from its lines as they are asked for.
struct Messages<'a, I: Iterator<Item = &'a [u8]>> {
    lines: I,
    /// The line reading goes on from.
    current: Option<&'a [u8]>,
}

impl<'a, I: Iterator<Item = &'a [u8]>> Iterator for Messages<'a, I> {
    type Item = Result<Message<'a>, Unported>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let current = self.current?;
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
            let Some(mut line) = self.lines.next() else {
                self.current = None;
                return None;
            };
            // A `From ` line right after another starts the next message: Tika takes it for
            // the first line of this one.
            if line.starts_with(RECORD_DIVIDER) {
                self.current = Some(line);
                continue;
            }
            let mut in_header = true;
            let mut headers: Vec<Vec<u8>> = Vec::new();
            let mut lines = Vec::new();
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
                lines.push(line);
                size += line.len() + 1;
                match self.lines.next() {
                    Some(next) if !next.starts_with(RECORD_DIVIDER) => {
                        if size >= MAIL_MAX_SIZE {
                            break (
                                self.lines.find(|line| line.starts_with(RECORD_DIVIDER)),
                                true,
                            );
                        }
                        line = next;
                    }
                    next => break (next, false),
                }
            };
            self.current = next;
            let charset = declared_charset(&headers);
            for header in &headers {
                if let Err(unported) = save(&mut metadata, &decode_line(header, charset)) {
                    return Some(Err(unported));
                }
            }
            return Some(Ok(Message {
                lines,
                metadata,
                cut,
            }));
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
pub(crate) struct MboxParser;

impl Parser for MboxParser {
    fn name(&self) -> &'static str {
        "extractous::MboxParser"
    }

    #[cfg(test)]
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
    ) -> Result<Prepared<'a>, Declined> {
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
    ) -> Result<Prepared<'a>, Declined> {
        let paging = Paging::new(pages);
        for message in messages(input).into_iter().flatten() {
            let message =
                message.map_err(|unported| Declined::Deferred(unported.reason().to_owned()))?;
            paging.read(message.size());
        }
        paging.release();
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set("Content-Type", "application/mbox");
            metadata.set("Content-Encoding", CHARSET);
            let messages = messages(input).ok_or_else(|| {
                Failure::Malformed("The mailbox has no first line to read".to_owned())
            })?;
            for (index, message) in messages.enumerate() {
                let message = message.map_err(|unported| {
                    Failure::Malformed(format!(
                        "Message {} of the mailbox reads differently the second time: {}",
                        index + 1,
                        unported.reason()
                    ))
                })?;
                paging.read(message.size());
                if message.cut {
                    xhtml.metadata_mut().add(
                        EXCEPTION_WARNING,
                        format!(
                            "Message {} of the mailbox was cut at {MAIL_MAX_SIZE} bytes",
                            index + 1
                        ),
                    );
                }
                xhtml.parse_embedded(
                    &message.bytes(),
                    &message.metadata,
                    Wrapping::PackageEntry,
                )?;
            }
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "org.apache.tika.parser.mbox.MboxParser";

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
        let messages: Vec<Message> = messages(mailbox)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
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
        assert!(messages[0].bytes().ends_with(b"\nBody \x81.\n"));
        assert!(!messages[0].bytes().starts_with(RECORD_DIVIDER));
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

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_mailboxes_as_tikas_path_does() {
        let scratch = Scratch::new("mbox");
        let mailbox = b"From ada@example.org Mon Jan  1 00:00:00 2024\r\n\
From: Ada Lovelace <ada@example.org>\r\n\
To: Bob <bob@example.org>\r\n\
Cc: carol@example.org\r\n\
Subject: First\r\n\
  continued subject\r\n\
Date: Mon, 1 Jan 2024 10:00:00 +0100\r\n\
Message-Id: <1@example.org>\r\n\
X-Custom: one\r\n\
X-Custom: two\r\n\
\r\n\
Body of the first message, caf\xe9.\r\n\
From bob@example.org Tue Jan  2 00:00:00 2024\n\
From: Bob <bob@example.org>\n\
Subject: Second\n\
\n\
Body of the second message.\n\
>From the quoted line.\n\
From carol@example.org Wed Jan  3 00:00:00 2024\n";
        let leading = b"From eve@example.org Wed Jan  3 00:00:00 2024\n\
From dave@example.org Thu Jan  4 00:00:00 2024\n\
 continuation first\n\
Subject: Leading\n\
\n\
Body.";
        let read = [
            scratch.write("mailbox.mbox", mailbox),
            scratch.write("leading.mbox", leading),
        ];
        let (parsed, deferred) = documents(&MboxParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
    }

    /// Mailboxes whose first message passes the write limit, in plain text and in HTML.
    #[cfg(feature = "tika")]
    fn past_the_limit(scratch: &Scratch) -> (PathBuf, Vec<PathBuf>) {
        let text = "word ".repeat(40);
        let html = format!("<html><body><p>{text}</p></body></html>");
        let second = "From c@d Mon Jan  1 00:00:00 2024\nSubject: y\n\nsecond body\n";
        let first = |headers: &str, body: &str| {
            format!("From a@b Mon Jan  1 00:00:00 2024\nSubject: x\n{headers}\n{body}\n{second}")
        };
        let alternative = format!(
            "--B\nContent-Type: text/plain\n\n{text}\n--B\nContent-Type: text/html\n\n{html}\n--B--"
        );
        (
            scratch.write("plain.mbox", first("", &text).as_bytes()),
            vec![
                scratch.write(
                    "html.mbox",
                    first("Content-Type: text/html\n", &html).as_bytes(),
                ),
                scratch.write(
                    "alternative.mbox",
                    first(
                        "Content-Type: multipart/alternative; boundary=\"B\"\n",
                        &alternative,
                    )
                    .as_bytes(),
                ),
            ],
        )
    }

    #[cfg(feature = "tika")]
    #[test]
    fn cuts_messages_at_the_limit_as_tikas_path_does() {
        let scratch = Scratch::new("mbox-limit");
        let (plain, html) = past_the_limit(&scratch);
        assert_same_as_tika(&[&plain], TIKA_NAME);
        assert_same_as_tika(
            &html.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
        let (xhtml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .set_extract_string_max_length(100)
            .extract_file_to_string(html[0].to_str().unwrap())
            .unwrap();
        assert!(xhtml.ends_with("word </p></div>"), "{xhtml}");
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_mailboxes_as_tikas_path_does() {
        let (parsed, deferred) = documents(&MboxParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 7);
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
