//! Java regular expressions as `fancy-regex` patterns with Java's meaning: `.` stops at
//! Java's line terminators, `$` matches before a final one, `\s`, `\d`, `\w` and `\b` are
//! ASCII, case folding is ASCII, `^` and the edges of the haystack are the region's anchoring,
//! opaque bounds. Bytes Tika matches as ISO-8859-1 text are matched as the characters U+0000 to
//! U+00FF. A construct the translation does not know fails it, so a table that holds one fails
//! to load rather than match differently.

use fancy_regex::{Regex, RegexBuilder};

/// How much of the haystack a match must cover: a prefix, as `Matcher.lookingAt` over a region
/// does, or all of it, as `Matcher.matches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    LookingAt,
    Matches,
}

/// A compiled Java regex.
#[derive(Debug)]
pub(crate) struct JavaRegex {
    regex: Regex,
}

impl JavaRegex {
    pub(crate) fn compile(source: &str, anchor: Anchor) -> Result<JavaRegex, String> {
        let translated = Translator::new(source).translate()?;
        let end = match anchor {
            Anchor::LookingAt => "",
            Anchor::Matches => "\\z",
        };
        let mut builder = RegexBuilder::new(&format!("\\A(?:{translated}){end}"));
        builder.backtrack_limit(usize::MAX);
        let regex = builder
            .build()
            .map_err(|error| format!("{source} translated to {translated}: {error}"))?;
        Ok(JavaRegex { regex })
    }

    /// Whether a region matches.
    pub(crate) fn is_match(&self, region: &str) -> bool {
        self.regex
            .is_match(region)
            .expect("a match without a backtrack limit cannot fail")
    }
}

const SPACES: &[(char, char)] = &[('\t', '\r'), (' ', ' ')];
const DIGITS: &[(char, char)] = &[('0', '9')];
const WORD: &[(char, char)] = &[('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z')];

/// The flags in force in a group.
#[derive(Debug, Clone, Copy)]
struct Scope {
    /// `(?s)`: `.` matches line terminators too.
    dotall: bool,
    /// `(?i)`: ASCII letters match either case.
    ignore_case: bool,
}

struct Translator<'a> {
    source: &'a str,
    chars: Vec<char>,
    position: usize,
    out: String,
    scopes: Vec<Scope>,
}

/// A character range as the target syntax writes it inside a class.
fn class_range(out: &mut String, (low, high): (char, char)) {
    out.push_str(&encode(low));
    if high != low {
        out.push('-');
        out.push_str(&encode(high));
    }
}

/// A character as the target syntax writes it, in or out of a class.
fn encode(c: char) -> String {
    if c.is_ascii_alphanumeric() || c == ' ' || c == '_' {
        c.to_string()
    } else {
        format!("\\x{{{:X}}}", u32::from(c))
    }
}

/// The ranges with, when case is ignored, the other case of their ASCII letters, as Java folds
/// case without `UNICODE_CASE`.
fn folded(ranges: &[(char, char)], ignore_case: bool) -> Vec<(char, char)> {
    let mut all = ranges.to_vec();
    if ignore_case {
        for &(low, high) in ranges {
            for (from, to, shift) in [('a', 'z', -32i32), ('A', 'Z', 32)] {
                let (start, end) = (low.max(from), high.min(to));
                if start <= end {
                    let moved = |c: char| {
                        char::from_u32((c as i32 + shift) as u32).expect("an ASCII letter")
                    };
                    all.push((moved(start), moved(end)));
                }
            }
        }
    }
    all
}

impl<'a> Translator<'a> {
    fn new(source: &'a str) -> Self {
        Translator {
            source,
            chars: source.chars().collect(),
            position: 0,
            out: String::new(),
            scopes: vec![Scope {
                dotall: false,
                ignore_case: false,
            }],
        }
    }

    fn scope(&self) -> Scope {
        *self.scopes.last().expect("a scope is open")
    }

    fn fail<T>(&self, what: &str) -> Result<T, String> {
        Err(format!(
            "{what} at {} of the Java regex {:?}",
            self.position, self.source
        ))
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.position).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        self.position += 1;
        c
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn translate(mut self) -> Result<String, String> {
        while let Some(c) = self.next() {
            match c {
                '\\' => self.escape()?,
                '[' => self.class()?,
                '(' => self.group()?,
                ')' => {
                    if self.scopes.len() == 1 {
                        return self.fail("an unbalanced )");
                    }
                    self.scopes.pop();
                    self.out.push(')');
                }
                '.' => {
                    if self.scope().dotall {
                        self.out.push_str(r"(?s:.)");
                    } else {
                        self.out.push_str(r"[^\n\r\x{85}\x{2028}\x{2029}]");
                    }
                }
                '^' => self.out.push_str(r"\A"),
                '$' => self.out.push_str(
                    r"(?:\z|(?=\r\n\z)|(?<!\r)(?=\n\z)|(?=[\r\x{85}\x{2028}\x{2029}]\z))",
                ),
                '*' | '+' | '?' => {
                    self.out.push(c);
                    self.quantifier_suffix()?;
                }
                '{' => {
                    self.out.push('{');
                    loop {
                        match self.next() {
                            Some('}') => break,
                            Some(d @ ('0'..='9' | ',')) => self.out.push(d),
                            _ => return self.fail("a malformed {} quantifier"),
                        }
                    }
                    self.out.push('}');
                    self.quantifier_suffix()?;
                }
                '|' => self.out.push('|'),
                c => self.literal(c),
            }
        }
        if self.scopes.len() != 1 {
            return self.fail("an unclosed group");
        }
        Ok(self.out)
    }

    fn quantifier_suffix(&mut self) -> Result<(), String> {
        if self.eat('?') {
            self.out.push('?');
        } else if self.peek() == Some('+') {
            return self.fail("a possessive quantifier");
        }
        Ok(())
    }

    fn group(&mut self) -> Result<(), String> {
        let inherited = self.scope();
        if !self.eat('?') {
            self.out.push('(');
            self.scopes.push(inherited);
            return Ok(());
        }
        let opener = match self.peek() {
            Some(':') => "(?:",
            Some('>') => "(?>",
            Some('=') => "(?=",
            Some('!') => "(?!",
            Some('<') => {
                self.position += 1;
                match self.peek() {
                    Some('=') => "(?<=",
                    Some('!') => "(?<!",
                    _ => return self.fail("a named group"),
                }
            }
            _ => return self.flags(inherited),
        };
        self.position += 1;
        self.out.push_str(opener);
        self.scopes.push(inherited);
        Ok(())
    }

    /// `(?is)` or `(?is:` flags, applied here rather than passed on.
    fn flags(&mut self, inherited: Scope) -> Result<(), String> {
        let mut on = true;
        let mut scope = inherited;
        loop {
            match self.next() {
                Some('-') => on = false,
                Some('s') => scope.dotall = on,
                Some('i') => scope.ignore_case = on,
                Some(')') => {
                    *self.scopes.last_mut().expect("a scope is open") = scope;
                    return Ok(());
                }
                Some(':') => {
                    self.out.push_str("(?:");
                    self.scopes.push(scope);
                    return Ok(());
                }
                _ => return self.fail("an unsupported flag"),
            }
        }
    }

    fn set(&mut self, ranges: &[(char, char)], negated: bool) {
        self.out.push('[');
        if negated {
            self.out.push('^');
        }
        for &range in ranges {
            class_range(&mut self.out, range);
        }
        self.out.push(']');
    }

    fn escape(&mut self) -> Result<(), String> {
        let Some(c) = self.next() else {
            return self.fail("a trailing backslash");
        };
        let word = "[0-9A-Z_a-z]";
        match c {
            'd' | 'D' => self.set(DIGITS, c == 'D'),
            's' | 'S' => self.set(SPACES, c == 'S'),
            'w' | 'W' => self.set(WORD, c == 'W'),
            'b' => self
                .out
                .push_str(&format!("(?:(?<={word})(?!{word})|(?<!{word})(?={word}))")),
            '1'..='9' => {
                if self.scope().ignore_case {
                    return self.fail("a back reference that ignores case");
                }
                self.out.push('\\');
                self.out.push(c);
            }
            'A' => self.out.push_str(r"\A"),
            'z' => self.out.push_str(r"\z"),
            _ => {
                let literal = self.escaped_literal(c)?;
                self.literal(literal);
            }
        }
        Ok(())
    }

    /// The character an escape other than a class or an assertion stands for.
    fn escaped_literal(&mut self, c: char) -> Result<char, String> {
        Ok(match c {
            't' => '\t',
            'n' => '\n',
            'r' => '\r',
            'f' => '\u{0C}',
            'x' => self.hex(2)?,
            'u' => self.hex(4)?,
            c if c.is_ascii_alphanumeric() => return self.fail("an unsupported escape"),
            c => c,
        })
    }

    fn hex(&mut self, digits: usize) -> Result<char, String> {
        let text: String = (0..digits).filter_map(|_| self.next()).collect();
        match u32::from_str_radix(&text, 16).ok().and_then(char::from_u32) {
            Some(c) if text.len() == digits => Ok(c),
            _ => self.fail("a malformed hexadecimal escape"),
        }
    }

    fn literal(&mut self, c: char) {
        if self.scope().ignore_case && c.is_ascii_alphabetic() {
            self.set(&folded(&[(c, c)], true), false);
        } else {
            self.out.push_str(&encode(c));
        }
    }

    fn class(&mut self) -> Result<(), String> {
        let negated = self.eat('^');
        let mut ranges: Vec<(char, char)> = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.next() else {
                return self.fail("an unclosed class");
            };
            let low = match c {
                ']' if !first => break,
                '[' => return self.fail("a nested class"),
                '&' if self.peek() == Some('&') => return self.fail("a class intersection"),
                '\\' => {
                    let Some(e) = self.next() else {
                        return self.fail("a trailing backslash");
                    };
                    match e {
                        'd' => {
                            ranges.extend_from_slice(DIGITS);
                            first = false;
                            continue;
                        }
                        's' => {
                            ranges.extend_from_slice(SPACES);
                            first = false;
                            continue;
                        }
                        'w' => {
                            ranges.extend_from_slice(WORD);
                            first = false;
                            continue;
                        }
                        _ => self.escaped_literal(e)?,
                    }
                }
                c => c,
            };
            first = false;
            let range_follows = self.peek() == Some('-')
                && self.chars.get(self.position + 1).is_some_and(|&c| c != ']');
            if !range_follows {
                ranges.push((low, low));
                continue;
            }
            self.position += 1;
            let high = match self.next() {
                Some('\\') => {
                    let e = self.next().ok_or("a trailing backslash".to_owned())?;
                    self.escaped_literal(e)?
                }
                Some('[') => return self.fail("a nested class"),
                Some(c) => c,
                None => return self.fail("an unclosed class"),
            };
            if high < low {
                return self.fail("a reversed class range");
            }
            ranges.push((low, high));
        }
        let ranges = folded(&ranges, self.scope().ignore_case);
        if ranges.is_empty() {
            return self.fail("an empty class");
        }
        self.set(&ranges, negated);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looks(pattern: &str, text: &[u8]) -> bool {
        let text: String = text.iter().map(|&b| char::from(b)).collect();
        JavaRegex::compile(pattern, Anchor::LookingAt)
            .unwrap()
            .is_match(&text)
    }

    fn fails(pattern: &str) -> bool {
        JavaRegex::compile(pattern, Anchor::LookingAt).is_err()
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
        let glob = JavaRegex::compile(r"\Ai_.*\.txt\z", Anchor::Matches).unwrap();
        assert!(glob.is_match("i_é.txt"));
        assert!(!glob.is_match("i_\u{2028}.txt"));
        let name = JavaRegex::compile("^rdf$", Anchor::Matches).unwrap();
        assert!(name.is_match("rdf"));
        assert!(!name.is_match("rdf\n"));
        assert!(!name.is_match("rdfx"));
    }
}
