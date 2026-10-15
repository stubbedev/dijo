//! What ports of Tika code need from Java itself to give Java's results: the iteration order of
//! a `java.util.HashMap` with string keys, and `Integer.parseInt`, `Long.parseLong` and
//! `String.trim`.

use std::collections::HashMap;

use crate::unicode::decimal_digit;

const INITIAL_CAPACITY: usize = 16;
const TREEIFY_THRESHOLD: usize = 8;
const MIN_TREEIFY_CAPACITY: usize = 64;

/// A `java.util.HashMap<String, V>`, iterated in Java's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JavaHashMap<V> {
    entries: Vec<Entry<V>>,
    index: HashMap<String, usize>,
    capacity: usize,
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
        }
    }
}

impl<V> JavaHashMap<V> {
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
            self.capacity = INITIAL_CAPACITY;
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
        if self.entries.len() > self.capacity / 4 * 3 {
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

/// `Integer.parseInt(text, 16)` for the two characters of an escape.
fn parse_hex(text: &str) -> Option<i32> {
    let (negative, digits) = match text.chars().next()? {
        '-' => (true, &text[1..]),
        '+' => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() {
        return None;
    }
    let magnitude = digits.chars().try_fold(0i32, |value, character| {
        Some(value * 16 + i32::try_from(hex_digit(character)?).ok()?)
    })?;
    let value = if negative { -magnitude } else { magnitude };
    (value >= 0).then_some(value)
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

/// `String.trim()`: without the characters up to U+0020 at either end.
pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(|character| character <= ' ')
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
}
