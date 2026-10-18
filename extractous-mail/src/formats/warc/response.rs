//! Bare HTTP responses, as Tika's `HttpParser` reads them through jwarc: the status line and
//! headers read leniently, then as many bytes of payload as `Content-Length` gives handed to the
//! embedded document extractor. Nothing of the headers is written.

use super::http::{self, Unread};
use crate::config::ParserConfigs;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Difference, Failure, Footprint, Opened, Parser, Prepared};
use crate::input::{Input, Pages};
use crate::java;

/// Tika's `HttpParser`.
pub struct HttpParser;

impl Parser for HttpParser {
    fn name(&self) -> &'static str {
        "extractous::HttpParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.http.HttpParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-httpresponse"]
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

    /// Reads the head, and hands the payload on in place, with the pages of the input.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let payload = payload(input);
        let cut = matches!(payload, Payload::Cut(_));
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            let embed = |xhtml: &mut crate::formats::xhtml::Xhtml, payload| {
                xhtml.parse_embedded_input_opened(
                    &Input::Within(payload, pages),
                    &TikaMetadata::new(),
                    Wrapping::PackageEntry,
                    Opened::Alone,
                )
            };
            match payload {
                Payload::Absent => Ok(()),
                Payload::Present(payload) => Ok(embed(xhtml, payload)?),
                Payload::Cut(payload) => {
                    embed(xhtml, payload)?;
                    Err(Failure::Malformed(CUT.to_owned()))
                }
                Payload::Thrown(thrown) => Err(Failure::Malformed(thrown)),
            }
        });
        if !cut {
            return Ok(reading);
        }
        Err(Decline::Differs {
            reading,
            difference: Difference::new(
                "a payload cut short of its Content-Length: the native parser parses what there \
                 is, then fails as jwarc's LengthedBody fails Tika's when the payload's parser \
                 lets its EOFException through, which a parser that catches it does not",
            ),
        })
    }
}

/// What a response holds for the embedded document extractor.
enum Payload<'a> {
    /// No payload: no `Content-Length`, or one that is not positive.
    Absent,
    Present(&'a [u8]),
    /// What there is of a payload cut short of its `Content-Length`.
    Cut(&'a [u8]),
    /// The exception the parse ends with.
    Thrown(String),
}

/// What jwarc's `LengthedBody` throws when the input ends before the payload does.
const CUT: &str = "EOFException: a payload cut short of its Content-Length";

/// The payload `Content-Length` gives, or what there is of one cut short of its length, which
/// Tika's `LengthedBody` throws on while the payload's parser reads it.
fn payload(input: &[u8]) -> Payload<'_> {
    if input.is_empty() {
        return Payload::Thrown("EOFException".to_owned());
    }
    let (head, end) = match http::head(input) {
        Ok(head) => head,
        Err(Unread::Header | Unread::CutChunks) => {
            return Payload::Thrown("ParsingException: invalid HTTP message".to_owned());
        }
    };
    let length = match head.headers.sole("Content-Length") {
        Err(not_sole) => {
            return Payload::Thrown(format!("IllegalArgumentException: {}", not_sole.0));
        }
        Ok(None) => 0,
        Ok(Some(value)) => match java::parse_long(value) {
            Some(length) => length,
            None => {
                return Payload::Thrown(format!(
                    "NumberFormatException: For input string: \"{value}\""
                ));
            }
        },
    };
    let Some(length) = usize::try_from(length).ok().filter(|&length| length > 0) else {
        return Payload::Absent;
    };
    let rest = &input[end..];
    rest.get(..length)
        .map_or(Payload::Cut(rest), Payload::Present)
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, assert_same_as_tika_departing,
        conformance_corpus, documents, Scratch,
    };

    const RESPONSE: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=UTF-8\r\n\
                            Content-Length: 22\r\n\r\nHej fra K\u{f8}benhavn\nmere tekst efter";

    #[test]
    fn extracts_http_responses_as_tikas_path_does() {
        let scratch = Scratch::new("http-response");
        let read = [
            scratch.write("response", RESPONSE.as_bytes()),
            scratch.write(
                "lenient",
                b"HTTP/1.0  404 Not Found\nServer : x\nContent-Length: 4\n\nnope",
            ),
            scratch.write(
                "empty",
                b"HTTP/1.1 204 No Content\r\nDate: Mon, 01 Jan 2024 00:00:00 GMT\r\n\r\nrest",
            ),
        ];
        let failing = [scratch.write(
            "twice",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nab",
        )];
        let cut = scratch.write("cut", &RESPONSE.as_bytes()[..RESPONSE.len() - 20]);
        let (parsed, deferred) = documents(&HttpParser, &[scratch.directory()]);
        assert_eq!(deferred, [cut]);
        assert_eq!(parsed.len(), read.len() + failing.len(), "{parsed:?}");
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            HttpParser.tika_name(),
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    /// Its page ends `</body>\n</html>`: the HTML parser puts the line feed in the body, so the
    /// native XHTML ends the page's `package-entry` with it, where Tika's has none (#84).
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_http_response_as_tikas_path_does() {
        let (parsed, deferred) = documents(&HttpParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 1, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika_departing(
            &paths,
            HttpParser.tika_name(),
            crate::formats::html::TREE_DEPARTURES,
        );
    }
}
