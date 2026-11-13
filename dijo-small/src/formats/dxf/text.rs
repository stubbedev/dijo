//! The text of a drawing's strings: decoded from the drawing's code page, or from UTF-8 from
//! AutoCAD 2007 on, its escapes resolved and, by what the string is, its control codes or
//! formatting codes taken out.
//!
//! Every string may hold `\U+XXXX`, a character outside the code page, `\M+NXXXX`, a character
//! of the double-byte code page `N` (1 to 5: 932, 950, 949, 1361 and 936), and the caret codes
//! `^I` (a tab), `^J` (a line break) and `^ ` (a caret). The text of a `TEXT` or an attribute
//! may hold the control codes `%%c`, `%%d` and `%%p`, the diameter, degree and plus-minus signs,
//! `%%%`, a percent sign, `%%nnn`, character `nnn`, and the toggles of underlining, overlining and
//! striking through, `%%u`, `%%o` and `%%k`. The text of an `MTEXT` or a dimension holds its
//! formatting codes: `{` and `}` group, `\P` (and `\N`, `\X`) breaks a paragraph, `\~` is a
//! no-break space, `\\`, `\{` and `\}` are the characters, `\S` stacks the text up to `;` as a
//! fraction, written `top/bottom` (or `top bottom` for a tolerance, stacked with `^`), and the
//! codes of fonts, heights, colours and the like (`\f…;`, `\H…;`, `\L`, `\l`, …) are dropped. A
//! dimension's text puts its measurement in place of `<>`.

use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Failure;
use crate::text::charset::Charset;

/// The most bytes of a string decoded at a time, so that a long one is held a piece at a time.
const PIECE: usize = 8192;
/// The longest escape: `\M+NXXXX`.
const LONGEST_ESCAPE: usize = 8;
/// The hexadecimal digits of `\U+XXXX` and `\M+NXXXX`.
const HEX_DIGITS: usize = 4;
/// The code pages of `\M+N`, by `N`.
const DOUBLE_BYTE_CODE_PAGES: [&str; 5] = [
    "windows-932",
    "windows-950",
    "windows-949",
    "x-Johab",
    "windows-936",
];

/// The charset of the code page `$DWGCODEPAGE` names, as AutoCAD names them.
pub(super) fn code_page(name: &str) -> Result<Charset, Failure> {
    let unknown = || {
        Failure::Malformed(format!(
            "The DXF code page {name:?} is not one AutoCAD writes"
        ))
    };
    let upper = name.trim().to_ascii_uppercase();
    let java = if let Some(number) = upper.strip_prefix("ANSI_") {
        match number {
            "1361" => "x-Johab".to_owned(),
            _ => format!("windows-{number}"),
        }
    } else if let Some(number) = upper.strip_prefix("DOS") {
        match number {
            "932" => "windows-932".to_owned(),
            _ => format!("IBM{number}"),
        }
    } else if let Some(part) = upper.strip_prefix("ISO8859-") {
        format!("ISO-8859-{part}")
    } else {
        match upper.as_str() {
            "ASCII" => "US-ASCII",
            "MACINTOSH" => "x-MacRoman",
            "BIG5" => "Big5",
            "GB2312" => "GB2312",
            "KSC5601" => "EUC-KR",
            "JOHAB" => "x-Johab",
            "UTF8" | "UTF-8" => "UTF-8",
            _ => return Err(unknown()),
        }
        .to_owned()
    };
    Charset::for_name(&java)
        .filter(|charset| charset.decodes())
        .ok_or_else(unknown)
}

/// What a string is, which decides the codes its text holds beyond the escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Syntax {
    /// A value of the header or of the drawing's properties: escapes alone.
    Value,
    /// The text of a `TEXT`, `ATTRIB` or `ATTDEF`, with its `%%` control codes.
    Text,
    /// The text of an `MTEXT`, with its formatting codes.
    Mtext,
    /// The text of a dimension: that of an `MTEXT`, its measurement in place of `<>`.
    Dimension,
}

/// What the decoder is inside, in the text of an `MTEXT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Text,
    /// The parameter of a formatting code, up to its `;`.
    Parameter,
    /// A stacked fraction, up to its `;`, after a backslash (`escaped`) or not.
    Stack {
        escaped: bool,
    },
}

/// What the characters of an escape that has begun come to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Escape {
    /// Too few to tell yet.
    Partial,
    /// No escape: its first character stands for itself.
    Literal,
    Char(char),
    /// `\U+XXXX`, a UTF-16 unit.
    Unit(u16),
    /// `\M+NXXXX`: the two bytes of a character of a double-byte code page.
    DoubleByte(usize, [u8; 2]),
    Break,
    /// A code that is dropped.
    Dropped,
    Enter(State),
    Measurement,
}

/// The text of strings of one [`Syntax`] as their characters come, in pieces, the escapes and
/// codes that straddle the pieces read whole. Paragraph breaks are written as line feeds.
pub(super) struct Decoder {
    syntax: Syntax,
    state: State,
    pending: [char; LONGEST_ESCAPE],
    pending_length: usize,
    /// A high surrogate of `\U+XXXX`, until its low one.
    high: Option<u16>,
    /// What a dimension's `<>` stands for.
    measurement: String,
}

impl Decoder {
    pub(super) fn new(syntax: Syntax) -> Self {
        Self {
            syntax,
            state: State::Text,
            pending: ['\0'; LONGEST_ESCAPE],
            pending_length: 0,
            high: None,
            measurement: String::new(),
        }
    }

    /// A decoder of a dimension's text, which puts `measurement` in place of `<>`.
    pub(super) fn dimension(measurement: String) -> Self {
        Self {
            measurement,
            ..Self::new(Syntax::Dimension)
        }
    }

    /// Decodes `bytes` in `charset`, a piece at a time, each piece's text added to `out` and
    /// handed to `each`, which takes it out.
    pub(super) fn decode<E>(
        &mut self,
        bytes: &[u8],
        charset: Charset,
        out: &mut String,
        mut each: impl FnMut(&mut String) -> Result<(), E>,
    ) -> Result<(), E> {
        let pieces = charset
            .pieces(bytes, PIECE)
            .expect("code_page gives only charsets that decode");
        for piece in pieces {
            for character in piece.chars() {
                self.feed(character, out);
            }
            each(out)?;
        }
        Ok(())
    }

    /// Ends the string: what is left of an escape stands for itself.
    pub(super) fn finish(&mut self, out: &mut String) {
        let pending = self.pending;
        let length = std::mem::take(&mut self.pending_length);
        for &character in &pending[..length] {
            self.emit(character, out);
        }
        self.end_unit(out);
        self.state = State::Text;
    }

    fn feed(&mut self, character: char, out: &mut String) {
        match self.state {
            State::Parameter => {
                if character == ';' {
                    self.state = State::Text;
                }
            }
            State::Stack { escaped: true } => {
                self.state = State::Stack { escaped: false };
                self.emit(character, out);
            }
            State::Stack { escaped: false } => match character {
                ';' => self.state = State::Text,
                '\\' => self.state = State::Stack { escaped: true },
                '/' | '#' => self.emit('/', out),
                '^' => self.emit(' ', out),
                _ => self.emit(character, out),
            },
            State::Text => self.feed_text(character, out),
        }
    }

    fn formatted(&self) -> bool {
        matches!(self.syntax, Syntax::Mtext | Syntax::Dimension)
    }

    fn feed_text(&mut self, character: char, out: &mut String) {
        if self.pending_length == 0 {
            match character {
                '{' | '}' if self.formatted() => return,
                '\\' | '^' => {}
                '%' if self.syntax == Syntax::Text => {}
                '<' if self.syntax == Syntax::Dimension => {}
                _ => return self.emit(character, out),
            }
        }
        self.pending[self.pending_length] = character;
        self.pending_length += 1;
        let escape = self.escape();
        if escape == Escape::Partial {
            return;
        }
        let pending = self.pending;
        let length = std::mem::take(&mut self.pending_length);
        match escape {
            Escape::Partial => unreachable!("returned above"),
            Escape::Literal => {
                self.emit(pending[0], out);
                for &character in &pending[1..length] {
                    self.feed(character, out);
                }
            }
            Escape::Char(character) => self.emit(character, out),
            Escape::Unit(unit) => self.unit(unit, out),
            Escape::DoubleByte(page, bytes) => {
                let text = Charset::named(DOUBLE_BYTE_CODE_PAGES[page])
                    .decode(&bytes)
                    .expect("the double-byte code pages decode");
                for character in text.chars() {
                    self.emit(character, out);
                }
            }
            Escape::Break => self.emit('\n', out),
            Escape::Dropped => {}
            Escape::Enter(state) => self.state = state,
            Escape::Measurement => {
                self.end_unit(out);
                out.push_str(&self.measurement);
            }
        }
    }

    /// What the pending characters come to.
    fn escape(&self) -> Escape {
        let pending = &self.pending[..self.pending_length];
        let Some(&second) = pending.get(1) else {
            return Escape::Partial;
        };
        match pending[0] {
            '\\' => self.backslash(second, pending),
            '^' => match second {
                'I' => Escape::Char('\t'),
                'J' => Escape::Break,
                ' ' => Escape::Char('^'),
                _ => Escape::Literal,
            },
            '%' => percent(pending),
            '<' if second == '>' => Escape::Measurement,
            _ => Escape::Literal,
        }
    }

    fn backslash(&self, second: char, pending: &[char]) -> Escape {
        match second {
            'U' => return unicode(pending),
            'M' => return double_byte(pending),
            _ if !self.formatted() => return Escape::Literal,
            _ => {}
        }
        match second {
            '\\' | '{' | '}' => Escape::Char(second),
            'P' | 'N' | 'X' => Escape::Break,
            '~' => Escape::Char('\u{a0}'),
            'L' | 'l' | 'O' | 'o' | 'K' | 'k' => Escape::Dropped,
            'A' | 'C' | 'c' | 'F' | 'f' | 'H' | 'Q' | 'T' | 'W' | 'p' => {
                Escape::Enter(State::Parameter)
            }
            'S' => Escape::Enter(State::Stack { escaped: false }),
            _ => Escape::Literal,
        }
    }

    fn unit(&mut self, unit: u16, out: &mut String) {
        match (self.high.take(), unit) {
            (Some(high), 0xdc00..=0xdfff) => {
                let code =
                    0x10000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(unit) - 0xdc00);
                out.push(char::from_u32(code).expect("a surrogate pair is a character"));
            }
            (high, _) => {
                if high.is_some() {
                    out.push(char::REPLACEMENT_CHARACTER);
                }
                match unit {
                    0xd800..=0xdbff => self.high = Some(unit),
                    _ => {
                        out.push(char::from_u32(unit.into()).unwrap_or(char::REPLACEMENT_CHARACTER))
                    }
                }
            }
        }
    }

    /// Ends a high surrogate that no low one follows.
    fn end_unit(&mut self, out: &mut String) {
        if self.high.take().is_some() {
            out.push(char::REPLACEMENT_CHARACTER);
        }
    }

    fn emit(&mut self, character: char, out: &mut String) {
        self.end_unit(out);
        out.push(character);
    }
}

/// The value of the [`HEX_DIGITS`] hexadecimal `digits` of an escape, or `None` while there
/// are too few of them to tell.
fn hexadecimal(digits: &[char]) -> Result<Option<u16>, ()> {
    if !digits.iter().all(char::is_ascii_hexdigit) {
        return Err(());
    }
    if digits.len() < HEX_DIGITS {
        return Ok(None);
    }
    Ok(Some(digits.iter().fold(0, |value, digit| {
        (value << 4) | digit.to_digit(16).expect("a hexadecimal digit") as u16
    })))
}

/// `\U+XXXX`.
fn unicode(pending: &[char]) -> Escape {
    match pending.get(2) {
        None => Escape::Partial,
        Some('+') => match hexadecimal(&pending[3..]) {
            Err(()) => Escape::Literal,
            Ok(None) => Escape::Partial,
            Ok(Some(unit)) => Escape::Unit(unit),
        },
        Some(_) => Escape::Literal,
    }
}

/// `\M+NXXXX`.
fn double_byte(pending: &[char]) -> Escape {
    match (pending.get(2), pending.get(3)) {
        (None, _) | (Some('+'), None) => Escape::Partial,
        (Some('+'), Some(page)) => {
            let Some(page) = page.to_digit(10).filter(|page| (1..=5).contains(page)) else {
                return Escape::Literal;
            };
            match hexadecimal(&pending[4..]) {
                Err(()) => Escape::Literal,
                Ok(None) => Escape::Partial,
                Ok(Some(code)) => Escape::DoubleByte(page as usize - 1, code.to_be_bytes()),
            }
        }
        (Some(_), _) => Escape::Literal,
    }
}

/// The `%%` control codes of a `TEXT`.
fn percent(pending: &[char]) -> Escape {
    if pending[1] != '%' {
        return Escape::Literal;
    }
    let Some(&code) = pending.get(2) else {
        return Escape::Partial;
    };
    match code.to_ascii_lowercase() {
        'c' => Escape::Char('\u{2205}'),
        'd' => Escape::Char('\u{b0}'),
        'p' => Escape::Char('\u{b1}'),
        '%' => Escape::Char('%'),
        'u' | 'o' | 'k' => Escape::Dropped,
        '0'..='9' => {
            let digits = &pending[2..];
            if !digits.iter().all(char::is_ascii_digit) {
                return Escape::Literal;
            }
            if digits.len() < 3 {
                return Escape::Partial;
            }
            let number = digits.iter().fold(0, |value, digit| {
                value * 10 + digit.to_digit(10).expect("a digit")
            });
            Escape::Char(char::from_u32(number).expect("three digits are a character"))
        }
        _ => Escape::Literal,
    }
}

/// The paragraphs text is written in, a paragraph opened at its first character and closed at a
/// line feed or the end of what is written in it, so that no paragraph is empty.
#[derive(Debug, Default)]
pub(super) struct Paragraphs {
    open: bool,
}

impl Paragraphs {
    /// Writes `text`, each line feed ending a paragraph, and empties it.
    pub(super) fn write(&mut self, text: &mut String, xhtml: &mut Xhtml) -> Result<(), Stop> {
        let mut lines = text.split('\n');
        if let Some(first) = lines.next() {
            self.characters(first, xhtml)?;
        }
        for line in lines {
            self.end(xhtml)?;
            self.characters(line, xhtml)?;
        }
        text.clear();
        Ok(())
    }

    fn characters(&mut self, text: &str, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if text.is_empty() {
            return Ok(());
        }
        if !self.open {
            xhtml.start_element("p")?;
            self.open = true;
        }
        xhtml.characters(text)
    }

    /// Ends the open paragraph, if there is one.
    pub(super) fn end(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if std::mem::take(&mut self.open) {
            xhtml.end_element("p")?;
        }
        Ok(())
    }
}
