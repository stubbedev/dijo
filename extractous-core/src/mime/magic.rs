//! The magic of `tika-mimetypes.xml`: Tika's `MagicMatch` clauses, combined into `and`, `or`
//! and `minShouldMatch` clauses, each evaluated as Tika's `MagicDetector` evaluates it.

use std::cell::OnceCell;

use super::java_regex::JavaRegex;

#[cfg(test)]
pub(crate) mod parse;

/// How far a regex clause reads past the start of each offset it tries.
const REGEX_WINDOW: usize = 8 * 1024;

/// The bytes clauses are evaluated over, and the same bytes as ISO-8859-1 text, decoded once for
/// every regex clause that reads them.
pub(crate) struct Haystack<'a> {
    bytes: &'a [u8],
    latin1: OnceCell<Latin1>,
}

/// Bytes as the characters U+0000 to U+00FF, with where each byte's character starts in the text
/// and, last, the text's length.
struct Latin1 {
    text: String,
    offsets: Vec<usize>,
}

impl<'a> Haystack<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Haystack {
            bytes,
            latin1: OnceCell::new(),
        }
    }

    /// The bytes from `start` to `end` as ISO-8859-1 text.
    fn text(&self, start: usize, end: usize) -> &str {
        let latin1 = self.latin1.get_or_init(|| {
            let text: String = self.bytes.iter().map(|&b| char::from(b)).collect();
            let mut offsets = Vec::with_capacity(self.bytes.len() + 1);
            offsets.extend(text.char_indices().map(|(offset, _)| offset));
            offsets.push(text.len());
            Latin1 { text, offsets }
        });
        &latin1.text[latin1.offsets[start]..latin1.offsets[end]]
    }
}

/// One `<match>`: a pattern, under a mask, at an offset or in a range of offsets. The pattern
/// and the mask are as long as `MagicDetector.getLength()`, the longer of the two as written.
#[derive(Debug, PartialEq)]
pub(crate) struct MagicMatch {
    pub(crate) pattern: &'static [u8],
    pub(crate) mask: &'static [u8],
    pub(crate) kind: Kind,
    pub(crate) begin: usize,
    pub(crate) end: usize,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Kind {
    Bytes,
    IgnoreCase,
    Regex(JavaRegex),
}

impl MagicMatch {
    /// The bytes the comparison window spans past each offset.
    fn length(&self) -> usize {
        match self.kind {
            Kind::Regex(_) => REGEX_WINDOW,
            _ => self.pattern.len(),
        }
    }

    /// `MagicDetector.matches`.
    pub(crate) fn matches(&self, haystack: &Haystack) -> bool {
        let data = haystack.bytes;
        match &self.kind {
            Kind::Regex(regex) => {
                if data.len() <= self.begin {
                    return false;
                }
                let last = self.end.min(data.len() - 1);
                let span = last - self.begin;
                let buffer_end = self.begin + (data.len() - self.begin).min(REGEX_WINDOW + span);
                (self.begin..=last).any(|start| {
                    let region_end = (start + REGEX_WINDOW).min(buffer_end);
                    start < region_end
                        && regex.may_start_with(char::from(data[start]))
                        && regex.is_match(haystack.text(start, region_end))
                })
            }
            Kind::Bytes | Kind::IgnoreCase => {
                let length = self.length();
                if data.len() < self.begin + length {
                    return false;
                }
                if length == 0 {
                    return true;
                }
                let last = self.end.min(data.len() - length);
                (self.begin..=last).any(|i| self.matches_at(&data[i..i + length]))
            }
        }
    }

    fn matches_at(&self, window: &[u8]) -> bool {
        let ignore_case = matches!(self.kind, Kind::IgnoreCase);
        window
            .iter()
            .zip(self.mask)
            .zip(self.pattern)
            .all(|((&byte, &mask), &pattern)| {
                let masked = byte & mask;
                let masked = if ignore_case && masked < 0x80 {
                    masked.to_ascii_lowercase()
                } else {
                    masked
                };
                masked == pattern
            })
    }
}

/// A clause of a `<magic>`: a match, or matches combined.
#[derive(Debug, PartialEq)]
pub(crate) enum Clause {
    Match(MagicMatch),
    /// A match and what its nested matches require.
    And(&'static [Clause]),
    Or(&'static [Clause]),
    MinShouldMatch(usize, &'static [Clause]),
}

impl Clause {
    /// Hands `visit` each regex of the clause.
    #[cfg(test)]
    pub(crate) fn for_each_regex(&self, visit: &mut impl FnMut(&JavaRegex)) {
        match self {
            Clause::Match(m) => {
                if let Kind::Regex(regex) = &m.kind {
                    visit(regex);
                }
            }
            Clause::And(clauses) | Clause::Or(clauses) | Clause::MinShouldMatch(_, clauses) => {
                for clause in *clauses {
                    clause.for_each_regex(visit);
                }
            }
        }
    }

    pub(crate) fn eval(&self, haystack: &Haystack) -> bool {
        match self {
            Clause::Match(m) => m.matches(haystack),
            Clause::And(clauses) => clauses.iter().all(|c| c.eval(haystack)),
            Clause::Or(clauses) => clauses.iter().any(|c| c.eval(haystack)),
            Clause::MinShouldMatch(min, clauses) => {
                clauses
                    .iter()
                    .filter(|c| c.eval(haystack))
                    .take(*min)
                    .count()
                    >= *min
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse::{decode_value, MatchAttributes, Read};
    use super::*;
    use crate::mime::media_type::MediaType;

    fn clause(kind: &str, offset: &str, value: &str, mask: Option<&str>) -> Read {
        MagicMatch::parse(
            &MediaType::named("application/x-test"),
            &MatchAttributes {
                kind: Some(kind),
                offset: Some(offset),
                value: Some(value),
                mask,
            },
        )
        .unwrap()
    }

    #[test]
    fn decodes_values_as_java_does() {
        assert_eq!(decode_value("\\x1F\\213", "string").unwrap(), [0x1F, 0x8B]);
        assert_eq!(decode_value("0x1f8b", "string").unwrap(), [0x1F, 0x8B]);
        assert_eq!(decode_value("12", "little16").unwrap(), [10, 0]);
        assert_eq!(
            decode_value("0xcafebabe", "big32").unwrap(),
            [0xCA, 0xFE, 0xBA, 0xBE]
        );
        assert_eq!(
            decode_value("0xcafebabe", "little32").unwrap(),
            [0xBE, 0xBA, 0xFE, 0xCA]
        );
        assert_eq!(
            decode_value("a\\tb", "string").unwrap(),
            [b'a', 0, b't', b'b']
        );
        assert_eq!(decode_value("AB", "unicodeLE").unwrap(), [b'A', 0, b'B', 0]);
    }

    #[test]
    fn matches_at_any_offset_of_a_range() {
        let m = clause("string", "0:4", "PK", None);
        assert!(m.clause.eval(&Haystack::new(b"xxxPKyy")));
        assert!(!m.clause.eval(&Haystack::new(b"xxxxxPK")));
        assert!(!m.clause.eval(&Haystack::new(b"P")));
    }

    #[test]
    fn applies_masks_and_case() {
        let m = clause("string", "0", "!BDN....SO", Some("0xFFFFFFFF00000000FFFF"));
        assert!(m.clause.eval(&Haystack::new(b"!BDNabcdSO")));
        let m = clause("stringignorecase", "0", "<HTML", None);
        assert!(m.clause.eval(&Haystack::new(b"<hTmL")));
        assert!(!m.clause.eval(&Haystack::new(b"<htm")));
    }

    #[test]
    fn matches_regexes_at_each_offset() {
        let m = clause("regex", "0:3", "BZh[1-9]", None);
        assert!(m.clause.eval(&Haystack::new(b"..BZh9")));
        assert!(!m.clause.eval(&Haystack::new(b"....BZh9")));
        assert_eq!(m.clause.size(), 8);
    }

    #[test]
    fn prints_as_java_does() {
        let a = clause("string", "0", "%PDF-", None);
        let b = clause("string", "1", "X", None);
        let and = Read::and(vec![a, Read::or(vec![b])]);
        assert_eq!(
            and.string,
            "and[application/x-test string 0 %PDF- null, or[application/x-test string 1 X null]]"
        );
    }
}
