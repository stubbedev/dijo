//! The bytes and bit fields of a movie's tags, read in turn, little-endian, failing where they
//! end early.

/// What was being read where the bytes ended early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Short(pub(super) &'static str);

/// Bytes read in turn, a bit field at a time between byte-aligned values, as SWF packs its
/// rectangles, matrices and glyph entries.
pub(super) struct Bytes<'d> {
    data: &'d [u8],
    at: usize,
    /// The bits of the byte at `at` already read by a bit field, none where the reading is
    /// byte-aligned.
    bit: u32,
}

impl<'d> Bytes<'d> {
    pub(super) fn new(data: &'d [u8]) -> Self {
        Self {
            data,
            at: 0,
            bit: 0,
        }
    }

    /// Where the next byte-aligned value starts.
    pub(super) fn at(&self) -> usize {
        self.at + usize::from(self.bit > 0)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.at() >= self.data.len()
    }

    pub(super) fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.at())
    }

    /// Ends a run of bit fields at the next byte.
    pub(super) fn align(&mut self) {
        self.at = self.at();
        self.bit = 0;
    }

    pub(super) fn take(&mut self, count: usize, what: &'static str) -> Result<&'d [u8], Short> {
        self.align();
        if count > self.remaining() {
            return Err(Short(what));
        }
        let taken = &self.data[self.at..self.at + count];
        self.at += count;
        Ok(taken)
    }

    pub(super) fn skip(&mut self, count: usize, what: &'static str) -> Result<(), Short> {
        self.take(count, what).map(|_| ())
    }

    /// The bytes left, all read.
    pub(super) fn rest(&mut self) -> &'d [u8] {
        self.align();
        let rest = &self.data[self.at.min(self.data.len())..];
        self.at = self.data.len();
        rest
    }

    pub(super) fn u8(&mut self, what: &'static str) -> Result<u8, Short> {
        Ok(self.take(1, what)?[0])
    }

    pub(super) fn u16(&mut self, what: &'static str) -> Result<u16, Short> {
        let bytes = self.take(2, what)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub(super) fn i16(&mut self, what: &'static str) -> Result<i16, Short> {
        let bytes = self.take(2, what)?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub(super) fn u32(&mut self, what: &'static str) -> Result<u32, Short> {
        let bytes = self.take(4, what)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A `STRING`: the bytes up to the NUL that ends it, which is read too.
    pub(super) fn string(&mut self, what: &'static str) -> Result<&'d [u8], Short> {
        self.align();
        let rest = &self.data[self.at.min(self.data.len())..];
        let end = memchr::memchr(0, rest).ok_or(Short(what))?;
        self.at += end + 1;
        Ok(&rest[..end])
    }

    /// An unsigned bit field of `bits` bits, at most 32, most significant bit first.
    pub(super) fn ub(&mut self, bits: u32, what: &'static str) -> Result<u32, Short> {
        assert!(bits <= 32, "a bit field of {bits} bits is wider than 32");
        let mut value = 0u64;
        for _ in 0..bits {
            let byte = *self.data.get(self.at).ok_or(Short(what))?;
            value = value << 1 | u64::from(byte >> (7 - self.bit) & 1);
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.at += 1;
            }
        }
        Ok(u32::try_from(value).expect("at most 32 bits"))
    }

    /// A signed bit field of `bits` bits, at most 32, in two's complement.
    pub(super) fn sb(&mut self, bits: u32, what: &'static str) -> Result<i32, Short> {
        let value = i64::from(self.ub(bits, what)?);
        let value = match bits > 0 && value >> (bits - 1) & 1 == 1 {
            true => value - (1i64 << bits),
            false => value,
        };
        Ok(i32::try_from(value).expect("a field of at most 32 bits"))
    }

    /// Reads past a `RECT`.
    pub(super) fn skip_rect(&mut self, what: &'static str) -> Result<(), Short> {
        self.align();
        let bits = self.ub(5, what)?;
        for _ in 0..4 {
            self.sb(bits, what)?;
        }
        self.align();
        Ok(())
    }

    /// Reads past a `MATRIX`.
    pub(super) fn skip_matrix(&mut self, what: &'static str) -> Result<(), Short> {
        self.align();
        for _ in 0..2 {
            if self.ub(1, what)? == 1 {
                let bits = self.ub(5, what)?;
                self.sb(bits, what)?;
                self.sb(bits, what)?;
            }
        }
        let bits = self.ub(5, what)?;
        self.sb(bits, what)?;
        self.sb(bits, what)?;
        self.align();
        Ok(())
    }
}
