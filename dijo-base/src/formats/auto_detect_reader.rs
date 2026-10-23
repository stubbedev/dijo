//! Tika's `AutoDetectReader`, as the parsers of text formats read through it: the charset
//! detection of [`charset::detect`], told the document's content type; the whole input decoded,
//! a piece at a time, with the detected charset's vendor superset; and the byte order mark
//! dropped from the start of the text, once: a U+FEFF the text starts with, unless the decoder
//! already read a byte order mark to set the byte order, as `UTF-16` does. Tika drops a second
//! U+FEFF after the byte order mark `UTF-16` drops, losing a character of the text (#186). A
//! declared charset the port does not decode is passed over ([`AutoDetectReader::difference`]).

use std::ops::Range;

use crate::formats::metadata::TikaMetadata;
use crate::formats::Difference;
use crate::input::{Pages, Paging};
use crate::text::charset::{self, Detection, Pieces};

/// The input decoded at a time into a piece of text, so that a large input is never held as text
/// whole.
const PIECE: usize = 1 << 20;

/// A document's text as an `AutoDetectReader` reads it.
pub struct AutoDetectReader {
    pub detection: Detection,
    pub text: String,
}

impl AutoDetectReader {
    /// Reads `input` as Tika does when the metadata's `Content-Type` is `content_type`, but for
    /// the [`AutoDetectReader::difference`].
    pub fn read(input: &[u8], content_type: Option<&str>) -> Self {
        let (detection, text) = Self::stream(input, content_type, Pages::IN_MEMORY);
        Self {
            detection,
            text: text.collect(),
        }
    }

    /// The detection of [`AutoDetectReader::read`], and the text, read a piece at a time, the
    /// pages of `input` released as the pieces are read.
    pub fn stream<'a>(
        input: &'a [u8],
        content_type: Option<&str>,
        pages: Pages<'a>,
    ) -> (Detection, Text<'a>) {
        let detection = charset::detect_decodable(input, content_type);
        let pieces = detection
            .decode_as
            .pieces(input, PIECE)
            .expect("the port decodes every charset its detection settles on");
        (
            detection,
            Text {
                started: pieces.dropped_bom(),
                pieces,
                paging: Paging::new(pages),
            },
        )
    }

    /// Where reading `input` of the content type `content_type` differs from Tika's
    /// `AutoDetectReader`: a declaration names a charset the native decoders do not decode,
    /// which the reader passes over where Tika's detection weighs it.
    pub fn difference(input: &[u8], content_type: Option<&str>) -> Option<Difference> {
        charset::undecodable_declaration(input, content_type).map(|charset| {
            Difference::new(format!(
                "a declaration names the charset {}, which the native decoders do not decode, \
                 so the text is read as if it named none",
                charset.name()
            ))
        })
    }
}

/// The text of an `AutoDetectReader`, a piece at a time, without its byte order mark.
pub struct Text<'a> {
    pieces: Pieces<'a>,
    paging: Paging<'a>,
    started: bool,
}

impl Iterator for Text<'_> {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        let mut piece = self.pieces.next()?;
        self.paging.read(PIECE);
        if !self.started && !piece.is_empty() {
            self.started = true;
            if piece.starts_with('\u{FEFF}') {
                piece.replace_range(..'\u{FEFF}'.len_utf8(), "");
            }
        }
        Some(piece)
    }
}

impl<'a> Text<'a> {
    /// The lines of [`lines`], read a piece at a time, so that only the line being read is
    /// held, and not the text.
    pub fn lines(self) -> Lines<'a> {
        Lines {
            text: self,
            pending: String::new(),
            at: 0,
            ended: false,
            after_carriage_return: false,
        }
    }
}

/// The lines of an `AutoDetectReader`'s [`Text`], as [`lines`] splits text.
pub struct Lines<'a> {
    text: Text<'a>,
    /// The text read but not yet returned, from `at` on.
    pending: String,
    at: usize,
    /// Whether the text has no more pieces.
    ended: bool,
    /// Whether the last line ended at a carriage return, so that a line feed right after it
    /// ends no line of its own.
    after_carriage_return: bool,
}

impl Lines<'_> {
    /// Drops what was returned and reads the next piece.
    fn read_on(&mut self) {
        self.pending.drain(..self.at);
        self.at = 0;
        match self.text.next() {
            Some(piece) => self.pending.push_str(&piece),
            None => self.ended = true,
        }
    }
}

impl Iterator for Lines<'_> {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        loop {
            if self.after_carriage_return {
                if self.at == self.pending.len() && !self.ended {
                    self.read_on();
                    continue;
                }
                self.after_carriage_return = false;
                if self.pending[self.at..].starts_with('\n') {
                    self.at += 1;
                }
            }
            let rest = &self.pending[self.at..];
            if let Some(end) = rest.find(['\n', '\r']) {
                let line = rest[..end].to_owned();
                self.after_carriage_return = rest.as_bytes()[end] == b'\r';
                self.at += end + 1;
                return Some(line);
            }
            if self.ended {
                if rest.is_empty() {
                    return None;
                }
                let line = rest.to_owned();
                self.at = self.pending.len();
                return Some(line);
            }
            self.read_on();
        }
    }
}

/// The characters of a piece of [`Text`], which the iterator owns.
pub struct OwnedChars {
    piece: String,
    at: usize,
}

impl Iterator for OwnedChars {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        let c = self.piece[self.at..].chars().next()?;
        self.at += c.len_utf8();
        Some(c)
    }
}

/// The characters of `piece`, which the iterator owns.
pub fn owned_chars(piece: String) -> OwnedChars {
    OwnedChars { piece, at: 0 }
}

/// Sets the metadata an `AutoDetectReader`'s detection sets, in Tika's order: the detected
/// charset, the detector that won, the trace, and the decoding charset (`getCharset`) when it is a
/// superset of the detected one.
pub fn set_metadata(detection: &Detection, metadata: &mut TikaMetadata) {
    metadata.set("tk:detected-encoding", detection.charset.name());
    metadata.set("tk:encoding-detector", detection.detector.tika_name());
    metadata.set("tk:encoding-detection-trace", detection.trace.to_string());
    if detection.decode_as != detection.charset {
        metadata.set("tk:decoded-charset", detection.decode_as.name());
    }
}

/// The lines Java's `BufferedReader.readLine` returns: split at a line feed, a carriage return,
/// or both in that order, the last line returned only when it holds characters.
pub fn lines(text: &str) -> impl Iterator<Item = &str> {
    line_ranges(text.as_bytes()).map(|range| &text[range])
}

/// Where the lines of [`lines`] lie in `bytes`, for text in a charset that encodes the line
/// feed and the carriage return as those single bytes and no other character with them.
pub fn line_ranges(bytes: &[u8]) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start == bytes.len() {
            return None;
        }
        let rest = &bytes[start..];
        let Some(end) = rest.iter().position(|&byte| byte == b'\n' || byte == b'\r') else {
            let line = start..bytes.len();
            start = bytes.len();
            return Some(line);
        };
        let terminator = if rest[end..].starts_with(b"\r\n") {
            2
        } else {
            1
        };
        let line = start..start + end;
        start += end + terminator;
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
    fn streams_the_lines_java_reads_across_pieces() {
        let streamed = |text: &str| -> Vec<String> {
            let (_, read) =
                AutoDetectReader::stream(text.as_bytes(), Some("text/plain"), Pages::IN_MEMORY);
            read.lines().collect()
        };
        for text in ["", "a", "a\n", "\n", "a\r\nb\rc\n\nd", "a\r\r\n"] {
            assert_eq!(streamed(text), lines(text).collect::<Vec<_>>(), "{text:?}");
        }
        let long = "a".repeat(PIECE - 1);
        for text in [
            format!("{long}\r\nb\r"),
            format!("{long}a\r\nb"),
            format!("{long}\n\nb\n"),
            format!("{long}{long}\rb"),
        ] {
            assert_eq!(streamed(&text), lines(&text).collect::<Vec<_>>());
        }
    }

    #[test]
    fn drops_one_byte_order_mark() {
        let read = |input: &[u8]| AutoDetectReader::read(input, Some("text/plain")).text;
        assert_eq!(read(b"\xEF\xBB\xBFplain text"), "plain text");
        assert_eq!(
            read(b"\xEF\xBB\xBF\xEF\xBB\xBFplain text"),
            "\u{FEFF}plain text"
        );
    }

    /// A declared `UTF-16` drops its byte order mark once, keeping a U+FEFF after it, which
    /// Tika drops as a second byte order mark (#186).
    #[test]
    fn drops_the_byte_order_mark_utf16_reads_once() {
        let read =
            |input: &[u8]| AutoDetectReader::read(input, Some("text/plain; charset=UTF-16")).text;
        assert_eq!(read(b"\xFE\xFF\x00a\x00b"), "ab");
        assert_eq!(read(b"\xFE\xFF\xFE\xFF\x00a\x00b"), "\u{FEFF}ab");
    }

    /// A piece's characters taken by the iterator that owns it are those collected from it.
    #[test]
    fn owns_the_characters_of_a_piece() {
        for piece in [
            "",
            "a",
            "plain text",
            "æøå\t\u{FEFF}",
            "\u{1F600}\r\n\u{10FFFF}",
            "\0x\u{80}",
        ] {
            let owned: Vec<char> = owned_chars(piece.to_owned()).collect();
            assert_eq!(owned, piece.chars().collect::<Vec<char>>(), "{piece:?}");
        }
    }
}
