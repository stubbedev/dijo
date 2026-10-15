//! jsoup's named character references (`Entities`): the base set, which a reference may use
//! without its semicolon and as a prefix of other letters, and the full set, which needs the
//! semicolon. The tables are jsoup's own (`EntitiesData`), which `just html-reference` writes to
//! `models/jsoup-entities.txt`.

use std::collections::HashMap;
use std::sync::LazyLock;

const DATA: &str = include_str!("../../../models/jsoup-entities.txt");
const BASE_COUNT: usize = 106;
const FULL_COUNT: usize = 2125;

struct Entities {
    /// The base set's names, in jsoup's (sorted) order.
    base: Vec<&'static str>,
    /// The base set's names, longest first, for prefix matching.
    base_longest_first: Vec<&'static str>,
    /// The full set's names, in jsoup's (sorted) order, with their code points.
    full: Vec<(&'static str, u32)>,
    /// The references to two code points.
    multipoints: HashMap<&'static str, [u32; 2]>,
}

static ENTITIES: LazyLock<Entities> = LazyLock::new(|| {
    let mut lines = DATA.lines();
    let mut multipoints = HashMap::new();
    let base: Vec<&str> = load(lines.next(), BASE_COUNT, &mut multipoints)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let full = load(lines.next(), FULL_COUNT, &mut multipoints);
    let mut base_longest_first = base.clone();
    base_longest_first.sort_by_key(|name| std::cmp::Reverse(name.len()));
    Entities {
        base,
        base_longest_first,
        full,
        multipoints,
    }
});

/// Reads one of jsoup's packed tables: `name=cp1[,cp2];index&`, numbers in base 36.
fn load(
    line: Option<&'static str>,
    count: usize,
    multipoints: &mut HashMap<&'static str, [u32; 2]>,
) -> Vec<(&'static str, u32)> {
    let line = line.unwrap_or_else(|| panic!("jsoup-entities.txt lacks a table"));
    let number = |text: &str| {
        u32::from_str_radix(text, 36)
            .unwrap_or_else(|e| panic!("jsoup-entities.txt has a bad number {text:?}: {e}"))
    };
    let entries: Vec<(&str, u32)> = line
        .split('&')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let (name, rest) = entry
                .split_once('=')
                .unwrap_or_else(|| panic!("jsoup-entities.txt has a bad entry {entry:?}"));
            let (points, _index) = rest
                .split_once(';')
                .unwrap_or_else(|| panic!("jsoup-entities.txt has a bad entry {entry:?}"));
            let first = match points.split_once(',') {
                Some((first, second)) => {
                    multipoints.insert(name, [number(first), number(second)]);
                    number(first)
                }
                None => number(points),
            };
            (name, first)
        })
        .collect();
    assert_eq!(
        entries.len(),
        count,
        "jsoup-entities.txt has a table of the wrong size"
    );
    assert!(
        entries.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "jsoup-entities.txt has a table out of order, which jsoup's binary search needs"
    );
    entries
}

/// Whether `name` is in the base set (`isBaseNamedEntity`).
pub(super) fn is_base(name: &str) -> bool {
    ENTITIES.base.binary_search(&name).is_ok()
}

/// Whether `name` is in the full set (`isNamedEntity`).
pub(super) fn is_named(name: &str) -> bool {
    ENTITIES
        .full
        .binary_search_by(|(candidate, _)| candidate.cmp(&name))
        .is_ok()
}

/// The longest base-set name `input` starts with, or "" (`findPrefix`).
pub(super) fn find_prefix(input: &str) -> &'static str {
    ENTITIES
        .base_longest_first
        .iter()
        .find(|name| input.starts_with(**name))
        .copied()
        .unwrap_or("")
}

/// The code points a name in the full set stands for (`codepointsForName`).
pub(super) fn codepoints(name: &str) -> Option<Vec<u32>> {
    if let Some(points) = ENTITIES.multipoints.get(name) {
        return Some(points.to_vec());
    }
    let index = ENTITIES
        .full
        .binary_search_by(|(candidate, _)| candidate.cmp(&name))
        .ok()?;
    Some(vec![ENTITIES.full[index].1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_jsoups_tables() {
        assert!(is_base("amp") && is_base("AElig") && !is_base("notin"));
        assert!(is_named("notin") && is_named("amp") && !is_named("amp;"));
        assert_eq!(find_prefix("notit"), "not");
        assert_eq!(find_prefix("xyz"), "");
        assert_eq!(codepoints("amp"), Some(vec![0x26]));
        assert_eq!(codepoints("acE"), Some(vec![0x223E, 0x333]));
        assert_eq!(codepoints("nonesuch"), None);
    }
}
