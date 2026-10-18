//! jwarc's `GeminiResponse`, as `WarcResponse.payload` reads the block of a Gemini response
//! record for `WARCParser`: the header, `status SP meta CRLF`, by `GeminiParser`'s strict
//! response machine, through a buffer of 8,192 bytes, which reads on past the header; the
//! header written again in its place in the block; and the rest of the block the payload.

/// The bytes of the buffer jwarc parses the header through, and reads the block into.
const BUFFER: usize = 8192;
/// The most bytes `GeminiParser.push` takes of the meta.
const MAX_META: usize = 1024;

/// What `GeminiResponse.parse` makes of a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Header {
    /// The header ends `length` bytes into the block.
    Whole {
        status: u32,
        meta: String,
        length: usize,
    },
    /// The block ends before the header does, where the parse stops without an error: the
    /// status of the digits read, and the meta, if its CR was read.
    Cut { status: u32, meta: Option<String> },
    /// The header is not one: a `ParsingException`.
    Invalid,
}

/// Where `GeminiParser`'s strict response machine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Status(usize),
    Space,
    Meta,
    LineFeed,
}

/// `GeminiResponse.parse` of the header at the start of `block`.
pub(super) fn header(block: &[u8]) -> Header {
    let mut status = 0;
    let mut meta = Vec::new();
    let mut state = State::Status(0);
    for (at, &byte) in block.iter().enumerate() {
        state = match (state, byte) {
            (State::Status(digits), b'0'..=b'9') => {
                status = status * 10 + u32::from(byte - b'0');
                if digits == 0 {
                    State::Status(1)
                } else {
                    State::Space
                }
            }
            (State::Space, b' ') => State::Meta,
            (State::Meta, b'\r') => State::LineFeed,
            (State::Meta, b'\n') => return Header::Invalid,
            (State::Meta, _) if meta.len() >= MAX_META => return Header::Invalid,
            (State::Meta, _) => {
                meta.push(byte);
                State::Meta
            }
            (State::LineFeed, b'\n') => {
                return Header::Whole {
                    status,
                    meta: String::from_utf8_lossy(&meta).into_owned(),
                    length: at + 1,
                }
            }
            _ => return Header::Invalid,
        };
    }
    Header::Cut {
        status,
        meta: (state == State::LineFeed).then(|| String::from_utf8_lossy(&meta).into_owned()),
    }
}

/// `serializeHeader`: the header written again, the text `null` for a meta never read.
pub(super) fn serialized(status: u32, meta: Option<&str>) -> String {
    format!("{status:02} {}\r\n", meta.unwrap_or("null"))
}

/// How far into a block of `length` bytes jwarc's body stands once the header is parsed: the
/// whole block where it ends before the header, else the bytes its buffer took of it.
pub(super) fn read(length: usize, header: &Header) -> usize {
    match header {
        Header::Cut { .. } => length,
        _ => length.min(BUFFER),
    }
}

/// `contentType`'s text of a response of `status` and `meta`: the meta of a success, else
/// none, for `application/octet-stream`; `Err` where Java dereferences a meta never read.
pub(super) fn content_type(status: u32, meta: Option<&str>) -> Result<Option<&str>, ()> {
    if !(20..30).contains(&status) {
        return Ok(None);
    }
    match meta {
        None => Err(()),
        Some("") => Ok(Some("text/gemini; charset=utf-8")),
        Some(meta) => Ok(Some(meta)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_headers_as_the_strict_machine_does() {
        assert_eq!(
            header(b"20 text/gemini\r\n# Hej"),
            Header::Whole {
                status: 20,
                meta: "text/gemini".to_owned(),
                length: 16
            }
        );
        assert_eq!(header(b"2x text\r\n"), Header::Invalid);
        assert_eq!(header(b"20text\r\n"), Header::Invalid);
        assert_eq!(header(b"20 a\nb\r\n"), Header::Invalid);
        assert_eq!(header(b"20 a\rb"), Header::Invalid);
        let long = [b"20 ".to_vec(), vec![b'm'; MAX_META + 1], b"\r\n".to_vec()].concat();
        assert_eq!(header(&long), Header::Invalid);
        assert_eq!(
            header(b"51 not found\r"),
            Header::Cut {
                status: 51,
                meta: Some("not found".to_owned())
            }
        );
        assert_eq!(
            header(b"2"),
            Header::Cut {
                status: 2,
                meta: None
            }
        );
        assert_eq!(serialized(2, None), "02 null\r\n");
        assert_eq!(
            content_type(20, Some("")),
            Ok(Some("text/gemini; charset=utf-8"))
        );
        assert_eq!(content_type(51, None), Ok(None));
        assert_eq!(content_type(20, None), Err(()));
    }
}
