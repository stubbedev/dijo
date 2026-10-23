//! The translation of a Java regex into the syntax `fancy-regex` reads, when the tables are
//! generated.

use std::sync::OnceLock;

use fancy_regex::Expr;

use super::{JavaRegex, Starts};

/// How much of the haystack a match must cover: a prefix, as `Matcher.lookingAt` over a region
/// does, or all of it, as `Matcher.matches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    LookingAt,
    Matches,
}

/// The characters a haystack holds: bytes as the ISO-8859-1 characters U+0000 to U+00FF, as
/// the magic reads them, or any text, as a file name. A class is written for the characters a
/// Latin-1 haystack can hold, which matches as the class does there and compiles to a fraction
/// of the automaton of a class over all of Unicode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Text {
    Latin1,
    Unicode,
}

impl JavaRegex {
    /// The regex of a Java `source`, translated and checked, with its pattern and its cache
    /// leaked for the life of the generator.
    pub(crate) fn compile(source: &str, anchor: Anchor, text: Text) -> Result<JavaRegex, String> {
        let Translation { translated, starts } = Translator::new(source, text).translate()?;
        let end = match anchor {
            Anchor::LookingAt => "",
            Anchor::Matches => "\\z",
        };
        let pattern = format!("\\A(?:{translated}){end}");
        Expr::parse_tree(&pattern)
            .map_err(|error| format!("{source} translated to {translated}: {error}"))?;
        Ok(JavaRegex::new(
            Box::leak(pattern.into_boxed_str()),
            starts,
            Box::leak(Box::new(OnceLock::new())),
        ))
    }
}

const LINE_TERMINATORS: &[(char, char)] = &[
    ('\n', '\n'),
    ('\r', '\r'),
    ('\u{85}', '\u{85}'),
    ('\u{2028}', '\u{2029}'),
];
const LATIN1: (char, char) = ('\0', '\u{FF}');
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

/// A Java regex in the target syntax, and the characters a match may start with.
struct Translation {
    translated: String,
    starts: Starts,
}

impl Starts {
    const NONE: Starts = Starts::new(&[], false);
    const ANY: Starts = Starts::new(&[(0, 0xFF)], true);

    /// The characters of `ranges`, or, if `negated`, at least those of none of them.
    fn of(ranges: &[(char, char)], negated: bool) -> Self {
        let mut starts = Starts::NONE;
        for &(low, high) in ranges {
            for code in u32::from(low)..=u32::from(high).min(0xFF) {
                starts.latin1[(code >> 7) as usize] |= 1u128 << (code & 0x7F);
            }
            starts.beyond |= u32::from(high) > 0xFF;
        }
        if negated {
            starts = Starts {
                latin1: starts.latin1.map(|bits| !bits),
                beyond: true,
            };
        }
        starts
    }

    fn union(self, other: Starts) -> Starts {
        Starts {
            latin1: [
                self.latin1[0] | other.latin1[0],
                self.latin1[1] | other.latin1[1],
            ],
            beyond: self.beyond || other.beyond,
        }
    }

    /// The ranges of U+0000 to U+00FF the set holds, and whether it holds those above.
    pub(crate) fn ranges(self) -> (Vec<(u8, u8)>, bool) {
        let mut ranges: Vec<(u8, u8)> = Vec::new();
        for code in 0..=0xFF {
            if !self.contains(char::from(code)) {
                continue;
            }
            match ranges.last_mut() {
                Some((_, high)) if *high + 1 == code => *high = code,
                _ => ranges.push((code, code)),
            }
        }
        (ranges, self.beyond)
    }
}

/// What a part of a regex may start with, and whether it may match nothing.
#[derive(Debug, Clone, Copy)]
struct Part {
    starts: Starts,
    empty: bool,
}

impl Part {
    /// What matches only where it is, as an assertion does.
    const EMPTY: Part = Part {
        starts: Starts::NONE,
        empty: true,
    };

    /// This part followed by `next`.
    fn then(self, next: Part) -> Part {
        if !self.empty {
            return self;
        }
        Part {
            starts: self.starts.union(next.starts),
            empty: next.empty,
        }
    }

    /// Either this part or `other`.
    fn or(self, other: Part) -> Part {
        Part {
            starts: self.starts.union(other.starts),
            empty: self.empty || other.empty,
        }
    }
}

/// The parts of a group read so far: the alternatives before the last `|`, the atoms of the
/// current alternative before its last, and its last, which a quantifier applies to.
#[derive(Debug)]
struct Sequence {
    /// A lookaround, which matches only where it is whatever its parts.
    lookaround: bool,
    alternatives: Option<Part>,
    before: Part,
    last: Option<Part>,
}

impl Sequence {
    fn new(lookaround: bool) -> Self {
        Sequence {
            lookaround,
            alternatives: None,
            before: Part::EMPTY,
            last: None,
        }
    }

    fn current(&self) -> Part {
        self.last.map_or(self.before, |last| self.before.then(last))
    }

    fn push(&mut self, atom: Part) {
        self.before = self.current();
        self.last = Some(atom);
    }

    /// A quantifier that allows no repetition of the last atom.
    fn optional(&mut self) {
        if let Some(last) = &mut self.last {
            last.empty = true;
        }
    }

    fn alternate(&mut self) {
        let current = self.current();
        self.alternatives = Some(self.alternatives.map_or(current, |done| done.or(current)));
        self.before = Part::EMPTY;
        self.last = None;
    }

    /// The group as a whole.
    fn finish(self) -> Part {
        if self.lookaround {
            return Part::EMPTY;
        }
        let current = self.current();
        self.alternatives.map_or(current, |done| done.or(current))
    }
}

struct Translator<'a> {
    source: &'a str,
    chars: Vec<char>,
    position: usize,
    out: String,
    scopes: Vec<Scope>,
    /// The parts read of each open group, the whole regex first.
    sequences: Vec<Sequence>,
    text: Text,
    /// The atom the last escape, class or literal wrote, if it wrote one that matches a character.
    atom: Option<Part>,
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

/// The Latin-1 characters none of `ranges` holds, as ranges.
fn latin1_complement(ranges: &[(char, char)]) -> Vec<(char, char)> {
    let mut held = [false; 256];
    for &(low, high) in ranges {
        for code in u32::from(low)..=u32::from(high).min(0xFF) {
            held[code as usize] = true;
        }
    }
    let mut complement: Vec<(char, char)> = Vec::new();
    for (code, _) in held.iter().enumerate().filter(|(_, &held)| !held) {
        let c = char::from(u8::try_from(code).expect("a Latin-1 code"));
        match complement.last_mut() {
            Some((_, high)) if u32::from(*high) + 1 == u32::from(c) => *high = c,
            _ => complement.push((c, c)),
        }
    }
    complement
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
    fn new(source: &'a str, text: Text) -> Self {
        Translator {
            source,
            chars: source.chars().collect(),
            position: 0,
            out: String::new(),
            scopes: vec![Scope {
                dotall: false,
                ignore_case: false,
            }],
            sequences: vec![Sequence::new(false)],
            text,
            atom: None,
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

    fn translate(mut self) -> Result<Translation, String> {
        while let Some(c) = self.next() {
            self.atom = None;
            match c {
                '\\' => self.escape()?,
                '[' => self.class()?,
                '(' => {
                    self.group()?;
                    continue;
                }
                ')' => {
                    if self.scopes.len() == 1 {
                        return self.fail("an unbalanced )");
                    }
                    self.scopes.pop();
                    self.out.push(')');
                    let group = self.sequences.pop().expect("a group is open");
                    self.atom = Some(group.finish());
                }
                '.' => match (self.scope().dotall, self.text) {
                    (true, Text::Unicode) => {
                        self.out.push_str(r"(?s:.)");
                        self.atom = Some(Part {
                            starts: Starts::ANY,
                            empty: false,
                        });
                    }
                    (true, Text::Latin1) => self.set(&[LATIN1], false),
                    (false, _) => self.set(LINE_TERMINATORS, true),
                },
                '^' => self.out.push_str(r"\A"),
                '$' => self.out.push_str(
                    r"(?:\z|(?=\r\n\z)|(?<!\r)(?=\n\z)|(?=[\r\x{85}\x{2028}\x{2029}]\z))",
                ),
                '*' | '+' | '?' => {
                    self.out.push(c);
                    self.quantifier_suffix()?;
                    if c != '+' {
                        self.sequence().optional();
                    }
                    continue;
                }
                '{' => {
                    self.out.push('{');
                    let bounds = self.out.len();
                    loop {
                        match self.next() {
                            Some('}') => break,
                            Some(d @ ('0'..='9' | ',')) => self.out.push(d),
                            _ => return self.fail("a malformed {} quantifier"),
                        }
                    }
                    let minimum = self.out[bounds..].split(',').next().unwrap_or_default();
                    let optional = minimum.bytes().all(|d| d == b'0');
                    self.out.push('}');
                    self.quantifier_suffix()?;
                    if optional {
                        self.sequence().optional();
                    }
                    continue;
                }
                '|' => {
                    self.out.push('|');
                    self.sequence().alternate();
                    continue;
                }
                c => self.literal(c),
            }
            let atom = self.atom.unwrap_or(Part::EMPTY);
            self.sequence().push(atom);
        }
        if self.scopes.len() != 1 {
            return self.fail("an unclosed group");
        }
        let whole = self.sequences.pop().expect("the regex is open").finish();
        Ok(Translation {
            translated: self.out,
            starts: if whole.empty {
                Starts::ANY
            } else {
                whole.starts
            },
        })
    }

    fn sequence(&mut self) -> &mut Sequence {
        self.sequences.last_mut().expect("a group is open")
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
            self.sequences.push(Sequence::new(false));
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
        self.sequences.push(Sequence::new(matches!(
            opener,
            "(?=" | "(?!" | "(?<=" | "(?<!"
        )));
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
                    self.sequences.push(Sequence::new(false));
                    return Ok(());
                }
                _ => return self.fail("an unsupported flag"),
            }
        }
    }

    fn set(&mut self, ranges: &[(char, char)], negated: bool) {
        if negated && self.text == Text::Latin1 {
            let complement = latin1_complement(ranges);
            if !complement.is_empty() {
                return self.set(&complement, false);
            }
        }
        self.out.push('[');
        if negated {
            self.out.push('^');
        }
        for &range in ranges {
            class_range(&mut self.out, range);
        }
        self.out.push(']');
        self.atom = Some(Part {
            starts: Starts::of(ranges, negated),
            empty: false,
        });
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
                self.atom = Some(Part {
                    starts: Starts::ANY,
                    empty: true,
                });
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
            self.atom = Some(Part {
                starts: Starts::of(&[(c, c)], false),
                empty: false,
            });
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
