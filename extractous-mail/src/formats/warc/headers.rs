//! jwarc's `MessageHeaders`: a `TreeMap` with `String.CASE_INSENSITIVE_ORDER`, each name kept as
//! first spelled, with its values in the order they came.

use crate::java;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Headers {
    entries: Vec<(String, Vec<String>)>,
}

/// What `MessageHeaders.sole` throws on a header given more than once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NotSole(pub(super) String);

impl Headers {
    fn position(&self, name: &str) -> Result<usize, usize> {
        self.entries
            .binary_search_by(|(existing, _)| java::compare_ignore_case(existing, name))
    }

    /// `computeIfAbsent(name, …).add(value)`.
    pub(super) fn add(&mut self, name: String, value: String) {
        match self.position(&name) {
            Ok(at) => self.entries[at].1.push(value),
            Err(at) => self.entries.insert(at, (name, vec![value])),
        }
    }

    /// `put(name, [value])`: a name already present keeps its first spelling.
    pub(super) fn set(&mut self, name: &str, value: String) {
        match self.position(name) {
            Ok(at) => self.entries[at].1 = vec![value],
            Err(at) => self.entries.insert(at, (name.to_owned(), vec![value])),
        }
    }

    /// `all(name)`.
    pub(super) fn all(&self, name: &str) -> &[String] {
        self.position(name)
            .map_or(&[], |at| self.entries[at].1.as_slice())
    }

    /// `first(name)`.
    pub(super) fn first(&self, name: &str) -> Option<&str> {
        self.all(name).first().map(String::as_str)
    }

    /// `sole(name)`: the value of a header given at most once.
    pub(super) fn sole(&self, name: &str) -> Result<Option<&str>, NotSole> {
        match self.all(name) {
            [] => Ok(None),
            [value] => Ok(Some(value)),
            values => Err(NotSole(format!(
                "record has {} {name} headers",
                values.len()
            ))),
        }
    }

    /// `contains(name, value)`: whether a value of the header, split at commas and the spaces
    /// and tabs around them, is `value` ignoring case.
    pub(super) fn contains(&self, name: &str, value: &str) -> bool {
        self.all(name)
            .iter()
            .flat_map(|raw| split_at_commas(raw))
            .any(|part| java::equals_ignore_case(part, value))
    }

    /// The names with their values, in the map's order.
    pub(super) fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.entries
            .iter()
            .map(|(name, values)| (name.as_str(), values.as_slice()))
    }
}

/// `Pattern.compile("[ \t]*,[ \t]*").split(raw)`: the parts between commas, without the spaces
/// and tabs around each comma, and without the empty parts at the end.
fn split_at_commas(raw: &str) -> Vec<&str> {
    if !raw.contains(',') {
        return vec![raw];
    }
    let count = raw.split(',').count();
    let mut parts: Vec<&str> = raw
        .split(',')
        .enumerate()
        .map(|(index, part)| {
            let part = if index > 0 {
                part.trim_start_matches([' ', '\t'])
            } else {
                part
            };
            if index + 1 < count {
                part.trim_end_matches([' ', '\t'])
            } else {
                part
            }
        })
        .collect();
    while parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_names_as_a_case_insensitive_tree_map_does() {
        let mut headers = Headers::default();
        headers.add("b".into(), "1".into());
        headers.add("A".into(), "2".into());
        headers.add("B".into(), "3".into());
        headers.set("a", "4".into());
        let entries: Vec<_> = headers.iter().collect();
        assert_eq!(
            entries,
            [
                ("A", ["4".to_owned()].as_slice()),
                ("b", ["1".to_owned(), "3".to_owned()].as_slice())
            ]
        );
        assert!(headers.sole("B").is_err());
        assert_eq!(headers.sole("c"), Ok(None));
    }

    #[test]
    fn splits_at_commas_as_java_does() {
        assert_eq!(split_at_commas("gzip , chunked"), ["gzip", "chunked"]);
        assert_eq!(split_at_commas(" chunked ,"), [" chunked"]);
        assert_eq!(split_at_commas(""), [""]);
        assert!(split_at_commas(",").is_empty());
    }
}
