//! The Java string semantics Tika's detection leans on: UTF-16 ordering and lengths, `trim`,
//! case mapping, the regex `\s` class, and the iteration order of a small `HashSet`.

use std::cmp::Ordering;

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
pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Whether Java's regex `\s` (without `UNICODE_CHARACTER_CLASS`) matches `c`.
pub(crate) fn is_regex_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r')
}

/// Whether `c` ends a line for Java's regex `.` and `$`.
pub(crate) fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Java's `String.trim()`: every character up to U+0020 removed from both ends.
pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches(|c: char| c <= ' ')
}

/// Java's `String.toLowerCase(Locale.ROOT)`, which `Locale.ENGLISH` matches.
pub(crate) fn to_lower_case(text: &str) -> String {
    text.to_lowercase()
}

/// Java's `String.toUpperCase(Locale.ROOT)`.
pub(crate) fn to_upper_case(text: &str) -> String {
    text.to_uppercase()
}

/// Java's `String.hashCode()`.
pub(crate) fn hash_code(text: &str) -> i32 {
    text.encode_utf16().fold(0i32, |h, unit| {
        h.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
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
    let mut capacity = 16usize;
    while unique.len() * 4 > capacity * 3 {
        capacity *= 2;
    }
    let bucket = |item: &str| {
        let h = hash_code(item) as u32;
        ((h ^ (h >> 16)) as usize) & (capacity - 1)
    };
    unique.sort_by_key(|item| bucket(item));
    unique
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_as_java_does() {
        assert_eq!(compare("\u{FFFF}", "\u{1F600}"), Ordering::Greater);
        assert_eq!(compare("a", "b"), Ordering::Less);
        assert_eq!(utf16_len("a\u{1F600}"), 3);
    }

    #[test]
    fn hashes_as_java_does() {
        assert_eq!(hash_code(""), 0);
        assert_eq!(hash_code("hello"), 99_162_322);
    }

    #[test]
    fn iterates_a_hash_set_as_java_does() {
        assert_eq!(
            hash_set_order(["presentation.apxl", "index.xml", "index.apxl"]),
            ["index.apxl", "index.xml", "presentation.apxl"]
        );
    }

    #[test]
    fn trims_as_java_does() {
        assert_eq!(trim("\u{0}\t a b \u{1F}"), "a b");
        assert_eq!(trim("\u{A0}a"), "\u{A0}a");
    }
}
