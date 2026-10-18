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
//! A header that needs a charset the native decoders do not decode, or names a time zone
//! java.time would look up in its time-zone database, is left out, and an HTML part the native
//! HTML parser reads otherwise than Tika's is read so: the message's reading then differs from
//! Tika's. A message defers to Tika when it holds a text or HTML part the native parsers of
//! those defer.
//!
//! A build with Tika reads a message whole before it writes anything of it, to decide first
//! whether Tika's path should read it. A build without Tika has nothing to decide: it writes
//! each part as it reads it, its body spooled, HTML written as its DOM is built, and records a
//! difference from Tika's reading once it meets it.

mod address;
mod codec;
mod date;
mod field;
mod handler;
#[cfg(all(test, not(feature = "tika")))]
mod large;
mod mbox;
#[cfg(test)]
mod reference;
mod stream;
mod structured;
#[cfg(all(test, feature = "tika"))]
mod synthetic;

pub use codec::decode_encoded_words;
pub use date::{parse_date, MailDate};
#[cfg(feature = "tika")]
use handler::Collected;
#[cfg(not(feature = "tika"))]
use handler::Sink;
use handler::{read, Inline, Item};
pub use mbox::MboxParser;
use stream::MimeError;

use crate::config::ParserConfigs;
#[cfg(not(feature = "tika"))]
use crate::formats::embedded::record_exception;
use crate::formats::embedded::Wrapping;
use crate::formats::handlers::parse_inline;
#[cfg(feature = "tika")]
use crate::formats::html;
#[cfg(not(feature = "tika"))]
use crate::formats::html::JSoupParser;
use crate::formats::metadata::TikaMetadata;
#[cfg(not(feature = "tika"))]
use crate::formats::noted;
use crate::formats::text;
#[cfg(feature = "tika")]
use crate::formats::unless_different;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Difference, Failure, Footprint, Parser, Prepared};
#[cfg(feature = "tika")]
use crate::input::Input;
use crate::input::{Pages, Paging};
use crate::java::parse_long;

/// What the port leaves to Tika.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unported {
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
pub struct Rfc822Parser;

/// Our Java `EmlxParser`: Apple Mail's `.emlx`, a line with the message's length in bytes, the
/// message, and a property list, its message read by `RFC822Parser`.
pub struct EmlxParser;

impl Parser for Rfc822Parser {
    fn name(&self) -> &'static str {
        "extractous::RFC822Parser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.mail.RFC822Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["message/rfc822"]
    }

    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &["message/rfc822"]
    }

    fn footprint(&self) -> Footprint {
        FOOTPRINT
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        prepare_message(self.name(), Ok(input), pages, metadata, configs)
    }
}

impl Parser for EmlxParser {
    fn name(&self) -> &'static str {
        "extractous::EmlxParser"
    }
    fn tika_name(&self) -> &'static str {
        "ai.yobix.EmlxParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["message/x-emlx"]
    }

    fn footprint(&self) -> Footprint {
        FOOTPRINT
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        prepare_message(self.name(), emlx_message(input), pages, metadata, configs)
    }
}

/// How the memory of a message's reading grows with it: a build with Tika reads it whole to
/// decide first whether Tika's path should read it; one without writes each part as it is read.
#[cfg(feature = "tika")]
const FOOTPRINT: Footprint = Footprint::Proportional;
#[cfg(not(feature = "tika"))]
const FOOTPRINT: Footprint = Footprint::Bounded;

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

impl From<MimeError> for Failure {
    fn from(error: MimeError) -> Self {
        Failure::Malformed(format!("Failed to parse an email message: {}", error.0))
    }
}

/// How the reading of a message the port reads part of otherwise than Tika, as `unported` says,
/// differs from Tika's.
fn unported_difference(unported: Unported) -> Difference {
    Difference::new(format!(
        "{}, so the native reading leaves out the header it is in",
        unported.reason()
    ))
}

/// `RFC822Parser.parse` of `message`, whose pages are `pages`, prepared from `metadata` for the
/// parser named `parser`: a message that is not there fails; any other is read as
/// [`read_message`] reads it.
fn prepare_message<'a>(
    parser: &'static str,
    message: Result<&'a [u8], Failure>,
    pages: Pages<'a>,
    metadata: &TikaMetadata,
    configs: &'a ParserConfigs,
) -> Result<Prepared<'a>, Decline<'a>> {
    let message = match message {
        Ok(message) => message,
        Err(failure) => return Ok(failed(failure)),
    };
    if message.is_empty() {
        return Ok(failed(Failure::Malformed(
            "rfc822 parser found zero bytes".into(),
        )));
    }
    read_message(
        parser,
        strip_emlx_framing(message),
        pages,
        metadata.clone(),
        configs,
    )
}

/// A reading that fails at once, writing nothing.
fn failed<'a>(failure: Failure) -> Prepared<'a> {
    Box::new(move |_| Err(failure))
}

/// A part of the message as writing it takes it.
#[cfg(feature = "tika")]
enum Part {
    /// `<div class="email-entry"><p>`, which starts a part of a multipart.
    StartEntry,
    /// `</p></div>`.
    EndEntry,
    /// A text or HTML part its native parser prepared, to parse inline.
    Inline(Prepared<'static>),
    /// A part handed to the embedded document extractor, with the metadata it is given.
    Embedded(Input<'static>, TikaMetadata),
}

/// The message read whole before anything of it is written, with the metadata it adds to
/// `metadata`, and its text and HTML parts prepared by their parsers, so that a build with Tika
/// decides first whether the port reads it as Tika does: a reading that differs from Tika's
/// where the port does not read something of it as Tika does; or why Tika's path should read
/// it, a part whose parser defers it.
#[cfg(feature = "tika")]
fn read_message<'a>(
    _parser: &'static str,
    message: &'a [u8],
    pages: Pages<'a>,
    mut metadata: TikaMetadata,
    _configs: &'a ParserConfigs,
) -> Result<Prepared<'a>, Decline<'a>> {
    let mut collected = Collected {
        metadata: &mut metadata,
        items: Vec::new(),
    };
    let unported = match read(message, &Paging::new(pages), &mut collected) {
        Ok(unported) => unported,
        Err(error) => return Ok(failed(Failure::from(error))),
    };
    let items = collected.items;
    let mut difference = unported.map(unported_difference);
    let mut parts = Vec::with_capacity(items.len());
    for item in items {
        parts.push(match item {
            Item::StartEntry => Part::StartEntry,
            Item::EndEntry => Part::EndEntry,
            Item::Inline(Inline::Text(content)) => Part::Inline(
                text::prepare_plain(content)
                    .map_err(|reason| Decline::Deferred(format!("a text part defers: {reason}")))?,
            ),
            Item::Inline(Inline::Html(content)) => {
                match html::prepare(&content, &TikaMetadata::new()) {
                    Ok(prepared) => Part::Inline(prepared),
                    Err(Decline::Differs {
                        reading,
                        difference: html,
                    }) => {
                        difference.get_or_insert_with(|| {
                            Difference::new(format!("an HTML part differs: {}", html.0))
                        });
                        Part::Inline(reading)
                    }
                    Err(declined) => {
                        return Err(Decline::Deferred(format!(
                            "an HTML part defers: {declined:?}"
                        )))
                    }
                }
            }
            Item::Embedded(content, metadata) => Part::Embedded(content, metadata),
        });
    }
    let reading: Prepared<'static> = Box::new(move |xhtml| {
        *xhtml.metadata_mut() = metadata;
        for part in parts {
            match part {
                Part::StartEntry => start_entry(xhtml)?,
                Part::EndEntry => end_entry(xhtml)?,
                Part::Inline(prepared) => parse_inline(prepared, xhtml)?,
                Part::Embedded(content, metadata) => {
                    xhtml.parse_embedded_input(&content, &metadata, Wrapping::Bare)?;
                }
            }
        }
        Ok(())
    });
    unless_different(reading, difference)
}

/// The message written as it is read, a part at a time, each part's body spooled and let go of
/// once written, HTML built only as far as it may still change. A detail the port reads
/// otherwise than Tika is met only once it is read, and recorded as the difference of a message
/// a build without Tika reads natively.
#[cfg(not(feature = "tika"))]
fn read_message<'a>(
    parser: &'static str,
    message: &'a [u8],
    pages: Pages<'a>,
    metadata: TikaMetadata,
    configs: &'a ParserConfigs,
) -> Result<Prepared<'a>, Decline<'a>> {
    Ok(Box::new(move |xhtml| {
        *xhtml.metadata_mut() = metadata;
        let mut written = Written {
            xhtml,
            configs,
            difference: None,
        };
        let unported = read(message, &Paging::new(pages), &mut written)?;
        if let Some(difference) = unported.map(unported_difference).or(written.difference) {
            record_exception(written.xhtml.metadata_mut(), &difference.warning(parser));
        }
        Ok(())
    }))
}

/// Writes the items of a message's body as they are read, noting the first part whose parser
/// reads it otherwise than Tika's.
#[cfg(not(feature = "tika"))]
struct Written<'w, 'x, 'c> {
    xhtml: &'w mut Xhtml<'x>,
    configs: &'c ParserConfigs,
    difference: Option<Difference>,
}

#[cfg(not(feature = "tika"))]
impl Written<'_, '_, '_> {
    /// Parses inline the `kind` of part its parser `prepared`.
    fn inline(
        &mut self,
        kind: &str,
        prepared: Result<Prepared<'_>, Decline<'_>>,
    ) -> Result<(), Failure> {
        let (reading, difference) = noted(prepared).map_err(|declined| {
            Failure::Malformed(format!(
                "the native parser of {kind} part declines it: {declined}"
            ))
        })?;
        if let Some(Difference(reason)) = difference {
            self.difference
                .get_or_insert_with(|| Difference::new(format!("{kind} part differs: {reason}")));
        }
        parse_inline(reading, self.xhtml)
    }
}

#[cfg(not(feature = "tika"))]
impl Sink for Written<'_, '_, '_> {
    type Error = Failure;

    fn metadata(&mut self) -> &mut TikaMetadata {
        self.xhtml.metadata_mut()
    }

    fn item(&mut self, item: Item) -> Result<(), Failure> {
        match item {
            Item::StartEntry => Ok(start_entry(self.xhtml)?),
            Item::EndEntry => Ok(end_entry(self.xhtml)?),
            Item::Inline(Inline::Text(content)) => {
                self.inline("a text", text::prepare_plain(content))
            }
            Item::Inline(Inline::Html(content)) => {
                let prepared = JSoupParser.prepare_paged(
                    &content,
                    content.pages(),
                    &TikaMetadata::new(),
                    self.configs,
                );
                self.inline("an HTML", prepared)
            }
            Item::Embedded(content, metadata) => {
                self.xhtml
                    .parse_embedded_input(&content, &metadata, Wrapping::Bare)
            }
        }
    }
}

/// `<div class="email-entry"><p>`, which starts a part of a multipart.
fn start_entry(xhtml: &mut Xhtml) -> Result<(), Stop> {
    xhtml.start_element_with("div", &[("class", "email-entry")])?;
    xhtml.start_element("p")
}

/// `</p></div>`, which ends a part of a multipart.
fn end_entry(xhtml: &mut Xhtml) -> Result<(), Stop> {
    xhtml.end_element("p")?;
    xhtml.end_element("div")
}

/// What the port does otherwise than Tika on purpose: Tika names a message after its
/// subject, and makes each line feed of a quoted-printable part a CRLF.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
const DEPARTURES: crate::formats::parity::Departures = &[
    crate::formats::parity::Departure::Values("tk:resource-name", "#79"),
    crate::formats::parity::Departure::LineBreaks("#73"),
];

/// The documents whose sender's name Tika's regular expression takes otherwise than the
/// port takes it from the parsed mailbox (#77), by file name.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
const OTHER_SENDER_NAMES: [&str; 1] = ["testGroupWiseEml.eml"];

/// What the port does otherwise than Tika on purpose with the message or mailbox at `path`:
/// the [`DEPARTURES`] of every message, and the sender's name of the [`OTHER_SENDER_NAMES`].
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub fn departures_of(path: &std::path::Path) -> Vec<crate::formats::parity::Departure<'static>> {
    let mut departures = DEPARTURES.to_vec();
    if path
        .file_name()
        .is_some_and(|file| OTHER_SENDER_NAMES.iter().any(|name| file == *name))
    {
        departures.push(crate::formats::parity::Departure::Values(
            "message:from-name",
            "#77",
        ));
    }
    departures
}

#[cfg(test)]
pub(crate) mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::assert_same_as_tika_departing;
    #[cfg(feature = "tika")]
    use crate::formats::parity::Departure;
    use crate::formats::parity::{conformance_corpus, documents};

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "org.apache.tika.parser.mail.RFC822Parser";
    #[cfg(feature = "tika")]
    const EMLX_NAME: &str = "ai.yobix.EmlxParser";

    /// Asserts parity on the documents of `parser` under `roots`, which it must parse all of,
    /// and that there are `count`, but for those named in `except`, which it returns.
    #[cfg(feature = "tika")]
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
        let named = |names: &[&str], path: &Path| {
            names
                .iter()
                .any(|name| path.file_name().is_some_and(|file| file == *name))
        };
        let (excepted, parsed): (Vec<PathBuf>, Vec<PathBuf>) =
            parsed.into_iter().partition(|path| named(except, path));
        let mut groups: Vec<(Vec<Departure>, Vec<&Path>)> = Vec::new();
        for path in parsed.iter().map(PathBuf::as_path) {
            let mut departures = departures_of(path);
            if named(&crate::formats::parity::ANCHORED_BLOCK_DOCUMENTS, path) {
                departures.extend_from_slice(crate::formats::parity::ANCHORED_BLOCKS);
            }
            match groups.iter_mut().find(|(group, _)| *group == departures) {
                Some((_, paths)) => paths.push(path),
                None => groups.push((departures, vec![path])),
            }
        }
        for (departures, paths) in &groups {
            assert_same_as_tika_departing(paths, tika_name, departures);
        }
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

    const ZONED: &[u8] =
        b"From: a@example.org\r\nSubject: Zoned\r\nDate: 5 Mar 2024 14:30 EST\r\n\r\nBody.\r\n";

    #[cfg(feature = "tika")]
    #[test]
    fn decides_first_that_a_date_naming_a_zone_differs() {
        let configs = ParserConfigs::default();
        let Err(Decline::Differs { difference, .. }) =
            Rfc822Parser.prepare(ZONED, &TikaMetadata::new(), &configs)
        else {
            panic!("a date naming a zone is not read as differing");
        };
        assert!(difference.0.contains("time zone"), "{difference:?}");
    }

    #[cfg(not(feature = "tika"))]
    #[test]
    fn leaves_out_a_date_that_names_a_zone_recording_the_difference() {
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(ZONED)
            .unwrap();
        assert!(text.contains("Body."), "{text}");
        assert_eq!(metadata.get("dc:title"), Some(&vec!["Zoned".to_owned()]));
        assert_eq!(metadata.get("dcterms:created"), None);
        let warnings = &metadata[crate::formats::embedded::EXCEPTION_WARNING];
        assert!(
            warnings.iter().any(|warning| warning.contains("time zone")),
            "{warnings:?}"
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

    #[cfg(feature = "tika")]
    const ROOTS: [&str; 3] = [
        "../test_files/documents",
        "../quality/corpus",
        "../quality/truth",
    ];

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_messages_as_tikas_path_does() {
        let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
        assert_parity(&Rfc822Parser, TIKA_NAME, &roots, 3, &[]);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_apple_mail_messages_as_tikas_path_does() {
        let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
        assert_parity(&EmlxParser, EMLX_NAME, &roots, 1, &[]);
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_messages_as_tikas_path_does() {
        // Its attached ZIP archive has an encrypted entry, whose name Tika writes as text
        // besides recording its failure (#99).
        let encrypted = assert_parity(
            &Rfc822Parser,
            TIKA_NAME,
            &[&conformance_corpus()],
            28,
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

    /// MHTML documents, typed `multipart/related`, which Tika's composite parser hands the
    /// parser of `message/rfc822`, its supertype, spool their parts as Tika's path does, once
    /// the line breaks of their quoted-printable parts are taken alike (#73). Where a budget cuts
    /// the spool, the departure moves the cut.
    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn spools_the_parts_of_tikas_mhtml_documents_as_tikas_path_does() {
        let mhtml: Vec<PathBuf> = documents(&Rfc822Parser, &[&conformance_corpus()])
            .0
            .into_iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "mhtml")
            })
            .collect();
        assert_eq!(mhtml.len(), 2, "{mhtml:?}");
        let spooled: usize = mhtml
            .iter()
            .map(|path| {
                crate::formats::parity::spools_as_tika_rewriting(path, u64::MAX, None, DEPARTURES)
                    .unwrap_or_else(|difference| panic!("{difference}"))
            })
            .sum();
        assert!(spooled > 0);
    }

    #[cfg(feature = "tika")]
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
