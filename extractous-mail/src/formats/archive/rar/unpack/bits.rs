//! junrar's `BitInput`: a buffer of packed bytes and the bit an unpacker reads next.

use super::{Thrown, OUT_OF_BOUNDS};

/// `BitInput.MAX_SIZE`: the bytes the buffer holds.
pub(super) const MAX_SIZE: i32 = 0x8000;

pub(super) struct BitInput {
    pub(super) in_addr: i32,
    pub(super) in_bit: i32,
    pub(super) buf: Box<[u8]>,
}

impl BitInput {
    pub(super) fn new() -> Self {
        BitInput {
            in_addr: 0,
            in_bit: 0,
            buf: vec![0; MAX_SIZE as usize].into_boxed_slice(),
        }
    }

    /// `InitBitInput`.
    pub(super) fn init(&mut self) {
        self.in_addr = 0;
        self.in_bit = 0;
    }

    pub(super) fn addbits(&mut self, bits: i32) {
        let bits = bits.wrapping_add(self.in_bit);
        self.in_addr = self.in_addr.wrapping_add((bits as u32 >> 3) as i32);
        self.in_bit = bits & 7;
    }

    /// The 16 bits from the current one on, failing as Java does past the buffer.
    pub(super) fn getbits(&self) -> Result<i32, Thrown> {
        let field = (self.byte(self.in_addr)? << 16)
            + (self.byte(self.in_addr.wrapping_add(1))? << 8)
            + self.byte(self.in_addr.wrapping_add(2))?;
        Ok((field as u32 >> (8 - self.in_bit)) as i32 & 0xffff)
    }

    /// The byte at `at`, failing as Java does outside the buffer.
    pub(super) fn byte(&self, at: i32) -> Result<i32, Thrown> {
        usize::try_from(at)
            .ok()
            .and_then(|at| self.buf.get(at))
            .map(|&byte| i32::from(byte))
            .ok_or(OUT_OF_BOUNDS)
    }

    /// `Overflow`: whether reading `increment` more bytes passes the buffer.
    pub(super) fn overflows(&self, increment: i32) -> bool {
        self.in_addr.wrapping_add(increment) >= MAX_SIZE
    }
}
