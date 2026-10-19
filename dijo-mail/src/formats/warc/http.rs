//! jwarc's `HttpResponse.parse` of a response record's block, leniently, as `WarcResponse.http`
//! parses it: the status line and headers by jwarc's HTTP machine, then the body, the rest of
//! the block, decoded from the chunked transfer coding when the headers name it, as
//! `ChunkedBody` decodes it leniently.

use super::headers::Headers;
use super::ragel::{Flow, Signed, Unsigned, ERROR};
use super::tables::{CHUNKED, HTTP, HTTP_EN_HTTP_RESPONSE_LENIENT};
use super::text::TextBuffer;
use crate::java;

/// A parsed HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HttpResponse<'a> {
    pub(super) status: i32,
    pub(super) reason: Option<String>,
    pub(super) headers: Headers,
    /// The payload as its stream reads it: the pieces of the block it is made of, in order, the
    /// data of each chunk of a chunked one.
    pub(super) body: Vec<&'a [u8]>,
    /// What `body().size()` gives: the length of a body read to the block's end, `-1` for a
    /// chunked one.
    pub(super) size: i64,
}

/// The status line and headers of a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Head {
    pub(super) status: i32,
    pub(super) reason: Option<String>,
    pub(super) headers: Headers,
}

/// Why the response is not read as jwarc reads it without an `IOException`, which Tika records
/// with its stack trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unread {
    /// The status line or headers do not parse: a `ParsingException`.
    Header,
    /// The chunked body ends before its last chunk: an `EOFException` while it is read.
    CutChunks,
}

/// The status line and headers of a response as jwarc's lenient `HttpParser` reads them from
/// the start of `message`, and where they end in it; the error a `ParsingException` reports.
pub(super) fn head(message: &[u8]) -> Result<(Head, usize), Unread> {
    let mut state = Parsing::default();
    let mut cs = HTTP_EN_HTTP_RESPONSE_LENIENT;
    let end = HTTP
        .exec(&mut cs, &Unsigned(message), 0, true, &mut |action, p| {
            Ok::<_, ()>(state.act(action, message.get(p).copied()))
        })
        .expect("the HTTP actions never fail");
    if cs == ERROR {
        return Err(Unread::Header);
    }
    Ok((
        Head {
            status: state.status,
            reason: state.reason,
            headers: state.headers,
        },
        end,
    ))
}

/// `HttpResponse.parse` of `block`, the record's block, with a lenient parser.
pub(super) fn parse(block: &[u8]) -> Result<HttpResponse<'_>, Unread> {
    let (head, end) = head(block)?;
    let rest = &block[end..];
    let (body, size) = if head.headers.contains("Transfer-Encoding", "chunked") {
        (dechunk(rest)?, -1)
    } else {
        (vec![rest], rest.len() as i64)
    };
    Ok(HttpResponse {
        status: head.status,
        reason: head.reason,
        headers: head.headers,
        body,
        size,
    })
}

/// The state `HttpParser`'s actions keep.
#[derive(Default)]
struct Parsing {
    buffer: TextBuffer,
    major: i32,
    minor: i32,
    status: i32,
    reason: Option<String>,
    name: Option<String>,
    headers: Headers,
}

impl Parsing {
    /// Runs action `action` of `HttpParser` on `byte`, the key it fires on, absent at the end.
    fn act(&mut self, action: u16, byte: Option<u8>) -> Flow {
        let digit = || i32::from(byte.expect("a digit action fires on a key")) - i32::from(b'0');
        match action {
            0 => self.buffer.push(byte.expect("push fires on a key")),
            1 => self.major = self.major * 10 + digit(),
            2 => self.minor = self.minor * 10 + digit(),
            3 => self.status = self.status * 10 + digit(),
            4 => self.buffer.end_text(),
            5 | 7 | 8 => self.buffer.restart(),
            6 => {
                self.reason = Some(java::latin1(self.buffer.pushed()));
                self.buffer.restart();
            }
            9 => return Flow::Break,
            10 => self.buffer.fold(),
            11 => {
                let name = java::ascii(self.buffer.pushed());
                self.name = Some(java::trim(&name).to_owned());
                self.buffer.restart();
            }
            12 => {
                let value = java::latin1(self.buffer.text());
                let name = self.name.clone().expect("a header value follows its name");
                self.headers.add(name, value);
                self.buffer.restart();
                self.buffer.forget_text();
            }
            _ => unreachable!("the HTTP machine has no action {action}"),
        }
        Flow::Continue
    }
}

/// What `ChunkedBody` reads of `rest`, leniently: the chunks' data up to the last chunk, or,
/// from a chunk header the machine rejects on, the rest as it is.
fn dechunk(rest: &[u8]) -> Result<Vec<&[u8]>, Unread> {
    let keys = Signed(rest);
    let mut body = Vec::new();
    let mut cs = super::tables::CHUNKED_START;
    let mut size: i64 = 0;
    let mut at = 0;
    loop {
        let mut chunk: Option<i64> = None;
        let header_start = at;
        let end = CHUNKED
            .exec(&mut cs, &keys, at, false, &mut |action, p| {
                match action {
                    0 => {
                        let digit = char::from(rest[p]).to_digit(16).map_or(-1, i64::from);
                        size = size.wrapping_mul(16).wrapping_add(digit);
                    }
                    1 => {
                        if size != 0 {
                            chunk = Some(size);
                            size = 0;
                            return Ok::<_, ()>(Flow::Break);
                        }
                    }
                    2 => chunk = Some(0),
                    _ => unreachable!("the chunked machine has no action {action}"),
                }
                Ok(Flow::Continue)
            })
            .expect("the chunked actions never fail");
        if cs == ERROR {
            body.push(&rest[header_start..]);
            return Ok(body);
        }
        at = end;
        match chunk {
            Some(0) => return Ok(body),
            Some(length) => {
                let available = (rest.len() - at) as i64;
                if length < 0 || length > available {
                    return Err(Unread::CutChunks);
                }
                let length = length as usize;
                body.push(&rest[at..at + length]);
                at += length;
            }
            None => return Err(Unread::CutChunks),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_responses_as_jwarc_does() {
        let response = parse(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nX-Folded: a\r\n  b\r\n\r\n<html>",
        )
        .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.reason.as_deref(), Some("OK"));
        assert_eq!(response.headers.first("content-type"), Some("text/html"));
        assert_eq!(response.headers.first("X-Folded"), Some("a b"));
        assert_eq!(response.body.concat(), b"<html>");
        assert_eq!(response.size, 6);
        let lenient = parse(b"HTTP/1.0 404\nServer: x").unwrap();
        assert_eq!(lenient.status, 404);
        assert_eq!(lenient.headers.first("Server"), Some("x"));
        assert!(lenient.body.concat().is_empty());
        assert_eq!(parse(b"\x00garbage"), Err(Unread::Header));
    }

    #[test]
    fn decodes_chunks_as_jwarc_does() {
        let chunked = parse(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(chunked.body.concat(), b"Wikipedia");
        assert_eq!(chunked.size, -1);
        assert_eq!(dechunk(b"4\r\nWi"), Err(Unread::CutChunks));
        assert_eq!(dechunk(b"zz\r\nraw").unwrap().concat(), b"zz\r\nraw");
    }
}
