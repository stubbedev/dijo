//! A small unsigned big integer, for the exact arithmetic the JDK's `FloatingDecimal`, `BigDecimal`
//! and POI's `NumberToTextConverter` do on doubles: products, powers, shifts, comparison and
//! division by a smaller number.

use std::cmp::Ordering;

/// An unsigned integer of any size, as little-endian 32-bit limbs without high zeros.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BigUint {
    limbs: Vec<u32>,
}

impl BigUint {
    pub(crate) fn zero() -> Self {
        Self { limbs: Vec::new() }
    }

    pub(crate) fn from_u64(value: u64) -> Self {
        let mut number = Self {
            limbs: vec![value as u32, (value >> 32) as u32],
        };
        number.trim();
        number
    }

    fn trim(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// `bitLength`: the bits up to and with the highest set bit.
    pub(crate) fn bit_length(&self) -> u32 {
        match self.limbs.last() {
            None => 0,
            Some(&top) => (self.limbs.len() as u32 - 1) * 32 + (32 - top.leading_zeros()),
        }
    }

    /// The low 64 bits, as `longValue` takes them.
    pub(crate) fn low_u64(&self) -> u64 {
        let low = u64::from(self.limbs.first().copied().unwrap_or(0));
        let high = u64::from(self.limbs.get(1).copied().unwrap_or(0));
        low | (high << 32)
    }

    pub(crate) fn shl(&self, bits: u32) -> Self {
        if self.is_zero() {
            return Self::zero();
        }
        let words = (bits / 32) as usize;
        let bits = bits % 32;
        let mut limbs = vec![0; words];
        let mut carry = 0u32;
        for &limb in &self.limbs {
            if bits == 0 {
                limbs.push(limb);
            } else {
                limbs.push((limb << bits) | carry);
                carry = limb >> (32 - bits);
            }
        }
        if carry != 0 {
            limbs.push(carry);
        }
        let mut number = Self { limbs };
        number.trim();
        number
    }

    pub(crate) fn shr(&self, bits: u32) -> Self {
        let words = (bits / 32) as usize;
        if words >= self.limbs.len() {
            return Self::zero();
        }
        let bits = bits % 32;
        let source = &self.limbs[words..];
        let mut limbs = Vec::with_capacity(source.len());
        for (i, &limb) in source.iter().enumerate() {
            let next = source.get(i + 1).copied().unwrap_or(0);
            limbs.push(if bits == 0 {
                limb
            } else {
                (limb >> bits) | (next << (32 - bits))
            });
        }
        let mut number = Self { limbs };
        number.trim();
        number
    }

    pub(crate) fn add(&self, other: &Self) -> Self {
        let length = self.limbs.len().max(other.limbs.len());
        let mut limbs = Vec::with_capacity(length + 1);
        let mut carry = 0u64;
        for i in 0..length {
            let sum = u64::from(self.limbs.get(i).copied().unwrap_or(0))
                + u64::from(other.limbs.get(i).copied().unwrap_or(0))
                + carry;
            limbs.push(sum as u32);
            carry = sum >> 32;
        }
        if carry != 0 {
            limbs.push(carry as u32);
        }
        let mut number = Self { limbs };
        number.trim();
        number
    }

    /// `self - other`, for `other <= self`.
    pub(crate) fn sub(&self, other: &Self) -> Self {
        let mut limbs = Vec::with_capacity(self.limbs.len());
        let mut borrow = 0i64;
        for i in 0..self.limbs.len() {
            let mut difference = i64::from(self.limbs[i])
                - i64::from(other.limbs.get(i).copied().unwrap_or(0))
                - borrow;
            borrow = 0;
            if difference < 0 {
                difference += 1 << 32;
                borrow = 1;
            }
            limbs.push(difference as u32);
        }
        let mut number = Self { limbs };
        number.trim();
        number
    }

    pub(crate) fn mul(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }
        let mut limbs = vec![0u32; self.limbs.len() + other.limbs.len()];
        for (i, &a) in self.limbs.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in other.limbs.iter().enumerate() {
                let product = u64::from(a) * u64::from(b) + u64::from(limbs[i + j]) + carry;
                limbs[i + j] = product as u32;
                carry = product >> 32;
            }
            let mut k = i + other.limbs.len();
            while carry != 0 {
                let sum = u64::from(limbs[k]) + carry;
                limbs[k] = sum as u32;
                carry = sum >> 32;
                k += 1;
            }
        }
        let mut number = Self { limbs };
        number.trim();
        number
    }

    pub(crate) fn mul_small(&self, factor: u32) -> Self {
        self.mul(&Self::from_u64(u64::from(factor)))
    }

    pub(crate) fn pow(base: u32, exponent: u32) -> Self {
        let mut result = Self::from_u64(1);
        let mut square = Self::from_u64(u64::from(base));
        let mut exponent = exponent;
        while exponent > 0 {
            if exponent & 1 == 1 {
                result = result.mul(&square);
            }
            exponent >>= 1;
            if exponent > 0 {
                square = square.mul(&square);
            }
        }
        result
    }

    /// `self / divisor` and `self % divisor`, by binary long division.
    pub(crate) fn div_rem(&self, divisor: &Self) -> (Self, Self) {
        assert!(!divisor.is_zero(), "division by zero");
        if self.cmp(divisor) == Ordering::Less {
            return (Self::zero(), self.clone());
        }
        let shift = self.bit_length() - divisor.bit_length();
        let mut remainder = self.clone();
        let mut quotient = Self::zero();
        let mut shifted = divisor.shl(shift);
        for bit in (0..=shift).rev() {
            if remainder.cmp(&shifted) != Ordering::Less {
                remainder = remainder.sub(&shifted);
                quotient = quotient.add(&Self::from_u64(1).shl(bit));
            }
            shifted = shifted.shr(1);
        }
        (quotient, remainder)
    }

    /// `self / divisor` and `self % divisor` for a divisor of one limb, limb by limb.
    pub(crate) fn div_rem_small(&self, divisor: u32) -> (Self, u32) {
        let mut limbs = vec![0u32; self.limbs.len()];
        let mut remainder = 0u64;
        for i in (0..self.limbs.len()).rev() {
            let current = (remainder << 32) | u64::from(self.limbs[i]);
            limbs[i] = (current / u64::from(divisor)) as u32;
            remainder = current % u64::from(divisor);
        }
        let mut quotient = Self { limbs };
        quotient.trim();
        (quotient, remainder as u32)
    }

    /// The decimal digits, most significant first; `"0"` for zero.
    pub(crate) fn to_decimal(&self) -> String {
        if self.is_zero() {
            return "0".to_owned();
        }
        let mut chunks = Vec::new();
        let mut rest = self.clone();
        while !rest.is_zero() {
            let (quotient, remainder) = rest.div_rem_small(1_000_000_000);
            chunks.push(u64::from(remainder));
            rest = quotient;
        }
        let mut text = chunks.pop().unwrap_or(0).to_string();
        for chunk in chunks.iter().rev() {
            text.push_str(&format!("{chunk:09}"));
        }
        text
    }

    /// Parses decimal digits.
    pub(crate) fn from_decimal(digits: &str) -> Self {
        let mut number = Self::zero();
        for chunk in digits.as_bytes().chunks(9) {
            let text = std::str::from_utf8(chunk).unwrap_or("0");
            let value: u64 = text.parse().unwrap_or(0);
            number = number
                .mul(&Self::pow(10, chunk.len() as u32))
                .add(&Self::from_u64(value));
        }
        number
    }
}

impl PartialOrd for BigUint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BigUint {
    fn cmp(&self, other: &Self) -> Ordering {
        self.limbs
            .len()
            .cmp(&other.limbs.len())
            .then_with(|| self.limbs.iter().rev().cmp(other.limbs.iter().rev()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_arithmetic() {
        let a = BigUint::from_decimal("123456789012345678901234567890");
        let b = BigUint::from_u64(987_654_321);
        assert_eq!(
            a.mul(&b).to_decimal(),
            "121932631124828532112482853211126352690"
        );
        let (q, r) = a.div_rem(&b);
        assert_eq!(q.to_decimal(), "124999998873437499901");
        assert_eq!(r.to_decimal(), "574845669");
        assert_eq!(BigUint::pow(5, 30).to_decimal(), "931322574615478515625");
        assert_eq!(a.shl(70).shr(70), a);
        assert_eq!(BigUint::from_u64(1).shl(64).bit_length(), 65);
    }
}
