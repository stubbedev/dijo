//! A cursor over the little-endian fields of a structure of a help file, and the compressed
//! numbers of topic records.

use super::Malformed;

/// The fields of `bytes`, read from the start on, which hold `what`.
pub(super) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub(super) fn new(bytes: &'a [u8], what: &'static str) -> Self {
        Self { bytes, at: 0, what }
    }

    /// The bytes not read yet.
    pub(super) fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }

    pub(super) fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    /// The next `length` bytes.
    pub(super) fn take(&mut self, length: usize) -> Result<&'a [u8], Malformed> {
        let taken = self
            .at
            .checked_add(length)
            .and_then(|end| self.bytes.get(self.at..end))
            .ok_or_else(|| {
                Malformed(format!(
                    "{} of {} bytes ends before the {length} bytes at {}",
                    self.what,
                    self.bytes.len(),
                    self.at
                ))
            })?;
        self.at += length;
        Ok(taken)
    }

    pub(super) fn skip(&mut self, length: usize) -> Result<(), Malformed> {
        self.take(length).map(|_| ())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Malformed> {
        Ok(self.take(N)?.try_into().expect("N bytes were taken"))
    }

    pub(super) fn u8(&mut self) -> Result<u8, Malformed> {
        Ok(self.array::<1>()?[0])
    }

    /// The next byte without reading it.
    pub(super) fn peek(&self) -> Result<u8, Malformed> {
        self.bytes.get(self.at).copied().ok_or_else(|| {
            Malformed(format!(
                "{} of {} bytes ends before its next field",
                self.what,
                self.bytes.len()
            ))
        })
    }

    pub(super) fn u16(&mut self) -> Result<u16, Malformed> {
        self.array().map(u16::from_le_bytes)
    }

    pub(super) fn i16(&mut self) -> Result<i16, Malformed> {
        self.array().map(i16::from_le_bytes)
    }

    pub(super) fn u32(&mut self) -> Result<u32, Malformed> {
        self.array().map(u32::from_le_bytes)
    }

    pub(super) fn i32(&mut self) -> Result<i32, Malformed> {
        self.array().map(i32::from_le_bytes)
    }

    /// The next NUL-terminated string, without its NUL, or the rest where no NUL ends it.
    pub(super) fn string(&mut self) -> &'a [u8] {
        let rest = self.rest();
        let length = memchr::memchr(0, rest).unwrap_or(rest.len());
        self.at += (length + 1).min(rest.len());
        &rest[..length]
    }

    /// A compressed unsigned short: a byte of twice the value, or, where it is odd, a 16-bit
    /// word of twice the value plus one.
    pub(super) fn compressed_u16(&mut self) -> Result<u16, Malformed> {
        if self.peek()? & 1 == 1 {
            Ok(self.u16()? >> 1)
        } else {
            Ok(u16::from(self.u8()? >> 1))
        }
    }

    /// A compressed signed short: a compressed unsigned short biased by 0x40 in a byte and by
    /// 0x4000 in a word.
    pub(super) fn compressed_i16(&mut self) -> Result<i16, Malformed> {
        if self.peek()? & 1 == 1 {
            Ok((self.u16()? >> 1) as i16 - 0x4000)
        } else {
            Ok(i16::from(self.u8()? >> 1) - 0x40)
        }
    }

    /// A compressed signed long: a 16-bit word of twice the value biased by 0x4000, or, where
    /// it is odd, a 32-bit one of twice the value biased by 0x40000000, plus one.
    pub(super) fn compressed_i32(&mut self) -> Result<i32, Malformed> {
        if self.peek()? & 1 == 1 {
            Ok((self.u32()? >> 1) as i32 - 0x4000_0000)
        } else {
            Ok(i32::from(self.u16()? >> 1) - 0x4000)
        }
    }
}
