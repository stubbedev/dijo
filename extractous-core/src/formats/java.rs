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

/// `Integer.parseInt(text)`: an optional sign and decimal digits of any script, in range.
pub(crate) fn parse_int(text: &str) -> Option<i32> {
    parse_long(text).and_then(|value| i32::try_from(value).ok())
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
        magnitude = magnitude * 10 + i128::from(decimal_digit(character)?);
        if magnitude > i128::from(i64::MAX) + 1 {
            return None;
        }
    }
    i64::try_from(if negative { -magnitude } else { magnitude }).ok()
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
        assert_eq!(parse_int("𝟗"), Some(9));
        assert_eq!(parse_int(" 1"), None);
        assert_eq!(parse_int("-"), None);
        assert_eq!(parse_long("9223372036854775807"), Some(i64::MAX));
        assert_eq!(parse_long("-9223372036854775808"), Some(i64::MIN));
        assert_eq!(parse_long("9223372036854775808"), None);
    }

    #[test]
    fn trims_as_java_does() {
        assert_eq!(trim("\u{1}\t a \u{a0}\n"), "a \u{a0}");
    }
}
