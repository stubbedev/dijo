//! Tika's `Metadata`: names with one or more values, which Tika keeps in a `java.util.HashMap`.
//! The order its `names()` lists them in is visible in the XHTML head, so [`TikaMetadata::names`]
//! reproduces that map's iteration order.

use crate::formats::java::JavaHashMap;
use crate::Metadata;

/// A document's metadata, in the order Tika's `HashMap` holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TikaMetadata {
    entries: JavaHashMap<Vec<String>>,
}

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

    /// Appends `value` to the values of `name`, as Tika's `Metadata.add`.
    pub(crate) fn add(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self.entries.get_mut(name) {
            Some(values) => values.push(value),
            None => self.entries.insert(name, vec![value]),
        }
    }

    /// The names in the order Tika's `Metadata.names()` returns them: the iteration order of a
    /// `java.util.HashMap` that saw the same insertions.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name)
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
