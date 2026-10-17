//! A port of Apache Commons CSV 1.14.1's `CSVParser` with the format TextAndCSVParser reads
//! with: `CSVFormat.EXCEL` with the sniffed delimiter. That is a double quote as the only quote
//! character, no escape character, no comments, spaces kept, empty lines kept as records of one
//! empty value, text after a closing quote kept in the value (`trailingData`), and a quote left
//! open at the end ending the last value (`lenientEof`). With those settings the parser never
//! fails, so TextAndCSVParser's recovery from a malformed table never runs.
//!
//! Records end at a line feed, a carriage return, or both in that order, outside quotes. A
//! doubled quote inside quotes is one quote. A record holds every value up to its line end; a
//! delimiter right before the text's end adds an empty last value; a line break right before the
//! text's end adds no record.

use std::iter::Peekable;

const QUOTE: char = '"';
const LINE_FEED: char = '\n';
const CARRIAGE_RETURN: char = '\r';

/// The records of a text, read from its characters as they come, each a list of its values.
pub(crate) struct Records<I: Iterator<Item = char>> {
    lexer: Lexer<I>,
}

impl<I: Iterator<Item = char>> Records<I> {
    pub(crate) fn new(text: I, delimiter: char) -> Self {
        Self {
            lexer: Lexer {
                reader: Reader {
                    rest: text.peekable(),
                    last: Last::Undefined,
                },
                delimiter,
                last_token_delimiter: false,
            },
        }
    }
}

impl<I: Iterator<Item = char>> Iterator for Records<I> {
    type Item = Vec<String>;

    fn next(&mut self) -> Option<Vec<String>> {
        let mut values = Vec::new();
        loop {
            let token = self.lexer.next_token();
            match token.kind {
                Kind::Value => values.push(token.content),
                Kind::RecordEnd => {
                    values.push(token.content);
                    break;
                }
                Kind::EndOfFile { ready } => {
                    if ready {
                        values.push(token.content);
                    }
                    break;
                }
            }
        }
        (!values.is_empty()).then_some(values)
    }
}

/// What `ExtendedBufferedReader.getLastChar` reports.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    Undefined,
    Char(char),
    EndOfFile,
}

struct Reader<I: Iterator<Item = char>> {
    rest: Peekable<I>,
    last: Last,
}

impl<I: Iterator<Item = char>> Reader<I> {
    fn read(&mut self) -> Option<char> {
        let c = self.rest.next();
        self.last = c.map_or(Last::EndOfFile, Last::Char);
        c
    }

    fn peek(&mut self) -> Option<char> {
        self.rest.peek().copied()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A value that a delimiter ends (`TOKEN`).
    Value,
    /// A value that a line end ends (`EORECORD`).
    RecordEnd,
    /// The text's end, with a last value when `ready` (`EOF`, `isReady`).
    EndOfFile { ready: bool },
}

struct Token {
    kind: Kind,
    content: String,
}

struct Lexer<I: Iterator<Item = char>> {
    reader: Reader<I>,
    delimiter: char,
    last_token_delimiter: bool,
}

impl<I: Iterator<Item = char>> Lexer<I> {
    fn next_token(&mut self) -> Token {
        let token = self.token();
        self.last_token_delimiter = token.kind == Kind::Value;
        token
    }

    fn token(&mut self) -> Token {
        let last = self.reader.last;
        let c = self.reader.read();
        let end_of_line = self.read_end_of_line(c);
        if last == Last::EndOfFile || (!self.last_token_delimiter && c.is_none()) {
            return token(Kind::EndOfFile { ready: false }, String::new());
        }
        match c {
            Some(c) if c == self.delimiter => token(Kind::Value, String::new()),
            _ if end_of_line => token(Kind::RecordEnd, String::new()),
            Some(QUOTE) => self.enclosed(),
            None => token(Kind::EndOfFile { ready: true }, String::new()),
            Some(c) => self.simple(c),
        }
    }

    /// Whether `c` ends a line, a carriage return taking a line feed after it along.
    fn read_end_of_line(&mut self, c: Option<char>) -> bool {
        if c == Some(CARRIAGE_RETURN) && self.reader.peek() == Some(LINE_FEED) {
            self.reader.read();
        }
        matches!(c, Some(LINE_FEED | CARRIAGE_RETURN))
    }

    fn simple(&mut self, first: char) -> Token {
        let mut content = String::new();
        let mut current = Some(first);
        loop {
            if self.read_end_of_line(current) {
                return token(Kind::RecordEnd, content);
            }
            match current {
                None => return token(Kind::EndOfFile { ready: true }, content),
                Some(c) if c == self.delimiter => return token(Kind::Value, content),
                Some(c) => content.push(c),
            }
            current = self.reader.read();
        }
    }

    fn enclosed(&mut self) -> Token {
        let mut content = String::new();
        loop {
            match self.reader.read() {
                Some(QUOTE) if self.reader.peek() == Some(QUOTE) => {
                    self.reader.read();
                    content.push(QUOTE);
                }
                Some(QUOTE) => return self.after_closing_quote(content),
                None => return token(Kind::EndOfFile { ready: true }, content),
                Some(c) => content.push(c),
            }
        }
    }

    /// The rest of a value after its closing quote, which is kept up to the value's end.
    fn after_closing_quote(&mut self, mut content: String) -> Token {
        loop {
            let c = self.reader.read();
            match c {
                Some(c) if c == self.delimiter => return token(Kind::Value, content),
                None => return token(Kind::EndOfFile { ready: true }, content),
                _ if self.read_end_of_line(c) => return token(Kind::RecordEnd, content),
                Some(c) => content.push(c),
            }
        }
    }
}

fn token(kind: Kind, content: String) -> Token {
    Token { kind, content }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(text: &str, expected: &[&[&str]]) {
        let expected: Vec<Vec<String>> = expected
            .iter()
            .map(|record| record.iter().map(|value| value.to_string()).collect())
            .collect();
        let records: Vec<Vec<String>> = Records::new(text.chars(), ',').collect();
        assert_eq!(records, expected, "{text:?}");
    }

    #[test]
    fn reads_as_commons_csv_does() {
        check("a,b\n1,2\n", &[&["a", "b"], &["1", "2"]]);
        check("a,b\r\n1,2", &[&["a", "b"], &["1", "2"]]);
        check("a,b,\n1,2,", &[&["a", "b", ""], &["1", "2", ""]]);
        check("a\n\nb\r\r\n", &[&["a"], &[""], &["b"], &[""]]);
        check("\"x,\"\"y\"\"\nz\",w\n", &[&["x,\"y\"\nz", "w"]]);
        check("\"x\"y \"z,w", &[&["xy \"z", "w"]]);
        check("a,\"open\nmore", &[&["a", "open\nmore"]]);
        check(" a , b ", &[&[" a ", " b "]]);
        check("", &[]);
        check(",", &[&["", ""]]);
    }
}
