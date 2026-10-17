//! Tika's `Metadata`: names with one or more values. Tika keeps them in a `java.util.HashMap`,
//! whose iteration order its XHTML head shows; [`TikaMetadata::names`] lists them sorted by name
//! instead, so the head is written in a defined order (#64).

use std::collections::HashMap;

use crate::java::{is_blank, utf16_length};
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
    pub(crate) const HAS_HIDDEN_SLIDES: &str = "msoffice:ppt:has-hidden-slides";
    pub(crate) const NUM_HIDDEN_SLIDES: &str = "msoffice:ppt:num-hidden-slides";
    pub(crate) const HAS_ANIMATIONS: &str = "msoffice:ppt:has-animations";
}

/// The names of Tika's `TikaPagedText` properties.
pub(crate) mod paged {
    pub(crate) const PAGE_NUMBER: &str = "tk:page:number";
    pub(crate) const PAGE_NUMBERS: &str = "tk:page:numbers";
}

/// `PageAnchoring.applyPageMetadata`: the pages a resource is on in order, -1 for none, and the
/// page when there is one, in place of any the metadata held.
pub(crate) fn apply_page_numbers(metadata: &mut TikaMetadata, pages: &[i32]) {
    let mut sorted = pages.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        sorted.push(-1);
    }
    metadata.remove(paged::PAGE_NUMBERS);
    metadata.remove(paged::PAGE_NUMBER);
    for page in &sorted {
        metadata.add(paged::PAGE_NUMBERS, page.to_string());
    }
    if let [page] = sorted[..] {
        if page != -1 {
            metadata.set(paged::PAGE_NUMBER, page.to_string());
        }
    }
}

/// The longest name `Metadata.add(KeyPrefix, name, value)` takes, in UTF-16 units.
const MAX_PREFIX_ROUTE_NAME_LENGTH: usize = 1024;
/// The most names `Metadata.add(KeyPrefix, name, value)` adds to one metadata.
const MAX_PREFIX_ROUTE_NAMES: usize = 10_000;

/// A document's metadata, its names in the order they were first set.
#[derive(Debug, Clone, Default)]
pub(crate) struct TikaMetadata {
    entries: Vec<(String, Vec<String>)>,
    /// Where each name is in `entries`.
    index: HashMap<String, usize>,
    /// The names added under a prefix so far, which Tika counts per metadata.
    prefix_route_names: usize,
}

impl PartialEq for TikaMetadata {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .all(|(name, values)| other.values(name) == values.as_slice())
    }
}

impl Eq for TikaMetadata {}

impl TikaMetadata {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn get_values(&self, name: &str) -> Option<&Vec<String>> {
        self.index.get(name).map(|&at| &self.entries[at].1)
    }

    /// Replaces the values of a name already present, which keeps its place, or adds it last.
    fn insert(&mut self, name: &str, values: Vec<String>) {
        match self.index.get(name) {
            Some(&at) => self.entries[at].1 = values,
            None => {
                self.index.insert(name.to_owned(), self.entries.len());
                self.entries.push((name.to_owned(), values));
            }
        }
    }

    /// The first value of `name`, as Tika's `Metadata.get`.
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.get_values(name)
            .and_then(|values| values.first())
            .map(String::as_str)
    }

    /// Every value of `name`, as Tika's `Metadata.getValues`.
    pub(crate) fn values(&self, name: &str) -> &[String] {
        self.get_values(name).map_or(&[], Vec::as_slice)
    }

    /// Replaces the values of `name` with `value`, as Tika's `Metadata.set`.
    pub(crate) fn set(&mut self, name: &str, value: impl Into<String>) {
        self.insert(name, vec![value.into()]);
    }

    /// Replaces the values of `name` with `values`, as Tika's `Metadata.set` with an array.
    pub(crate) fn set_values(&mut self, name: &str, values: Vec<String>) {
        self.insert(name, values);
    }

    /// Replaces the values of `name` with `values` as Tika's `Metadata.set` does for a
    /// `Property` with an array: it removes the name and adds it again, last, and leaves it out
    /// when `values` is empty.
    pub(crate) fn reset_values(&mut self, name: &str, values: Vec<String>) {
        self.remove(name);
        if !values.is_empty() {
            self.insert(name, values);
        }
    }

    /// Removes `name`, as Tika's `Metadata.set` with a null value does.
    pub(crate) fn remove(&mut self, name: &str) {
        let Some(at) = self.index.remove(name) else {
            return;
        };
        self.entries.remove(at);
        for position in self.index.values_mut() {
            if *position > at {
                *position -= 1;
            }
        }
    }

    /// Appends `value` to the values of `name`, as Tika's `Metadata.add`.
    pub(crate) fn add(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.index.get(name) {
            Some(&at) => self.entries[at].1.push(value),
            None => self.insert(name, vec![value]),
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
        if self.get_values(&key).is_none() {
            if self.prefix_route_names >= MAX_PREFIX_ROUTE_NAMES {
                return;
            }
            self.prefix_route_names += 1;
        }
        self.add(&key, value);
    }

    /// The names sorted by name, as the XHTML head lists them. Tika's `Metadata.names()` returns
    /// them in the iteration order of its `java.util.HashMap`, which follows the names' hash
    /// codes and the map's size (#64).
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        let mut names: Vec<&str> = self.entries.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        names.into_iter()
    }

    /// Every name with each of its values, in the order the names were first set.
    #[cfg(any(test, feature = "tika"))]
    pub(crate) fn pairs(&self) -> Vec<(&str, &str)> {
        self.entries
            .iter()
            .flat_map(|(name, values)| {
                values
                    .iter()
                    .map(move |value| (name.as_str(), value.as_str()))
            })
            .collect()
    }

    /// The metadata as the crate's public [`Metadata`] map.
    pub(crate) fn to_map(&self) -> Metadata {
        self.entries.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_names_sorted_by_name() {
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
                "Content-Length",
                "Content-Type",
                "dcterms:created",
                "tk:content-type-magic-detected",
                "tk:parsed-by",
                "tk:resource-name",
            ]
        );
    }

    #[test]
    fn resetting_values_moves_a_name_last_among_the_pairs() {
        let mut metadata = TikaMetadata::new();
        metadata.set("a", "1");
        metadata.set("q", "1");
        metadata.set_values("a", vec!["2".to_owned()]);
        assert_eq!(metadata.pairs(), [("a", "2"), ("q", "1")]);
        metadata.reset_values("a", vec!["3".to_owned(), "4".to_owned()]);
        assert_eq!(metadata.pairs(), [("q", "1"), ("a", "3"), ("a", "4")]);
        assert_eq!(metadata.values("a"), ["3", "4"]);
        metadata.reset_values("q", Vec::new());
        assert_eq!(metadata.names().collect::<Vec<_>>(), ["a"]);
        assert_eq!(metadata.get("q"), None);
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
