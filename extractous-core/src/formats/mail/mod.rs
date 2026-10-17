//! Email as Tika 4.1's `RFC822Parser` reads it, the parser extractous maps `message/rfc822` to,
//! and Apple Mail's `.emlx` as our Java `EmlxParser` hands its message to that parser:
//!
//! - an `.emlx` framing line and property list are dropped as `RFC822Parser` drops them;
//! - a port of mime4j 0.8.15's stream parser reads the header fields and parts ([`stream`]),
//!   their structured fields as its strict and lenient field parsers read them ([`field`],
//!   [`structured`], [`address`]), encoded words and bodies decoded as its codecs decode them
//!   ([`codec`]);
//! - a port of Tika's `MailContentHandler` makes metadata of the message's headers, its dates
//!   read in the forms `MailDateParser` reads ([`date`]), and of its parts the message's body
//!   ([`handler`]): text and HTML parsed inline by the native text and HTML parsers, other parts
//!   handed to the embedded document extractor.
//!
//! Where mime4j and Tika slip, the port reads mail as the RFCs say instead: the modules name
//! each such place with the issue that records Tika's behaviour.
//!
//! A message defers to Tika when it needs a charset the native decoders do not decode, names a
//! time zone java.time would look up in its time-zone database, or holds a text or HTML part
//! the native parsers of those defer.

mod address;
mod codec;
mod date;
mod field;
mod handler;
mod mbox;
#[cfg(test)]
mod reference;
mod stream;
mod structured;
#[cfg(test)]
mod synthetic;

pub(crate) use codec::decode_encoded_words;
pub(crate) use date::{parse_date, MailDate};
use handler::{read, Inline, Item, Problem};
pub(crate) use mbox::MboxParser;

use crate::config::ParserConfigs;
use crate::formats::embedded::Wrapping;
use crate::formats::handlers::parse_inline;
use crate::formats::html;
use crate::formats::metadata::TikaMetadata;
use crate::formats::text;
use crate::formats::{Declined, Deferral, Failure, Parser, Prepared};
use crate::java::parse_long;

/// What the port leaves to Tika.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unported {
    /// A charset the JDK knows and the native decoders do not decode.
    Charset,
    /// A time zone named by a region or a name, which java.time resolves with its time-zone
    /// database.
    TimeZone,
}

impl Unported {
    fn reason(self) -> &'static str {
        match self {
            Unported::Charset => "the message needs a charset the native decoders do not decode",
            Unported::TimeZone => {
                "a date of the message names a time zone java.time looks up in its database"
            }
        }
    }
}

/// Tika's `RFC822Parser`.
pub(crate) struct Rfc822Parser;

/// Our Java `EmlxParser`: Apple Mail's `.emlx`, a line with the message's length in bytes, the
/// message, and a property list, its message read by `RFC822Parser`.
pub(crate) struct EmlxParser;

impl Parser for Rfc822Parser {
    fn name(&self) -> &'static str {
        "extractous::RFC822Parser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.mail.RFC822Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["message/rfc822"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare_message(Ok(input), metadata).map_err(Declined::Deferred)
    }
}

impl Parser for EmlxParser {
    fn name(&self) -> &'static str {
        "extractous::EmlxParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "ai.yobix.EmlxParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["message/x-emlx"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare_message(emlx_message(input), metadata).map_err(Declined::Deferred)
    }
}

/// The digits of an `.emlx` length line that `EmlxParser` reads at most, past which it gives up.
const MAX_LENGTH_DIGITS: usize = 19;

/// `EmlxParser.parse`'s message: the bytes after the first line, at most as many as the line
/// says, a negative count none at all.
fn emlx_message(input: &[u8]) -> Result<&[u8], Failure> {
    let malformed =
        || Failure::Malformed("The .emlx file does not start with the message's length".into());
    let mut digits = Vec::new();
    let mut at = 0;
    loop {
        let byte = *input.get(at).ok_or_else(malformed)?;
        at += 1;
        if byte == b'\n' {
            break;
        }
        if digits.len() > MAX_LENGTH_DIGITS {
            return Err(malformed());
        }
        if byte != b'\r' && byte != b' ' {
            digits.push(byte);
        }
    }
    let digits: String = digits
        .iter()
        .map(|&byte| {
            if byte.is_ascii() {
                char::from(byte)
            } else {
                '\u{FFFD}'
            }
        })
        .collect();
    let length = parse_long(&digits).ok_or_else(malformed)?;
    let rest = &input[at..];
    Ok(match usize::try_from(length) {
        Ok(length) => &rest[..length.min(rest.len())],
        Err(_) => rest,
    })
}

/// The longest `.emlx` count line `RFC822Parser` looks for: digits, Apple Mail's padding to ten
/// columns, and the line feed.
const EMLX_COUNT_LINE_MAX: usize = 11;
const PLIST_START: &[u8] = b"<?xml";

/// `RFC822Parser.stripEmlxFraming`: after a first line of digits, optionally padded with
/// spaces, the message bounded by the count, when Apple Mail's property list starts where the
/// count says. Tika strips such a first line from any message, even without the property
/// list, so a message that starts with a number on its own line loses it (#83).
fn strip_emlx_framing(input: &[u8]) -> &[u8] {
    let mut count: u64 = 0;
    let mut digits = 0;
    let mut padding = false;
    let mut line_length = None;
    for (index, &byte) in input.iter().take(EMLX_COUNT_LINE_MAX).enumerate() {
        match byte {
            b'0'..=b'9' if !padding => {
                count = count * 10 + u64::from(byte - b'0');
                digits += 1;
            }
            b' ' if digits > 0 => padding = true,
            b'\n' if digits > 0 => {
                line_length = Some(index + 1);
                break;
            }
            _ => break,
        }
    }
    let Some(line_length) = line_length else {
        return input;
    };
    let rest = &input[line_length..];
    let plist = usize::try_from(count)
        .ok()
        .and_then(|count| rest.get(count..count.checked_add(PLIST_START.len())?));
    match plist {
        Some(start) if start == PLIST_START => &rest[..count as usize],
        _ => input,
    }
}

/// A part of the message as writing it takes it.
enum Part {
    /// `<div class="email-entry"><p>`, which starts a part of a multipart.
    StartEntry,
    /// `</p></div>`.
    EndEntry,
    /// A text or HTML part its native parser prepared, to parse inline.
    Inline(Prepared<'static>),
    /// A part handed to the embedded document extractor, with the metadata it is given.
    Embedded(Vec<u8>, TikaMetadata),
}

/// `RFC822Parser.parse` of `message` prepared: the message read, with the metadata it adds to
/// `metadata`, and its text and HTML parts prepared by their parsers; or why Tika's path should
/// read it, something of it the port does not read or a part whose parser defers it.
fn prepare_message(
    message: Result<&[u8], Failure>,
    metadata: &TikaMetadata,
) -> Result<Prepared<'static>, Deferral> {
    let failed = |failure: Failure| -> Prepared<'static> { Box::new(move |_| Err(failure)) };
    let message = match message {
        Ok(message) => message,
        Err(failure) => return Ok(failed(failure)),
    };
    if message.is_empty() {
        return Ok(failed(Failure::Malformed(
            "rfc822 parser found zero bytes".into(),
        )));
    }
    let mut metadata = metadata.clone();
    let items = match read(strip_emlx_framing(message), &mut metadata) {
        Ok(items) => items,
        Err(Problem::Unported(unported)) => return Err(unported.reason().to_owned()),
        Err(Problem::Malformed(reason)) => {
            return Ok(failed(Failure::Malformed(format!(
                "Failed to parse an email message: {reason}"
            ))))
        }
    };
    let mut parts = Vec::with_capacity(items.len());
    for item in items {
        parts.push(match item {
            Item::StartEntry => Part::StartEntry,
            Item::EndEntry => Part::EndEntry,
            Item::Inline(Inline::Text(content)) => Part::Inline(
                text::prepare_plain(&content)
                    .map_err(|reason| format!("a text part defers: {reason}"))?,
            ),
            Item::Inline(Inline::Html(content)) => Part::Inline(
                html::prepare(&content, &TikaMetadata::new())
                    .map_err(|reason| format!("an HTML part defers: {reason}"))?,
            ),
            Item::Embedded(content, metadata) => Part::Embedded(content, metadata),
        });
    }
    Ok(Box::new(move |xhtml| {
        *xhtml.metadata_mut() = metadata;
        for part in parts {
            match part {
                Part::StartEntry => {
                    xhtml.start_element_with("div", &[("class", "email-entry")])?;
                    xhtml.start_element("p")?;
                }
                Part::EndEntry => {
                    xhtml.end_element("p")?;
                    xhtml.end_element("div")?;
                }
                Part::Inline(prepared) => parse_inline(prepared, xhtml)?,
                Part::Embedded(content, metadata) => {
                    xhtml.parse_embedded(&content, &metadata, Wrapping::Bare)?;
                }
            }
        }
        Ok(())
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_same_as_tika_departing, conformance_corpus, documents, Departure, Departures,
    };

    const TIKA_NAME: &str = "org.apache.tika.parser.mail.RFC822Parser";
    const EMLX_NAME: &str = "ai.yobix.EmlxParser";

    /// What the port does otherwise than Tika on purpose: Tika names a message after its
    /// subject, and makes each line feed of a quoted-printable part a CRLF.
    pub(crate) const DEPARTURES: Departures = &[
        Departure::Values("tk:resource-name", "#79"),
        Departure::LineBreaks("#73"),
    ];

    /// The documents whose sender's name Tika's regular expression takes otherwise than the
    /// port takes it from the parsed mailbox (#77), by file name.
    pub(crate) const OTHER_SENDER_NAMES: [&str; 1] = ["testGroupWiseEml.eml"];

    /// Asserts parity on the documents of `parser` under `roots`, which it must parse all of,
    /// and that there are `count`, but for those named in `except`, which it returns.
    fn assert_parity(
        parser: &dyn Parser,
        tika_name: &str,
        roots: &[&Path],
        count: usize,
        except: &[&str],
    ) -> Vec<PathBuf> {
        let (parsed, deferred) = documents(parser, roots);
        assert!(deferred.is_empty(), "deferred: {deferred:?}");
        assert_eq!(parsed.len(), count, "documents of {tika_name}");
        let (excepted, parsed): (Vec<PathBuf>, Vec<PathBuf>) =
            parsed.into_iter().partition(|path| {
                except
                    .iter()
                    .any(|name| path.file_name().is_some_and(|file| file == *name))
            });
        let (other, paths): (Vec<&Path>, Vec<&Path>) =
            parsed.iter().map(PathBuf::as_path).partition(|path| {
                OTHER_SENDER_NAMES
                    .iter()
                    .any(|name| path.file_name().is_some_and(|file| file == *name))
            });
        assert_same_as_tika_departing(&paths, tika_name, DEPARTURES);
        let mut departures = DEPARTURES.to_vec();
        departures.push(Departure::Values("message:from-name", "#77"));
        assert_same_as_tika_departing(&other, tika_name, &departures);
        excepted
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn takes_the_sender_name_from_the_mailbox() {
        let path = documents(&Rfc822Parser, &[&conformance_corpus()])
            .0
            .into_iter()
            .find(|path| path.ends_with("testGroupWiseEml.eml"))
            .expect("Tika's GroupWise message");
        let (_, metadata) = crate::Extractor::new()
            .extract_file_to_string(path.to_str().unwrap())
            .unwrap();
        assert_eq!(
            metadata.get("message:from-name"),
            Some(&vec!["Novell GroupWise".to_owned()])
        );
    }

    #[test]
    fn keeps_the_name_of_the_message_file() {
        let (_, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(
                b"From: a@example.org\r\nSubject: A/subject: of sorts\r\n\r\nBody.\r\n",
            )
            .unwrap();
        assert_eq!(
            metadata.get("dc:title"),
            Some(&vec!["A/subject: of sorts".to_owned()])
        );
        assert_eq!(metadata.get("tk:resource-name"), None);
    }

    const ROOTS: [&str; 3] = [
        "../test_files/documents",
        "../quality/corpus",
        "../quality/truth",
    ];

    #[test]
    fn extracts_messages_as_tikas_path_does() {
        let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
        assert_parity(&Rfc822Parser, TIKA_NAME, &roots, 3, &[]);
    }

    #[test]
    fn extracts_apple_mail_messages_as_tikas_path_does() {
        let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
        assert_parity(&EmlxParser, EMLX_NAME, &roots, 1, &[]);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_messages_as_tikas_path_does() {
        // Its attached ZIP archive has an encrypted entry, whose name Tika writes as text
        // besides recording its failure (#99).
        let encrypted = assert_parity(
            &Rfc822Parser,
            TIKA_NAME,
            &[&conformance_corpus()],
            26,
            &["testRFC822_encrypted_zip"],
        );
        let [encrypted] = &encrypted[..] else {
            panic!("{encrypted:?}");
        };
        let (text, _) = crate::Extractor::new()
            .extract_file_to_string(encrypted.to_str().unwrap())
            .unwrap();
        assert!(!text.trim().is_empty());
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_apple_mail_messages_as_tikas_path_does() {
        assert_parity(&EmlxParser, EMLX_NAME, &[&conformance_corpus()], 3, &[]);
    }

    #[test]
    fn strips_emlx_framing_with_its_property_list() {
        assert_eq!(strip_emlx_framing(b"5\nabcde<?xml x"), b"abcde");
        assert_eq!(strip_emlx_framing(b"5   \nabcdefg"), b"5   \nabcdefg");
        assert_eq!(strip_emlx_framing(b"2\nab<?xml"), b"ab");
        assert_eq!(strip_emlx_framing(b"From: x\n"), b"From: x\n");
        assert_eq!(strip_emlx_framing(b"5 6\nab"), b"5 6\nab");
    }

    #[test]
    fn reads_emlx_lengths_as_emlx_parser_does() {
        assert_eq!(emlx_message(b"3 \r\nabcdef").unwrap(), b"abc");
        assert_eq!(emlx_message(b"-1\nabc").unwrap(), b"abc");
        assert!(emlx_message(b"x\nabc").is_err());
        assert!(emlx_message(b"12").is_err());
    }
}
