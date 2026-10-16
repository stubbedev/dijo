//! The magic of `tika-mimetypes.xml`: Tika's `MagicMatch` clauses, combined into `and`, `or`
//! and `minShouldMatch` clauses, each evaluated as Tika's `MagicDetector` evaluates it.

use std::cell::OnceCell;

use super::java_regex::JavaRegex;

#[cfg(test)]
pub(crate) mod parse;

/// How far a regex clause reads past the start of each offset it tries.
const REGEX_WINDOW: usize = 8 * 1024;

/// How many bytes of the haystack are decoded first: as far as the regexes of the tables read,
/// which start within the first 8 KiB.
const DECODED_FIRST: usize = 2 * REGEX_WINDOW;

/// How many bytes [`Latin1`] counts the bytes from 0x80 up in at a time.
const BLOCK: usize = 64;

/// The bytes clauses are evaluated over, and the same bytes as ISO-8859-1 text, decoded once for
/// every regex clause that reads them: the first [`DECODED_FIRST`] bytes, and all of them only
/// when a region reaches past those.
pub(crate) struct Haystack<'a> {
    bytes: &'a [u8],
    first: OnceCell<Latin1>,
    whole: OnceCell<Latin1>,
}

/// Bytes as the characters U+0000 to U+00FF, and how many of the bytes before each [`BLOCK`]
/// encode as two UTF-8 bytes, which places each byte's character in the text.
struct Latin1 {
    text: String,
    wide_before: Vec<usize>,
}

impl Latin1 {
    fn decode(bytes: &[u8]) -> Latin1 {
        let mut utf8 = Vec::with_capacity(2 * bytes.len());
        let mut wide_before = Vec::with_capacity(bytes.len() / BLOCK + 1);
        let mut wide = 0;
        for block in bytes.chunks(BLOCK) {
            wide_before.push(wide);
            if block.is_ascii() {
                utf8.extend_from_slice(block);
                continue;
            }
            for &byte in block {
                if byte < 0x80 {
                    utf8.push(byte);
                } else {
                    utf8.extend_from_slice(&[0xC0 | byte >> 6, 0x80 | byte & 0x3F]);
                    wide += 1;
                }
            }
        }
        let text = String::from_utf8(utf8).expect("Latin-1 encodes as UTF-8");
        Latin1 { text, wide_before }
    }

    /// Where the character of byte `index` of `bytes`, the bytes decoded, starts in the text.
    fn offset(&self, bytes: &[u8], index: usize) -> usize {
        let block = index / BLOCK;
        let Some(&wide) = self.wide_before.get(block) else {
            return self.text.len();
        };
        let within = bytes[block * BLOCK..index]
            .iter()
            .filter(|&&byte| byte >= 0x80)
            .count();
        index + wide + within
    }
}

impl<'a> Haystack<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Haystack {
            bytes,
            first: OnceCell::new(),
            whole: OnceCell::new(),
        }
    }

    /// The bytes from `start` to `end` as ISO-8859-1 text.
    fn text(&self, start: usize, end: usize) -> &str {
        let (bytes, cell) = if end <= DECODED_FIRST {
            (
                &self.bytes[..self.bytes.len().min(DECODED_FIRST)],
                &self.first,
            )
        } else {
            (self.bytes, &self.whole)
        };
        let latin1 = cell.get_or_init(|| Latin1::decode(bytes));
        &latin1.text[latin1.offset(bytes, start)..latin1.offset(bytes, end)]
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
                let at = |i: usize| self.matches_at(&data[i..i + length]);
                let starts = &data[self.begin..=last];
                match self.first_bytes() {
                    Some((only, None)) => {
                        memchr::memchr_iter(only, starts).any(|i| at(self.begin + i))
                    }
                    Some((one, Some(other))) => {
                        memchr::memchr2_iter(one, other, starts).any(|i| at(self.begin + i))
                    }
                    None => (self.begin..=last).any(at),
                }
            }
        }
    }

    /// The one or two bytes a window that matches can start with, where the first byte is
    /// compared whole: the pattern's, and its upper case where case is ignored.
    fn first_bytes(&self) -> Option<(u8, Option<u8>)> {
        let (&first, &mask) = (self.pattern.first()?, self.mask.first()?);
        if mask != 0xFF {
            return None;
        }
        match self.kind {
            Kind::Bytes => Some((first, None)),
            Kind::IgnoreCase if first.is_ascii_lowercase() => {
                Some((first, Some(first.to_ascii_uppercase())))
            }
            Kind::IgnoreCase if !first.is_ascii_uppercase() => Some((first, None)),
            Kind::IgnoreCase | Kind::Regex(_) => None,
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
    fn decodes_any_region_as_latin1() {
        let bytes: Vec<u8> = (0..3 * DECODED_FIRST + 17)
            .map(|i| match (i / 300) % 3 {
                0 => b'a' + (i % 26) as u8,
                _ => (i * 7 % 256) as u8,
            })
            .collect();
        let haystack = Haystack::new(&bytes);
        let latin1 = |start: usize, end: usize| -> String {
            bytes[start..end].iter().map(|&b| char::from(b)).collect()
        };
        for (start, end) in [
            (0, 0),
            (0, 1),
            (63, 65),
            (100, DECODED_FIRST),
            (DECODED_FIRST - 1, DECODED_FIRST + 1),
            (5000, bytes.len()),
            (bytes.len(), bytes.len()),
        ] {
            assert_eq!(
                haystack.text(start, end),
                latin1(start, end),
                "{start}..{end}"
            );
        }
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
        let m = clause("stringignorecase", "0:8", "html", None);
        assert!(m.clause.eval(&Haystack::new(b"<!-- <HtMl")));
        assert!(m.clause.eval(&Haystack::new(b"<!-- <hTML")));
        assert!(!m.clause.eval(&Haystack::new(b"<!-- -- <html")));
        let m = clause("string", "0:8", "AB", Some("0xDFFF"));
        assert!(m.clause.eval(&Haystack::new(b"....aB")));
        assert!(!m.clause.eval(&Haystack::new(b"....aC")));
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
