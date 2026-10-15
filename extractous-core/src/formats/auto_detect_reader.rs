//! Tika's `AutoDetectReader`, as the parsers of text formats read through it: the charset
//! detection of [`charset::detect`], told the document's content type; the whole input decoded
//! with the detected charset's vendor superset by the JDK's decoders; and one U+FEFF dropped from
//! the start of the text. That drops the byte order mark of UTF-8, UTF-16LE/BE and UTF-32LE/BE,
//! which Java's decoders for those charsets keep as a character; Java's `UTF-16` decoder, named
//! only by a declared charset, drops a byte order mark itself, so a second one goes too.

use crate::formats::metadata::TikaMetadata;
use crate::formats::Failure;
use crate::text::charset::{self, Detection};

/// A document's text as an `AutoDetectReader` reads it.
pub(crate) struct AutoDetectReader {
    pub(crate) detection: Detection,
    pub(crate) text: String,
}

impl AutoDetectReader {
    /// Reads `input` as Tika does when the metadata's `Content-Type` is `content_type`.
    pub(crate) fn read(input: &[u8], content_type: &str) -> Result<Self, Failure> {
        let detection = charset::detect(input, Some(content_type));
        let mut text = detection.decode_as.decode(input).ok_or_else(|| {
            Failure::Malformed(format!(
                "the text is detected as {}, which the native decoders do not decode",
                detection.decode_as.name()
            ))
        })?;
        if text.starts_with('\u{FEFF}') {
            text.replace_range(..'\u{FEFF}'.len_utf8(), "");
        }
        Ok(Self { detection, text })
    }

    /// Why a parser reading through an `AutoDetectReader` should leave `input` to Tika: a
    /// detector may name a charset the native decoders do not decode.
    pub(crate) fn defers(input: &[u8]) -> Option<&'static str> {
        charset::undecodable(input)
            .map(|_| "a detector may report a charset the native decoders do not decode")
    }
}

/// Sets the metadata an `AutoDetectReader`'s detection sets, in Tika's order: the detected
/// charset, the detector that won, the trace, and the decoding charset (`getCharset`) when it is a
/// superset of the detected one.
pub(crate) fn set_metadata(detection: &Detection, metadata: &mut TikaMetadata) {
    metadata.set("tk:detected-encoding", detection.charset.name());
    metadata.set("tk:encoding-detector", detection.detector.tika_name());
    metadata.set("tk:encoding-detection-trace", detection.trace.to_string());
    if detection.decode_as != detection.charset {
        metadata.set("tk:decoded-charset", detection.decode_as.name());
    }
}

/// The lines Java's `BufferedReader.readLine` returns: split at a line feed, a carriage return,
/// or both in that order, the last line returned only when it holds characters.
pub(crate) fn lines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let Some(end) = rest.find(['\n', '\r']) else {
            return Some(std::mem::take(&mut rest));
        };
        let line = &rest[..end];
        let terminator = if rest[end..].starts_with("\r\n") {
            2
        } else {
            1
        };
        rest = &rest[end + terminator..];
        Some(line)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lines_as_java_does() {
        fn read(text: &str) -> Vec<&str> {
            lines(text).collect()
        }
        assert_eq!(read(""), Vec::<&str>::new());
        assert_eq!(read("a"), ["a"]);
        assert_eq!(read("a\n"), ["a"]);
        assert_eq!(read("\n"), [""]);
        assert_eq!(read("a\r\nb\rc\n\nd"), ["a", "b", "c", "", "d"]);
        assert_eq!(read("a\r\r\n"), ["a", ""]);
    }

    #[test]
    fn drops_one_byte_order_mark() {
        let read = |input: &[u8]| AutoDetectReader::read(input, "text/plain").unwrap().text;
        assert_eq!(read(b"\xEF\xBB\xBFplain text"), "plain text");
        assert_eq!(
            read(b"\xEF\xBB\xBF\xEF\xBB\xBFplain text"),
            "\u{FEFF}plain text"
        );
    }
}
