//! Java regular expressions as `fancy-regex` patterns with Java's meaning: `.` stops at
//! Java's line terminators, `$` matches before a final one, `\s`, `\d`, `\w` and `\b` are
//! ASCII, case folding is ASCII, `^` and the edges of the haystack are the region's anchoring,
//! opaque bounds. Bytes Tika matches as ISO-8859-1 text are matched as the characters U+0000 to
//! U+00FF. The translation, which [`translate`] does when the tables are generated, fails on a
//! construct it does not know, so a table that holds one fails to generate rather than match
//! differently.

use std::sync::OnceLock;

use fancy_regex::{Regex, RegexBuilder};

#[cfg(test)]
pub(crate) mod translate;

/// A Java regex, translated when the tables were generated, and compiled when it first matches:
/// most of a table's regexes never run in one process.
#[derive(Debug)]
pub(crate) struct JavaRegex {
    pattern: &'static str,
    /// The characters a match may start with: a region that starts otherwise does not match,
    /// without the regex compiled.
    starts: Starts,
    regex: &'static OnceLock<Regex>,
}

impl PartialEq for JavaRegex {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern && self.starts == other.starts
    }
}

/// A set of the characters a match may start with: a bit for each of U+0000 to U+00FF, and
/// whether any character above them may.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Starts {
    latin1: [u128; 2],
    beyond: bool,
}

impl Starts {
    /// The characters of the inclusive `ranges` of U+0000 to U+00FF, and those above if `beyond`.
    pub(crate) const fn new(ranges: &[(u8, u8)], beyond: bool) -> Self {
        let mut latin1 = [0u128; 2];
        let mut i = 0;
        while i < ranges.len() {
            let (low, high) = ranges[i];
            let mut code = low as usize;
            while code <= high as usize {
                latin1[code >> 7] |= 1u128 << (code & 0x7F);
                code += 1;
            }
            i += 1;
        }
        Starts { latin1, beyond }
    }

    fn contains(self, c: char) -> bool {
        match u8::try_from(c) {
            Ok(code) => (self.latin1[usize::from(code >> 7)] >> (code & 0x7F)) & 1 != 0,
            Err(_) => self.beyond,
        }
    }
}

impl JavaRegex {
    /// The regex of a translated `pattern`, compiled into `regex` when it first matches.
    pub(crate) const fn new(
        pattern: &'static str,
        starts: Starts,
        regex: &'static OnceLock<Regex>,
    ) -> Self {
        JavaRegex {
            pattern,
            starts,
            regex,
        }
    }

    fn regex(&self) -> &Regex {
        self.regex.get_or_init(|| {
            RegexBuilder::new(self.pattern)
                .backtrack_limit(usize::MAX)
                .build()
                .unwrap_or_else(|error| panic!("{} does not compile: {error}", self.pattern))
        })
    }

    /// Whether a match may start with `c`, which rules out a region that starts otherwise without
    /// compiling the regex or decoding the region.
    pub(crate) fn may_start_with(&self, c: char) -> bool {
        self.starts.contains(c)
    }

    /// Whether a region matches.
    pub(crate) fn is_match(&self, region: &str) -> bool {
        if region
            .chars()
            .next()
            .is_some_and(|c| !self.may_start_with(c))
        {
            return false;
        }
        self.regex()
            .is_match(region)
            .expect("a match without a backtrack limit cannot fail")
    }

    /// The translated pattern.
    #[cfg(test)]
    pub(crate) fn pattern(&self) -> &'static str {
        self.pattern
    }

    /// The characters a match may start with.
    #[cfg(test)]
    pub(crate) fn starts(&self) -> Starts {
        self.starts
    }

    /// Compiles the regex now, as its first match would.
    #[cfg(test)]
    pub(crate) fn force(&self) {
        self.regex();
    }
}

#[cfg(test)]
mod tests {
    use super::translate::{Anchor, Text};
    use super::*;

    fn looks(pattern: &str, text: &[u8]) -> bool {
        let text: String = text.iter().map(|&b| char::from(b)).collect();
        JavaRegex::compile(pattern, Anchor::LookingAt, Text::Latin1)
            .unwrap()
            .is_match(&text)
    }

    fn fails(pattern: &str) -> bool {
        JavaRegex::compile(pattern, Anchor::LookingAt, Text::Latin1).is_err()
    }

    /// Every string of one or two Latin-1 characters, as haystacks.
    fn latin1_haystacks() -> Vec<String> {
        let latin1 = || (0u8..=0xFF).map(char::from);
        latin1()
            .map(String::from)
            .chain(latin1().flat_map(|a| latin1().map(move |b| format!("{a}{b}"))))
            .collect()
    }

    fn starts(pattern: &str) -> (Vec<(u8, u8)>, bool) {
        JavaRegex::compile(pattern, Anchor::LookingAt, Text::Latin1)
            .unwrap()
            .starts()
            .ranges()
    }

    #[test]
    fn knows_the_characters_a_match_starts_with() {
        let any = (vec![(0, 0xFF)], true);
        assert_eq!(
            starts("(?s)\\xFF[\\xF0\\xF1].{2}"),
            (vec![(0xFF, 0xFF)], false)
        );
        assert_eq!(starts("^BZh[1-9]"), (vec![(b'B', b'B')], false));
        assert_eq!(starts("\\Aowl"), (vec![(b'o', b'o')], false));
        assert_eq!(starts("a+b"), (vec![(b'a', b'a')], false));
        assert_eq!(starts("a"), (vec![(b'a', b'a')], false));
        assert_eq!(starts("a*b"), (vec![(b'a', b'b')], false));
        assert_eq!(starts("a?b"), (vec![(b'a', b'b')], false));
        assert_eq!(starts("a{0,2}b"), (vec![(b'a', b'b')], false));
        assert_eq!(starts("a{1,2}b"), (vec![(b'a', b'a')], false));
        assert_eq!(starts("ab|c"), (vec![(b'a', b'a'), (b'c', b'c')], false));
        assert_eq!(starts("(?i)ab"), (vec![(b'A', b'A'), (b'a', b'a')], false));
        assert_eq!(starts("[ab]c"), (vec![(b'a', b'b')], false));
        assert_eq!(starts("(?:a)b"), (vec![(b'a', b'a')], false));
        assert_eq!(starts("(a|)b"), (vec![(b'a', b'b')], false));
        assert_eq!(starts("(?=b)a"), (vec![(b'a', b'a')], false));
        assert_eq!(starts("(a)\\1b"), (vec![(b'a', b'a')], false));
        assert_eq!(
            starts(".a"),
            (vec![(0, 9), (11, 12), (14, 0x84), (0x86, 0xFF)], false)
        );
        assert_eq!(
            starts("[^a]"),
            (vec![(0, b'a' - 1), (b'a' + 1, 0xFF)], false)
        );
        assert_eq!(starts("\\da"), (vec![(b'0', b'9')], false));
        assert_eq!(starts("\\ba"), (vec![(b'a', b'a')], false));
        assert_eq!(starts("^"), any);
        assert_eq!(starts("a*"), any);
        assert_eq!(starts("(?=a)"), any);
        let unicode = |pattern: &str| {
            JavaRegex::compile(pattern, Anchor::LookingAt, Text::Unicode)
                .unwrap()
                .starts()
                .ranges()
        };
        assert_eq!(
            unicode("[^a]"),
            (vec![(0, b'a' - 1), (b'a' + 1, 0xFF)], true)
        );
        assert_eq!(unicode("\\u0100a"), (vec![], true));
        assert!(!looks("(?s)\\xFF.", b"\xFE\xFF"));
        assert!(looks("(?s)\\xFF.", b"\xFF\xFE"));
    }

    #[test]
    fn rules_out_only_regions_that_do_not_match() {
        let mut haystacks = latin1_haystacks();
        haystacks.extend(
            [
                "\r%AI5_FileFormat 3\n",
                "\n%AI5_FileFormat 9\n",
                "999\r\nx \n0\nSECTION\n",
                "0\r\nSECTION\r\n",
                "aab",
                "abab",
                "\u{0}\u{0}ID3",
                "%%x\n%PDF-1.",
            ]
            .map(str::to_owned),
        );
        for pattern in [
            "a*b",
            "a?b",
            "a{0,2}b",
            "ab|c",
            "(?i)ab",
            "[ab]c",
            "(a|)b",
            "(?=b)a",
            "(?!a).",
            "(?<=a)b",
            "(a)\\1b",
            "(a*)\\1b",
            ".a",
            "[^a]",
            "\\ba",
            "a$",
            "^",
            "a*",
            "(?>a|ab)b",
            "[\r\n]%AI5_FileFormat [1-4][\r\n]",
            "(999\r?\n[^\r\n]{0,64}\\s+)?0\r?\nSECTION\r?\n",
            "(?:\\x0D\\x0A|\\x00{1,1024})(?:\\xff[\\xe3\\xf2]|ID3)",
            "(?>(?:%%[^\\r\\n]{0,148})?(?:\\r\\n|[\\r\\n])){1,50}%PDF-[12]\\.",
        ] {
            let regex = JavaRegex::compile(pattern, Anchor::LookingAt, Text::Latin1).unwrap();
            for haystack in &haystacks {
                assert_eq!(
                    regex.is_match(haystack),
                    regex.regex().is_match(haystack).unwrap(),
                    "{pattern:?} on {haystack:?}"
                );
            }
        }
    }

    #[test]
    fn matches_latin1_text_as_over_all_of_unicode() {
        let haystacks = latin1_haystacks();
        for pattern in [
            ".",
            "(?s).",
            "[^a]",
            "[^\\x00]{2}",
            "\\D",
            "\\S",
            "\\W\\w",
            "(?i)[^a-c]",
            "[^\\x00-\\xFF]",
            "a$",
            "\\bb",
        ] {
            let latin1 = JavaRegex::compile(pattern, Anchor::LookingAt, Text::Latin1).unwrap();
            let unicode = JavaRegex::compile(pattern, Anchor::LookingAt, Text::Unicode).unwrap();
            for haystack in &haystacks {
                assert_eq!(
                    latin1.is_match(haystack),
                    unicode.is_match(haystack),
                    "{pattern:?} on {haystack:?}"
                );
            }
        }
    }

    #[test]
    fn dot_stops_at_java_line_terminators() {
        assert!(!looks("a.b", b"a\x85b"));
        assert!(!looks("a.b", b"a\rb"));
        assert!(looks("a.b", b"a\xFFb"));
        assert!(looks("(?s)a.b", b"a\x85b"));
        assert!(looks("(?s:a.)b", b"a\nb"));
    }

    #[test]
    fn classes_are_ascii() {
        assert!(!looks(r"\s", b"\xA0"));
        assert!(looks(r"\s", b"\x0B"));
        assert!(!looks(r"\xE9\b", b"\xE9 "));
        assert!(looks(r"a\b", b"a-"));
        assert!(looks(r"(?i)<html[ >]", b"<HTML>"));
        assert!(!looks(r"(?i)\xe9", b"\xC9"));
    }

    #[test]
    fn supports_the_table_s_fancy_constructs() {
        assert!(looks(
            r"(?s)^([\x05-\x0F])[\x00\x20-\x7E]{2}\1",
            b"\x07ab\x07"
        ));
        assert!(!looks(
            r"(?s)^([\x05-\x0F])[\x00\x20-\x7E]{2}\1",
            b"\x07ab\x08"
        ));
        assert!(looks(
            r"(?>(?:%%[^\r\n]{0,148})?(?:\r\n|[\r\n])){1,50}%PDF-[12]\.",
            b"%%x\n%PDF-1."
        ));
        assert!(looks(r"(?s)^.{3}(?<=[\x00][^\x00]{0,10})[A-Z]", b"\x00abC"));
        assert!(looks(
            r"JAVA PROFILE \d\.\d\.\d\u0000",
            b"JAVA PROFILE 1.0.2\x00"
        ));
    }

    #[test]
    fn dollar_matches_before_a_final_line_terminator() {
        assert!(looks("ab$", b"ab"));
        assert!(looks("ab$", b"ab\n"));
        assert!(looks("ab$", b"ab\r\n"));
        assert!(!looks("ab$", b"ab\n\n"));
    }

    #[test]
    fn rejects_what_it_does_not_translate() {
        assert!(fails(r"\p{L}"));
        assert!(fails(r"a*+"));
        assert!(fails(r"[a&&b]"));
        assert!(fails(r"(?i)(a)\1"));
    }

    #[test]
    fn matches_unicode_names_whole() {
        let glob = JavaRegex::compile(r"\Ai_.*\.txt\z", Anchor::Matches, Text::Unicode).unwrap();
        assert!(glob.is_match("i_é.txt"));
        assert!(!glob.is_match("i_\u{2028}.txt"));
        let name = JavaRegex::compile("^rdf$", Anchor::Matches, Text::Unicode).unwrap();
        assert!(name.is_match("rdf"));
        assert!(!name.is_match("rdf\n"));
        assert!(!name.is_match("rdfx"));
    }
}
