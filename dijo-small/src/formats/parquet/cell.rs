//! The text of a value, by how its column's values are written ([`Kind`]): text as it is;
//! integers in decimal, unsigned ones as such; decimals as Java's `BigDecimal.toString` writes
//! them; floating-point numbers as Java's `Float.toString` and `Double.toString`; dates, times
//! and timestamps as `java.time` writes them (`2024-03-01`, `06:30:00.250`,
//! `2024-03-01T06:30:00Z`), the seconds always written, and a timestamp not adjusted to UTC
//! without its `Z`; UUIDs in their hyphenated form; intervals as ISO 8601 durations.

use std::fmt::Write;

use super::footer::Unit;
use super::schema::Kind;
use super::values::Raw;
use super::Problem;
use crate::java::decimal::BigDecimal;
use crate::java::dtoa::{java_double_to_string, java_float_to_string};
use crate::java::time::instant_string;

const SECONDS_PER_DAY: i64 = 86_400;
const NANOS_PER_SECOND: i64 = 1_000_000_000;
/// The Julian day of 1970-01-01.
const JULIAN_EPOCH: i64 = 2_440_588;
/// The power of ten a decimal's digits are divided out by at a time.
const DECIMAL_CHUNK: u64 = 1_000_000_000;
/// The most bytes a decimal is read from: those of a number of 617 digits.
const MAX_DECIMAL_BYTES: usize = 256;

/// Appends the text of `raw`, a value of a column of `kind`, to `out`, which a value of binary
/// data or of bytes that are not UTF-8 leaves as it is.
pub fn write(kind: Kind, raw: Raw<'_>, out: &mut String) -> Result<(), Problem> {
    match (kind, raw) {
        (Kind::Text, Raw::Bytes(bytes)) => out.push_str(&String::from_utf8_lossy(bytes)),
        (Kind::Bytes, Raw::Bytes(bytes)) => {
            if let Ok(text) = std::str::from_utf8(bytes) {
                out.push_str(text);
            }
        }
        (Kind::Binary, Raw::Bytes(_)) => {}
        (Kind::Boolean, Raw::Boolean(value)) => out.push_str(if value { "true" } else { "false" }),
        (Kind::Signed, Raw::Int32(value)) => push(out, value),
        (Kind::Signed, Raw::Int64(value)) => push(out, value),
        (Kind::Unsigned, Raw::Int32(value)) => push(out, value as u32),
        (Kind::Unsigned, Raw::Int64(value)) => push(out, value as u64),
        (Kind::Decimal { scale }, Raw::Int32(value)) => decimal_of(i128::from(value), scale, out),
        (Kind::Decimal { scale }, Raw::Int64(value)) => decimal_of(i128::from(value), scale, out),
        (Kind::Decimal { scale }, Raw::Bytes(bytes)) => decimal(bytes, scale, out)?,
        (Kind::Date, Raw::Int32(days)) => {
            let instant = instant_string(i64::from(days) * SECONDS_PER_DAY, 0);
            out.push_str(&instant[..instant.find('T').expect("an instant has a time")]);
        }
        (Kind::Time(unit), Raw::Int32(value)) => time(i64::from(value) * unit.nanos(), out),
        (Kind::Time(unit), Raw::Int64(value)) => time(value.saturating_mul(unit.nanos()), out),
        (Kind::Timestamp { utc, unit }, Raw::Int64(value)) => timestamp(value, unit, utc, out),
        (Kind::Int96Timestamp, Raw::Int96(bytes)) => {
            let nanos = i64::from_le_bytes(bytes[..8].try_into().expect("eight bytes"));
            let day = i64::from(i32::from_le_bytes(
                bytes[8..].try_into().expect("four bytes"),
            ));
            let seconds =
                (day - JULIAN_EPOCH) * SECONDS_PER_DAY + nanos.div_euclid(NANOS_PER_SECOND);
            out.push_str(&instant_string(
                seconds,
                nanos.rem_euclid(NANOS_PER_SECOND) as u32,
            ));
        }
        (Kind::Float, Raw::Float(value)) => out.push_str(&java_float_to_string(value)),
        (Kind::Double, Raw::Double(value)) => out.push_str(&java_double_to_string(value)),
        (Kind::Float16, Raw::Bytes(bytes)) => {
            let half = u16::from_le_bytes(bytes.try_into().expect("two bytes"));
            out.push_str(&java_float_to_string(float16(half)));
        }
        (Kind::Uuid, Raw::Bytes(bytes)) => {
            for (index, byte) in bytes.iter().enumerate() {
                if matches!(index, 4 | 6 | 8 | 10) {
                    out.push('-');
                }
                write!(out, "{byte:02x}").expect("a string takes what is written to it");
            }
        }
        (Kind::Interval, Raw::Bytes(bytes)) => {
            let field =
                |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
            let millis = field(8);
            write!(out, "P{}M{}DT{}", field(0), field(4), millis / 1000)
                .expect("a string takes what is written to it");
            if millis % 1000 != 0 {
                write!(out, ".{:03}", millis % 1000).expect("a string takes what is written to it");
            }
            out.push('S');
        }
        (kind, raw) => {
            return Err(Problem::Malformed(format!(
                "the value {raw:?} in a column of {kind:?}"
            )))
        }
    }
    Ok(())
}

fn push(out: &mut String, value: impl std::fmt::Display) {
    write!(out, "{value}").expect("a string takes what is written to it");
}

/// A time of day `nanos` nanoseconds after midnight, as `LocalTime.toString` writes it with its
/// seconds always written.
fn time(nanos: i64, out: &mut String) {
    let instant = instant_string(
        nanos.div_euclid(NANOS_PER_SECOND),
        nanos.rem_euclid(NANOS_PER_SECOND) as u32,
    );
    let start = instant.find('T').expect("an instant has a time") + 1;
    out.push_str(&instant[start..instant.len() - 1]);
}

/// The timestamp `value` units after the epoch, with its `Z` if it is `utc`.
fn timestamp(value: i64, unit: Unit, utc: bool, out: &mut String) {
    let per_second = NANOS_PER_SECOND / unit.nanos();
    let nanos = value.rem_euclid(per_second) * unit.nanos();
    let instant = instant_string(value.div_euclid(per_second), nanos as u32);
    out.push_str(if utc {
        &instant
    } else {
        &instant[..instant.len() - 1]
    });
}

/// The decimal of the unscaled `value` over 10 to the `scale`.
fn decimal_of(value: i128, scale: i32, out: &mut String) {
    let digits = value.unsigned_abs().to_string();
    out.push_str(&BigDecimal::from_digits(value < 0, &digits, i64::from(scale)).to_java_string());
}

/// The decimal whose unscaled value is the big-endian two's complement `bytes`, over 10 to the
/// `scale`.
fn decimal(bytes: &[u8], scale: i32, out: &mut String) -> Result<(), Problem> {
    if bytes.len() > MAX_DECIMAL_BYTES {
        return Err(Problem::Malformed(format!(
            "a decimal of {} bytes, more than the {MAX_DECIMAL_BYTES} read",
            bytes.len()
        )));
    }
    if bytes.len() <= 16 {
        let negative = bytes.first().is_some_and(|&byte| byte & 0x80 != 0);
        let mut word = [if negative { 0xFF } else { 0 }; 16];
        word[16 - bytes.len()..].copy_from_slice(bytes);
        decimal_of(i128::from_be_bytes(word), scale, out);
        return Ok(());
    }
    let negative = bytes[0] & 0x80 != 0;
    let mut magnitude = bytes.to_vec();
    if negative {
        let mut carry = true;
        for byte in magnitude.iter_mut().rev() {
            *byte = !*byte;
            if carry {
                (*byte, carry) = byte.overflowing_add(1);
            }
        }
    }
    let digits = decimal_digits(&mut magnitude);
    out.push_str(&BigDecimal::from_digits(negative, &digits, i64::from(scale)).to_java_string());
    Ok(())
}

/// The decimal digits of the big-endian unsigned `magnitude`, which is divided down to zero.
fn decimal_digits(magnitude: &mut [u8]) -> String {
    let mut chunks = Vec::new();
    let mut start = 0;
    loop {
        while start < magnitude.len() && magnitude[start] == 0 {
            start += 1;
        }
        if start == magnitude.len() {
            break;
        }
        let mut remainder = 0u64;
        for byte in &mut magnitude[start..] {
            let value = remainder << 8 | u64::from(*byte);
            *byte = (value / DECIMAL_CHUNK) as u8;
            remainder = value % DECIMAL_CHUNK;
        }
        chunks.push(remainder);
    }
    let mut digits = chunks
        .pop()
        .map_or_else(|| "0".to_owned(), |high| high.to_string());
    for chunk in chunks.iter().rev() {
        write!(digits, "{chunk:09}").expect("a string takes what is written to it");
    }
    digits
}

/// The IEEE 754 half-precision `half` as a float.
fn float16(half: u16) -> f32 {
    let sign = if half & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from(half >> 10 & 0x1F);
    let fraction = f32::from(half & 0x03FF);
    sign * match exponent {
        0 => fraction * 2f32.powi(-24),
        31 if fraction == 0.0 => f32::INFINITY,
        31 => f32::NAN,
        _ => (1.0 + fraction / 1024.0) * 2f32.powi(exponent - 15),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(kind: Kind, raw: Raw<'_>) -> String {
        let mut out = String::new();
        write(kind, raw, &mut out).unwrap();
        out
    }

    #[test]
    fn writes_numbers_and_decimals() {
        assert_eq!(text(Kind::Signed, Raw::Int64(-3)), "-3");
        assert_eq!(text(Kind::Unsigned, Raw::Int32(-1)), "4294967295");
        assert_eq!(text(Kind::Double, Raw::Double(1e-05)), "1.0E-5");
        assert_eq!(text(Kind::Double, Raw::Double(48.0)), "48.0");
        assert_eq!(text(Kind::Float, Raw::Float(-0.25)), "-0.25");
        let scaled = Kind::Decimal { scale: 2 };
        assert_eq!(text(scaled, Raw::Int32(15000)), "150.00");
        assert_eq!(text(scaled, Raw::Bytes(&(-5i32).to_be_bytes())), "-0.05");
        assert_eq!(
            text(scaled, Raw::Bytes(&[0x07, 0x5B, 0xCD, 0x15])),
            "1234567.89"
        );
        let mut wide = vec![0xFF; 20];
        wide[19] = 0x9C;
        assert_eq!(text(scaled, Raw::Bytes(&wide)), "-1.00");
        let mut large = vec![0; 20];
        large[0] = 0x01;
        assert_eq!(
            text(Kind::Decimal { scale: 0 }, Raw::Bytes(&large)),
            "5708990770823839524233143877797980545530986496"
        );
        assert!(write(scaled, Raw::Bytes(&[1; 257]), &mut String::new()).is_err());
        assert_eq!(text(Kind::Float16, Raw::Bytes(&[0x00, 0x3E])), "1.5");
        assert_eq!(text(Kind::Float16, Raw::Bytes(&[0x00, 0xC0])), "-2.0");
    }

    #[test]
    fn writes_dates_and_times() {
        assert_eq!(text(Kind::Date, Raw::Int32(19783)), "2024-03-01");
        assert_eq!(text(Kind::Date, Raw::Int32(-1)), "1969-12-31");
        let micros = Kind::Time(Unit::Micros);
        assert_eq!(text(micros, Raw::Int64(60_300_000_000)), "16:45:00");
        assert_eq!(text(micros, Raw::Int64(1)), "00:00:00.000001");
        let utc = Kind::Timestamp {
            utc: true,
            unit: Unit::Millis,
        };
        assert_eq!(
            text(utc, Raw::Int64(1_709_402_712_250)),
            "2024-03-02T18:05:12.250Z"
        );
        let local = Kind::Timestamp {
            utc: false,
            unit: Unit::Nanos,
        };
        assert_eq!(
            text(local, Raw::Int64(1_709_280_000_123_456_789)),
            "2024-03-01T08:00:00.123456789"
        );
        let mut int96 = [0u8; 12];
        int96[..8].copy_from_slice(&(3_600 * NANOS_PER_SECOND).to_le_bytes());
        int96[8..].copy_from_slice(&2_440_589i32.to_le_bytes());
        assert_eq!(
            text(Kind::Int96Timestamp, Raw::Int96(&int96)),
            "1970-01-02T01:00:00Z"
        );
    }

    #[test]
    fn writes_uuids_intervals_and_text() {
        let uuid: Vec<u8> = (0..16).collect();
        assert_eq!(
            text(Kind::Uuid, Raw::Bytes(&uuid)),
            "00010203-0405-0607-0809-0a0b0c0d0e0f"
        );
        let mut interval = [0u8; 12];
        interval[..4].copy_from_slice(&1u32.to_le_bytes());
        interval[4..8].copy_from_slice(&2u32.to_le_bytes());
        interval[8..].copy_from_slice(&3_003u32.to_le_bytes());
        assert_eq!(text(Kind::Interval, Raw::Bytes(&interval)), "P1M2DT3.003S");
        assert_eq!(text(Kind::Text, Raw::Bytes(b"Hav\xC3\xB8rn")), "Havørn");
        assert_eq!(text(Kind::Text, Raw::Bytes(b"a\xFFb")), "a\u{FFFD}b");
        assert_eq!(text(Kind::Bytes, Raw::Bytes(b"a\xFFb")), "");
        assert_eq!(text(Kind::Binary, Raw::Bytes(b"text")), "");
        assert!(write(Kind::Date, Raw::Bytes(b"x"), &mut String::new()).is_err());
    }
}
