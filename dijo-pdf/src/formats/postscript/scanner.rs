//! The PostScript scanner: a program's bytes as the tokens the language reads, with the
//! document structuring comments that start a line kept for the interpreter, and every other
//! comment skipped.

/// A token of a PostScript program.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Token<'a> {
    Object(Object<'a>),
    ProcStart,
    ProcEnd,
    /// A comment of `%%` or `%!` that starts a line: its text after those two characters, without
    /// the line end.
    Structuring(&'a [u8]),
}

/// A token that stands for an object.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Object<'a> {
    Number(f64),
    /// A literal name, `/name`.
    Literal(&'a [u8]),
    /// An immediately evaluated name, `//name`, which stands for its value when it is read.
    Immediate(&'a [u8]),
    /// An executable name, including the self-delimiting `[`, `]`, `<<` and `>>`.
    Executable(&'a [u8]),
    /// A literal, hexadecimal or ASCII base-85 string, decoded.
    String(Vec<u8>),
}

/// A position in a PostScript program's bytes.
pub(super) struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
    pushed_back: Option<Token<'a>>,
}

/// Whether `byte` is PostScript white space.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0x00 | 0x04)
}

/// Whether `byte` delimits a name or number.
fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

impl<'a> Scanner<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            pushed_back: None,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn at_line_start(&self, start: usize) -> bool {
        start == 0 || matches!(self.bytes[start - 1], b'\n' | b'\r')
    }

    /// The next token, `None` at the end of the program.
    pub(super) fn next_token(&mut self) -> Option<Token<'a>> {
        if let Some(token) = self.pushed_back.take() {
            return Some(token);
        }
        loop {
            let byte = self.peek()?;
            let start = self.at;
            self.at += 1;
            match byte {
                _ if is_space(byte) => {}
                b'%' => {
                    let line = self.rest_of_line();
                    let structuring =
                        self.at_line_start(start) && matches!(line.first(), Some(b'%' | b'!'));
                    if structuring {
                        return Some(Token::Structuring(&line[1..]));
                    }
                }
                b'(' => return Some(Token::Object(Object::String(self.literal_string()))),
                b'<' => match self.peek() {
                    Some(b'<') => {
                        self.at += 1;
                        return Some(Token::Object(Object::Executable(b"<<")));
                    }
                    Some(b'~') => {
                        self.at += 1;
                        return Some(Token::Object(Object::String(self.base85_string())));
                    }
                    _ => return Some(Token::Object(Object::String(self.hex_string()))),
                },
                b'>' => {
                    if self.peek() == Some(b'>') {
                        self.at += 1;
                        return Some(Token::Object(Object::Executable(b">>")));
                    }
                }
                b'[' => return Some(Token::Object(Object::Executable(b"["))),
                b']' => return Some(Token::Object(Object::Executable(b"]"))),
                b'{' => return Some(Token::ProcStart),
                b'}' => return Some(Token::ProcEnd),
                b')' => {}
                b'/' => {
                    let immediate = self.peek() == Some(b'/');
                    if immediate {
                        self.at += 1;
                    }
                    let name = self.regular();
                    self.skip_token_end();
                    return Some(match immediate {
                        true => Token::Object(Object::Immediate(name)),
                        false => Token::Object(Object::Literal(name)),
                    });
                }
                _ => {
                    self.at = start;
                    let text = self.regular();
                    self.skip_token_end();
                    return Some(match number(text) {
                        Some(value) => Token::Object(Object::Number(value)),
                        None => Token::Object(Object::Executable(text)),
                    });
                }
            }
        }
    }

    /// Whether a structuring comment starts here, which no string runs on past: a string that
    /// would is data the program reads in a way the interpreter does not follow, and it ends there.
    fn at_structuring_line(&self) -> bool {
        self.at > 0
            && matches!(self.bytes[self.at - 1], b'\n' | b'\r')
            && self.bytes[self.at..].starts_with(b"%%")
    }

    /// Makes `token` the next token again.
    pub(super) fn push_back(&mut self, token: Token<'a>) {
        self.pushed_back = Some(token);
    }

    /// The run of regular characters from here.
    fn regular(&mut self) -> &'a [u8] {
        let start = self.at;
        while let Some(byte) = self.peek() {
            if is_space(byte) || is_delimiter(byte) {
                break;
            }
            self.at += 1;
        }
        &self.bytes[start..self.at]
    }

    /// The rest of the line from here, without its line end, which is passed.
    fn rest_of_line(&mut self) -> &'a [u8] {
        let start = self.at;
        let end = memchr::memchr2(b'\n', b'\r', &self.bytes[start..])
            .map_or(self.bytes.len(), |offset| start + offset);
        self.at = end;
        self.skip_line_end();
        &self.bytes[start..end]
    }

    /// Passes the one white-space character that ends a name or number, as the language's scanner
    /// does, so that data the program reads from its own file starts after it.
    fn skip_token_end(&mut self) {
        match self.peek() {
            Some(b'\r' | b'\n') => self.skip_line_end(),
            Some(byte) if is_space(byte) => self.at += 1,
            _ => {}
        }
    }

    /// Reads up to `count` bytes of the program as data.
    pub(super) fn read_bytes(&mut self, count: usize) -> &'a [u8] {
        let start = self.at;
        self.skip_bytes(count);
        &self.bytes[start..self.at]
    }

    /// Reads up to `count` bytes of the program as hexadecimal data, other characters skipped.
    pub(super) fn read_hex(&mut self, count: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(count.min(self.bytes.len() - self.at));
        let mut high: Option<u8> = None;
        while bytes.len() < count {
            let Some(byte) = self.peek() else {
                break;
            };
            self.at += 1;
            let Some(digit) = char::from(byte).to_digit(16) else {
                continue;
            };
            match high.take() {
                Some(high) => bytes.push(high << 4 | digit as u8),
                None => high = Some(digit as u8),
            }
        }
        bytes
    }

    /// Reads a line of the program as data, `None` at its end.
    pub(super) fn read_line(&mut self) -> Option<&'a [u8]> {
        (self.at < self.bytes.len()).then(|| self.rest_of_line())
    }

    fn skip_line_end(&mut self) {
        match self.peek() {
            Some(b'\r') => {
                self.at += 1;
                if self.peek() == Some(b'\n') {
                    self.at += 1;
                }
            }
            Some(b'\n') => self.at += 1,
            _ => {}
        }
    }

    /// Skips `count` bytes, as a `%%BeginBinary` or `%%BeginData` comment announces them.
    pub(super) fn skip_bytes(&mut self, count: usize) {
        self.at = self.at.saturating_add(count).min(self.bytes.len());
    }

    /// Skips `count` lines, as a `%%BeginData` comment announces them.
    pub(super) fn skip_lines(&mut self, count: usize) {
        for _ in 0..count {
            if self.at >= self.bytes.len() {
                break;
            }
            self.rest_of_line();
        }
    }

    /// Skips past the next `marker`, or to the end where there is none.
    pub(super) fn skip_past(&mut self, marker: &[u8]) {
        self.at = memchr::memmem::find(&self.bytes[self.at..], marker)
            .map_or(self.bytes.len(), |offset| self.at + offset + marker.len());
    }

    /// A literal string after its `(`: balanced parentheses kept, escapes resolved, and every
    /// unescaped line end read as a line feed. One the program ends inside ends there.
    fn literal_string(&mut self) -> Vec<u8> {
        let mut text = Vec::new();
        let mut depth = 0usize;
        while let Some(byte) = self.peek() {
            if self.at_structuring_line() {
                break;
            }
            self.at += 1;
            match byte {
                b'(' => {
                    depth += 1;
                    text.push(byte);
                }
                b')' if depth == 0 => break,
                b')' => {
                    depth -= 1;
                    text.push(byte);
                }
                b'\r' => {
                    if self.peek() == Some(b'\n') {
                        self.at += 1;
                    }
                    text.push(b'\n');
                }
                b'\\' => self.escape(&mut text),
                _ => text.push(byte),
            }
        }
        text
    }

    /// The character an escape after its backslash stands for, if any.
    fn escape(&mut self, text: &mut Vec<u8>) {
        let Some(byte) = self.peek() else {
            return;
        };
        self.at += 1;
        match byte {
            b'n' => text.push(b'\n'),
            b'r' => text.push(b'\r'),
            b't' => text.push(b'\t'),
            b'b' => text.push(0x08),
            b'f' => text.push(0x0C),
            b'\r' => {
                if self.peek() == Some(b'\n') {
                    self.at += 1;
                }
            }
            b'\n' => {}
            b'0'..=b'7' => {
                let mut code = u32::from(byte - b'0');
                for _ in 0..2 {
                    match self.peek() {
                        Some(digit @ b'0'..=b'7') => {
                            self.at += 1;
                            code = code * 8 + u32::from(digit - b'0');
                        }
                        _ => break,
                    }
                }
                text.push((code & 0xFF) as u8);
            }
            other => text.push(other),
        }
    }

    /// A hexadecimal string after its `<`: white space ignored, an odd last digit followed by 0.
    fn hex_string(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut high: Option<u8> = None;
        while let Some(byte) = self.peek() {
            if self.at_structuring_line() {
                break;
            }
            self.at += 1;
            if byte == b'>' {
                break;
            }
            let Some(digit) = char::from(byte).to_digit(16) else {
                continue;
            };
            let digit = digit as u8;
            match high.take() {
                Some(high) => bytes.push(high << 4 | digit),
                None => high = Some(digit),
            }
        }
        if let Some(high) = high {
            bytes.push(high << 4);
        }
        bytes
    }

    /// An ASCII base-85 string after its `<~`, up to its `~>`.
    fn base85_string(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut group = [0u8; 5];
        let mut filled = 0;
        while let Some(byte) = self.peek() {
            if self.at_structuring_line() {
                break;
            }
            self.at += 1;
            match byte {
                b'~' => {
                    if self.peek() == Some(b'>') {
                        self.at += 1;
                    }
                    break;
                }
                b'z' if filled == 0 => bytes.extend_from_slice(&[0; 4]),
                b'!'..=b'u' => {
                    group[filled] = byte - b'!';
                    filled += 1;
                    if filled == 5 {
                        bytes.extend_from_slice(&base85_group(&group)[..4]);
                        filled = 0;
                    }
                }
                _ => {}
            }
        }
        if filled > 1 {
            group[filled..].fill(84);
            bytes.extend_from_slice(&base85_group(&group)[..filled - 1]);
        }
        bytes
    }
}

/// The four bytes of a group of five base-85 digits.
fn base85_group(group: &[u8; 5]) -> [u8; 4] {
    let value = group.iter().fold(0u32, |value, &digit| {
        value.wrapping_mul(85).wrapping_add(u32::from(digit))
    });
    value.to_be_bytes()
}

/// A PostScript number: an integer, a real, or `base#digits` in a base from 2 to 36.
fn number(text: &[u8]) -> Option<f64> {
    let text = std::str::from_utf8(text).ok()?;
    if let Some((base, digits)) = text.split_once('#') {
        let base: u32 = base.parse().ok().filter(|base| (2..=36).contains(base))?;
        return u64::from_str_radix(digits, base)
            .ok()
            .map(|value| value as f64);
    }
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(at) => (&unsigned[..at], Some(&unsigned[at + 1..])),
        None => (unsigned, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let valid_mantissa =
        digits(whole) && digits(fraction) && !(whole.is_empty() && fraction.is_empty());
    let valid_exponent = exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !exponent.is_empty() && digits(exponent)
    });
    if !(valid_mantissa && valid_exponent) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(program: &[u8]) -> Vec<Token<'_>> {
        let mut scanner = Scanner::new(program);
        std::iter::from_fn(|| scanner.next_token()).collect()
    }

    #[test]
    fn reads_numbers_names_and_delimiters() {
        assert_eq!(
            tokens(b"12 -3.5 .5 1e2 16#FF /Name //Bound name [ ] << >> { }"),
            [
                Token::Object(Object::Number(12.0)),
                Token::Object(Object::Number(-3.5)),
                Token::Object(Object::Number(0.5)),
                Token::Object(Object::Number(100.0)),
                Token::Object(Object::Number(255.0)),
                Token::Object(Object::Literal(b"Name")),
                Token::Object(Object::Immediate(b"Bound")),
                Token::Object(Object::Executable(b"name")),
                Token::Object(Object::Executable(b"[")),
                Token::Object(Object::Executable(b"]")),
                Token::Object(Object::Executable(b"<<")),
                Token::Object(Object::Executable(b">>")),
                Token::ProcStart,
                Token::ProcEnd,
            ]
        );
        assert_eq!(
            tokens(b"1.2.3 e5 -"),
            [
                Token::Object(Object::Executable(b"1.2.3")),
                Token::Object(Object::Executable(b"e5")),
                Token::Object(Object::Executable(b"-")),
            ]
        );
    }

    #[test]
    fn decodes_literal_strings_with_nesting_and_escapes() {
        assert_eq!(
            tokens(b"(a (nested) \\(paren\\) \\101\\7\\0123 \\\nline\r\nend\\q)"),
            [Token::Object(Object::String(
                b"a (nested) (paren) A\x07\n3 line\nendq".to_vec()
            ))]
        );
        assert_eq!(
            tokens(b"(open"),
            [Token::Object(Object::String(b"open".to_vec()))]
        );
    }

    #[test]
    fn ends_strings_at_a_structuring_comment() {
        assert_eq!(
            tokens(b"(open\n%%Page: 2 2\n<41\r%%EOF"),
            [
                Token::Object(Object::String(b"open\n".to_vec())),
                Token::Structuring(b"Page: 2 2"),
                Token::Object(Object::String(b"A".to_vec())),
                Token::Structuring(b"EOF"),
            ]
        );
    }

    #[test]
    fn decodes_hex_and_base85_strings() {
        assert_eq!(
            tokens(b"<48 65 6c6C 6f2> <~87cURD]i,\"Ebo80~>"),
            [
                Token::Object(Object::String(b"Hello ".to_vec())),
                Token::Object(Object::String(b"Hello World!".to_vec())),
            ]
        );
        assert_eq!(
            tokens(b"<~z~>"),
            [Token::Object(Object::String(vec![0; 4]))]
        );
    }

    #[test]
    fn keeps_structuring_comments_at_line_starts_only() {
        assert_eq!(
            tokens(b"%!PS-Adobe-3.0\n%%Title: x\r\n1 % a comment %%Not\n%%EOF"),
            [
                Token::Structuring(b"PS-Adobe-3.0"),
                Token::Structuring(b"Title: x"),
                Token::Object(Object::Number(1.0)),
                Token::Structuring(b"EOF"),
            ]
        );
    }

    #[test]
    fn reads_data_after_the_white_space_that_ends_a_name() {
        let mut scanner =
            Scanner::new(b"readline\r\nfirst line\nreadstring ab1 readhexstring 4 1\n");
        assert_eq!(
            scanner.next_token(),
            Some(Token::Object(Object::Executable(b"readline")))
        );
        assert_eq!(scanner.read_line(), Some(&b"first line"[..]));
        assert_eq!(
            scanner.next_token(),
            Some(Token::Object(Object::Executable(b"readstring")))
        );
        assert_eq!(scanner.read_bytes(2), b"ab");
        assert_eq!(
            scanner.next_token(),
            Some(Token::Object(Object::Number(1.0)))
        );
        assert_eq!(
            scanner.next_token(),
            Some(Token::Object(Object::Executable(b"readhexstring")))
        );
        assert_eq!(scanner.read_hex(1), [0x41]);
        assert_eq!(scanner.read_line(), Some(&b""[..]));
        assert_eq!(scanner.read_line(), None);
    }

    #[test]
    fn skips_announced_data_and_encrypted_sections() {
        let mut scanner = Scanner::new(b"abc\ndef\nghi tail");
        scanner.skip_lines(2);
        assert_eq!(
            scanner.next_token(),
            Some(Token::Object(Object::Executable(b"ghi")))
        );
        let mut scanner = Scanner::new(b"\x00(\x01 0000 cleartomark after");
        scanner.skip_past(b"cleartomark");
        assert_eq!(
            scanner.next_token(),
            Some(Token::Object(Object::Executable(b"after")))
        );
        let mut scanner = Scanner::new(b"12345after");
        scanner.skip_bytes(5);
        assert_eq!(
            scanner.next_token(),
            Some(Token::Object(Object::Executable(b"after")))
        );
    }
}
