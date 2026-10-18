//! For the parity tests: the Access databases as Tika's path writes them, where the port
//! departs from it on purpose (#122).

use std::path::Path;

use super::column::Decimal;
use super::format;
use crate::formats::parity::{ComparedMetadata, NativeRewrite, Normalisation};

/// The rewrite of the native result of the database at `path` as Tika's path decodes it, and
/// the normalisation of both texts that makes their cells alike: a Jet 3 database's text
/// decoded from its code page, where Tika decodes it as UTF-8, and the cells
/// [`as_tika_formats_cells`] makes alike.
pub fn as_tika_writes(path: &Path) -> (NativeRewrite, Normalisation) {
    let rewrite: NativeRewrite = if is_jet3(path) {
        as_tika_decodes_jet3
    } else {
        as_tika_decodes
    };
    (rewrite, as_tika_formats_cells)
}

/// Whether the database at `path` is a Jet 3 database, whose text is in a code page.
pub(super) fn is_jet3(path: &Path) -> bool {
    let database =
        std::fs::read(path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    matches!(
        format::detect(&database),
        Ok(format) if format.encoding == format::Encoding::CodePage
    )
}

/// The native result of a database Tika decodes as the native parser does, as it is.
fn as_tika_decodes(_: &mut String, _: &mut ComparedMetadata, _: &ComparedMetadata) {}

/// The native result of a Jet 3 database in code page 1252, as each of the fixtures is, with
/// its text and metadata decoded as Tika decodes them: the bytes of each run of characters past
/// ASCII, in code page 1252, decoded as UTF-8.
fn as_tika_decodes_jet3(text: &mut String, metadata: &mut ComparedMetadata, _: &ComparedMetadata) {
    *text = decoded_as_utf8(text);
    for (_, values) in metadata.iter_mut() {
        for value in values.iter_mut() {
            *value = decoded_as_utf8(value);
        }
    }
}

/// `text`, each run of characters past ASCII encoded in code page 1252 and decoded as Java
/// decodes UTF-8.
fn decoded_as_utf8(text: &str) -> String {
    let cp1252 = crate::text::charset::Charset::named("windows-1252");
    let byte_of = |c: char| {
        (0x80..=0xFF_u8)
            .find(|&byte| cp1252.decode(&[byte]).unwrap().starts_with(c))
            .unwrap_or_else(|| panic!("{c:?} is in code page 1252"))
    };
    let mut decoded = String::with_capacity(text.len());
    let mut run = Vec::new();
    for c in text.chars().chain(std::iter::once('\0')) {
        if c.is_ascii() {
            decoded.push_str(&crate::java::utf8(&run));
            run.clear();
            decoded.push(c);
        } else {
            run.push(byte_of(c));
        }
    }
    decoded.pop();
    decoded
}

/// Both texts with their cells made alike where the native parser writes them otherwise on
/// purpose (#122), and with nothing else changed:
/// - a date's time, which only the native parser writes, left out;
/// - a date before the Gregorian calendar of 1582, which Tika writes in the Julian calendar,
///   and its year without leading zeros, as `early date`;
/// - a currency amount, which Tika writes with the currency sign `¤` (`??` in US-ASCII) and
///   grouped thousands, and a number with an exponent, which Tika writes as
///   `BigDecimal.toString` does, as plain numbers; and every number with more than two decimals
///   rounded to two, as Tika rounds a currency amount.
fn as_tika_formats_cells(text: &mut String) {
    use regex::{Captures, Regex};
    let time = Regex::new(r"(\d{1,4}-\d\d-\d\d) \d\d:\d\d:\d\d(\.\d{3})?").unwrap();
    let date = Regex::new(r"\b(\d{1,4})-\d\d-\d\d\b").unwrap();
    let money = Regex::new("(-?)(?:\u{a4}\u{a0}|\\?\\?)([\\d,]+\\.\\d\\d)").unwrap();
    let exponent = Regex::new(r"(-?)(\d)(?:\.(\d+))?E([+-]\d+)").unwrap();
    let decimals = Regex::new(r"(-?\d+)\.(\d\d)(\d+)").unwrap();
    let mut normalised = time.replace_all(text, "$1").into_owned();
    normalised = date
        .replace_all(&normalised, |captures: &Captures| {
            if captures[1].parse::<u32>().unwrap() < 1583 {
                "early date".to_owned()
            } else {
                captures[0].to_owned()
            }
        })
        .into_owned();
    normalised = money
        .replace_all(&normalised, |captures: &Captures| {
            format!("{}{}", &captures[1], captures[2].replace(',', ""))
        })
        .into_owned();
    normalised = exponent
        .replace_all(&normalised, |captures: &Captures| {
            let digits = format!(
                "{}{}",
                &captures[2],
                captures.get(3).map_or("", |m| m.as_str())
            );
            let fraction = captures.get(3).map_or(0, |m| m.len()) as i64;
            let exponent: i64 = captures[4].parse().unwrap();
            let negative = !captures[1].is_empty();
            let decimal = Decimal {
                negative,
                magnitude: digits.parse().unwrap(),
                scale: fraction - exponent,
            };
            decimal.plain()
        })
        .into_owned();
    normalised = decimals
        .replace_all(&normalised, |captures: &Captures| {
            rounded_half_even(&captures[1], &captures[2], &captures[3])
        })
        .into_owned();
    *text = normalised;
}

/// The number of integer part `integer`, two decimals `kept` and further decimals `dropped`,
/// rounded half to even to its two decimals.
fn rounded_half_even(integer: &str, kept: &str, dropped: &str) -> String {
    let negative = integer.starts_with('-');
    let hundredths: u128 = format!("{}{kept}", integer.trim_start_matches('-'))
        .parse()
        .unwrap();
    let first = dropped.as_bytes()[0];
    let rest_zero = dropped[1..].bytes().all(|b| b == b'0');
    let up = first > b'5' || (first == b'5' && (!rest_zero || hundredths % 2 == 1));
    Decimal {
        negative,
        magnitude: hundredths + u128::from(up),
        scale: 2,
    }
    .plain()
}
