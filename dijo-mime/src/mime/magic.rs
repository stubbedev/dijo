//! The magic of `tika-mimetypes.xml`: Tika's `MagicMatch` clauses, combined into `and`, `or`
//! and `minShouldMatch` clauses, each evaluated as Tika's `MagicDetector` evaluates it.

use std::cell::RefCell;
use std::sync::OnceLock;

use memchr::memmem::Finder;

use super::java_regex::JavaRegex;

#[cfg(test)]
pub(crate) mod parse;

/// How far a regex clause reads past the start of each offset it tries.
const REGEX_WINDOW: usize = 8 * 1024;

/// How many bytes [`Latin1`] decodes at a time, counting those from 0x80 up.
const BLOCK: usize = 64;

/// The bytes clauses are evaluated over, and the same bytes as ISO-8859-1 text, decoded once for
/// every regex clause that reads them, as far as the furthest region read yet.
pub(crate) struct Haystack<'a> {
    bytes: &'a [u8],
    latin1: RefCell<Latin1>,
}

/// The first blocks of the bytes as the characters U+0000 to U+00FF, and how many of the bytes
/// before each block encode as two UTF-8 bytes, which places each byte's character in the text.
#[derive(Default)]
struct Latin1 {
    text: String,
    wide_before: Vec<usize>,
    wide: usize,
}

impl Latin1 {
    /// How many of the bytes are decoded.
    fn decoded(&self, bytes: &[u8]) -> usize {
        (self.wide_before.len() * BLOCK).min(bytes.len())
    }

    /// Decodes the blocks of `bytes` up to the one holding byte `end - 1`.
    fn decode_to(&mut self, bytes: &[u8], end: usize) {
        let from = self.decoded(bytes);
        if end <= from {
            return;
        }
        let to = end.div_ceil(BLOCK) * BLOCK;
        let more = &bytes[from..to.min(bytes.len())];
        self.text.reserve(2 * more.len());
        for block in more.chunks(BLOCK) {
            self.wide_before.push(self.wide);
            if block.is_ascii() {
                self.text
                    .push_str(std::str::from_utf8(block).expect("ASCII is UTF-8"));
                continue;
            }
            for &byte in block {
                self.text.push(char::from(byte));
                self.wide += usize::from(byte >= 0x80);
            }
        }
    }

    /// Where the character of byte `index` of `bytes`, decoded up to there, starts in the text.
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
            latin1: RefCell::default(),
        }
    }

    /// `read` of the bytes from `start` to `end` as ISO-8859-1 text.
    fn with_text<R>(&self, start: usize, end: usize, read: impl FnOnce(&str) -> R) -> R {
        let mut latin1 = self.latin1.borrow_mut();
        latin1.decode_to(self.bytes, end);
        let text = &latin1.text[latin1.offset(self.bytes, start)..latin1.offset(self.bytes, end)];
        read(text)
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
    /// What a window that matches holds, for a range of offsets of bytes compared whole.
    pub(crate) anchor: Option<Anchor>,
}

/// What a window that matches must hold at a place in it, which finds the candidate windows of
/// a range: where case counts, the longest run of bytes compared whole; where case is ignored,
/// the first byte compared whole, in either case if it is a letter.
#[derive(Debug, PartialEq)]
pub(crate) enum Anchor {
    Run {
        offset: usize,
        run: Run,
    },
    Byte {
        offset: usize,
        byte: u8,
    },
    #[cfg_attr(not(test), expect(dead_code))]
    EitherByte {
        offset: usize,
        bytes: [u8; 2],
    },
}

/// A run of bytes to search for, with its searcher built when it is first searched for.
#[derive(Debug)]
pub(crate) struct Run {
    bytes: &'static [u8],
    finder: &'static OnceLock<Finder<'static>>,
}

impl PartialEq for Run {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Run {
    /// The run of `bytes`, its searcher built into `finder` when it is first searched for.
    pub(crate) const fn new(
        bytes: &'static [u8],
        finder: &'static OnceLock<Finder<'static>>,
    ) -> Self {
        Run { bytes, finder }
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> &'static [u8] {
        self.bytes
    }

    fn finder(&self) -> &Finder<'static> {
        self.finder.get_or_init(|| Finder::new(self.bytes))
    }
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
                        && haystack.with_text(start, region_end, |text| regex.is_match(text))
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
                if self.begin == last {
                    return at(last);
                }
                match &self.anchor {
                    Some(Anchor::Run { offset, run }) => {
                        let window = &data[self.begin + offset..last + offset + run.bytes.len()];
                        let finder = run.finder();
                        let mut from = 0;
                        while let Some(found) = finder.find(&window[from..]) {
                            if at(self.begin + from + found) {
                                return true;
                            }
                            from += found + 1;
                        }
                        false
                    }
                    Some(Anchor::Byte { offset, byte }) => {
                        let window = &data[self.begin + offset..=last + offset];
                        memchr::memchr_iter(*byte, window).any(|i| at(self.begin + i))
                    }
                    Some(Anchor::EitherByte { offset, bytes }) => {
                        let window = &data[self.begin + offset..=last + offset];
                        memchr::memchr2_iter(bytes[0], bytes[1], window).any(|i| at(self.begin + i))
                    }
                    None => (self.begin..=last).any(at),
                }
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
    /// Hands `visit` each match of the clause.
    #[cfg(test)]
    pub(crate) fn for_each_match(&self, visit: &mut impl FnMut(&MagicMatch)) {
        match self {
            Clause::Match(m) => visit(m),
            Clause::And(clauses) | Clause::Or(clauses) | Clause::MinShouldMatch(_, clauses) => {
                for clause in *clauses {
                    clause.for_each_match(visit);
                }
            }
        }
    }

    /// Hands `visit` each regex of the clause.
    #[cfg(test)]
    pub(crate) fn for_each_regex(&self, visit: &mut impl FnMut(&JavaRegex)) {
        self.for_each_match(&mut |m| {
            if let Kind::Regex(regex) = &m.kind {
                visit(regex);
            }
        });
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
        let bytes: Vec<u8> = (0..3 * REGEX_WINDOW + 17)
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
            (100, REGEX_WINDOW),
            (10, 20),
            (REGEX_WINDOW - 1, REGEX_WINDOW + 1),
            (5000, bytes.len()),
            (64, 128),
            (bytes.len(), bytes.len()),
        ] {
            let text = haystack.with_text(start, end, str::to_owned);
            assert_eq!(text, latin1(start, end), "{start}..{end}");
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
        let m = clause("string", "0:4", "aa\\x00c", Some("0xFFFF00FF"));
        assert!(m.clause.eval(&Haystack::new(b"aaaxc")));
        assert!(!m.clause.eval(&Haystack::new(b"aaaxd")));
        let m = clause("stringignorecase", "0:8", "x<html", Some("0x00FFFFFFFFFF"));
        assert!(m.clause.eval(&Haystack::new(b"....?<HTml")));
    }

    #[test]
    fn anchors_find_what_every_offset_finds() {
        let matches = [
            clause("string", "0:16", "PK", None),
            clause("string", "2:9", "\\nFrom: ", None),
            clause("string", "0:12", "aab", None),
            clause("string", "1:20", "x", None),
            clause("string", "0:8", "AB", Some("0xDFFF")),
            clause("string", "0:10", "aa\\x00c", Some("0xFFFF00FF")),
            clause("string", "0:10", "a\\x00bc\\x00", Some("0xFF00FFFF00")),
            clause("string", "0:10", "ab", Some("0x0000")),
            clause("string", "3:3", "ab", None),
            clause("stringignorecase", "0:12", "html", None),
            clause("stringignorecase", "0:12", "<html", None),
            clause("stringignorecase", "0:12", "x<html", Some("0x00FFFFFFFFFF")),
            clause("stringignorecase", "0:12", "ab", Some("0x0000")),
        ];
        let inputs: [&[u8]; 14] = [
            b"",
            b"P",
            b"PK",
            b"xxxPKyyPK",
            b"aaab aab aaab",
            b"....aB....ab",
            b"aaaxcaa\0c",
            b"a\0bc\0a1bc2abc",
            b"<!-- <HtMl <hTML",
            b"....?<HTml x<html",
            b"\n\nFrom: me\nFrom: you",
            b"xxxxxxxxxxxxxxxxxxxxxxxx",
            b"ab",
            b"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdab",
        ];
        for read in &matches {
            let Clause::Match(m) = &read.clause else {
                unreachable!()
            };
            let length = m.length();
            for input in inputs {
                for cut in 0..=input.len() {
                    let data = &input[..cut];
                    let every = data.len() >= m.begin + length
                        && (length == 0
                            || (m.begin..=m.end.min(data.len() - length))
                                .any(|i| m.matches_at(&data[i..i + length])));
                    assert_eq!(
                        m.matches(&Haystack::new(data)),
                        every,
                        "{} on {data:?}",
                        read.string
                    );
                }
            }
        }
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
