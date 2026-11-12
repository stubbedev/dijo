//! EBML (RFC 8794), the binary markup Matroska and WebM are written in: elements read in place,
//! each an ID and a size, both variable-length integers, then its body, and the values a body
//! holds.

use std::fmt;

use crate::java;

/// An element's ID, its variable-length integer with the length marker kept, as the
/// specifications write it.
pub(super) type Id = u32;

/// The longest ID, in bytes, the Matroska and WebM headers allow.
const MAX_ID_LENGTH: usize = 4;
/// The longest size, in bytes, the Matroska and WebM headers allow.
const MAX_SIZE_LENGTH: usize = 8;

/// Why the EBML of a document is not well-formed, with where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Malformed(pub(super) String);

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The value of a variable-length integer at `at` of `bytes`, with its length marker kept or
/// not, as many bytes long as its first byte says, at most `max_length`; and its length.
fn vint(
    bytes: &[u8],
    at: usize,
    max_length: usize,
    marker_kept: bool,
) -> Result<(u64, usize), Malformed> {
    let first = *bytes
        .get(at)
        .ok_or_else(|| Malformed(format!("the data ends at {at}, inside an element header")))?;
    let length = first.leading_zeros() as usize + 1;
    if length > max_length {
        return Err(Malformed(format!(
            "the variable-length integer at {at} starts with {first:#04x}, which makes it longer \
             than {max_length} bytes"
        )));
    }
    let field = bytes.get(at..at + length).ok_or_else(|| {
        Malformed(format!(
            "the {length}-byte variable-length integer at {at} runs past the end of the data"
        ))
    })?;
    let mut value = if marker_kept {
        u64::from(first)
    } else {
        u64::from(first) & (0xFF >> length)
    };
    for &byte in &field[1..] {
        value = value << 8 | u64::from(byte);
    }
    Ok((value, length))
}

/// The unsigned variable-length integer at `at`, as a block's track number is written, and its
/// length.
pub(super) fn unsigned_vint(bytes: &[u8], at: usize) -> Result<(u64, usize), Malformed> {
    vint(bytes, at, MAX_SIZE_LENGTH, false)
}

/// The signed variable-length integer at `at`, as EBML lacing writes the differences between
/// frame sizes, and its length.
pub(super) fn signed_vint(bytes: &[u8], at: usize) -> Result<(i64, usize), Malformed> {
    let (value, length) = unsigned_vint(bytes, at)?;
    let bias = (1i64 << (7 * length - 1)) - 1;
    Ok((value as i64 - bias, length))
}

/// An element, read in place: its ID, where its header starts and where its body lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Element {
    pub(super) id: Id,
    pub(super) start: usize,
    pub(super) body_start: usize,
    pub(super) end: usize,
}

impl Element {
    /// Its body, of the `bytes` it was read from.
    pub(super) fn body<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[self.body_start..self.end]
    }

    /// How many bytes it takes, header and body.
    pub(super) fn length(&self) -> usize {
        self.end - self.start
    }
}

/// Where an element of unknown size ends: at the end of its parent, or before the first element
/// that is not one of its children, which its ID tells.
#[derive(Debug, Clone, Copy)]
pub(super) enum Unsized {
    ToParentEnd,
    BeforeOther(&'static [Id]),
}

/// The elements in a range of bytes, read one after the other.
pub(super) struct Children<'a> {
    bytes: &'a [u8],
    at: usize,
    end: usize,
    unknown_sized: fn(Id) -> Option<Unsized>,
}

impl<'a> Children<'a> {
    /// The elements from `start` to `end` of `bytes`, of which those `unknown_sized` names may
    /// have an unknown size.
    pub(super) fn new(
        bytes: &'a [u8],
        start: usize,
        end: usize,
        unknown_sized: fn(Id) -> Option<Unsized>,
    ) -> Self {
        Self {
            bytes,
            at: start,
            end,
            unknown_sized,
        }
    }

    /// The elements of the body of `parent`, none of which may have an unknown size.
    pub(super) fn of(bytes: &'a [u8], parent: &Element) -> Self {
        Self::new(bytes, parent.body_start, parent.end, |_| None)
    }

    /// The next element, or none past the last.
    pub(super) fn next(&mut self) -> Result<Option<Element>, Malformed> {
        if self.at >= self.end {
            return Ok(None);
        }
        let element = self.header(self.at)?;
        self.at = element.end;
        Ok(Some(element))
    }

    /// The element whose header starts at `start`.
    fn header(&self, start: usize) -> Result<Element, Malformed> {
        let (id, id_length) = vint(self.bytes, start, MAX_ID_LENGTH, true)?;
        let id = Id::try_from(id).expect("an ID of at most four bytes");
        let (size, size_length) = vint(self.bytes, start + id_length, MAX_SIZE_LENGTH, false)?;
        let body_start = start + id_length + size_length;
        let unknown = size == (1u64 << (7 * size_length)) - 1;
        let end = if unknown {
            match (self.unknown_sized)(id) {
                Some(Unsized::ToParentEnd) => self.end,
                Some(Unsized::BeforeOther(children)) => self.unsized_end(body_start, children)?,
                None => {
                    return Err(Malformed(format!(
                        "the element {id:#X} at {start} has an unknown size, which only a \
                         Segment or a Cluster may have"
                    )))
                }
            }
        } else {
            usize::try_from(size)
                .ok()
                .and_then(|size| body_start.checked_add(size))
                .filter(|&end| end <= self.end)
                .ok_or_else(|| {
                    Malformed(format!(
                        "the element {id:#X} at {start} declares {size} bytes, past the end of \
                         its parent at {}",
                        self.end
                    ))
                })?
        };
        Ok(Element {
            id,
            start,
            body_start,
            end,
        })
    }

    /// Where an element of unknown size whose body starts at `body_start` ends: before the
    /// first element whose ID is not among `children`, or at the end of its parent.
    fn unsized_end(&self, body_start: usize, children: &[Id]) -> Result<usize, Malformed> {
        let inner = Self::new(self.bytes, body_start, self.end, |_| None);
        let mut at = body_start;
        while at < self.end {
            let (id, _) = vint(self.bytes, at, MAX_ID_LENGTH, true)?;
            if !children.iter().any(|&child| u64::from(child) == id) {
                break;
            }
            at = inner.header(at)?.end;
        }
        Ok(at)
    }
}

/// An unsigned integer element's value.
pub(super) fn unsigned(element: &Element, bytes: &[u8]) -> Result<u64, Malformed> {
    let body = element.body(bytes);
    if body.len() > 8 {
        return Err(Malformed(format!(
            "the unsigned integer {:#X} at {} is {} bytes long, more than 8",
            element.id,
            element.start,
            body.len()
        )));
    }
    Ok(body
        .iter()
        .fold(0u64, |value, &byte| value << 8 | u64::from(byte)))
}

/// A signed integer element's value.
pub(super) fn signed(element: &Element, bytes: &[u8]) -> Result<i64, Malformed> {
    let length = element.end - element.body_start;
    let value = unsigned(element, bytes)?;
    if length == 0 || length == 8 {
        return Ok(value as i64);
    }
    let shift = 64 - 8 * length as u32;
    Ok(((value << shift) as i64) >> shift)
}

/// A float element's value: none in an empty body, or a 4- or 8-byte IEEE 754 float.
pub(super) fn float(element: &Element, bytes: &[u8]) -> Result<f64, Malformed> {
    match element.body(bytes) {
        [] => Ok(0.0),
        &[a, b, c, d] => Ok(f64::from(f32::from_be_bytes([a, b, c, d]))),
        &[a, b, c, d, e, f, g, h] => Ok(f64::from_be_bytes([a, b, c, d, e, f, g, h])),
        body => Err(Malformed(format!(
            "the float {:#X} at {} is {} bytes long, neither 4 nor 8",
            element.id,
            element.start,
            body.len()
        ))),
    }
}

/// A string or UTF-8 element's value, without the zero bytes that may pad it.
pub(super) fn text(element: &Element, bytes: &[u8]) -> String {
    let body = element.body(bytes);
    let end = body
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(body.len());
    java::utf8(&body[..end])
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// The bytes of `id` as it is written, without its leading zero bytes.
    fn id_bytes(id: Id) -> Vec<u8> {
        let bytes = id.to_be_bytes();
        let first = bytes.iter().position(|&byte| byte != 0).unwrap_or(3);
        bytes[first..].to_vec()
    }

    /// An element of `id` around `body`, its size written in eight bytes.
    pub(in super::super) fn element(id: Id, body: &[u8]) -> Vec<u8> {
        let mut bytes = id_bytes(id);
        let size = body.len() as u64 | 1 << 56;
        bytes.extend_from_slice(&size.to_be_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    /// An element of `id` of unknown size, followed by `body`.
    pub(in super::super) fn unsized_element(id: Id, body: &[u8]) -> Vec<u8> {
        let mut bytes = id_bytes(id);
        bytes.push(0xFF);
        bytes.extend_from_slice(body);
        bytes
    }

    /// An unsigned integer element.
    pub(in super::super) fn unsigned_element(id: Id, value: u64) -> Vec<u8> {
        element(id, &value.to_be_bytes())
    }

    #[test]
    fn reads_variable_length_integers() {
        assert_eq!(unsigned_vint(&[0x81], 0), Ok((1, 1)));
        assert_eq!(unsigned_vint(&[0x40, 0x02], 0), Ok((2, 2)));
        assert_eq!(signed_vint(&[0x5F, 0xFE], 0), Ok((-1, 2)));
        assert_eq!(signed_vint(&[0xBF], 0), Ok((0, 1)));
        assert!(unsigned_vint(&[0x00, 1, 2, 3, 4, 5, 6, 7, 8], 0).is_err());
        assert!(unsigned_vint(&[0x40], 0).is_err());
        assert_eq!(
            vint(&[0x1A, 0x45, 0xDF, 0xA3], 0, 4, true),
            Ok((0x1A45DFA3, 4))
        );
    }

    #[test]
    fn reads_elements_and_their_values() {
        let mut bytes = element(0x4489, &1.5f32.to_be_bytes());
        bytes.extend(element(0xFB, &[0xFF, 0x38]));
        bytes.extend(element(0x7BA9, b"Title\0\0"));
        let mut children = Children::new(&bytes, 0, bytes.len(), |_| None);
        let duration = children.next().unwrap().unwrap();
        assert_eq!(float(&duration, &bytes), Ok(1.5));
        let reference = children.next().unwrap().unwrap();
        assert_eq!(signed(&reference, &bytes), Ok(-200));
        assert_eq!(unsigned(&reference, &bytes), Ok(0xFF38));
        let title = children.next().unwrap().unwrap();
        assert_eq!(text(&title, &bytes), "Title");
        assert_eq!(children.next(), Ok(None));
    }

    #[test]
    fn ends_an_element_of_unknown_size_before_one_it_cannot_hold() {
        let mut bytes = unsized_element(0x1F43B675, &unsigned_element(0xE7, 5));
        bytes.extend(element(0x1C53BB6B, b""));
        let mut children = Children::new(&bytes, 0, bytes.len(), |_| {
            Some(Unsized::BeforeOther(&[0xE7]))
        });
        let cluster = children.next().unwrap().unwrap();
        assert_eq!(cluster.end, bytes.len() - 12);
        assert_eq!(children.next().unwrap().unwrap().id, 0x1C53BB6B);
        let mut strict = Children::new(&bytes, 0, bytes.len(), |_| None);
        assert!(strict.next().is_err());
    }

    #[test]
    fn rejects_an_element_past_its_parent() {
        let bytes = element(0x7BA9, b"Title");
        let mut children = Children::new(&bytes, 0, bytes.len() - 1, |_| None);
        assert!(children.next().unwrap_err().0.contains("past the end"));
    }
}
