//! Web archives, WARC and ARC, as Tika's `WARCParser` reads them through jwarc 0.37.0: each
//! response record holding an HTTP response hands its payload to the embedded document extractor,
//! with the record's and the response's headers as `warc:` and `warc:http:` metadata. Other
//! records write nothing.
//!
//! What Tika's path records as jwarc throws it, a response whose HTTP header does not parse, a
//! chunked payload cut short, a record cut short of its length or with a negative one, the
//! native path records without jwarc's message, and a record ID it does not check as
//! `URI.create` does it takes as it stands: such an archive's reading differs from Tika's.
//! Gemini responses, which the port does not read, stay with Tika's path.

mod headers;
mod http;
mod media_type;
mod ragel;
mod reader;
mod response;
mod tables;
mod text;
mod wacz;

use headers::Headers;
use media_type::MediaType;
use reader::{Record, Stop};
pub use response::HttpParser;
pub use wacz::WaczParser;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{record_exception, Wrapping, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Deferral, Difference, Failure, Opened, Parser, Prepared};
use crate::java;

const WARC_PREFIX: &str = "warc:";
const WARC_HTTP_PREFIX: &str = "warc:http:";
const RECORD_CONTENT_TYPE: &str = "warc:record-content-type";
const PAYLOAD_CONTENT_TYPE: &str = "warc:payload-content-type";
const HTTP_STATUS: &str = "warc:http:status";
const HTTP_STATUS_REASON: &str = "warc:http:status:reason";

pub struct WarcParser;

impl Parser for WarcParser {
    fn name(&self) -> &'static str {
        "extractous::WARCParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.warc.WARCParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/warc",
            "application/warc+gz",
            "application/x-internet-archive",
            "application/arc+gz",
        ]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let data = reader::decompress(input)
            .map_err(|thrown| Decline::Fails(Error::ParseError(thrown)))?;
        let mut difference = None;
        let payloads = payloads(&data, &mut difference)?;
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            let written = payloads.write(xhtml);
            xhtml.end_document()?;
            written
        });
        match difference {
            None => Ok(reading),
            Some(difference) => Err(Decline::Differs {
                reading,
                difference,
            }),
        }
    }
}

/// A payload `WARCParser` hands the embedded document extractor.
struct Payload {
    body: Vec<u8>,
    metadata: TikaMetadata,
}

/// What `WARCParser` makes of a record it reads on past: a payload, or an exception it records.
enum Item {
    Payload(Payload),
    Recorded(String),
}

/// The items of an archive, and what jwarc or `WARCParser` throws after them, failing the
/// parse.
struct Payloads {
    items: Vec<Item>,
    thrown: Option<String>,
}

impl Payloads {
    /// Hands each payload to the embedded document extractor, then fails as Tika's parse does
    /// after them, if it does.
    fn write(&self, xhtml: &mut Xhtml<'_>) -> Result<(), Failure> {
        for item in &self.items {
            match item {
                Item::Payload(payload) => xhtml.parse_embedded_opened(
                    &payload.body,
                    &payload.metadata,
                    Wrapping::PackageEntry,
                    Opened::Alone,
                )?,
                Item::Recorded(exception) => record_exception(xhtml.metadata_mut(), exception),
            }
        }
        match &self.thrown {
            Some(thrown) => Err(Failure::Malformed(thrown.clone())),
            None => Ok(()),
        }
    }
}

/// What `processRecord` makes of a record.
enum Processed {
    Payload(Payload),
    /// An exception `WARCParser` records, going on with the next record.
    Recorded(String),
    Nothing,
    /// A runtime exception, which fails the parse.
    Thrown(String),
}

/// Notes `what` as how the native reading differs from Tika's, unless an earlier difference is.
fn differs(difference: &mut Option<Difference>, what: &str) {
    difference.get_or_insert_with(|| Difference::new(what));
}

/// The payloads of the records of `data`, read as `WARCParser.parse` reads them, with the first
/// detail in which the native reading differs from it noted in `difference`, or why Tika's path
/// should read the archive.
fn payloads(data: &[u8], difference: &mut Option<Difference>) -> Result<Payloads, Deferral> {
    let (records, stop) = reader::records(data);
    let mut items = Vec::new();
    for record in &records {
        match process(record, difference)? {
            Processed::Payload(payload) => items.push(Item::Payload(payload)),
            Processed::Recorded(exception) => items.push(Item::Recorded(exception)),
            Processed::Nothing => {}
            Processed::Thrown(thrown) => {
                return Ok(Payloads {
                    items,
                    thrown: Some(thrown),
                })
            }
        }
    }
    let thrown = match stop {
        None => None,
        Some(Stop::Thrown(thrown)) => Some(thrown),
        Some(Stop::NegativeLength) => {
            differs(
                difference,
                "a record with a negative Content-Length, which jwarc's LengthedBody \
                 mishandles, where the native parser records the record's failure and stops",
            );
            items.push(Item::Recorded(
                "ParsingException: a record with a negative Content-Length".to_owned(),
            ));
            None
        }
        Some(Stop::CutBlock) => {
            differs(
                difference,
                "a record cut short of its Content-Length, which Tika records or fails on as \
                 the record's parser reads it, where the native parser records it and stops",
            );
            items.push(Item::Recorded(
                "EOFException: a record cut short of its Content-Length".to_owned(),
            ));
            None
        }
    };
    Ok(Payloads { items, thrown })
}

/// `processRecord` and `processResponse` of `record`, noting in `difference` how the native
/// reading differs from Tika's.
fn process(
    record: &Record<'_>,
    difference: &mut Option<Difference>,
) -> Result<Processed, Deferral> {
    let headers = &record.headers;
    let kind = match headers.sole("WARC-Type") {
        Ok(Some(kind)) => kind,
        Ok(None) => {
            return Ok(Processed::Thrown(
                "NoSuchElementException: a record without WARC-Type".to_owned(),
            ))
        }
        Err(not_sole) => return Ok(Processed::Thrown(not_sole.0)),
    };
    if kind != "response" {
        return Ok(Processed::Nothing);
    }
    let content_type = match content_type_of(headers) {
        Ok(content_type) => content_type,
        Err(thrown) => return Ok(Processed::Thrown(thrown)),
    };
    if content_type.has_base("application", "gemini") {
        return Err("a Gemini response, which the port does not read".to_owned());
    }
    if !content_type.has_base("application", "http") {
        return Ok(Processed::Nothing);
    }
    let response = match http::parse(record.block) {
        Ok(response) => response,
        Err(http::Unread::Header) => {
            differs(
                difference,
                "a response whose HTTP header jwarc fails on, which Tika records with jwarc's \
                 message",
            );
            return Ok(Processed::Recorded(
                "ParsingException: invalid HTTP message".to_owned(),
            ));
        }
        Err(http::Unread::CutChunks) => {
            differs(
                difference,
                "a chunked payload cut short, which Tika records as its parser reads it, after \
                 what that parser writes of it",
            );
            return Ok(Processed::Recorded(
                "EOFException: a chunked payload cut short".to_owned(),
            ));
        }
    };
    let payload_type = match content_type_of(&response.headers) {
        Ok(payload_type) => payload_type,
        Err(thrown) => return Ok(Processed::Thrown(thrown)),
    };
    let mut metadata = TikaMetadata::new();
    metadata.set(RECORD_CONTENT_TYPE, content_type.to_string());
    metadata.set(PAYLOAD_CONTENT_TYPE, payload_type.to_string());
    for (name, values) in headers.iter() {
        for value in values {
            metadata.add_prefixed(WARC_PREFIX, name, value.clone());
        }
    }
    metadata.set(HTTP_STATUS, response.status.to_string());
    if let Some(reason) = response
        .reason
        .as_deref()
        .filter(|reason| !java::is_blank(reason))
    {
        metadata.set(HTTP_STATUS_REASON, reason);
    }
    for (name, values) in response.headers.iter() {
        for value in values {
            metadata.add_prefixed(WARC_HTTP_PREFIX, name, value.clone());
        }
    }
    match resource_name(headers, difference) {
        Ok(Some(name)) => metadata.set(RESOURCE_NAME, name),
        Ok(None) => {}
        Err(thrown) => return Ok(Processed::Thrown(thrown)),
    }
    metadata.set(RECORD_CONTENT_TYPE, payload_type.to_string());
    metadata.set("Content-Length", response.size.to_string());
    Ok(Processed::Payload(Payload {
        body: response.body,
        metadata,
    }))
}

/// `Message.contentType()` of a message with `headers`: its `Content-Type` read leniently, else
/// `application/octet-stream`; or what the lenient parse throws.
fn content_type_of(headers: &Headers) -> Result<MediaType, String> {
    match headers.first("Content-Type") {
        Some(text) => MediaType::parse_leniently(text)
            .map_err(|unparsable| format!("IllegalArgumentException: {}", unparsable.0)),
        None => Ok(media_type::octet_stream()),
    }
}

/// The record ID `WARCParser` names the payload of a record that names its `warcinfo` record
/// by: `id().toString()`, or what jwarc throws, noting in `difference` an ID the port does not
/// check as `URI.create` does.
fn resource_name(
    headers: &Headers,
    difference: &mut Option<Difference>,
) -> Result<Option<String>, String> {
    let warcinfo = match headers.sole("WARC-Warcinfo-ID") {
        Ok(Some(warcinfo)) => warcinfo,
        Ok(None) => return Ok(None),
        Err(not_sole) => return Err(not_sole.0),
    };
    record_id(warcinfo, difference);
    let id = match headers.sole("WARC-Record-ID") {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Err("NoSuchElementException: a record without WARC-Record-ID".to_owned())
        }
        Err(not_sole) => return Err(not_sole.0),
    };
    Ok(Some(record_id(id, difference).to_owned()))
}

/// `WarcRecord.parseRecordID`: the ID without its angle brackets, where `URI.create` takes it,
/// which the port checks for the opaque URIs record IDs are, such as `urn:uuid:…`. Another ID
/// is taken as it stands, noted in `difference`, as `URI.create` may take or refuse it.
fn record_id<'t>(text: &'t str, difference: &mut Option<Difference>) -> &'t str {
    let id = text
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(text);
    let opaque = id.split_once(':').is_some_and(|(scheme, rest)| {
        scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && !rest.is_empty()
            && !rest.starts_with('/')
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-._~:@!$&'()*+,;=?/".contains(c))
    });
    if !opaque {
        differs(
            difference,
            &format!("a record ID the port does not check as URI.create does: {id}"),
        );
    }
    id
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "org.apache.tika.parser.warc.WARCParser";

    fn record(headers: &[(&str, &str)], block: &[u8]) -> Vec<u8> {
        let mut record = b"WARC/1.0\r\n".to_vec();
        for (name, value) in headers {
            record.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        record.extend_from_slice(format!("Content-Length: {}\r\n\r\n", block.len()).as_bytes());
        record.extend_from_slice(block);
        record.extend_from_slice(b"\r\n\r\n");
        record
    }

    fn response(id: &str, http: &[u8]) -> Vec<u8> {
        record(
            &[
                ("WARC-Type", "response"),
                ("WARC-Record-ID", id),
                ("WARC-Warcinfo-ID", "<urn:uuid:info>"),
                ("WARC-Target-URI", "http://example.com/"),
                ("Content-Type", "application/http; msgtype=response"),
            ],
            http,
        )
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_web_archives_as_tikas_path_does() {
        let scratch = Scratch::new("warc");
        let html = b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
                     <html><body><p>Hej verden</p></body></html>";
        let chunked =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n\
                        5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let mut archive = record(
            &[
                ("WARC-Type", "warcinfo"),
                ("WARC-Record-ID", "<urn:uuid:info>"),
            ],
            b"software: test\r\n",
        );
        archive.extend(response("<urn:uuid:one>", html));
        archive.extend(response("<urn:uuid:two>", chunked));
        archive.extend(record(
            &[
                ("WARC-Type", "request"),
                ("WARC-Record-ID", "<urn:uuid:three>"),
            ],
            b"GET / HTTP/1.1\r\n\r\n",
        ));
        let mut gzipped = Vec::new();
        for member in [&archive[..], &response("<urn:uuid:four>", html)] {
            gzipped.extend(crate::compress::gzip::tests::gzip(member, None));
        }
        let arc_page = b"HTTP/1.0 200 OK\nContent-Type: text/html\n\n<p>arkiv</p>";
        let mut arc =
            b"filedesc://test.arc 0.0.0.0 20080430204825 text/plain 11\n1 0 test\nX\n\n".to_vec();
        arc.extend_from_slice(
            format!(
                "http://example.com/ 1.2.3.4 20080430204826 text/html {}\n",
                arc_page.len()
            )
            .as_bytes(),
        );
        arc.extend_from_slice(arc_page);
        arc.push(b'\n');
        let read = [
            scratch.write("archive.warc", &archive),
            scratch.write("archive.warc.gz", &gzipped),
            scratch.write("archive.arc", &arc),
        ];
        let failing = [scratch.write(
            "no-type.warc",
            &[
                archive.clone(),
                record(&[("WARC-Record-ID", "<urn:uuid:x>")], b""),
            ]
            .concat(),
        )];
        let (parsed, deferred) = documents(&WarcParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len() + failing.len(), "{parsed:?}");
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_web_archives_as_tikas_path_does() {
        let (parsed, deferred) = documents(&WarcParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 6, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    fn parses_what_there_is_of_a_payload_cut_short_as_differing() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 99\r\n\r\nshort";
        let configs = ParserConfigs::default();
        let Err(Decline::Differs { difference, .. }) =
            HttpParser.prepare(response, &TikaMetadata::new(), &configs)
        else {
            panic!("a payload cut short is not read as differing");
        };
        assert!(difference.0.contains("cut short"), "{difference:?}");
        if cfg!(feature = "tika") {
            return;
        }
        let (mut reader, _) = crate::Extractor::new()
            .extract_bytes(response)
            .expect("the extraction starts");
        let mut text = String::new();
        let read = std::io::Read::read_to_string(&mut reader, &mut text);
        assert!(text.contains("short"), "{text}");
        let error = read.expect_err("the extraction fails at the payload's end");
        assert!(error.to_string().contains("EOFException"), "{error}");
    }

    #[test]
    fn records_what_tika_records_as_differing() {
        let recorded = |data: &[u8]| {
            let mut difference = None;
            let payloads = payloads(data, &mut difference).unwrap();
            let recorded: Vec<String> = payloads
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Recorded(exception) => Some(exception.clone()),
                    Item::Payload(_) => None,
                })
                .collect();
            (recorded, difference.expect("the reading differs").0)
        };
        let cut = [
            response(
                "<urn:uuid:a>",
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nshort",
            ),
            response("<urn:uuid:b>", b"HTTP/1.1 200 OK\r\n\r\nafter"),
        ]
        .concat();
        let (exceptions, difference) = recorded(&cut);
        assert_eq!(exceptions, ["EOFException: a chunked payload cut short"]);
        assert!(difference.contains("chunked"), "{difference}");
        let (exceptions, difference) =
            recorded(&response("<not a uri>", b"HTTP/1.1 200 OK\r\n\r\nx"));
        assert!(exceptions.is_empty(), "{exceptions:?}");
        assert!(difference.contains("URI.create"), "{difference}");
        let truncated = b"WARC/1.0\r\nWARC-Type: response\r\nContent-Length: 99\r\n\r\nx";
        let (exceptions, difference) = recorded(truncated);
        assert_eq!(
            exceptions,
            ["EOFException: a record cut short of its Content-Length"]
        );
        assert!(difference.contains("cut short"), "{difference}");
    }
}
