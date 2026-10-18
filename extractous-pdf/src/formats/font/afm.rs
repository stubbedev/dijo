//! Adobe font metrics files as FontBox 3's `AFMParser` reads them in full: every key it knows,
//! the character metrics, kerning data and composites, failing where it throws. What Tika's
//! `AdobeFontMetricParser` reads of the result is kept, its comments handed on one at a time, so
//! that a file of any size is read in bounded memory, its pages released as it is read.

use crate::input::Paging;
use crate::java;

/// Why `AFMParser` fails, which fails the parse.
pub(super) type Thrown = String;

/// What `AdobeFontMetricParser` reads of FontBox's `FontMetrics`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct FontMetrics {
    pub(super) afm_version: f32,
    pub(super) font_name: Option<String>,
    pub(super) full_name: Option<String>,
    pub(super) family_name: Option<String>,
    pub(super) weight: Option<String>,
    pub(super) font_version: Option<String>,
    pub(super) notice: Option<String>,
    pub(super) underline_thickness: f32,
    widths: Widths,
}

/// The positive `WX` of the character metrics, summed in their order as
/// `getAverageCharacterWidth` sums them, in `float` arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Widths {
    total: f32,
    count: f32,
}

impl Widths {
    fn add(&mut self, width: f32) {
        if width > 0.0 {
            self.total += width;
            self.count += 1.0;
        }
    }
}

impl FontMetrics {
    /// `getAverageCharacterWidth`: the mean of the positive widths, in `float` arithmetic.
    pub(super) fn average_character_width(&self) -> f32 {
        if self.widths.total > 0.0 {
            self.widths.total / self.widths.count
        } else {
            0.0
        }
    }
}

/// Why a parse ends early: `AFMParser` throws, or the receiver of the comments stops it.
#[derive(Debug, PartialEq)]
pub(super) enum Interrupted<E> {
    Thrown(Thrown),
    By(E),
}

impl<E> From<Thrown> for Interrupted<E> {
    fn from(thrown: Thrown) -> Self {
        Self::Thrown(thrown)
    }
}

/// The byte stream `AFMParser` reads, each byte a Latin-1 character and the end of the stream
/// `(char) -1`, U+FFFF.
struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
    paging: &'a Paging<'a>,
}

const END: char = '\u{FFFF}';

fn is_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

impl Input<'_> {
    fn read(&mut self) -> Option<char> {
        let byte = self.bytes.get(self.at)?;
        self.at += 1;
        self.paging.read(1);
        Some(char::from(*byte))
    }

    /// `readString` (`stop` whitespace) or `readLine` (`stop` a line end): the first character
    /// after the white space, then up to the end or a stop.
    fn read_until(&mut self, stop: fn(char) -> bool) -> String {
        let mut text = String::new();
        let mut next = self.read();
        while next.is_some_and(is_whitespace) {
            next = self.read();
        }
        text.push(next.unwrap_or(END));
        loop {
            match self.read() {
                Some(character) if !stop(character) => text.push(character),
                _ => return text,
            }
        }
    }

    fn string(&mut self) -> String {
        self.read_until(is_whitespace)
    }

    fn line(&mut self) -> String {
        self.read_until(|character| character == '\r' || character == '\n')
    }

    fn command(&mut self, expected: &str) -> Result<(), Thrown> {
        let command = self.string();
        if command == expected {
            Ok(())
        } else {
            Err(format!("Error: Expected '{expected}' actual '{command}'"))
        }
    }

    fn float(&mut self) -> Result<f32, Thrown> {
        parse_float(&self.string())
    }

    fn int(&mut self) -> Result<i32, Thrown> {
        parse_int(&self.string(), 10)
    }
}

/// `Float.parseFloat`, which fails the document where it throws.
fn parse_float(text: &str) -> Result<f32, Thrown> {
    java::parse_float(text).ok_or_else(|| format!("Error parsing AFM document: {text}"))
}

/// `Integer.parseInt(text, radix)`.
fn parse_int(text: &str, radix: u32) -> Result<i32, Thrown> {
    let parsed = if radix == 16 {
        java::parse_hex_int(text)
    } else {
        java::parse_int(text)
    };
    parsed.ok_or_else(|| format!("Error parsing AFM document: {text}"))
}

/// A `StringTokenizer`'s tokens, past the last of which `nextToken` throws.
struct Tokens<'a> {
    tokens: std::vec::IntoIter<&'a str>,
}

impl<'a> Tokens<'a> {
    fn new(text: &'a str, delimiters: &'static [char]) -> Self {
        let tokens: Vec<&str> = text
            .split(|character| delimiters.contains(&character))
            .filter(|token| !token.is_empty())
            .collect();
        Self {
            tokens: tokens.into_iter(),
        }
    }

    fn next(&mut self) -> Result<&'a str, Thrown> {
        self.tokens
            .next()
            .ok_or_else(|| "NoSuchElementException".to_owned())
    }

    fn has_more(&self) -> bool {
        self.tokens.len() > 0
    }

    /// `verifySemicolon`.
    fn semicolon(&mut self) -> Result<(), Thrown> {
        if !self.has_more() {
            return Err("CharMetrics is missing a semicolon after a command".to_owned());
        }
        let token = self.next()?;
        if token == ";" {
            Ok(())
        } else {
            Err(format!(
                "Error: Expected semicolon in stream actual='{token}'"
            ))
        }
    }
}

const WHITESPACE: &[char] = &[' ', '\t', '\n', '\r', '\u{C}'];
const COMPOSITE_DELIMITERS: &[char] = &[' ', ';'];

/// `AFMParser.parse()`, each comment handed to `comment` as it is read, which may stop the
/// parse.
pub(super) fn parse<'a, E>(
    bytes: &'a [u8],
    paging: &'a Paging<'a>,
    mut comment: impl FnMut(String) -> Result<(), E>,
) -> Result<FontMetrics, Interrupted<E>> {
    let mut input = Input {
        bytes,
        at: 0,
        paging,
    };
    input.command("StartFontMetrics")?;
    let mut metrics = FontMetrics {
        afm_version: input.float()?,
        ..FontMetrics::default()
    };
    loop {
        let command = input.string();
        match command.as_str() {
            "EndFontMetrics" => return Ok(metrics),
            "FontName" => metrics.font_name = Some(input.line()),
            "FullName" => metrics.full_name = Some(input.line()),
            "FamilyName" => metrics.family_name = Some(input.line()),
            "Weight" => metrics.weight = Some(input.line()),
            "Version" => metrics.font_version = Some(input.line()),
            "Notice" => metrics.notice = Some(input.line()),
            "EncodingScheme" | "CharacterSet" => {
                input.line();
            }
            "FontBBox" => {
                for _ in 0..4 {
                    input.float()?;
                }
            }
            "MappingScheme" | "EscChar" | "Characters" => {
                input.int()?;
            }
            "IsBaseFont" | "IsFixedV" | "IsFixedPitch" => {
                input.string();
            }
            "VVector" | "CharWidth" => {
                input.float()?;
                input.float()?;
            }
            "CapHeight" | "XHeight" | "Ascender" | "Descender" | "StdHW" | "StdVW"
            | "UnderlinePosition" | "ItalicAngle" => {
                input.float()?;
            }
            "UnderlineThickness" => metrics.underline_thickness = input.float()?,
            "Comment" => comment(input.line()).map_err(Interrupted::By)?,
            "StartCharMetrics" => {
                let count = input.int()?;
                for _ in 0..count.max(0) {
                    metrics.widths.add(char_metric(&input.line())?);
                }
                input.command("EndCharMetrics")?;
            }
            "StartKernData" => kern_data(&mut input)?,
            "StartComposites" => {
                let count = input.int()?;
                for _ in 0..count.max(0) {
                    composite(&input.line())?;
                }
                input.command("EndComposites")?;
            }
            _ => return Err(format!("Unknown AFM key '{command}'").into()),
        }
    }
}

/// `parseCharMetric`: the line's commands, each followed by a semicolon; its `WX`.
fn char_metric(line: &str) -> Result<f32, Thrown> {
    let mut tokens = Tokens::new(line, WHITESPACE);
    let mut width = 0.0;
    while tokens.has_more() {
        let command = tokens.next()?;
        match command {
            "C" => {
                parse_int(tokens.next()?, 10)?;
            }
            "CH" => {
                parse_int(tokens.next()?, 16)?;
            }
            "WX" => width = parse_float(tokens.next()?)?,
            "N" => {
                tokens.next()?;
            }
            "B" => {
                for _ in 0..4 {
                    parse_float(tokens.next()?)?;
                }
            }
            "L" => {
                tokens.next()?;
                tokens.next()?;
            }
            "W0X" | "W1X" | "WY" | "W0Y" | "W1Y" => {
                parse_float(tokens.next()?)?;
            }
            "W" | "W0" | "W1" | "VV" => {
                parse_float(tokens.next()?)?;
                parse_float(tokens.next()?)?;
            }
            _ => return Err(format!("Unknown CharMetrics command '{command}'")),
        }
        tokens.semicolon()?;
    }
    Ok(width)
}

/// `parseKernData`.
fn kern_data(input: &mut Input) -> Result<(), Thrown> {
    loop {
        let command = input.string();
        match command.as_str() {
            "EndKernData" => return Ok(()),
            "StartTrackKern" => {
                let count = input.int()?;
                for _ in 0..count.max(0) {
                    input.int()?;
                    for _ in 0..4 {
                        input.float()?;
                    }
                }
                input.command("EndTrackKern")?;
            }
            "StartKernPairs" | "StartKernPairs0" | "StartKernPairs1" => {
                let count = input.int()?;
                for _ in 0..count.max(0) {
                    kern_pair(input)?;
                }
                input.command("EndKernPairs")?;
            }
            _ => return Err(format!("Unknown kerning data type '{command}'")),
        }
    }
}

/// `parseKernPair`.
fn kern_pair(input: &mut Input) -> Result<(), Thrown> {
    let command = input.string();
    match command.as_str() {
        "KP" => {
            input.string();
            input.string();
            input.float()?;
            input.float()?;
        }
        "KPH" => {
            hex_to_string(&input.string())?;
            hex_to_string(&input.string())?;
            input.float()?;
            input.float()?;
        }
        "KPX" | "KPY" => {
            input.string();
            input.string();
            input.float()?;
        }
        _ => {
            return Err(format!(
                "Error expected kern pair command actual='{command}'"
            ))
        }
    }
    Ok(())
}

/// `hexToString`, for what it throws: a string that is not hex digits in angle brackets, an
/// odd count of digits included.
fn hex_to_string(text: &str) -> Result<(), Thrown> {
    let characters: Vec<char> = text.chars().collect();
    if characters.len() < 2 {
        return Err(format!(
            "Error: Expected hex string of length >= 2 not='{text}"
        ));
    }
    if characters[0] != '<' || characters[characters.len() - 1] != '>' {
        return Err(format!(
            "String should be enclosed by angle brackets '{text}'"
        ));
    }
    let digits = &characters[1..characters.len() - 1];
    for pair in digits.chunks(2) {
        let [high, low] = pair else {
            return Err("StringIndexOutOfBoundsException".to_owned());
        };
        parse_int(&format!("{high}{low}"), 16)?;
    }
    Ok(())
}

/// `parseComposite`, for what it throws.
fn composite(line: &str) -> Result<(), Thrown> {
    let mut tokens = Tokens::new(line, COMPOSITE_DELIMITERS);
    let cc = tokens.next()?;
    if cc != "CC" {
        return Err(format!("Expected 'CC' actual='{cc}'"));
    }
    tokens.next()?;
    let parts = parse_int(tokens.next()?, 10)?;
    for _ in 0..parts.max(0) {
        let pcc = tokens.next()?;
        if pcc != "PCC" {
            return Err(format!("Expected 'PCC' actual='{pcc}'"));
        }
        tokens.next()?;
        parse_int(tokens.next()?, 10)?;
        parse_int(tokens.next()?, 10)?;
    }
    Ok(())
}

/// `AdobeFontMetricParser.extractCreationDate` of `comment`, if it is the one it takes the date
/// from, which the first comment matching `.*Creation\sDate.*` is: the date after its first colon
/// and the character after it.
pub(super) fn creation_date(comment: &str) -> Option<Result<String, Thrown>> {
    let matches = !comment.chars().any(java::is_line_terminator)
        && comment.match_indices("Creation").any(|(at, _)| {
            let rest = &comment[at + "Creation".len()..];
            rest.chars().next().is_some_and(java::is_regex_space) && rest[1..].starts_with("Date")
        });
    if !matches {
        return None;
    }
    let characters: Vec<char> = comment.chars().collect();
    let start = characters
        .iter()
        .position(|&character| character == ':')
        .map_or(1, |colon| colon + 2);
    Some(
        characters
            .get(start..)
            .map(|date| date.iter().collect())
            .ok_or_else(|| "StringIndexOutOfBoundsException".to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::input::Pages;

    /// The metrics of `bytes` and their comments.
    fn read(bytes: &[u8]) -> Result<(FontMetrics, Vec<String>), Interrupted<()>> {
        let paging = Paging::new(Pages::IN_MEMORY);
        let mut comments = Vec::new();
        let metrics = parse(bytes, &paging, |comment| {
            comments.push(comment);
            Ok(())
        })?;
        Ok((metrics, comments))
    }

    #[test]
    fn reads_metrics_as_fontbox_does() {
        let (metrics, comments) = read(
            b"StartFontMetrics 4.1\r\nComment One\r\nFullName  A B \r\nStartCharMetrics 2\r\n\
              C 1 ; WX 10.5 ; N a ;\r\nC 2 ; WX 0 ; N b ;\r\nEndCharMetrics\r\nEndFontMetrics",
        )
        .unwrap();
        assert_eq!(metrics.afm_version, 4.1);
        assert_eq!(metrics.full_name.as_deref(), Some("A B "));
        assert_eq!(comments, ["One"]);
        assert_eq!(metrics.average_character_width(), 10.5);
        assert!(read(b"StartFontMetrics 4.1\nC 1 ;\nEndFontMetrics").is_err());
        assert!(read(b"StartFontMetrics x").is_err());
        assert!(read(b"StartFontMetrics 1\nFontName x").is_err());
    }

    #[test]
    fn a_comment_receiver_stops_the_parse() {
        let paging = Paging::new(Pages::IN_MEMORY);
        let stopped = parse(
            b"StartFontMetrics 2\nComment a\nBogus\nEndFontMetrics",
            &paging,
            |_| Err("stop"),
        );
        assert_eq!(stopped, Err(Interrupted::By("stop")));
    }

    #[test]
    fn finds_the_creation_date_as_tika_does() {
        assert_eq!(creation_date("a"), None);
        assert_eq!(
            creation_date("Creation Date: Today"),
            Some(Ok("Today".to_owned()))
        );
        assert!(matches!(creation_date("Creation\tDate:"), Some(Err(_))));
        assert_eq!(creation_date("CreationDate: x"), None);
    }
}
