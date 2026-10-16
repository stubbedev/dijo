//! Tika's `Metadata`: names with one or more values, which Tika keeps in a `java.util.HashMap`.
//! The order its `names()` lists them in is visible in the XHTML head, so [`TikaMetadata::names`]
//! reproduces that map's iteration order.

use crate::formats::handlers::{is_blank, utf16_length};
use crate::formats::java::JavaHashMap;
use crate::Metadata;

/// Office's `USER_DEFINED` key prefix, of a document's custom properties.
pub(crate) const USER_DEFINED_PREFIX: &str = "custom:";

/// The names of Tika's `Office` properties and the core properties the Office extractors of
/// several formats set.
pub(crate) mod office {
    pub(crate) const HAS_COMMENTS: &str = "msoffice:has-comments";
    pub(crate) const COMMENT_PERSONS: &str = "msoffice:comment-person-display-name";
    pub(crate) const HAS_HIDDEN_TEXT: &str = "msoffice:doc:has-hidden-text";
    pub(crate) const HAS_TRACK_CHANGES: &str = "msoffice:has-track-changes";
    pub(crate) const EMBEDDED_STORAGE_CLASS_ID: &str = "msoffice:embedded-storage-class-id";
    pub(crate) const OCX_NAME: &str = "msoffice:ocx-name";
    pub(crate) const ORIGINAL_RESOURCE_NAME: &str = "tk:orig-resource-name";
    pub(crate) const RESOURCE_NAME_EXTENSION_INFERRED: &str = "tk:resource-name-extension-inferred";
}

/// The longest name `Metadata.add(KeyPrefix, name, value)` takes, in UTF-16 units.
const MAX_PREFIX_ROUTE_NAME_LENGTH: usize = 1024;
/// The most names `Metadata.add(KeyPrefix, name, value)` adds to one metadata.
const MAX_PREFIX_ROUTE_NAMES: usize = 10_000;

/// A document's metadata, in the order Tika's `HashMap` holds it.
#[derive(Debug, Clone, Default)]
pub(crate) struct TikaMetadata {
    entries: JavaHashMap<Vec<String>>,
    /// The names added under a prefix so far, which Tika counts per metadata.
    prefix_route_names: usize,
}

impl PartialEq for TikaMetadata {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for TikaMetadata {}

impl TikaMetadata {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The first value of `name`, as Tika's `Metadata.get`.
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .get(name)
            .and_then(|values| values.first())
            .map(String::as_str)
    }

    /// Every value of `name`, as Tika's `Metadata.getValues`.
    pub(crate) fn values(&self, name: &str) -> &[String] {
        self.entries.get(name).map_or(&[], Vec::as_slice)
    }

    /// Replaces the values of `name` with `value`, as Tika's `Metadata.set`. A name already
    /// present keeps its place in the map.
    pub(crate) fn set(&mut self, name: &str, value: impl Into<String>) {
        self.entries.insert(name, vec![value.into()]);
    }

    /// Replaces the values of `name` with `values`, as Tika's `Metadata.set` with an array.
    pub(crate) fn set_values(&mut self, name: &str, values: Vec<String>) {
        self.entries.insert(name, values);
    }

    /// Replaces the values of `name` with `values` as Tika's `Metadata.set` does for a
    /// `Property` with an array: it removes the name and adds it again, which puts it last in
    /// its bucket of the map, and leaves it out when `values` is empty.
    pub(crate) fn reset_values(&mut self, name: &str, values: Vec<String>) {
        self.entries.remove(name);
        if !values.is_empty() {
            self.entries.insert(name, values);
        }
    }

    /// Removes `name`, as Tika's `Metadata.set` with a null value does.
    pub(crate) fn remove(&mut self, name: &str) {
        self.entries.remove(name);
    }

    /// Appends `value` to the values of `name`, as Tika's `Metadata.add`.
    pub(crate) fn add(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.entries.get_mut(name) {
            Some(values) => values.push(value),
            None => self.entries.insert(name, vec![value]),
        }
    }

    /// Appends `value` to `name` under `prefix`, as Tika's `Metadata.add(KeyPrefix, name,
    /// value)` does with names taken from a document: a blank name, or one longer than 1024
    /// UTF-16 units, is passed over, as is a new name once the metadata has 10,000.
    pub(crate) fn add_prefixed(&mut self, prefix: &str, name: &str, value: impl Into<String>) {
        if is_blank(name) || utf16_length(name) > MAX_PREFIX_ROUTE_NAME_LENGTH {
            return;
        }
        let key = format!("{prefix}{name}");
        if self.entries.get(&key).is_none() {
            if self.prefix_route_names >= MAX_PREFIX_ROUTE_NAMES {
                return;
            }
            self.prefix_route_names += 1;
        }
        self.add(&key, value);
    }

    /// The names in the order Tika's `Metadata.names()` returns them: the iteration order of a
    /// `java.util.HashMap` that saw the same insertions.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name)
    }

    /// Whether two of `names` share a bucket of Tika's `HashMap`, where it iterates them in the
    /// order they were first set.
    pub(crate) fn share_a_bucket(&self, names: &[&str]) -> bool {
        self.entries.share_a_bucket(names)
    }

    /// Every name with each of its values, in the order the names were first set.
    pub(crate) fn pairs(&self) -> Vec<(&str, &str)> {
        self.entries
            .iter_inserted()
            .flat_map(|(name, values)| values.iter().map(move |value| (name, value.as_str())))
            .collect()
    }

    /// The metadata as the crate's public [`Metadata`] map.
    pub(crate) fn to_map(&self) -> Metadata {
        self.entries
            .iter_inserted()
            .map(|(name, values)| (name.to_owned(), values.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
