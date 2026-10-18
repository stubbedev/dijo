//! Web archives, WARC and ARC, as Tika's `WARCParser` reads them through jwarc 0.37.0: each
//! response record holding an HTTP response hands its payload to the embedded document extractor,
//! with the record's and the response's headers as `warc:` and `warc:http:` metadata. Other
//! records write nothing.
//!
//! What Tika's path records as jwarc throws it, a response whose HTTP header does not parse, a
//! chunked payload cut short, a record cut short of its length or with a negative one, the
//! native path records without jwarc's message, and a record ID it does not check as
//! `URI.create` does it takes as it stands: such an archive's reading differs from Tika's.
//! A Gemini response's payload is read by jwarc's Gemini parser ([`gemini`]), without the HTTP
//! response `WARCParser` asks for all the same (#184): jwarc refuses it once the payload has
//! been read into, failing the parse, or fails to parse it, which Tika records, where the native
//! parser hands on the payload, without `warc:http:` metadata.

mod gemini;
mod headers;
mod http;
#[cfg(all(test, not(feature = "tika")))]
mod large;
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
use std::convert::Infallible;

use crate::formats::{Decline, Difference, Failure, Footprint, Opened, Parser, Prepared};
use crate::input::{Input, Pages, Paging};
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

    /// Decompresses the archive, to a spool where it is gzipped, and reads its records to rule
    /// out what Tika's path reads otherwise, keeping none of them; they are read again as they
    /// are written, a record at a time.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let data = reader::decompress(input, pages)
            .map_err(|thrown| Decline::Fails(Error::ParseError(thrown)))?;
        let mut difference = None;
        let checked = visit(&data, &mut difference, |_| Ok::<(), Infallible>(()));
        if let Err(never) = checked {
            match never {}
        }
        Paging::new(data.pages()).release();
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            let written = write(&data, xhtml);
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

/// A payload `WARCParser` hands the embedded document extractor: the pieces of its record's
/// block it is made of.
struct Payload<'a> {
    body: Vec<&'a [u8]>,
    metadata: TikaMetadata,
}

/// What `WARCParser` makes of a record it reads on past: a payload, or an exception it records.
enum Item<'a> {
    Payload(Payload<'a>),
    Recorded(String),
}

/// Hands each payload of the archive `data` to the embedded document extractor, a record at a
/// time, the pages of the archive released after each, then fails as Tika's parse does after
/// them, if it does.
fn write(data: &Input<'_>, xhtml: &mut Xhtml<'_>) -> Result<(), Failure> {
    let pages = data.pages();
    let paging = Paging::new(pages);
    let mut difference = None;
    let visited = visit(data, &mut difference, |item| {
        match item {
            Item::Payload(payload) => {
                let document = Input::joined(&payload.body, pages).map_err(|error| {
                    Failure::Extraction(Error::IoError(format!(
                        "spooling the payload of a WARC record fails: {error}"
                    )))
                })?;
                xhtml.parse_embedded_input_opened(
                    &document,
                    &payload.metadata,
                    Wrapping::PackageEntry,
                    Opened::Alone,
                )?;
            }
            Item::Recorded(exception) => record_exception(xhtml.metadata_mut(), &exception),
        }
        paging.release();
        Ok::<(), Failure>(())
    });
    match visited? {
        Some(thrown) => Err(Failure::Malformed(thrown)),
        None => Ok(()),
    }
}

/// What `processRecord` makes of a record.
enum Processed<'a> {
    Payload(Payload<'a>),
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

/// Hands `each` the items of the records of `data` in turn, read as `WARCParser.parse` reads
/// them, with the first detail in which the native reading differs from it noted in
/// `difference`: what jwarc or `WARCParser` throws after them, failing the parse, if anything;
/// or the failure of `each`, which ends them.
fn visit<'a, E>(
    data: &'a [u8],
    difference: &mut Option<Difference>,
    mut each: impl FnMut(Item<'a>) -> Result<(), E>,
) -> Result<Option<String>, E> {
    let mut at = 0;
    let stop = loop {
        let record = match reader::next(data, &mut at) {
            Ok(Some(record)) => record,
            Ok(None) => break None,
            Err(stop) => break Some(stop),
        };
        let item = match process(&record, difference) {
            Processed::Payload(payload) => Item::Payload(payload),
            Processed::Recorded(exception) => Item::Recorded(exception),
            Processed::Nothing => continue,
            Processed::Thrown(thrown) => return Ok(Some(thrown)),
        };
        each(item)?;
    };
    let (thrown, recorded) = match stop {
        None => (None, None),
        Some(Stop::Thrown(thrown)) => (Some(thrown), None),
        Some(Stop::NegativeLength) => {
            differs(
                difference,
                "a record with a negative Content-Length, which jwarc's LengthedBody \
                 mishandles, where the native parser records the record's failure and stops",
            );
            (
                None,
                Some("ParsingException: a record with a negative Content-Length"),
            )
        }
        Some(Stop::CutBlock) => {
            differs(
                difference,
                "a record cut short of its Content-Length, which Tika records or fails on as \
                 the record's parser reads it, where the native parser records it and stops",
            );
            (
                None,
                Some("EOFException: a record cut short of its Content-Length"),
            )
        }
    };
    if let Some(recorded) = recorded {
        each(Item::Recorded(recorded.to_owned()))?;
    }
    Ok(thrown)
}

/// `processRecord` and `processResponse` of `record`, noting in `difference` how the native
/// reading differs from Tika's.
fn process<'a>(record: &Record<'a>, difference: &mut Option<Difference>) -> Processed<'a> {
    let headers = &record.headers;
    let kind = match headers.sole("WARC-Type") {
        Ok(Some(kind)) => kind,
        Ok(None) => {
            return Processed::Thrown(
                "NoSuchElementException: a record without WARC-Type".to_owned(),
            )
        }
        Err(not_sole) => return Processed::Thrown(not_sole.0),
    };
    if kind != "response" {
        return Processed::Nothing;
    }
    let content_type = match content_type_of(headers) {
        Ok(content_type) => content_type,
        Err(thrown) => return Processed::Thrown(thrown),
    };
    let read = if content_type.has_base("application", "gemini") {
        gemini_response(record.block, difference)
    } else if content_type.has_base("application", "http") {
        http_response(record.block, difference)
    } else {
        return Processed::Nothing;
    };
    let Response {
        http,
        payload_type,
        body,
        size,
    } = match read {
        Ok(response) => response,
        Err(processed) => return processed,
    };
    let mut metadata = TikaMetadata::new();
    metadata.set(RECORD_CONTENT_TYPE, content_type.to_string());
    metadata.set(PAYLOAD_CONTENT_TYPE, payload_type.to_string());
    for (name, values) in headers.iter() {
        for value in values {
            metadata.add_prefixed(WARC_PREFIX, name, value.clone());
        }
    }
    if let Some(response) = http {
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
    }
    match resource_name(headers, difference) {
        Ok(Some(name)) => metadata.set(RESOURCE_NAME, name),
        Ok(None) => {}
        Err(thrown) => return Processed::Thrown(thrown),
    }
    metadata.set(RECORD_CONTENT_TYPE, payload_type.to_string());
    metadata.set("Content-Length", size.to_string());
    Processed::Payload(Payload { body, metadata })
}

/// What `processResponse` reads of a response record: the HTTP response whose status and
/// headers it records, none for a Gemini response, and the payload, its type, the pieces of the
/// block it is made of and what `body().size()` gives.
struct Response<'a> {
    http: Option<http::Head>,
    payload_type: MediaType,
    body: Vec<&'a [u8]>,
    size: i64,
}

/// The response of an HTTP response record, whose block is `block`, as jwarc parses it, or what
/// `processResponse` makes of a record it fails on, noting in `difference` how the native
/// reading differs from Tika's.
fn http_response<'a>(
    block: &'a [u8],
    difference: &mut Option<Difference>,
) -> Result<Response<'a>, Processed<'a>> {
    let response = match http::parse(block) {
        Ok(response) => response,
        Err(http::Unread::Header) => return Err(unparsed_http(difference)),
        Err(http::Unread::CutChunks) => {
            differs(
                difference,
                "a chunked payload cut short, which Tika records as its parser reads it, after \
                 what that parser writes of it",
            );
            return Err(Processed::Recorded(
                "EOFException: a chunked payload cut short".to_owned(),
            ));
        }
    };
    let payload_type = content_type_of(&response.headers).map_err(Processed::Thrown)?;
    Ok(Response {
        http: Some(http::Head {
            status: response.status,
            reason: response.reason,
            headers: response.headers,
        }),
        payload_type,
        body: response.body,
        size: response.size,
    })
}

/// A response whose HTTP header jwarc fails on, which `WARCParser` records.
fn unparsed_http<'a>(difference: &mut Option<Difference>) -> Processed<'a> {
    differs(
        difference,
        "a response whose HTTP header jwarc fails on, which Tika records with jwarc's message",
    );
    Processed::Recorded("ParsingException: invalid HTTP message".to_owned())
}

/// The response of a Gemini response record, whose block is `block`: the payload as jwarc's
/// Gemini parser reads it, the rest of the block after the header, and its type the header's.
/// jwarc writes the header it parsed back in front of what its buffer read on past it, and
/// throws where that header is longer than what it read.
fn gemini_response<'a>(
    block: &'a [u8],
    difference: &mut Option<Difference>,
) -> Result<Response<'a>, Processed<'a>> {
    let header = gemini::header(block);
    let (status, meta, length) = match &header {
        gemini::Header::Invalid => {
            differs(
                difference,
                "a Gemini response whose header jwarc fails on, which Tika records with \
                 jwarc's message",
            );
            return Err(Processed::Recorded(
                "ParsingException: invalid gemini message".to_owned(),
            ));
        }
        gemini::Header::Whole {
            status,
            meta,
            length,
        } => (*status, Some(meta.as_str()), *length),
        gemini::Header::Cut { status, meta } => (*status, meta.as_deref(), block.len()),
    };
    if gemini::read(block.len(), &header) < gemini::serialized(status, meta).len() {
        return Err(Processed::Thrown(
            "IllegalArgumentException: pushback would result in negative position".to_owned(),
        ));
    }
    let payload_type = match gemini::content_type(status, meta) {
        Ok(Some(text)) => MediaType::parse_leniently(text).map_err(|unparsable| {
            Processed::Thrown(format!("IllegalArgumentException: {}", unparsable.0))
        })?,
        Ok(None) => media_type::octet_stream(),
        Err(()) => {
            return Err(Processed::Thrown(
                "NullPointerException: the meta of a Gemini response cut short".to_owned(),
            ))
        }
    };
    let body = &block[length..];
    Ok(Response {
        http: None,
        payload_type,
        body: vec![body],
        size: body.len() as i64,
    })
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
        assert_fails_as_tika, assert_read_where_tika_fails, assert_same_as_tika,
        conformance_corpus, documents, Scratch,
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
            let mut recorded = Vec::new();
            let visited = visit(data, &mut difference, |item| {
                if let Item::Recorded(exception) = item {
                    recorded.push(exception);
                }
                Ok::<(), Infallible>(())
            });
            assert!(matches!(visited, Ok(None)));
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

    /// A Gemini response record of `block`.
    fn gemini(block: &[u8]) -> Vec<u8> {
        record(
            &[
                ("WARC-Type", "response"),
                ("WARC-Record-ID", "<urn:uuid:gemini>"),
                ("WARC-Target-URI", "gemini://example.dk/"),
                ("Content-Type", "application/gemini; msgtype=response"),
            ],
            block,
        )
    }

    /// The exceptions recorded and the one thrown when the records of `data` are visited.
    fn visited(data: &[u8]) -> (Vec<String>, Option<String>) {
        let mut difference = None;
        let mut recorded = Vec::new();
        let thrown = visit(data, &mut difference, |item| {
            if let Item::Recorded(exception) = item {
                recorded.push(exception);
            }
            Ok::<(), Infallible>(())
        })
        .unwrap_or_else(|never| match never {});
        (recorded, thrown)
    }

    /// The payloads the records of `data` hand on, each its body and its metadata, and the
    /// exceptions recorded.
    fn payloads(data: &[u8]) -> (Vec<(Vec<u8>, TikaMetadata)>, Vec<String>) {
        let mut difference = None;
        let mut payloads = Vec::new();
        let mut recorded = Vec::new();
        let thrown = visit(data, &mut difference, |item| {
            match item {
                Item::Payload(payload) => payloads.push((payload.body.concat(), payload.metadata)),
                Item::Recorded(exception) => recorded.push(exception),
            }
            Ok::<(), Infallible>(())
        })
        .unwrap_or_else(|never| match never {});
        assert_eq!(thrown, None);
        (payloads, recorded)
    }

    /// A Gemini response's payload is handed on, without the HTTP response jwarc fails on or
    /// refuses once the payload is read into, where Tika records that failure or fails the
    /// parse (#184).
    #[test]
    fn reads_the_payloads_of_gemini_responses_without_an_http_response() {
        let after = response("<urn:uuid:after>", b"HTTP/1.1 200 OK\r\n\r\nafter");
        let archive = [
            gemini(b"20 text/gemini\r\n# Hej verden\n"),
            gemini(b"51 not found\r\n"),
            after,
        ]
        .concat();
        let (payloads, recorded) = payloads(&archive);
        assert!(recorded.is_empty(), "{recorded:?}");
        let read: Vec<(&[u8], Option<&str>, Option<&str>)> = payloads
            .iter()
            .map(|(body, metadata)| {
                (
                    body.as_slice(),
                    metadata.get(RECORD_CONTENT_TYPE),
                    metadata.get(HTTP_STATUS),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                (&b"# Hej verden\n"[..], Some("text/gemini"), None),
                (&b""[..], Some("application/octet-stream"), None),
                (&b"after"[..], Some("application/octet-stream"), Some("200")),
            ]
        );
        assert_eq!(payloads[0].1.get("Content-Length"), Some("13"));
        assert!(
            payloads[0]
                .1
                .names()
                .all(|name| !name.starts_with(WARC_HTTP_PREFIX)),
            "{:?}",
            payloads[0].1.names().collect::<Vec<_>>()
        );
    }

    /// Web archives of Gemini responses with a payload, on which Tika's path fails, and the
    /// native parser reads (#184).
    #[cfg(feature = "tika")]
    #[test]
    fn reads_gemini_payloads_where_tikas_path_fails() {
        let scratch = Scratch::new("warc-gemini");
        let after = response("<urn:uuid:after>", b"HTTP/1.1 200 OK\r\n\r\nafter");
        let written = [
            scratch.write(
                "gemini-payload.warc",
                &[gemini(b"20 text/gemini\r\n# Hej verden\n"), after.clone()].concat(),
            ),
            scratch.write(
                "gemini-empty-and-payload.warc",
                &[
                    gemini(b"51 not found\r\n"),
                    gemini(b"20 text/plain\r\nHej verden\n"),
                    after,
                ]
                .concat(),
            ),
        ];
        let paths: Vec<&Path> = written.iter().map(PathBuf::as_path).collect();
        assert_read_where_tika_fails(&paths, "#184");
    }

    #[test]
    fn reads_gemini_responses_as_jwarc_and_warc_parser_do() {
        let after = response("<urn:uuid:after>", b"HTTP/1.1 200 OK\r\n\r\nafter");
        let invalid = [gemini(b"2x text/gemini\r\n# Hej"), after].concat();
        assert_eq!(
            visited(&invalid),
            (
                vec!["ParsingException: invalid gemini message".to_owned()],
                None
            )
        );
        let (_, thrown) = visited(&gemini(b"2"));
        assert!(
            thrown.is_some_and(|thrown| thrown.starts_with("IllegalArgumentException")),
            "a header cut short is pushed back past the body's start"
        );
    }
}
