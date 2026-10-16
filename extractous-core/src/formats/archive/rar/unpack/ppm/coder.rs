//! junrar's `RangeCoder`: the arithmetic decoder of RAR's PPMd blocks, its 32-bit registers
//! kept in `long`s, which `ariDecNormalize` adds without wrapping.

use super::super::io::Input;
use super::super::Thrown;

const TOP: i64 = 1 << 24;
const BOT: i64 = 1 << 15;
const MASK: i64 = 0xFFFF_FFFF;
const DIVIDED_BY_ZERO: Thrown = Thrown("an ArithmeticException, a division by zero");

#[derive(Default)]
pub(super) struct RangeCoder {
    low: i64,
    code: i64,
    range: i64,
    pub(super) low_count: i64,
    pub(super) high_count: i64,
    pub(super) scale: i64,
}

impl RangeCoder {
    /// `initDecoder`.
    pub(super) fn init(&mut self, input: &mut Input) -> Result<(), Thrown> {
        self.low = 0;
        self.code = 0;
        self.range = MASK;
        for _ in 0..4 {
            self.code = ((self.code << 8) | i64::from(input.get_char()?)) & MASK;
        }
        Ok(())
    }

    /// `getCurrentCount`.
    pub(super) fn current_count(&mut self) -> Result<i32, Thrown> {
        if self.scale == 0 {
            return Err(DIVIDED_BY_ZERO);
        }
        self.range = (self.range / self.scale) & MASK;
        if self.range == 0 {
            return Err(DIVIDED_BY_ZERO);
        }
        Ok(((self.code - self.low) / self.range) as i32)
    }

    /// `getCurrentShiftCount`.
    pub(super) fn current_shift_count(&mut self, shift: u32) -> Result<i64, Thrown> {
        self.range = (self.range as u64 >> shift) as i64;
        if self.range == 0 {
            return Err(DIVIDED_BY_ZERO);
        }
        Ok(((self.code - self.low) / self.range) & MASK)
    }

    pub(super) fn set_low_count(&mut self, count: i64) {
        self.low_count = count & MASK;
    }

    pub(super) fn set_high_count(&mut self, count: i64) {
        self.high_count = count & MASK;
    }

    pub(super) fn set_scale(&mut self, scale: i64) {
        self.scale = scale & MASK;
    }

    /// `decode`.
    pub(super) fn decode(&mut self) {
        self.low = self
            .low
            .wrapping_add(self.range.wrapping_mul(self.low_count))
            & MASK;
        self.range = self
            .range
            .wrapping_mul(self.high_count.wrapping_sub(self.low_count))
            & MASK;
    }

    /// `ariDecNormalize`.
    pub(super) fn normalize(&mut self, input: &mut Input) -> Result<(), Thrown> {
        loop {
            let settled = (self.low ^ (self.low + self.range)) >= TOP;
            let short = settled && self.range < BOT;
            if settled && !short {
                return Ok(());
            }
            if short {
                self.range = (-self.low & (BOT - 1)) & MASK;
            }
            self.code = ((self.code << 8) | i64::from(input.get_char()?)) & MASK;
            self.range = (self.range << 8) & MASK;
            self.low = (self.low << 8) & MASK;
        }
    }
}
