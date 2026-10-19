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
        "dijo::HttpParser"
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
