//! Tika's `Metadata`: names with one or more values, which Tika keeps in a `java.util.HashMap`.
//! The order its `names()` lists them in is visible in the XHTML head, so [`TikaMetadata::names`]
//! reproduces that map's iteration order.

use crate::Metadata;

const INITIAL_CAPACITY: usize = 16;
const TREEIFY_THRESHOLD: usize = 8;
const MIN_TREEIFY_CAPACITY: usize = 64;

/// A document's metadata, in the order Tika's `HashMap` holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TikaMetadata {
    entries: Vec<Entry>,
    capacity: usize,
    next_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    values: Vec<String>,
    hash: u32,
    sequence: u64,
}

impl TikaMetadata {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The first value of `name`, as Tika's `Metadata.get`.
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.entry(name)
            .and_then(|entry| entry.values.first())
            .map(String::as_str)
    }

    /// Every value of `name`, as Tika's `Metadata.getValues`.
    pub(crate) fn values(&self, name: &str) -> &[String] {
        self.entry(name)
            .map_or(&[], |entry| entry.values.as_slice())
    }

    /// Replaces the values of `name` with `value`, as Tika's `Metadata.set`. A name already
    /// present keeps its place in the map.
    pub(crate) fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.entry_mut(name) {
            Some(entry) => entry.values = vec![value],
            None => self.insert(name, vec![value]),
        }
    }

    /// Replaces the values of `name` with `values`, as Tika's `Metadata.set` with an array.
    pub(crate) fn set_values(&mut self, name: &str, values: Vec<String>) {
        match self.entry_mut(name) {
            Some(entry) => entry.values = values,
            None => self.insert(name, values),
        }
    }

    /// Replaces the values of `name` with `values` as Tika's `Metadata.set` does for a
    /// `Property` with an array: it removes the name and adds it again, which puts it last in
    /// its bucket of the map, and leaves it out when `values` is empty.
    pub(crate) fn reset_values(&mut self, name: &str, values: Vec<String>) {
        self.entries.retain(|entry| entry.name != name);
        if !values.is_empty() {
            self.insert(name, values);
        }
    }

    /// Appends `value` to the values of `name`, as Tika's `Metadata.add`.
    pub(crate) fn add(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.entry_mut(name) {
            Some(entry) => entry.values.push(value),
            None => self.insert(name, vec![value]),
        }
    }

    /// The names in the order Tika's `Metadata.names()` returns them: the iteration order of a
    /// `java.util.HashMap` that saw the same insertions. Past eight names in one bucket of a
    /// table of 64 or more, where Java turns the bucket into a tree, the order stays the order
    /// of insertion.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        let mask = self.capacity.saturating_sub(1);
        let mut order: Vec<&Entry> = self.entries.iter().collect();
        order.sort_by_key(|entry| (entry.hash as usize & mask, entry.sequence));
        order.into_iter().map(|entry| entry.name.as_str())
    }

    /// The metadata as the crate's public [`Metadata`] map.
    pub(crate) fn to_map(&self) -> Metadata {
        self.entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.values.clone()))
            .collect()
    }

    fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    fn entry_mut(&mut self, name: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|entry| entry.name == name)
    }

    fn insert(&mut self, name: &str, values: Vec<String>) {
        if self.capacity == 0 {
            self.capacity = INITIAL_CAPACITY;
        }
        let hash = spread(java_string_hash(name));
        self.entries.push(Entry {
            name: name.to_owned(),
            values,
            hash,
            sequence: self.next_sequence,
        });
        self.next_sequence += 1;
        let bucket = hash as usize & (self.capacity - 1);
        let in_bucket = self
            .entries
            .iter()
            .filter(|entry| entry.hash as usize & (self.capacity - 1) == bucket)
            .count();
        if in_bucket > TREEIFY_THRESHOLD && self.capacity < MIN_TREEIFY_CAPACITY {
            self.capacity *= 2;
        }
        if self.entries.len() > self.capacity / 4 * 3 {
            self.capacity *= 2;
        }
    }
}

/// `String.hashCode()`: over the UTF-16 code units, wrapping.
fn java_string_hash(text: &str) -> u32 {
    text.encode_utf16().fold(0u32, |hash, unit| {
        hash.wrapping_mul(31).wrapping_add(u32::from(unit))
    })
}

/// `HashMap.hash`: the hash with its high half folded into its low half.
fn spread(hash: u32) -> u32 {
    hash ^ (hash >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_strings_as_java_does() {
        assert_eq!(java_string_hash(""), 0);
        assert_eq!(java_string_hash("Content-Type") as i32, 949_037_134);
        assert_eq!(java_string_hash("é😀") as i32, 1_996_812);
    }

    #[test]
    fn lists_names_in_the_order_java_hash_map_iterates_them() {
        let mut metadata = TikaMetadata::new();
        for name in [
            "tk:content-type-magic-detected",
            "tk:resource-name",
            "Content-Length",
            "Content-Type",
            "tk:parsed-by",
            "dcterms:created",
        ] {
            metadata.set(name, "x");
        }
        assert_eq!(
            metadata.names().collect::<Vec<_>>(),
            [
                "tk:content-type-magic-detected",
                "tk:resource-name",
                "dcterms:created",
                "Content-Length",
                "tk:parsed-by",
                "Content-Type",
            ]
        );
    }

    #[test]
    fn follows_java_hash_map_through_resizes() {
        let mut metadata = TikaMetadata::new();
        for i in 0..40 {
            metadata.set(&format!("key{}", i * 7), "x");
        }
        let expected = "key0 key217 key238 key119 key84 key7 key259 key154 key231 key21 key175 \
                        key252 key42 key112 key63 key133 key210 key14 key35 key196 key273 key56 \
                        key91 key147 key224 key168 key245 key105 key70 key126 key203 key77 key98 \
                        key189 key266 key161 key182 key140 key28 key49";
        assert_eq!(
            metadata.names().collect::<Vec<_>>(),
            expected.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn resetting_values_moves_a_name_to_the_end_of_its_bucket() {
        let mut metadata = TikaMetadata::new();
        metadata.set("a", "1");
        metadata.set("q", "1");
        metadata.set_values("a", vec!["2".to_owned()]);
        assert_eq!(metadata.names().collect::<Vec<_>>(), ["a", "q"]);
        metadata.reset_values("a", vec!["3".to_owned(), "4".to_owned()]);
        assert_eq!(metadata.names().collect::<Vec<_>>(), ["q", "a"]);
        assert_eq!(metadata.values("a"), ["3", "4"]);
        metadata.reset_values("q", Vec::new());
        assert_eq!(metadata.names().collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn sets_and_adds_values_as_java_does() {
        let mut metadata = TikaMetadata::new();
        metadata.set("a", "1");
        metadata.set("q", "1");
        metadata.set("a", "2");
        assert_eq!(metadata.values("a"), ["2"]);
        metadata.add("a", "3");
        assert_eq!(metadata.values("a"), ["2", "3"]);
        assert_eq!(metadata.get("a"), Some("2"));
        assert_eq!(metadata.get("b"), None);
        assert!(metadata.values("b").is_empty());
    }
}
