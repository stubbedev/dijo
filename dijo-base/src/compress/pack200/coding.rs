//! The band codings of JSR 200 section 6: BHSD codings, the run and population codings built
//! of them, and the band coding specifiers that select them, read as commons-compress's
//! `BandSet.decodeBandInt` and `CodecEncoding.getCodec` read them.
//!
//! Three of commons-compress's departures from the specification are not followed: a delta
//! coding's sums are brought back into the range of its coding (section 6.4) rather than left
//! as 32-bit sums, a population coding's central value is the negative one of two equally
//! close to zero (section 6.5.1) where `PopulationCodec` takes the positive one, and the token
//! coding of a population coding with 256 favoured values or more is the shortest that holds
//! every token (section 6.7.3), where `PopulationCodec` takes BYTE1 for 256, whose largest
//! token it cannot hold.

use super::{malformed, Result};

/// A (B,H,S,D) coding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Bhsd {
    b: u8,
    h: u16,
    s: u8,
    delta: bool,
}

pub(super) const BYTE1: Bhsd = Bhsd::new(1, 256, 0, false);
pub(super) const CHAR3: Bhsd = Bhsd::new(3, 128, 0, false);
pub(super) const BCI5: Bhsd = Bhsd::new(5, 4, 0, false);
pub(super) const BRANCH5: Bhsd = Bhsd::new(5, 4, 2, false);
pub(super) const UNSIGNED5: Bhsd = Bhsd::new(5, 64, 0, false);
pub(super) const SIGNED5: Bhsd = Bhsd::new(5, 64, 1, false);
pub(super) const UDELTA5: Bhsd = Bhsd::new(5, 64, 0, true);
pub(super) const DELTA5: Bhsd = Bhsd::new(5, 64, 1, true);
pub(super) const MDELTA5: Bhsd = Bhsd::new(5, 64, 2, true);

/// The canonical codings a band coding specifier byte in `1..=115` names, `CANONICAL[byte - 1]`.
const CANONICAL: [(u8, u16, u8, bool); 115] = [
    (1, 256, 0, false),
    (1, 256, 1, false),
    (1, 256, 0, true),
    (1, 256, 1, true),
    (2, 256, 0, false),
    (2, 256, 1, false),
    (2, 256, 0, true),
    (2, 256, 1, true),
    (3, 256, 0, false),
    (3, 256, 1, false),
    (3, 256, 0, true),
    (3, 256, 1, true),
    (4, 256, 0, false),
    (4, 256, 1, false),
    (4, 256, 0, true),
    (4, 256, 1, true),
    (5, 4, 0, false),
    (5, 4, 1, false),
    (5, 4, 2, false),
    (5, 16, 0, false),
    (5, 16, 1, false),
    (5, 16, 2, false),
    (5, 32, 0, false),
    (5, 32, 1, false),
    (5, 32, 2, false),
    (5, 64, 0, false),
    (5, 64, 1, false),
    (5, 64, 2, false),
    (5, 128, 0, false),
    (5, 128, 1, false),
    (5, 128, 2, false),
    (5, 4, 0, true),
    (5, 4, 1, true),
    (5, 4, 2, true),
    (5, 16, 0, true),
    (5, 16, 1, true),
    (5, 16, 2, true),
    (5, 32, 0, true),
    (5, 32, 1, true),
    (5, 32, 2, true),
    (5, 64, 0, true),
    (5, 64, 1, true),
    (5, 64, 2, true),
    (5, 128, 0, true),
    (5, 128, 1, true),
    (5, 128, 2, true),
    (2, 192, 0, false),
    (2, 224, 0, false),
    (2, 240, 0, false),
    (2, 248, 0, false),
    (2, 252, 0, false),
    (2, 8, 0, true),
    (2, 8, 1, true),
    (2, 16, 0, true),
    (2, 16, 1, true),
    (2, 32, 0, true),
    (2, 32, 1, true),
    (2, 64, 0, true),
    (2, 64, 1, true),
    (2, 128, 0, true),
    (2, 128, 1, true),
    (2, 192, 0, true),
    (2, 192, 1, true),
    (2, 224, 0, true),
    (2, 224, 1, true),
    (2, 240, 0, true),
    (2, 240, 1, true),
    (2, 248, 0, true),
    (2, 248, 1, true),
    (3, 192, 0, false),
    (3, 224, 0, false),
    (3, 240, 0, false),
    (3, 248, 0, false),
    (3, 252, 0, false),
    (3, 8, 0, true),
    (3, 8, 1, true),
    (3, 16, 0, true),
    (3, 16, 1, true),
    (3, 32, 0, true),
    (3, 32, 1, true),
    (3, 64, 0, true),
    (3, 64, 1, true),
    (3, 128, 0, true),
    (3, 128, 1, true),
    (3, 192, 0, true),
    (3, 192, 1, true),
    (3, 224, 0, true),
    (3, 224, 1, true),
    (3, 240, 0, true),
    (3, 240, 1, true),
    (3, 248, 0, true),
    (3, 248, 1, true),
    (4, 192, 0, false),
    (4, 224, 0, false),
    (4, 240, 0, false),
    (4, 248, 0, false),
    (4, 252, 0, false),
    (4, 8, 0, true),
    (4, 8, 1, true),
    (4, 16, 0, true),
    (4, 16, 1, true),
    (4, 32, 0, true),
    (4, 32, 1, true),
    (4, 64, 0, true),
    (4, 64, 1, true),
    (4, 128, 0, true),
    (4, 128, 1, true),
    (4, 192, 0, true),
    (4, 192, 1, true),
    (4, 224, 0, true),
    (4, 224, 1, true),
    (4, 240, 0, true),
    (4, 240, 1, true),
    (4, 248, 0, true),
    (4, 248, 1, true),
];

/// The L values of a population coding whose token coding the specifier byte derives,
/// `TDEF_L[TDefL - 1]`.
const TDEF_L: [u16; 11] = [4, 8, 16, 32, 64, 128, 192, 224, 240, 248, 252];

impl Bhsd {
    const fn new(b: u8, h: u16, s: u8, delta: bool) -> Self {
        Self { b, h, s, delta }
    }

    /// The coding (B,H,S,D) a specifier names, if it is a legal one.
    fn checked(b: u8, h: u16, s: u8, delta: bool) -> Result<Self> {
        let legal = (1..=5).contains(&b)
            && (1..=256).contains(&h)
            && s <= 2
            && (b != 1 || h == 256)
            && (h != 256 || b != 5);
        if legal {
            Ok(Self::new(b, h, s, delta))
        } else {
            Err(malformed(format!(
                "a band coding specifier names the illegal coding ({b},{h},{s},{})",
                u8::from(delta)
            )))
        }
    }

    /// L, the number of low byte values.
    fn low(self) -> u64 {
        256 - u64::from(self.h)
    }

    /// Card(B,H), how many whole numbers the coding represents.
    fn cardinality(self) -> u64 {
        let h = u64::from(self.h);
        if h == 1 {
            return u64::from(self.b) * 255 + 1;
        }
        let power = h.pow(u32::from(self.b));
        self.low() * (power - 1) / (h - 1) + power
    }

    /// The next value, sign converted but neither summed nor truncated.
    fn whole(self, input: &mut Cursor) -> Result<i64> {
        let mut value = 0u64;
        let mut power = 1u64;
        for _ in 0..self.b {
            let byte = u64::from(input.byte()?);
            value += byte * power;
            if byte < self.low() {
                break;
            }
            power *= u64::from(self.h);
        }
        Ok(if self.s == 0 {
            value as i64
        } else {
            let sign = (1u64 << self.s) - 1;
            if value & sign == sign {
                -((value >> self.s) as i64) - 1
            } else {
                (value - (value >> self.s)) as i64
            }
        })
    }

    /// A header value: the next value, sign converted.
    pub(super) fn scalar(self, input: &mut Cursor) -> Result<i64> {
        self.whole(input)
    }

    /// The next value after `last`, a delta coding's running sum brought back into the range of
    /// (B,H,0): truncated to 32 bits where that range is full, else reduced modulo its
    /// cardinality, which is 2^31 where (B,H) holds more values than that but fewer than 2^32.
    fn next(self, input: &mut Cursor, last: i64) -> Result<i64> {
        let value = self.whole(input)?;
        if !self.delta {
            return Ok(i64::from(value as i32));
        }
        let sum = last + value;
        let cardinality = self.cardinality();
        Ok(if cardinality >= 1 << 32 {
            i64::from(sum as i32)
        } else {
            sum.rem_euclid(cardinality.min(1 << 31) as i64)
        })
    }

    fn band(self, n: usize, input: &mut Cursor) -> Result<Vec<i32>> {
        let mut values = Vec::with_capacity(n);
        let mut last = 0;
        for _ in 0..n {
            last = self.next(input, last)?;
            values.push(last as i32);
        }
        Ok(values)
    }
}

/// A coding a band coding specifier selects.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Coding {
    Bhsd(Bhsd),
    /// The first `k` values in `a`, the rest in `b`.
    Run {
        k: usize,
        a: Box<Coding>,
        b: Box<Coding>,
    },
    /// Favoured values in `favoured`, then tokens in `tokens` (or, if none, a coding derived
    /// from the number of favoured values and `low`), then the unfavoured values in
    /// `unfavoured`.
    Population {
        favoured: Box<Coding>,
        tokens: Option<Box<Coding>>,
        low: u16,
        unfavoured: Box<Coding>,
    },
}

impl Coding {
    /// The `n` values of a band, none read for an empty band, which takes no bytes.
    fn band(&self, n: usize, input: &mut Cursor) -> Result<Vec<i32>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        match self {
            Coding::Bhsd(coding) => coding.band(n, input),
            Coding::Run { k, a, b } => {
                let k = (*k).min(n);
                let mut values = a.band(k, input)?;
                values.extend(b.band(n - k, input)?);
                Ok(values)
            }
            Coding::Population {
                favoured,
                tokens,
                low,
                unfavoured,
            } => {
                let favoured = favoured_values(favoured, n, input)?;
                let k = favoured.len();
                let tokens = match tokens {
                    Some(tokens) => tokens.band(n, input)?,
                    None => token_coding(k, *low)?.band(n, input)?,
                };
                let unfavoured_count = tokens.iter().filter(|&&token| token == 0).count();
                let mut unfavoured = unfavoured.band(unfavoured_count, input)?.into_iter();
                tokens
                    .into_iter()
                    .map(|token| match token {
                        0 => Ok(unfavoured.next().expect("one value per zero token")),
                        token => usize::try_from(token - 1)
                            .ok()
                            .and_then(|index| favoured.get(index).copied())
                            .ok_or_else(|| {
                                malformed("a population coding token names no favoured value")
                            }),
                    })
                    .collect()
            }
        }
    }
}

/// The favoured values of a population coding, read with `coding` up to the sentinel: a
/// repetition of the value before it, or of the value closest to zero so far, the negative one
/// of two equally close.
fn favoured_values(coding: &Coding, n: usize, input: &mut Cursor) -> Result<Vec<i32>> {
    let mut stages = Vec::new();
    let mut rest = coding;
    loop {
        match rest {
            Coding::Bhsd(coding) => {
                stages.push((usize::MAX, *coding));
                break;
            }
            Coding::Run { k, a, b } => {
                let Coding::Bhsd(a) = **a else {
                    return Err(malformed("a population coding nests another"));
                };
                stages.push((*k, a));
                rest = b;
            }
            Coding::Population { .. } => {
                return Err(malformed("a population coding nests another"));
            }
        }
    }
    let mut favoured: Vec<i32> = Vec::new();
    let mut central: Option<i32> = None;
    let (mut stage, mut used, mut last) = (0, 0, 0i64);
    loop {
        if used == stages[stage].0 {
            stage += 1;
            used = 0;
            last = 0;
        }
        last = stages[stage].1.next(input, last)?;
        used += 1;
        let value = last as i32;
        if favoured.last() == Some(&value) || central == Some(value) {
            return Ok(favoured);
        }
        if favoured.len() == n {
            return Err(malformed(
                "a population coding favours more values than its band holds",
            ));
        }
        favoured.push(value);
        let closer = central.is_none_or(|central| {
            let (distance, best) = (value.unsigned_abs(), central.unsigned_abs());
            distance < best || (distance == best && value < central)
        });
        if closer {
            central = Some(value);
        }
    }
}

/// The token coding of a population coding of `k` favoured values whose specifier gives only
/// `low`: BYTE1 for fewer than 256, else the shortest (B,256-L) coding that represents `k`.
fn token_coding(k: usize, low: u16) -> Result<Coding> {
    if k < 256 {
        return Ok(Coding::Bhsd(BYTE1));
    }
    (2..=5)
        .map(|b| Bhsd::new(b, 256 - low, 0, false))
        .find(|coding| coding.cardinality() > k as u64)
        .map(Coding::Bhsd)
        .ok_or_else(|| malformed("no token coding represents a population coding's tokens"))
}

/// Bytes read in order.
#[derive(Debug, Clone)]
pub(super) struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub(super) fn byte(&mut self) -> Result<u8> {
        let byte = *self
            .data
            .get(self.position)
            .ok_or_else(|| malformed("the stream ends inside a band"))?;
        self.position += 1;
        Ok(byte)
    }

    /// The next `n` bytes.
    pub(super) fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let bytes = self
            .position
            .checked_add(n)
            .and_then(|end| self.data.get(self.position..end))
            .ok_or_else(|| malformed("the stream ends inside the file bits"))?;
        self.position += n;
        Ok(bytes)
    }

    pub(super) fn remaining(&self) -> usize {
        self.data.len() - self.position
    }

    pub(super) fn position(&self) -> usize {
        self.position
    }
}

/// The bands of a segment and the band headers that hold their coding specifiers.
pub(super) struct Bands<'a> {
    pub(super) input: Cursor<'a>,
    headers: Cursor<'a>,
}

impl<'a> Bands<'a> {
    pub(super) fn new(input: Cursor<'a>, headers: &'a [u8]) -> Self {
        Self {
            input,
            headers: Cursor::new(headers),
        }
    }

    /// The next band, of `n` values whose primary coding is `coding`, decoded with the coding
    /// its specifier, if it starts with one, selects.
    pub(super) fn band(&mut self, coding: Bhsd, n: usize) -> Result<Vec<i32>> {
        if n > self.input.remaining() {
            return Err(malformed("a band is longer than the rest of the stream"));
        }
        if n == 0 || coding.b == 1 {
            return coding.band(n, &mut self.input);
        }
        let start = self.input.clone();
        let first = coding.whole(&mut self.input)?;
        let low = coding.low() as i64;
        let specifier = if coding.s != 0 && (-256..=-1).contains(&first) {
            Some(-1 - first)
        } else if coding.s == 0 && (low..=low + 255).contains(&first) {
            Some(first - low)
        } else {
            None
        };
        match specifier {
            Some(specifier) => {
                let selected = self.specified(specifier as u8, coding)?;
                selected.band(n, &mut self.input)
            }
            None => {
                self.input = start;
                coding.band(n, &mut self.input)
            }
        }
    }

    /// A band of `n` counts in `coding`, which must not be negative.
    pub(super) fn counts(&mut self, coding: Bhsd, n: usize) -> Result<Vec<usize>> {
        self.band(coding, n)?
            .into_iter()
            .map(|count| {
                usize::try_from(count)
                    .map_err(|_| malformed(format!("a band holds the negative count {count}")))
            })
            .collect()
    }

    /// The coding the specifier starting with `byte` selects, the band's primary coding being
    /// `default`.
    fn specified(&mut self, byte: u8, default: Bhsd) -> Result<Coding> {
        Ok(match byte {
            0 => Coding::Bhsd(default),
            1..=115 => {
                let (b, h, s, delta) = CANONICAL[usize::from(byte) - 1];
                Coding::Bhsd(Bhsd::new(b, h, s, delta))
            }
            116 => {
                let code = self.headers.byte()?;
                let h = u16::from(self.headers.byte()?) + 1;
                Coding::Bhsd(Bhsd::checked(
                    (code >> 3 & 7) + 1,
                    h,
                    code >> 1 & 3,
                    code & 1 == 1,
                )?)
            }
            117..=140 => {
                let offset = byte - 117;
                let kx = u32::from(offset & 3);
                let kb = if offset >> 2 & 1 == 1 {
                    usize::from(self.headers.byte()?)
                } else {
                    3
                };
                let (a_default, b_default) = (offset >> 3 & 1 == 1, offset >> 4 & 1 == 1);
                if a_default && b_default {
                    return Err(malformed("a run coding specifier defaults both codings"));
                }
                let a = self.nested(a_default, default)?;
                let b = self.nested(b_default, default)?;
                Coding::Run {
                    k: (kb + 1) * 16usize.pow(kx),
                    a: Box::new(a),
                    b: Box::new(b),
                }
            }
            141..=188 => {
                let offset = byte - 141;
                let favoured_default = offset & 1 == 1;
                let unfavoured_default = offset >> 1 & 1 == 1;
                let tdef_l = usize::from(offset >> 2);
                let favoured = self.nested(favoured_default, default)?;
                let (tokens, low) = if tdef_l == 0 {
                    (Some(Box::new(self.nested(false, default)?)), 0)
                } else {
                    (None, TDEF_L[tdef_l - 1])
                };
                let unfavoured = self.nested(unfavoured_default, default)?;
                Coding::Population {
                    favoured: Box::new(favoured),
                    tokens,
                    low,
                    unfavoured: Box::new(unfavoured),
                }
            }
            _ => {
                return Err(malformed(format!(
                    "the band coding specifier byte {byte} names no coding"
                )))
            }
        })
    }

    /// The coding of a part of a run or population coding: `default` where its specifier says
    /// so, else the one the next band header byte specifies.
    fn nested(&mut self, is_default: bool, default: Bhsd) -> Result<Coding> {
        if is_default {
            Ok(Coding::Bhsd(default))
        } else {
            let byte = self.headers.byte()?;
            self.specified(byte, default)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(data: &[u8], headers: &[u8], coding: Bhsd, n: usize) -> Result<Vec<i32>> {
        Bands::new(Cursor::new(data), headers).band(coding, n)
    }

    #[test]
    fn decodes_bhsd_values_with_signs_and_deltas() {
        assert_eq!(band(&[5, 200, 1], &[], UNSIGNED5, 2), Ok(vec![5, 264]));
        assert_eq!(band(&[4, 3], &[], SIGNED5, 2), Ok(vec![2, -2]));
        assert_eq!(band(&[2, 3, 1], &[], DELTA5, 3), Ok(vec![1, -1, -2]));
        assert_eq!(band(&[0xFE, 0xFF], &[], BYTE1, 2), Ok(vec![254, 255]));
    }

    #[test]
    fn reads_a_band_coding_specifier_before_the_values() {
        assert_eq!(band(&[1, 14, 18], &[], SIGNED5, 2), Ok(vec![7, 9]));
        assert_eq!(band(&[3, 200, 1], &[], SIGNED5, 1), Ok(vec![200]));
        assert_eq!(
            band(&[192 + 5, 0, 0x34, 0x12], &[], UNSIGNED5, 1),
            Ok(vec![0x1234])
        );
        assert_eq!(band(&[195, 1, 14, 9], &[0, 1], SIGNED5, 2), Ok(vec![7, 9]));
    }

    #[test]
    fn decodes_a_population_coding() {
        let data = [212, 2, 20, 10, 10, 1, 2, 0, 1, 7];
        assert_eq!(band(&data, &[], UNSIGNED5, 4), Ok(vec![20, 10, 7, 20]));
    }

    #[test]
    fn fails_on_bands_past_the_end() {
        assert!(band(&[200], &[], UNSIGNED5, 1).is_err());
        assert!(band(&[1], &[], UNSIGNED5, 2).is_err());
    }
}
