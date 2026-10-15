//! Mailboxes in the mbox format, as Tika's `MboxParser` reads them: the input read as
//! windows-1252 line by line; each message, from a `From ` line to the next, handed to the
//! embedded document extractor as `message/rfc822`, its lines ending in a line feed, with
//! metadata from the `From ` line and the message's header lines.
//!
//! Tika's quirks stay: the header lines are read for the first message alone, as the parser
//! never starts reading a header again; a continuation line joins the oldest header line
//! waiting, not the last, and the text `null` when none is; bytes windows-1252 leaves
//! undefined reach the message as `?`; a message stops at 50,000,000 bytes, and the lines up to
//! the next `From ` line are dropped.

use std::collections::VecDeque;

use super::date::parse_date_lenient;
use super::handler::set_person_and_email;
use super::Unported;
use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::line_ranges;
use crate::formats::embedded::Wrapping;
use crate::formats::handlers::{is_blank, utf16_length};
use crate::formats::java::trim;
use crate::formats::java_time::format_instant;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::mime::java::to_lower_case;
use crate::text::charset::Charset;

const RECORD_DIVIDER: &[u8] = b"From ";
const MAIL_MAX_SIZE: usize = 50_000_000;
const CHARSET: &str = "windows-1252";
const MESSAGE: &str = "message/rfc822";
const HEADER_PREFIX: &str = "mbox:";
/// The bytes the JDK's windows-1252 decoder reads as U+FFFD, which its encoder writes as `?`.
const UNDEFINED: [u8; 5] = [0x81, 0x8D, 0x8F, 0x90, 0x9D];
/// `Metadata.MAX_PREFIX_ROUTE_NAME_LENGTH`, in UTF-16 code units.
const MAX_HEADER_NAME_LENGTH: usize = 1024;
/// `Metadata.MAX_PREFIX_ROUTE_NAMES`.
const MAX_HEADER_NAMES: usize = 10_000;

/// A message of the mailbox: its bytes and the metadata the extractor is handed with them.
struct Message {
    bytes: Vec<u8>,
    metadata: TikaMetadata,
}

/// What `MboxParser.parse` hands the extractor, or `None` for an empty input, on whose missing
/// first line it fails.
fn messages(input: &[u8]) -> Result<Option<Vec<Message>>, Unported> {
    let charset = Charset::named(CHARSET);
    let decode = |bytes: &[u8]| {
        charset
            .decode(bytes)
            .expect("the JDK's windows-1252 decodes natively")
    };
    let mut lines = line_ranges(input).map(|range| &input[range]);
    let Some(mut current) = lines.next() else {
        return Ok(None);
    };
    let mut messages = Vec::new();
    let mut in_header = true;
    loop {
        if !current.starts_with(RECORD_DIVIDER) {
            match lines.next() {
                Some(next) => current = next,
                None => break,
            }
            continue;
        }
        let mut metadata = Headers::new();
        metadata.add_header("from", decode(&current[RECORD_DIVIDER.len()..]));
        metadata.metadata.set("Content-Type", MESSAGE);
        metadata
            .metadata
            .set("tk:content-type-parser-override", MESSAGE);
        let Some(mut line) = lines.next() else {
            break;
        };
        let mut waiting = VecDeque::new();
        let mut bytes = Vec::new();
        let next = loop {
            let text = decode(line);
            if in_header && is_blank(&text) {
                in_header = false;
            }
            if in_header {
                if text.starts_with([' ', '\t']) {
                    let oldest = waiting.pop_front().unwrap_or_else(|| "null".to_owned());
                    waiting.push_back(format!("{oldest} {}", trim(&text)));
                } else {
                    waiting.push_back(text);
                }
            }
            bytes.extend(line.iter().map(|&byte| {
                if UNDEFINED.contains(&byte) {
                    b'?'
                } else {
                    byte
                }
            }));
            bytes.push(b'\n');
            match lines.next() {
                Some(next) if !next.starts_with(RECORD_DIVIDER) && bytes.len() < MAIL_MAX_SIZE => {
                    line = next;
                }
                next => break next,
            }
        };
        for header in &waiting {
            metadata.save(header)?;
        }
        messages.push(Message {
            bytes,
            metadata: metadata.metadata,
        });
        match next {
            Some(next) => current = next,
            None => break,
        }
    }
    Ok(Some(messages))
}

/// A message's metadata, with the count of header names its `mbox:` prefix route has added.
struct Headers {
    metadata: TikaMetadata,
    names: usize,
}

impl Headers {
    fn new() -> Self {
        Self {
            metadata: TikaMetadata::new(),
            names: 0,
        }
    }

    /// `Metadata.add(EMAIL_HEADER, name, value)`: no blank or overlong name, and no new name
    /// past the most the route takes.
    fn add_header(&mut self, name: &str, value: String) {
        if is_blank(name) || utf16_length(name) > MAX_HEADER_NAME_LENGTH {
            return;
        }
        let key = format!("{HEADER_PREFIX}{name}");
        if self.metadata.values(&key).is_empty() {
            if self.names >= MAX_HEADER_NAMES {
                return;
            }
            self.names += 1;
        }
        self.metadata.add(&key, value);
    }

    /// `saveHeaderInMetadata`.
    fn save(&mut self, line: &str) -> Result<(), Unported> {
        let Some((tag, content)) = header(line) else {
            return Ok(());
        };
        let tag = to_lower_case(tag);
        let metadata = &mut self.metadata;
        match tag.as_str() {
            "from" => {
                metadata.set("dc:creator", content);
                set_person_and_email(content, metadata);
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
                if let Some(millis) = parse_date_lenient(content)? {
                    metadata.set("dcterms:created", format_instant(millis));
                }
            }
            "message-id" => metadata.set("dc:identifier", content),
            "in-reply-to" => metadata.set("dc:relation", content),
            "content-type" => {
                metadata.set("Content-Type", content);
                metadata.set("dc:format", content);
            }
            _ => self.add_header(&tag, content.to_owned()),
        }
        Ok(())
    }
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

    fn media_types(&self) -> &'static [&'static str] {
        &["application/mbox"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let messages =
            messages(input).map_err(|unported| Declined::Deferred(unported.reason().to_owned()))?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set("Content-Type", "application/mbox");
            metadata.set("Content-Encoding", CHARSET);
            let messages = messages.ok_or_else(|| {
                Failure::Malformed("The mailbox has no first line to read".to_owned())
            })?;
            for message in messages {
                xhtml.parse_embedded(&message.bytes, &message.metadata, Wrapping::PackageEntry)?;
            }
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_same_as_tika, assert_same_as_tika_but_body_end_leaked_at_limit, conformance_corpus,
        documents, Scratch,
    };

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
        let charset = Charset::named(CHARSET);
        let undefined: Vec<u8> = (0..=255u8)
            .filter(|&byte| charset.decode(&[byte]).unwrap() == "\u{FFFD}")
            .collect();
        assert_eq!(undefined, UNDEFINED);
    }

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
Body of the first message, caf\xe9 \x81.\r\n\
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

    #[test]
    fn cuts_messages_at_the_limit_as_tikas_path_does() {
        let scratch = Scratch::new("mbox-limit");
        let (plain, html) = past_the_limit(&scratch);
        assert_same_as_tika(&[&plain], TIKA_NAME);
        assert_same_as_tika_but_body_end_leaked_at_limit(
            &html.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
    }

    /// A mailbox whose first message's HTML part passes the write limit.
    const HTML_PAST_THE_LIMIT: &str = "testMBOX_complex.mbox";

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_mailboxes_as_tikas_path_does() {
        let (parsed, deferred) = documents(&MboxParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 7);
        let (html, rest): (Vec<&Path>, Vec<&Path>) = parsed
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| path.ends_with(HTML_PAST_THE_LIMIT));
        assert_eq!(html.len(), 1);
        assert_same_as_tika(&rest, TIKA_NAME);
        assert_same_as_tika_but_body_end_leaked_at_limit(&html, TIKA_NAME);
    }
}
