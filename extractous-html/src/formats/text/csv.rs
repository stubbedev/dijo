//! A port of Apache Commons CSV 1.14.1's `CSVParser`, with the two formats Tika reads with:
//! [`Excel`], `CSVFormat.EXCEL` with the delimiter TextAndCSVParser sniffs, and [`TabDelimited`],
//! `CSVFormat.TDF`, which the ISA-Tab parser reads with.
//!
//! Both take a double quote as the only quote character, no escape character and no comments.
//! Excel keeps spaces and empty lines, an empty line a record of one empty value, keeps text
//! after a closing quote in the value (`trailingData`), and ends the last value at a quote left
//! open at the end (`lenientEof`), so that it never fails and TextAndCSVParser's recovery from a
//! malformed table never runs. TDF drops the white space around each value
//! (`ignoreSurroundingSpaces`) and skips empty lines (`ignoreEmptyLines`), and fails on anything
//! but white space after a closing quote and on a quote left open at the end.
//!
//! Records end at a line feed, a carriage return, or both in that order, outside quotes. A
//! doubled quote inside quotes is one quote. A record holds every value up to its line end; a
//! delimiter right before the text's end adds an empty last value; a line break right before the
//! text's end adds no record.

use std::convert::Infallible;
use std::iter::Peekable;

use crate::unicode::is_whitespace;

const QUOTE: char = '"';
const LINE_FEED: char = '\n';
const CARRIAGE_RETURN: char = '\r';
const TAB: char = '\t';

/// What a `CSVFormat` decides of how its records are read.
pub trait Format {
    /// What reading fails with: [`Infallible`] for a format that never fails.
    type Error;

    /// `ignoreSurroundingSpaces`.
    const TRIMS: bool;

    /// `ignoreEmptyLines`.
    const SKIPS_EMPTY_LINES: bool;

    fn delimiter(&self) -> char;

    /// Whether `character`, after a value's closing quote and before its end, is kept in the
    /// value (`trailingData`), passed over as white space, or fails the read.
    fn after_quote(&self, character: char) -> Result<bool, Self::Error>;

    /// What a quote left open at the end of the text does: end the value (`lenientEof`) or fail
    /// the read.
    fn open_at_end(&self) -> Result<(), Self::Error>;
}

/// `CSVFormat.EXCEL` with this delimiter.
#[derive(Debug, Clone, Copy)]
pub struct Excel(pub char);

impl Format for Excel {
    type Error = Infallible;
    const TRIMS: bool = false;
    const SKIPS_EMPTY_LINES: bool = false;

    fn delimiter(&self) -> char {
        self.0
    }

    fn after_quote(&self, _character: char) -> Result<bool, Infallible> {
        Ok(true)
    }

    fn open_at_end(&self) -> Result<(), Infallible> {
        Ok(())
    }
}

/// `CSVFormat.TDF`.
#[derive(Debug, Clone, Copy)]
pub struct TabDelimited;

impl Format for TabDelimited {
    /// The message of the `CSVException` Commons CSV throws.
    type Error = String;
    const TRIMS: bool = true;
    const SKIPS_EMPTY_LINES: bool = true;

    fn delimiter(&self) -> char {
        TAB
    }

    fn after_quote(&self, character: char) -> Result<bool, String> {
        if is_whitespace(character) {
            Ok(false)
        } else {
            Err(format!(
                "Invalid character {character:?} between encapsulated token and delimiter"
            ))
        }
    }

    fn open_at_end(&self) -> Result<(), String> {
        Err("EOF reached before encapsulated token finished".to_owned())
    }
}

/// The records of a text, read from its characters as they come, each a list of its values, in
/// the format `F`; after a failure, none.
pub struct Records<I: Iterator<Item = char>, F: Format> {
    lexer: Lexer<I, F>,
    failed: bool,
}

impl<I: Iterator<Item = char>, F: Format> Records<I, F> {
    pub fn new(text: I, format: F) -> Self {
        Self {
            lexer: Lexer {
                reader: Reader {
                    rest: text.peekable(),
                    last: Last::Undefined,
                },
                format,
                last_token_delimiter: false,
            },
            failed: false,
        }
    }
}

impl<I: Iterator<Item = char>, F: Format> Iterator for Records<I, F> {
    type Item = Result<Vec<String>, F::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let mut values = Vec::new();
        loop {
            let token = match self.lexer.next_token() {
                Ok(token) => token,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            };
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
        (!values.is_empty()).then_some(Ok(values))
    }
}

/// What `ExtendedBufferedReader.getLastChar` reports.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    Undefined,
    Char(char),
    EndOfFile,
}

impl Last {
    /// `Lexer.isStartOfLine`.
    fn starts_line(self) -> bool {
        matches!(
            self,
            Self::Undefined | Self::Char(LINE_FEED | CARRIAGE_RETURN)
        )
    }
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

struct Lexer<I: Iterator<Item = char>, F: Format> {
    reader: Reader<I>,
    format: F,
    last_token_delimiter: bool,
}

impl<I: Iterator<Item = char>, F: Format> Lexer<I, F> {
    fn next_token(&mut self) -> Result<Token, F::Error> {
        let token = self.token()?;
        self.last_token_delimiter = token.kind == Kind::Value;
        Ok(token)
    }

    fn token(&mut self) -> Result<Token, F::Error> {
        let mut last = self.reader.last;
        let mut c = self.reader.read();
        let mut end_of_line = self.read_end_of_line(c);
        if F::SKIPS_EMPTY_LINES {
            while end_of_line && last.starts_line() {
                last = c.map_or(Last::EndOfFile, Last::Char);
                c = self.reader.read();
                end_of_line = self.read_end_of_line(c);
                if c.is_none() {
                    return Ok(token(Kind::EndOfFile { ready: false }, String::new()));
                }
            }
        }
        if last == Last::EndOfFile || (!self.last_token_delimiter && c.is_none()) {
            return Ok(token(Kind::EndOfFile { ready: false }, String::new()));
        }
        if F::TRIMS {
            while let Some(space) = c {
                if !is_whitespace(space) || space == self.format.delimiter() || end_of_line {
                    break;
                }
                c = self.reader.read();
                end_of_line = self.read_end_of_line(c);
            }
        }
        match c {
            Some(c) if c == self.format.delimiter() => Ok(token(Kind::Value, String::new())),
            _ if end_of_line => Ok(token(Kind::RecordEnd, String::new())),
            Some(QUOTE) => self.enclosed(),
            None => Ok(token(Kind::EndOfFile { ready: true }, String::new())),
            Some(c) => Ok(self.simple(c)),
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
        let kind = loop {
            if self.read_end_of_line(current) {
                break Kind::RecordEnd;
            }
            match current {
                None => break Kind::EndOfFile { ready: true },
                Some(c) if c == self.format.delimiter() => break Kind::Value,
                Some(c) => content.push(c),
            }
            current = self.reader.read();
        };
        if F::TRIMS {
            let kept = content.trim_end_matches(is_whitespace).len();
            content.truncate(kept);
        }
        token(kind, content)
    }

    fn enclosed(&mut self) -> Result<Token, F::Error> {
        let mut content = String::new();
        loop {
            match self.reader.read() {
                Some(QUOTE) if self.reader.peek() == Some(QUOTE) => {
                    self.reader.read();
                    content.push(QUOTE);
                }
                Some(QUOTE) => return self.after_closing_quote(content),
                None => {
                    self.format.open_at_end()?;
                    return Ok(token(Kind::EndOfFile { ready: true }, content));
                }
                Some(c) => content.push(c),
            }
        }
    }

    /// The rest of a value after its closing quote, up to the value's end.
    fn after_closing_quote(&mut self, mut content: String) -> Result<Token, F::Error> {
        loop {
            let c = self.reader.read();
            match c {
                Some(c) if c == self.format.delimiter() => return Ok(token(Kind::Value, content)),
                None => return Ok(token(Kind::EndOfFile { ready: true }, content)),
                _ if self.read_end_of_line(c) => return Ok(token(Kind::RecordEnd, content)),
                Some(c) => {
                    if self.format.after_quote(c)? {
                        content.push(c);
                    }
                }
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

    fn records<F: Format>(text: &str, format: F) -> Vec<Result<Vec<String>, F::Error>> {
        Records::new(text.chars(), format).collect()
    }

    fn check(text: &str, expected: &[&[&str]]) {
        let expected: Vec<Vec<String>> = expected
            .iter()
            .map(|record| record.iter().map(|value| value.to_string()).collect())
            .collect();
        let records: Vec<Vec<String>> = records(text, Excel(','))
            .into_iter()
            .map(|record| {
                let Ok(record) = record;
                record
            })
            .collect();
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

    #[test]
    fn reads_tab_delimited_text_as_commons_csv_does() {
        let read = |text: &str| -> Vec<Vec<String>> {
            records(text, TabDelimited)
                .into_iter()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(
            read("\"Source Name\"\t \"Sample Name\" \t\n\n\r\n culture1 \tC-0.07\t\n"),
            [
                vec!["Source Name", "Sample Name", ""],
                vec!["culture1", "C-0.07", ""]
            ]
        );
        assert_eq!(read("a\t\"b\nc\"\r\nd"), [vec!["a", "b\nc"], vec!["d"]]);
        let failed = records("a\t\"b\"c\td\nnext", TabDelimited);
        assert_eq!(failed.len(), 1);
        assert!(failed[0].is_err());
        assert!(records("\"open", TabDelimited)[0].is_err());
    }
}
