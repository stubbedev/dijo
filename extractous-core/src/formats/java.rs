//! What ports of Tika code need from Java itself to give Java's results: the iteration order of
//! a `java.util.HashMap` with string keys, `Integer.parseInt`, `Long.parseLong`,
//! `Double.parseDouble`, `Math.round`, `String.trim`, `String.strip` and `System.lineSeparator`.

use std::collections::HashMap;

pub(crate) use crate::mime::java_trim as trim;
use crate::unicode::{decimal_digit, is_whitespace};

/// `System.lineSeparator()`, which ends each line of a stack trace Java prints.
pub(crate) const LINE_SEPARATOR: &str = if cfg!(windows) { "\r\n" } else { "\n" };

const INITIAL_CAPACITY: usize = 16;
const TREEIFY_THRESHOLD: usize = 8;
const MIN_TREEIFY_CAPACITY: usize = 64;

/// A `java.util.HashMap<String, V>`, iterated in Java's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JavaHashMap<V> {
    entries: Vec<Entry<V>>,
    index: HashMap<String, usize>,
    /// The table size, 0 until the first entry allocates the table.
    capacity: usize,
    /// The table size the first entry allocates.
    initial_capacity: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry<V> {
    key: String,
    value: V,
    hash: u32,
}

impl<V> Default for JavaHashMap<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
            capacity: 0,
            initial_capacity: INITIAL_CAPACITY,
        }
    }
}

impl<V> JavaHashMap<V> {
    /// An empty map whose table `putAll` of a map of `size` entries sizes, as
    /// `HashMap.putMapEntries` does for a map without a table yet.
    pub(crate) fn sized_for_put_all(size: usize) -> Self {
        let mut map = Self::default();
        if size > 0 {
            map.initial_capacity = (size * 4).div_ceil(3).next_power_of_two();
        }
        map
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// `putIfAbsent`.
    pub(crate) fn insert_if_absent(&mut self, key: &str, value: V) {
        if !self.contains_key(key) {
            self.insert_new(key, value);
        }
    }

    /// A copy of the map as `new HashMap<>()` followed by `putAll(this)` builds it.
    pub(crate) fn copied(&self) -> Self
    where
        V: Clone,
    {
        let mut copy = Self::sized_for_put_all(self.len());
        for (key, value) in self.iter() {
            copy.insert(key, value.clone());
        }
        copy
    }

    pub(crate) fn get(&self, key: &str) -> Option<&V> {
        self.index.get(key).map(|&index| &self.entries[index].value)
    }

    pub(crate) fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.index
            .get(key)
            .map(|&index| &mut self.entries[index].value)
    }

    /// `put`: replaces the value of a key already present, which keeps its place in the map.
    pub(crate) fn insert(&mut self, key: &str, value: V) {
        match self.get_mut(key) {
            Some(present) => *present = value,
            None => self.insert_new(key, value),
        }
    }

    /// `remove`: the key's entry goes; the table keeps its size, as Java's does.
    pub(crate) fn remove(&mut self, key: &str) {
        if let Some(index) = self.index.remove(key) {
            self.entries.remove(index);
            for position in self.index.values_mut() {
                if *position > index {
                    *position -= 1;
                }
            }
        }
    }

    /// `clear`: every entry goes; the table keeps its size, as Java's does.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
    }

    /// The entries in insertion order.
    pub(crate) fn iter_inserted(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries
            .iter()
            .map(|entry| (entry.key.as_str(), &entry.value))
    }

    /// The entries in the order Java iterates them: by bucket of the table, then in insertion
    /// order within a bucket. Past eight keys in one bucket of a table of 64 or more, where Java
    /// turns the bucket into a tree, the order stays the order of insertion.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        let mask = self.capacity.saturating_sub(1);
        let mut order: Vec<(usize, &Entry<V>)> = self.entries.iter().enumerate().collect();
        order.sort_by_key(|(sequence, entry)| (entry.hash as usize & mask, *sequence));
        order
            .into_iter()
            .map(|(_, entry)| (entry.key.as_str(), &entry.value))
    }

    fn insert_new(&mut self, key: &str, value: V) {
        if self.capacity == 0 {
            self.capacity = self.initial_capacity;
        }
        let hash = spread(string_hash(key));
        let bucket = hash as usize & (self.capacity - 1);
        let in_bucket = 1 + self
            .entries
            .iter()
            .filter(|entry| entry.hash as usize & (self.capacity - 1) == bucket)
            .count();
        self.index.insert(key.to_owned(), self.entries.len());
        self.entries.push(Entry {
            key: key.to_owned(),
            value,
            hash,
        });
        if in_bucket > TREEIFY_THRESHOLD && self.capacity < MIN_TREEIFY_CAPACITY {
            self.capacity *= 2;
        }
        if self.entries.len() > self.capacity * 3 / 4 {
            self.capacity *= 2;
        }
    }
}

/// `String.hashCode()`: over the UTF-16 code units, wrapping.
pub(crate) fn string_hash(text: &str) -> u32 {
    text.encode_utf16().fold(0u32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(u32::from(unit))
    })
}

/// `HashMap.hash`: the hash with its high half folded into its low half.
fn spread(hash: u32) -> u32 {
    hash ^ (hash >> 16)
}

/// `Character.digit(c, 10)` on a `char`: the value of a decimal digit of any script in the Basic
/// Multilingual Plane. A digit beyond it is two `char`s to Java, neither of them a digit.
pub(crate) fn digit(character: char) -> Option<u32> {
    if character >= '\u{10000}' {
        return None;
    }
    decimal_digit(character)
}

/// `Character.isDigit` on a `char`.
pub(crate) fn is_digit(character: char) -> bool {
    digit(character).is_some()
}

/// `Character.digit(c, 16)` on a `char`: a decimal digit, or a Latin letter from A to F in either
/// case, halfwidth or fullwidth.
fn hex_digit(character: char) -> Option<u32> {
    match character {
        'a'..='f' | 'A'..='F' => character.to_digit(16),
        '\u{ff21}'..='\u{ff26}' => Some(u32::from(character) - 0xff21 + 10),
        '\u{ff41}'..='\u{ff46}' => Some(u32::from(character) - 0xff41 + 10),
        _ => digit(character),
    }
}

/// `Integer.parseInt(text)`: an optional sign and decimal digits of any script, in range.
pub(crate) fn parse_int(text: &str) -> Option<i32> {
    parse_long(text).and_then(|value| i32::try_from(value).ok())
}

/// `URLDecoder.decode(text, UTF-8)`: `+` is a space, and each run of `%` and two hexadecimal
/// digits, as `Integer.parseInt` reads them, sign included, is UTF-8, with malformed sequences
/// replaced. None where Java throws: on an escape that is cut short, negative or no number.
pub(crate) fn url_decode(text: &str) -> Option<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut decoded = String::with_capacity(text.len());
    let mut position = 0;
    while position < units.len() {
        match units[position] {
            0x2b => {
                decoded.push(' ');
                position += 1;
            }
            0x25 => {
                let mut bytes = Vec::new();
                while position + 2 < units.len() && units[position] == 0x25 {
                    let escape = String::from_utf16(&units[position + 1..position + 3]).ok()?;
                    let value = parse_hex(&escape)?;
                    bytes.push(u8::try_from(value).ok()?);
                    position += 3;
                }
                if units.get(position) == Some(&0x25) {
                    return None;
                }
                decoded.push_str(&String::from_utf8_lossy(&bytes));
            }
            _ => {
                let end = units[position..]
                    .iter()
                    .position(|&unit| unit == 0x2b || unit == 0x25)
                    .map_or(units.len(), |offset| position + offset);
                decoded.push_str(&String::from_utf16_lossy(&units[position..end]));
                position = end;
            }
        }
    }
    Some(decoded)
}

/// `Integer.parseInt(text, 16)` for the two characters of an escape, which must not be negative.
fn parse_hex(text: &str) -> Option<i32> {
    parse_hex_int(text).filter(|&value| value >= 0)
}

/// `Integer.parseInt(text, 16)`: an optional sign and hexadecimal digits as `Character.digit`
/// reads them, in range.
pub(crate) fn parse_hex_int(text: &str) -> Option<i32> {
    let (negative, digits) = match text.chars().next()? {
        '-' => (true, &text[1..]),
        '+' => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() {
        return None;
    }
    let mut magnitude: i64 = 0;
    for character in digits.chars() {
        magnitude = magnitude * 16 + i64::from(hex_digit(character)?);
        if magnitude > i64::from(i32::MAX) + 1 {
            return None;
        }
    }
    i32::try_from(if negative { -magnitude } else { magnitude }).ok()
}

/// `Long.parseLong(text)`: an optional sign and decimal digits of any script, in range.
pub(crate) fn parse_long(text: &str) -> Option<i64> {
    let (negative, digits) = match text.chars().next()? {
        '-' => (true, &text[1..]),
        '+' => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() {
        return None;
    }
    let mut magnitude: i128 = 0;
    for character in digits.chars() {
        magnitude = magnitude * 10 + i128::from(digit(character)?);
        if magnitude > i128::from(i64::MAX) + 1 {
            return None;
        }
    }
    i64::try_from(if negative { -magnitude } else { magnitude }).ok()
}

/// `String.equalsIgnoreCase`: the same length, and each pair of characters the same, or the same
/// once both are upper-cased, or once those are lower-cased, each by its one-character mapping.
pub(crate) fn equals_ignore_case(a: &str, b: &str) -> bool {
    fn upper(character: char) -> char {
        single(character.to_uppercase()).unwrap_or(character)
    }
    fn lower(character: char) -> char {
        single(character.to_lowercase()).unwrap_or(character)
    }
    fn single(mut mapped: impl Iterator<Item = char>) -> Option<char> {
        let first = mapped.next()?;
        mapped.next().is_none().then_some(first)
    }
    a.chars().count() == b.chars().count()
        && a.chars()
            .zip(b.chars())
            .all(|(x, y)| x == y || upper(x) == upper(y) || lower(upper(x)) == lower(upper(y)))
}

/// `String.strip()`: without the whitespace `Character.isWhitespace` sees at either end.
pub(crate) fn strip(text: &str) -> &str {
    text.trim_matches(is_whitespace)
}

/// The double `Double.parseDouble(text)` gives, or `None` where it throws.
pub(crate) fn parse_double(text: &str) -> Option<f64> {
    let text = trim(text);
    let (negative, body) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let magnitude = match body {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        _ if body.starts_with("0x") || body.starts_with("0X") => parse_hex_double(&body[2..])?,
        _ => parse_decimal(body)?,
    };
    Some(if negative { -magnitude } else { magnitude })
}

/// `Math.round(double)`: the nearest `long`, a tie towards positive infinity, NaN as 0 and the
/// infinities and anything beyond saturated.
pub(crate) fn round(value: f64) -> i64 {
    let floor = value.floor();
    let rounded = if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    rounded as i64
}

fn strip_suffix(text: &str) -> &str {
    text.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(text)
}

fn parse_decimal(text: &str) -> Option<f64> {
    let text = strip_suffix(text);
    let bytes = text.as_bytes();
    let mut at = 0;
    let integer = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    at += integer;
    let mut fraction = 0;
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        fraction = bytes[at..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        at += fraction;
    }
    if integer + fraction == 0 {
        return None;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let exponent = bytes[at..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if exponent == 0 {
            return None;
        }
        at += exponent;
    }
    if at != bytes.len() {
        return None;
    }
    text.parse().ok()
}

/// A hexadecimal significand `h.hhh` with its required `p` exponent, rounded to nearest even.
fn parse_hex_double(text: &str) -> Option<f64> {
    let text = strip_suffix(text);
    let (significand, exponent) = text.split_once(['p', 'P'])?;
    let exponent_digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    if exponent_digits.is_empty() || !exponent_digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let exponent: i64 = exponent.parse().unwrap_or(if exponent.starts_with('-') {
        i64::MIN / 4
    } else {
        i64::MAX / 4
    });
    let (whole, fraction) = significand.split_once('.').unwrap_or((significand, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole
        .bytes()
        .chain(fraction.bytes())
        .all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let mut mantissa: u128 = 0;
    let mut sticky = false;
    let mut binary_exponent = exponent.saturating_sub(4 * fraction.len() as i64);
    for digit in whole.chars().chain(fraction.chars()) {
        let value = u128::from(digit.to_digit(16).unwrap_or(0));
        if mantissa >> 120 == 0 {
            mantissa = mantissa * 16 + value;
        } else {
            sticky |= value != 0;
            binary_exponent = binary_exponent.saturating_add(4);
        }
    }
    if mantissa == 0 {
        return Some(0.0);
    }
    let bits = 128 - i64::from(mantissa.leading_zeros());
    let mut unbiased = binary_exponent + bits - 1;
    let precision: i64 = if unbiased < -1022 {
        (53 - (-1022 - unbiased)).max(0)
    } else {
        53
    };
    let shift = bits - precision;
    let mut rounded = if shift > 0 {
        let shift = u32::try_from(shift).unwrap_or(127).min(127);
        let kept = mantissa >> shift;
        let remainder = mantissa & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        let above_half = remainder > half || (remainder == half && (sticky || kept & 1 == 1));
        kept + u128::from(above_half)
    } else {
        mantissa << u32::try_from(-shift).unwrap_or(0)
    };
    let mut scale = binary_exponent + shift;
    if rounded >> precision.max(0) != 0 && precision > 0 {
        rounded >>= 1;
        scale += 1;
        unbiased += 1;
    }
    if unbiased > 1023 {
        return Some(f64::INFINITY);
    }
    let value = rounded as f64;
    Some(value * 2f64.powi(i32::try_from(scale.clamp(-1100, 1100)).unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_strings_as_java_does() {
        assert_eq!(string_hash(""), 0);
        assert_eq!(string_hash("Content-Type") as i32, 949_037_134);
        assert_eq!(string_hash("é😀") as i32, 1_996_812);
    }

    #[test]
    fn sizes_a_put_all_table_as_java_does() {
        let capacities: Vec<usize> = (1..=13)
            .map(|size| JavaHashMap::<()>::sized_for_put_all(size).initial_capacity)
            .collect();
        assert_eq!(capacities, [2, 4, 4, 8, 8, 8, 16, 16, 16, 16, 16, 16, 32]);
    }

    #[test]
    fn follows_java_hash_map_through_resizes() {
        let mut map = JavaHashMap::default();
        for i in 0..40 {
            map.insert(&format!("key{}", i * 7), ());
        }
        let expected = "key0 key217 key238 key119 key84 key7 key259 key154 key231 key21 key175 \
                        key252 key42 key112 key63 key133 key210 key14 key35 key196 key273 key56 \
                        key91 key147 key224 key168 key245 key105 key70 key126 key203 key77 key98 \
                        key189 key266 key161 key182 key140 key28 key49";
        assert_eq!(
            map.iter().map(|(key, ())| key).collect::<Vec<_>>(),
            expected.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn parses_numbers_as_java_does() {
        assert_eq!(parse_int("42"), Some(42));
        assert_eq!(parse_int("+7"), Some(7));
        assert_eq!(parse_int("-2147483648"), Some(i32::MIN));
        assert_eq!(parse_int("2147483648"), None);
        assert_eq!(parse_int("٣"), Some(3));
        assert_eq!(parse_int("𝟗"), None);
        assert_eq!(parse_int(" 1"), None);
        assert_eq!(parse_int("-"), None);
        assert_eq!(parse_long("9223372036854775807"), Some(i64::MAX));
        assert_eq!(parse_long("-9223372036854775808"), Some(i64::MIN));
        assert_eq!(parse_long("9223372036854775808"), None);
    }

    #[test]
    fn decodes_urls_as_java_does() {
        assert_eq!(url_decode("a%20b+c").as_deref(), Some("a b c"));
        assert_eq!(url_decode("%C3%A6%C3").as_deref(), Some("æ\u{fffd}"));
        assert_eq!(url_decode("%+1").as_deref(), Some("\u{1}"));
        assert_eq!(url_decode("plain😀").as_deref(), Some("plain😀"));
        assert_eq!(url_decode("%4"), None);
        assert_eq!(url_decode("%-1"), None);
        assert_eq!(url_decode("%zz"), None);
        assert_eq!(url_decode("%41%"), None);
    }

    #[test]
    fn compares_ignoring_case_as_java_does() {
        assert!(equals_ignore_case("itemRef", "itemref"));
        assert!(equals_ignore_case("rootfıle", "ROOTFILE"));
        assert!(!equals_ignore_case("item", "items"));
        assert!(!equals_ignore_case("straße", "STRASSE"));
    }

    #[test]
    fn trims_as_java_does() {
        assert_eq!(trim("\u{1}\t a \u{a0}\n"), "a \u{a0}");
    }

    #[test]
    fn parses_what_java_parses() {
        assert_eq!(parse_double("1.5"), Some(1.5));
        assert_eq!(parse_double(" -2e3 "), Some(-2000.0));
        assert_eq!(parse_double("1."), Some(1.0));
        assert_eq!(parse_double(".5d"), Some(0.5));
        assert_eq!(parse_double("+7f"), Some(7.0));
        assert!(parse_double("NaN").unwrap().is_nan());
        assert_eq!(parse_double("-Infinity"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_double("0x1p3"), Some(8.0));
        assert_eq!(parse_double("0x1.8p1"), Some(3.0));
        for invalid in ["", "e5", "1e", "1,5", "inf", "nan", "0x1", "1.2.3", "1 2"] {
            assert_eq!(parse_double(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn rounds_as_java_does() {
        assert_eq!(round(2.5), 3);
        assert_eq!(round(-2.5), -2);
        assert_eq!(round(0.49999999999999994), 0);
        assert_eq!(round(f64::NAN), 0);
        assert_eq!(round(1e300), i64::MAX);
    }
}
