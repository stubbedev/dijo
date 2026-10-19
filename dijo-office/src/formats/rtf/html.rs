//! Tika's `RTFHtmlDecapsulator`: the HTML an RTF document made from HTML encapsulates, read
//! with Tika's JFlex `RTFTokenizer` and `RTFState`: the text of its `\*\htmltag` groups and of
//! what lies outside them, without what `\htmlrtf` marks as RTF only, decoded with the charset
//! of the font in effect. A document with a picture or an embedded object, which Tika parses as
//! a document of its own while it reads the HTML, is not read.

use std::collections::HashMap;

use super::charsets::{for_fcharset, named};
use crate::formats::Unported;
use crate::text::charset::Charset;

/// The character classes of the tokenizer's DFA, by character up to U+00FF; every other
/// character is of class 0.
const CLASSES: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    3, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 5, 0, 0, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 0, 0, 0, 0, 0, 0,
    0, 7, 7, 7, 7, 7, 7, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 0, 9, 0, 0, 0,
    0, 7, 10, 7, 7, 7, 7, 8, 8, 11, 8, 8, 8, 8, 12, 8, 8, 8, 8, 8, 8, 13, 8, 8, 8, 8, 8, 14, 0, 15,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0,
];
/// The action of each state.
const ACTIONS: [u8; 25] = [
    0, 1, 2, 2, 0, 3, 4, 5, 5, 6, 6, 6, 0, 6, 0, 7, 6, 0, 8, 9, 7, 6, 8, 10, 10,
];
/// The first transition of each state.
const ROWS: [u16; 25] = [
    0, 16, 16, 32, 48, 16, 16, 16, 64, 80, 96, 112, 128, 16, 144, 160, 176, 192, 208, 16, 16, 224,
    16, 240, 16,
];
const TRANSITIONS: [i8; 256] = [
    1, 2, 3, 1, 1, 1, 1, 1, 1, 4, 1, 1, 1, 1, 5, 6, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, 2, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 7, -1, -1, 7, 8,
    7, -1, 9, 9, 7, 10, 9, 9, 11, 7, 7, -1, -1, -1, -1, -1, -1, 12, 12, -1, -1, 12, -1, -1, -1, -1,
    -1, -1, -1, -1, 13, -1, 14, 15, 9, 9, -1, 9, 9, 9, 9, -1, -1, -1, -1, -1, 13, -1, 14, 15, 9, 9,
    -1, 9, 16, 9, 9, -1, -1, -1, -1, -1, 13, -1, 17, 18, 9, 9, -1, 9, 9, 9, 9, -1, -1, -1, -1, -1,
    -1, -1, -1, 19, 19, -1, -1, 19, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 15, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1, 20, -1, -1, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    13, -1, 14, 15, 9, 9, -1, 9, 9, 21, 9, -1, -1, -1, -1, -1, -1, -1, -1, 18, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, 22, -1, -1, 18, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 13,
    -1, 14, 23, 9, 9, -1, 9, 9, 9, 9, -1, -1, -1, -1, -1, 24, -1, -1, 23, -1, -1, -1, -1, -1, -1,
    -1, -1, -1,
];
/// Bit 1: the state accepts; bit 8: it ends a token without looking ahead.
const ATTRIBUTES: [u8; 25] = [
    0, 9, 9, 1, 0, 9, 9, 9, 1, 1, 1, 1, 0, 9, 0, 1, 1, 0, 1, 9, 9, 1, 9, 1, 9,
];
const REPLACEMENT: u16 = 0xFFFD;

/// An `RTFToken`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    GroupOpen,
    GroupClose,
    Word {
        name: String,
        parameter: Option<i32>,
    },
    Symbol(u16),
    Hex(u8),
    Unicode(i32),
    Text(u16),
    Bin,
    Line,
}

/// Tika's JFlex `RTFTokenizer` over the document read as US-ASCII.
struct Tokenizer<'a> {
    input: &'a [u8],
    at: usize,
}

impl Tokenizer<'_> {
    fn unit(&self, at: usize) -> u16 {
        let byte = self.input[at];
        if byte < 0x80 {
            u16::from(byte)
        } else {
            REPLACEMENT
        }
    }

    /// `yylex`: the longest token from here, `None` at the end; a token no state accepts makes
    /// JFlex throw, which fails Tika's parse.
    fn next(&mut self) -> Result<Option<Token>, Unported> {
        let start = self.at;
        if start >= self.input.len() {
            return Ok(None);
        }
        let mut state = 0usize;
        let mut action = None;
        let mut marked = start;
        let mut position = start;
        while position < self.input.len() {
            let unit = self.unit(position);
            position += 1;
            let class = if unit < 256 {
                usize::from(CLASSES[usize::from(unit)])
            } else {
                0
            };
            let next = TRANSITIONS[usize::from(ROWS[state]) + class];
            if next < 0 {
                break;
            }
            state = next as usize;
            if ATTRIBUTES[state] & 1 == 1 {
                action = Some(ACTIONS[state]);
                marked = position;
                if ATTRIBUTES[state] & 8 == 8 {
                    break;
                }
            }
        }
        let Some(action) = action else {
            return Err(Unported(
                "the RTF ends in a token Tika's tokenizer cannot match, which it throws on"
                    .to_owned(),
            ));
        };
        self.at = marked;
        let text: Vec<u16> = (start..marked).map(|at| self.unit(at)).collect();
        Ok(Some(token(action, &text)))
    }
}

/// The token of an action, from its text.
fn token(action: u8, text: &[u16]) -> Token {
    let length = if text.last() == Some(&u16::from(b' ')) {
        text.len() - 1
    } else {
        text.len()
    };
    match action {
        1 => Token::Text(text[0]),
        2 => Token::Line,
        3 => Token::GroupOpen,
        4 => Token::GroupClose,
        5 => Token::Symbol(text[1]),
        6 => Token::Word {
            name: String::from_utf16_lossy(&text[1..length]),
            parameter: None,
        },
        7 => {
            let name_end = (1..length)
                .find(|&at| !char::from_u32(u32::from(text[at])).is_some_and(char::is_alphabetic))
                .unwrap_or(length);
            Token::Word {
                name: String::from_utf16_lossy(&text[1..name_end]),
                parameter: Some(parse_int(&text[name_end..length])),
            }
        }
        8 => {
            let value = parse_int(&text[2..length]);
            Token::Unicode(if value < 0 { 65536 + value } else { value })
        }
        9 => {
            let digit = |unit: u16| char::from_u32(u32::from(unit)).and_then(|c| c.to_digit(16));
            let high = digit(text[2]).expect("a hexadecimal digit");
            let low = digit(text[3]).expect("a hexadecimal digit");
            Token::Hex(((high << 4) | low) as u8)
        }
        10 => Token::Bin,
        _ => unreachable!("the tokenizer has ten actions"),
    }
}

/// `parseIntFromBuffer`: an optional minus and digits, overflowing as Java's `int` does.
fn parse_int(text: &[u16]) -> i32 {
    let (negative, digits) = match text.first() {
        Some(&unit) if unit == u16::from(b'-') => (true, &text[1..]),
        _ => (false, text),
    };
    let value = digits.iter().fold(0i32, |value, &unit| {
        value
            .wrapping_mul(10)
            .wrapping_add(i32::from(unit) - i32::from(b'0'))
    });
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

/// `RTFGroupState`, as far as the decapsulator looks at it.
#[derive(Debug, Clone, Default)]
struct Group {
    depth: i32,
    font_charset: Option<Charset>,
    uc_skip: i32,
    ignore: bool,
    objdata: bool,
    pict_depth: i32,
}

impl Group {
    fn child(&self) -> Group {
        Group {
            depth: self.depth + 1,
            font_charset: self.font_charset,
            uc_skip: self.uc_skip,
            ignore: self.ignore,
            objdata: false,
            pict_depth: if self.pict_depth > 0 {
                self.pict_depth + 1
            } else {
                0
            },
        }
    }
}

const MAX_GROUP_DEPTH: usize = 10_000;

/// `RTFState`.
struct State {
    global_charset: Charset,
    global_default_font: i32,
    font_charsets: HashMap<i32, Charset>,
    stack: Vec<Group>,
    current: Group,
    ansi_skip: i32,
    font_table_state: i32,
    font_table_depth: i32,
    current_font: i32,
    in_header: bool,
}

impl State {
    fn new() -> State {
        State {
            global_charset: named("WINDOWS-1252"),
            global_default_font: -1,
            font_charsets: HashMap::new(),
            stack: Vec::new(),
            current: Group {
                uc_skip: 1,
                ..Group::default()
            },
            ansi_skip: 0,
            font_table_state: 0,
            font_table_depth: -1,
            current_font: -1,
            in_header: true,
        }
    }

    /// `processToken`: whether the state consumed the token.
    fn process(&mut self, token: &Token) -> bool {
        match token {
            Token::GroupOpen => {
                if self.stack.len() < MAX_GROUP_DEPTH {
                    let child = self.current.child();
                    self.stack.push(std::mem::replace(&mut self.current, child));
                }
                false
            }
            Token::GroupClose => {
                if let Some(parent) = self.stack.pop() {
                    self.current = parent;
                }
                if self.font_table_state == 1 && self.current.depth < self.font_table_depth {
                    self.font_table_state = 2;
                }
                false
            }
            Token::Symbol(symbol) => {
                if *symbol == u16::from(b'*') {
                    self.current.ignore = true;
                }
                false
            }
            Token::Word { name, parameter } => self.word(name, *parameter),
            Token::Unicode(_) => {
                self.ansi_skip = self.current.uc_skip;
                false
            }
            Token::Hex(_) | Token::Text(_) => {
                if self.ansi_skip > 0 {
                    self.ansi_skip -= 1;
                    true
                } else {
                    false
                }
            }
            Token::Bin | Token::Line => false,
        }
    }

    /// `processControlWord`.
    fn word(&mut self, name: &str, parameter: Option<i32>) -> bool {
        match name {
            "ansi" => {
                self.global_charset = named("WINDOWS-1252");
                return true;
            }
            "pca" => {
                self.global_charset = named("cp850");
                return true;
            }
            "pc" => {
                self.global_charset = named("cp437");
                return true;
            }
            "mac" => {
                self.global_charset = named("MacRoman");
                return true;
            }
            "ansicpg" => {
                if let Some(page) = parameter {
                    self.global_charset = code_page(page);
                }
                return true;
            }
            "deff" => {
                if let Some(font) = parameter {
                    self.global_default_font = font;
                }
                return true;
            }
            "fonttbl" => {
                self.font_table_state = 1;
                self.font_table_depth = self.current.depth;
                self.current.ignore = true;
                return true;
            }
            _ => {}
        }
        if self.font_table_state == 1 {
            if self.current.depth < self.font_table_depth {
                self.font_table_state = 2;
            } else if let (Some(value), "f") = (parameter, name) {
                self.current_font = value;
                return true;
            } else if let (Some(value), "fcharset") = (parameter, name) {
                if let Some(charset) = for_fcharset(value) {
                    self.font_charsets.insert(self.current_font, charset);
                }
                return true;
            }
        }
        if let (Some(value), "uc") = (parameter, name) {
            self.current.uc_skip = value;
            return true;
        }
        if let (Some(value), "f") = (parameter, name) {
            self.current.font_charset = self.font_charsets.get(&value).copied();
            if self.font_table_state == 2 && !self.current.ignore {
                self.in_header = false;
            }
            return false;
        }
        if self.in_header
            && !self.current.ignore
            && matches!(
                name,
                "par"
                    | "pard"
                    | "sect"
                    | "sectd"
                    | "plain"
                    | "ltrch"
                    | "rtlch"
                    | "htmlrtf"
                    | "line"
            )
        {
            self.in_header = false;
        }
        match name {
            "objdata" => {
                self.current.objdata = true;
                return false;
            }
            "pict" => {
                self.current.pict_depth = 1;
                return false;
            }
            _ => {}
        }
        if self.in_header && matches!(name, "colortbl" | "stylesheet") {
            self.current.ignore = true;
            return true;
        }
        false
    }

    /// `getCurrentCharset`.
    fn charset(&self) -> Charset {
        if let Some(charset) = self.current.font_charset {
            return charset;
        }
        if self.global_default_font != -1 && !self.in_header {
            if let Some(&charset) = self.font_charsets.get(&self.global_default_font) {
                return charset;
            }
        }
        self.global_charset
    }
}

/// `RTFCharsetMaps.ANSICPG_MAP`, else `resolveCodePage`.
fn code_page(page: i32) -> Charset {
    let mapped = match page {
        709 => Some("windows-709"),
        710 => Some("windows-710"),
        711 => Some("windows-711"),
        720 => Some("windows-720"),
        _ => None,
    };
    if let Some(name) = mapped {
        return named(name);
    }
    if let Some(charset) = super::charsets::for_ansicpg(page) {
        return charset;
    }
    Charset::for_name(&format!("windows-{page}"))
        .or_else(|| Charset::for_name(&format!("cp{page}")))
        .unwrap_or_else(|| named("WINDOWS-1252"))
}

/// `RTFHtmlDecapsulator.extract`: the encapsulated HTML, `None` where the document encapsulates
/// none.
pub(crate) fn decapsulate(rtf: &[u8]) -> Result<Option<Vec<u16>>, Unported> {
    if rtf.is_empty() {
        return Ok(None);
    }
    let mut tokenizer = Tokenizer { input: rtf, at: 0 };
    let mut state = State::new();
    let mut html: Vec<u16> = Vec::with_capacity(rtf.len() / 2);
    let mut pending: Vec<u8> = Vec::new();
    let mut found_from_html = false;
    let mut found_html_tag = false;
    let mut skipping = false;
    let mut saw_ignorable = false;
    let mut tag_depth = -1;
    let mut in_tag = false;
    while let Some(token) = tokenizer.next()? {
        let flushes = matches!(token, Token::GroupClose)
            || matches!(&token, Token::Word { name, parameter: Some(_) } if name == "f");
        if flushes {
            flush(&mut pending, &mut html, &state)?;
        }
        let consumed = state.process(&token);
        if !consumed {
            if let Token::Word { name, .. } = &token {
                if name == "objdata" || name == "pict" {
                    return Err(Unported(
                        "the RTF body has a picture or an embedded object, which Tika parses as \
                         a document of its own"
                            .to_owned(),
                    ));
                }
            }
        }
        let group = &state.current;
        if !consumed && (group.objdata || group.pict_depth > 0) {
            continue;
        }
        let writing = found_html_tag && !skipping && (in_tag || tag_depth == -1);
        match token {
            Token::GroupOpen => saw_ignorable = false,
            Token::GroupClose => {
                if in_tag && state.current.depth < tag_depth {
                    flush(&mut pending, &mut html, &state)?;
                    in_tag = false;
                    tag_depth = -1;
                }
            }
            Token::Symbol(symbol) => {
                if symbol == u16::from(b'*') {
                    saw_ignorable = true;
                }
                if writing && b"{}\\".map(u16::from).contains(&symbol) {
                    flush(&mut pending, &mut html, &state)?;
                    html.push(symbol);
                }
            }
            Token::Word { name, parameter } => {
                if consumed {
                    continue;
                }
                match name.as_str() {
                    "fromhtml" => found_from_html = true,
                    "htmltag" if saw_ignorable => {
                        if found_from_html {
                            found_html_tag = true;
                            flush(&mut pending, &mut html, &state)?;
                            in_tag = true;
                            tag_depth = state.current.depth;
                        }
                    }
                    "htmlrtf" => {
                        flush(&mut pending, &mut html, &state)?;
                        skipping = parameter != Some(0);
                    }
                    _ if writing => {
                        flush(&mut pending, &mut html, &state)?;
                        match name.as_str() {
                            "par" | "pard" => html.push(u16::from(b'\n')),
                            "tab" => html.push(u16::from(b'\t')),
                            "line" => html.extend("<br>".encode_utf16()),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            Token::Hex(value) => {
                if !consumed && writing {
                    pending.push(value);
                }
            }
            Token::Unicode(code_point) => {
                if writing {
                    flush(&mut pending, &mut html, &state)?;
                    if let Ok(code_point) = u32::try_from(code_point) {
                        if code_point <= 0x10FFFF {
                            push_code_point(&mut html, code_point);
                        }
                    }
                }
            }
            Token::Text(unit) => {
                if !consumed && writing {
                    flush(&mut pending, &mut html, &state)?;
                    html.push(unit);
                }
            }
            Token::Bin | Token::Line => {}
        }
    }
    flush(&mut pending, &mut html, &state)?;
    if !found_from_html || html.is_empty() {
        return Ok(None);
    }
    Ok(Some(html))
}

/// `StringBuilder.appendCodePoint`.
fn push_code_point(html: &mut Vec<u16>, code_point: u32) {
    match char::from_u32(code_point) {
        Some(character) => {
            let mut units = [0u16; 2];
            html.extend_from_slice(character.encode_utf16(&mut units));
        }
        None => html.push(code_point as u16),
    }
}

/// `flushPendingBytes`: the bytes of hexadecimal escapes, decoded in the charset in effect.
fn flush(pending: &mut Vec<u8>, html: &mut Vec<u16>, state: &State) -> Result<(), Unported> {
    if pending.is_empty() {
        return Ok(());
    }
    let charset = state.charset();
    let text = charset.decode(pending).ok_or_else(|| {
        Unported(format!(
            "the RTF body has text in {}, which the native decoders do not decode",
            charset.name()
        ))
    })?;
    html.extend(text.encode_utf16());
    pending.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(rtf: &str) -> Option<String> {
        decapsulate(rtf.as_bytes())
            .unwrap()
            .map(|units| String::from_utf16(&units).unwrap())
    }

    #[test]
    fn decapsulates_the_html_an_rtf_body_holds() {
        let rtf = r"{\rtf1\ansi\fromhtml1 {\fonttbl{\f0\fswiss Arial;}}{\*\htmltag19 <html>}{\*\htmltag34 <body>}\htmlrtf {\htmlrtf0 caf\'e9 \{x\} \par}{\*\htmltag0 </body>}}";
        assert_eq!(html(rtf).as_deref(), Some("<html><body>café {x} \n</body>"));
        assert_eq!(html(r"{\rtf1\ansi plain}"), None);
    }

    #[test]
    fn tokenizes_as_tikas_tokenizer_does() {
        let mut tokenizer = Tokenizer {
            input: br"\u-3? \'4g\bin12 \foo-7\par{",
            at: 0,
        };
        let mut tokens = Vec::new();
        while let Some(token) = tokenizer.next().unwrap() {
            tokens.push(token);
        }
        assert_eq!(tokens[0], Token::Unicode(65533));
        assert_eq!(tokens[1], Token::Text(u16::from(b'?')));
        assert_eq!(tokens[3], Token::Symbol(u16::from(b'\'')));
        assert!(tokens.contains(&Token::Bin));
        assert!(tokens.contains(&Token::Word {
            name: "foo".to_owned(),
            parameter: Some(-7)
        }));
        assert_eq!(tokens.last(), Some(&Token::GroupOpen));
        assert!(Tokenizer {
            input: b"a\\",
            at: 1
        }
        .next()
        .is_err());
    }

    #[test]
    fn leaves_pictures_to_tika() {
        assert!(decapsulate(br"{\rtf1\fromhtml1{\*\htmltag <p>}{\pict 00}}").is_err());
    }
}
