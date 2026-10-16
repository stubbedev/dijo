//! What ports of Java code need from Java itself to give Java's results: the iteration order of
//! a `java.util.HashMap` and `HashSet` with string keys, string comparison, hashing, lengths,
//! trimming and case mapping, `Character.isWhitespace` and the regex classes `\s` and `.`,
//! `Integer.parseInt`, `Long.parseLong`, `Double.parseDouble`, `Math.round` and
//! `System.lineSeparator`; with `Double.toString` ([`dtoa`]) and the `BigInteger` arithmetic it
//! needs ([`bigint`]), `SimpleDateFormat` ([`simple_date_format`]) and `java.time` and
//! `TimeZone` ([`time`]).

pub(crate) mod bigint;
pub(crate) mod dtoa;
pub(crate) mod simple_date_format;
pub(crate) mod time;

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::text::charset::Charset;
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
    parse_signed(text, 16, hex_digit).and_then(|value| i32::try_from(value).ok())
}

/// `Long.parseLong(text, 16)`: an optional sign and hexadecimal digits as `Character.digit`
/// reads them, in range.
pub(crate) fn parse_hex_long(text: &str) -> Option<i64> {
    parse_signed(text, 16, hex_digit)
}

/// `Long.parseLong(text)`: an optional sign and decimal digits of any script, in range.
pub(crate) fn parse_long(text: &str) -> Option<i64> {
    parse_signed(text, 10, digit)
}

/// An optional sign and digits of `radix` as `value` reads them, as a `long`.
fn parse_signed(text: &str, radix: i128, value: fn(char) -> Option<u32>) -> Option<i64> {
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
        magnitude = magnitude * radix + i128::from(value(character)?);
        if magnitude > i128::from(i64::MAX) + 1 {
            return None;
        }
    }
    i64::try_from(if negative { -magnitude } else { magnitude }).ok()
}

/// `Base64.getMimeDecoder().decode(text)`: the text's ISO-8859-1 bytes, a character beyond it
/// as `?`, decoded with everything outside the alphabet skipped, up to padding that must close a
/// unit; `None` where Java throws.
pub(crate) fn base64_mime_decode(text: &str) -> Option<Vec<u8>> {
    let value = |byte: u8| -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let bytes: Vec<u8> = text
        .chars()
        .map(|character| u8::try_from(character).unwrap_or(b'?'))
        .collect();
    let mut decoded = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut bits: u32 = 0;
    let mut shift: i32 = 18;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        at += 1;
        if byte == b'=' {
            if (shift == 6 && bytes.get(at).copied() != Some(b'=')) || shift == 18 {
                return None;
            }
            if shift == 6 {
                at += 1;
            }
            break;
        }
        let Some(digit) = value(byte) else {
            continue;
        };
        bits |= digit << shift;
        shift -= 6;
        if shift < 0 {
            decoded.extend_from_slice(&bits.to_be_bytes()[1..]);
            shift = 18;
            bits = 0;
        }
    }
    match shift {
        6 => decoded.push(bits.to_be_bytes()[1]),
        0 => decoded.extend_from_slice(&bits.to_be_bytes()[1..3]),
        12 => return None,
        _ => {}
    }
    if bytes[at..].iter().any(|&byte| value(byte).is_some()) {
        return None;
    }
    Some(decoded)
}

/// `new String(bytes, ISO_8859_1)`.
pub(crate) fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

/// `new String(bytes, UTF_8)`: malformed input replaced as the JDK's decoder replaces it.
pub(crate) fn utf8(bytes: &[u8]) -> String {
    if bytes.is_ascii() {
        return latin1(bytes);
    }
    Charset::named("UTF-8")
        .decode(bytes)
        .expect("the JDK decoders decode UTF-8")
}

/// `String.equalsIgnoreCase`: the same length, and each pair of characters the same, or the same
/// once both are upper-cased, or once those are lower-cased, each by its one-character mapping.
pub(crate) fn equals_ignore_case(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count()
        && a.chars()
            .zip(b.chars())
            .all(|(x, y)| x == y || upper(x) == upper(y) || lower(upper(x)) == lower(upper(y)))
}

/// `String.CASE_INSENSITIVE_ORDER`: the first characters that differ once each is upper-cased
/// and that lower-cased, by their one-character mappings, decide; else the shorter string comes
/// first.
pub(crate) fn compare_ignore_case(a: &str, b: &str) -> Ordering {
    let mut left = a.chars();
    let mut right = b.chars();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let (x, y) = (lower(upper(x)), lower(upper(y)));
                if x != y {
                    return x.cmp(&y);
                }
            }
        }
    }
}

/// `Character.toUpperCase` on a code point: its mapping where that is one character.
fn upper(character: char) -> char {
    single(character.to_uppercase()).unwrap_or(character)
}

/// `Character.toLowerCase` on a code point: its mapping where that is one character.
fn lower(character: char) -> char {
    single(character.to_lowercase()).unwrap_or(character)
}

fn single(mut mapped: impl Iterator<Item = char>) -> Option<char> {
    let first = mapped.next()?;
    mapped.next().is_none().then_some(first)
}

/// `String.trim()`: without the characters up to U+0020 at either end.
pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(|character| character <= ' ')
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

/// Java's `String.compareTo`: UTF-16 code units compared in order.
pub(crate) fn compare(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// A string ordered as Java orders strings, for sorted maps keyed as a `TreeMap<String, _>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JavaString(pub(crate) String);

impl Ord for JavaString {
    fn cmp(&self, other: &Self) -> Ordering {
        compare(&self.0, &other.0)
    }
}

impl PartialOrd for JavaString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Java's `String.length()`: the number of UTF-16 code units.
pub(crate) fn utf16_length(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Java's `String.isBlank`: empty or only whitespace as `Character.isWhitespace` defines it.
pub(crate) fn is_blank(text: &str) -> bool {
    text.chars().all(is_whitespace)
}

/// Whether Java's regex `\s` (without `UNICODE_CHARACTER_CLASS`) matches `c`.
pub(crate) fn is_regex_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r')
}

/// Whether `c` ends a line for Java's regex `.` and `$`.
pub(crate) fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Java's `String.toLowerCase(Locale.ROOT)`, which `Locale.ENGLISH` matches.
pub(crate) fn to_lower_case(text: &str) -> String {
    text.to_lowercase()
}

/// Java's `String.toUpperCase(Locale.ROOT)`.
pub(crate) fn to_upper_case(text: &str) -> String {
    text.to_uppercase()
}

/// The order a `java.util.HashSet<String>` of the default capacity iterates `items` in, added
/// in the given order: by bucket of the spread hash in the table the set has grown to, and in
/// insertion order within a bucket. Equal items are kept once.
pub(crate) fn hash_set_order<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut unique: Vec<&str> = Vec::new();
    for item in items {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    let mut capacity = INITIAL_CAPACITY;
    while unique.len() * 4 > capacity * 3 {
        capacity *= 2;
    }
    unique.sort_by_key(|item| spread(string_hash(item)) as usize & (capacity - 1));
    unique
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bytes_as_new_string_does() {
        assert_eq!(latin1(b"\xA9xyz"), "\u{A9}xyz");
        assert_eq!(utf8(b"plain"), "plain");
        assert_eq!(utf8(b"K\xC3\xB8ge"), "K\u{F8}ge");
        assert_eq!(utf8(b"A\xC3\xFF"), "A\u{FFFD}\u{FFFD}");
    }

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
        assert_eq!(parse_hex_long("-7fffffffffffffff"), Some(-i64::MAX));
        assert_eq!(parse_hex_long("8000000000000000"), None);
        assert_eq!(parse_hex_long("1F"), Some(31));
        assert_eq!(
            base64_mime_decode("SGVs\r\nbG8=").as_deref(),
            Some(b"Hello".as_slice())
        );
        assert_eq!(
            base64_mime_decode("QQ==!").as_deref(),
            Some(b"A".as_slice())
        );
        assert_eq!(base64_mime_decode("QQ=").as_deref(), None);
        assert_eq!(base64_mime_decode("Q").as_deref(), None);
        assert_eq!(base64_mime_decode("QUJD=QQ").as_deref(), None);
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
        assert_eq!(
            compare_ignore_case("Content-Type", "content-type"),
            Ordering::Equal
        );
        assert_eq!(
            compare_ignore_case("WARC-Date", "warc-type"),
            Ordering::Less
        );
        assert_eq!(compare_ignore_case("_x", "a"), Ordering::Less);
        assert_eq!(compare_ignore_case("ab", "A"), Ordering::Greater);
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

    #[test]
    fn orders_as_java_does() {
        assert_eq!(compare("\u{FFFF}", "\u{1F600}"), Ordering::Greater);
        assert_eq!(compare("a", "b"), Ordering::Less);
        assert_eq!(utf16_length("a\u{1F600}"), 3);
    }

    #[test]
    fn iterates_a_hash_set_as_java_does() {
        assert_eq!(
            hash_set_order(["presentation.apxl", "index.xml", "index.apxl"]),
            ["index.apxl", "index.xml", "presentation.apxl"]
        );
    }
}
