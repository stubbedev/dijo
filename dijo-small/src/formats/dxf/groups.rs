//! The groups of a DXF drawing, each a group code and its value, read one at a time from the
//! ASCII form (a line of the code, a line of the value) or the binary form (after its sentinel,
//! the code in one byte, two after a byte of 255, or, from AutoCAD Release 13 on, in two, and the
//! value in the binary form of the type its code gives).

use crate::formats::Failure;
use crate::input::{Pages, Paging};

/// What a binary DXF drawing starts with.
const BINARY_SENTINEL: &[u8] = b"AutoCAD Binary DXF\r\n\x1a\0";
/// What a one-byte group code of a binary drawing is when a two-byte one follows.
const WIDE_CODE: u8 = 255;
const UTF8_BOM: &[u8] = b"\xef\xbb\xbf";
/// The most bytes of a string an error shows.
const SHOWN: usize = 64;

/// A group's value. An ASCII drawing's values are all text, numbers read from it on demand.
#[derive(Debug, Clone, Copy)]
pub(super) enum Value<'a> {
    Text(&'a [u8]),
    Real(f64),
    Integer(i64),
    /// A chunk of binary data, which is not read.
    Bytes,
}

impl<'a> Value<'a> {
    /// The bytes of a string value.
    pub(super) fn text(self) -> Option<&'a [u8]> {
        match self {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    /// The value of a real number, or of one written as text.
    pub(super) fn real(self) -> Option<f64> {
        match self {
            Value::Real(real) => Some(real),
            Value::Text(text) => std::str::from_utf8(text).ok()?.trim().parse().ok(),
            Value::Integer(_) | Value::Bytes => None,
        }
    }

    /// The value as an error shows it: a string quoted, its first [`SHOWN`] bytes at most.
    pub(super) fn shown(self) -> String {
        match self {
            Value::Text(text) => {
                format!(
                    "{:?}",
                    String::from_utf8_lossy(&text[..text.len().min(SHOWN)])
                )
            }
            Value::Real(real) => real.to_string(),
            Value::Integer(integer) => integer.to_string(),
            Value::Bytes => "binary data".to_owned(),
        }
    }

    /// The value of an integer, or of one written as text.
    pub(super) fn integer(self) -> Option<i64> {
        match self {
            Value::Integer(integer) => Some(integer),
            Value::Text(text) => std::str::from_utf8(text).ok()?.trim().parse().ok(),
            Value::Real(_) | Value::Bytes => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Group<'a> {
    pub(super) code: u16,
    pub(super) value: Value<'a>,
}

/// The form a drawing is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Ascii,
    /// Binary, its group codes in two bytes (`wide`) or one.
    Binary {
        wide: bool,
    },
}

/// The binary type of a group's value, which its code gives.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Text,
    Real,
    Int16,
    Int32,
    Int64,
    Bool,
    Bytes,
}

impl Kind {
    /// The type of the values of group `code`, as the DXF reference gives the code ranges.
    fn of(code: u16) -> Option<Kind> {
        Some(match code {
            0..=9
            | 100..=109
            | 300..=309
            | 320..=369
            | 390..=399
            | 410..=419
            | 430..=439
            | 470..=481
            | 999..=1003
            | 1005..=1009 => Kind::Text,
            10..=59 | 110..=149 | 210..=239 | 460..=469 | 1010..=1059 => Kind::Real,
            60..=79 | 170..=179 | 270..=289 | 370..=389 | 400..=409 | 1060..=1070 => Kind::Int16,
            90..=99 | 420..=429 | 440..=459 | 1071 => Kind::Int32,
            160..=169 => Kind::Int64,
            290..=299 => Kind::Bool,
            310..=319 | 1004 => Kind::Bytes,
            _ => return None,
        })
    }
}

/// The groups of a drawing, read in order, its pages released as they are.
pub(super) struct Groups<'a> {
    data: &'a [u8],
    at: usize,
    form: Form,
    paging: Paging<'a>,
}

impl<'a> Groups<'a> {
    pub(super) fn new(data: &'a [u8], pages: Pages<'a>) -> Self {
        let (form, at) = match data.strip_prefix(BINARY_SENTINEL) {
            Some(rest) => (
                Form::Binary {
                    wide: rest.get(1) == Some(&0),
                },
                BINARY_SENTINEL.len(),
            ),
            None if data.starts_with(UTF8_BOM) => (Form::Ascii, UTF8_BOM.len()),
            None => (Form::Ascii, 0),
        };
        Self {
            data,
            at,
            form,
            paging: Paging::new(pages),
        }
    }

    /// The next group, or `None` at the end of the drawing.
    pub(super) fn next_group(&mut self) -> Result<Option<Group<'a>>, Failure> {
        if self.at >= self.data.len() {
            return Ok(None);
        }
        match self.form {
            Form::Ascii => self.ascii().map(Some),
            Form::Binary { wide } => self.binary(wide).map(Some),
        }
    }

    fn take(&mut self, length: usize, what: &str) -> Result<&'a [u8], Failure> {
        let taken = self
            .data
            .get(self.at..self.at.saturating_add(length))
            .ok_or_else(|| {
                Failure::Malformed(format!(
                    "The DXF drawing ends inside {what} at byte {}",
                    self.at
                ))
            })?;
        self.at += length;
        self.paging.read(length);
        Ok(taken)
    }

    fn array<const N: usize>(&mut self, what: &str) -> Result<[u8; N], Failure> {
        Ok(self
            .take(N, what)?
            .try_into()
            .expect("take gives the length asked for"))
    }

    /// Where the string at the reading position ends, at the first `end` after it, which is
    /// read past: the bytes searched are counted read as they are.
    fn until(&mut self, end: u8) -> Option<&'a [u8]> {
        let rest = &self.data[self.at..];
        let length = self.paging.find(rest, &[end])?;
        self.at += length + 1;
        Some(&rest[..length])
    }

    /// The next line, without its line end, of an ASCII drawing that has one.
    fn line(&mut self) -> Option<&'a [u8]> {
        if self.at >= self.data.len() {
            return None;
        }
        let line = self.until(b'\n').unwrap_or_else(|| {
            let rest = &self.data[self.at..];
            self.at = self.data.len();
            rest
        });
        Some(line.strip_suffix(b"\r").unwrap_or(line))
    }

    fn ascii(&mut self) -> Result<Group<'a>, Failure> {
        let start = self.at;
        let line = self
            .line()
            .expect("next_group reads on only before the end");
        let code = std::str::from_utf8(line)
            .ok()
            .and_then(|code| code.trim().parse::<u16>().ok())
            .ok_or_else(|| {
                Failure::Malformed(format!(
                    "The DXF line {} at byte {start} is not a group code",
                    Value::Text(line).shown()
                ))
            })?;
        let value = self.line().ok_or_else(|| {
            Failure::Malformed(format!(
                "The DXF drawing ends after the group code {code} at byte {start}"
            ))
        })?;
        Ok(Group {
            code,
            value: Value::Text(value),
        })
    }

    fn binary(&mut self, wide: bool) -> Result<Group<'a>, Failure> {
        let start = self.at;
        const CODE: &str = "a group code";
        let code = if wide {
            u16::from_le_bytes(self.array(CODE)?)
        } else {
            match self.array::<1>(CODE)?[0] {
                WIDE_CODE => u16::from_le_bytes(self.array(CODE)?),
                code => u16::from(code),
            }
        };
        let kind = Kind::of(code).ok_or_else(|| {
            Failure::Malformed(format!(
                "The DXF group code {code} at byte {start} is not one the format defines"
            ))
        })?;
        const VALUE: &str = "a group value";
        let value = match kind {
            Kind::Text => Value::Text(self.until(0).ok_or_else(|| {
                Failure::Malformed(format!(
                    "The DXF string of group {code} at byte {start} has no end"
                ))
            })?),
            Kind::Real => Value::Real(f64::from_le_bytes(self.array(VALUE)?)),
            Kind::Int16 => Value::Integer(i16::from_le_bytes(self.array(VALUE)?).into()),
            Kind::Int32 => Value::Integer(i32::from_le_bytes(self.array(VALUE)?).into()),
            Kind::Int64 => Value::Integer(i64::from_le_bytes(self.array(VALUE)?)),
            Kind::Bool => Value::Integer(self.array::<1>(VALUE)?[0].into()),
            Kind::Bytes => {
                let length = self.array::<1>(VALUE)?[0];
                self.take(usize::from(length), VALUE)?;
                Value::Bytes
            }
        };
        Ok(Group { code, value })
    }
}
