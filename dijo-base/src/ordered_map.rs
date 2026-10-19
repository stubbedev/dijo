//! A map from strings that iterates its entries in the order their keys were first inserted, for
//! readers whose output lists entries as the document gives them.

use std::collections::HashMap;

/// Entries by string key, iterated in the order each key was first inserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedMap<V> {
    entries: Vec<(String, V)>,
    index: HashMap<String, usize>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<V> OrderedMap<V> {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.index.get(key).map(|&at| &self.entries[at].1)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.index.get(key).map(|&at| &mut self.entries[at].1)
    }

    /// Replaces the value of a key already present, which keeps its place, or adds the key last.
    pub fn insert(&mut self, key: &str, value: V) {
        match self.get_mut(key) {
            Some(present) => *present = value,
            None => self.push(key, value),
        }
    }

    /// Adds the key last with `value` where it is not present yet.
    pub fn insert_if_absent(&mut self, key: &str, value: V) {
        if !self.contains_key(key) {
            self.push(key, value);
        }
    }

    /// The entries in the order their keys were first inserted.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    fn push(&mut self, key: &str, value: V) {
        self.index.insert(key.to_owned(), self.entries.len());
        self.entries.push((key.to_owned(), value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iterates_in_the_order_keys_were_first_inserted() {
        let mut map = OrderedMap::default();
        for key in ["key7", "key0", "key217", "key14"] {
            map.insert(key, 1);
        }
        map.insert("key0", 2);
        map.insert_if_absent("key7", 3);
        map.insert_if_absent("key5", 4);
        assert_eq!(
            map.iter().collect::<Vec<_>>(),
            [
                ("key7", &1),
                ("key0", &2),
                ("key217", &1),
                ("key14", &1),
                ("key5", &4)
            ]
        );
        assert_eq!(map.len(), 5);
        assert_eq!(map.get("key217"), Some(&1));
        assert!(!map.contains_key("key1"));
    }
}
