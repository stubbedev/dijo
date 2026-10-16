//! Adobe font metrics files as FontBox 3's `AFMParser` reads them in full: every key it knows,
//! the character metrics, kerning data and composites, failing where it throws. What Tika's
//! `AdobeFontMetricParser` reads of the result is kept.

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
    pub(super) comments: Vec<String>,
    /// The `WX` of each character metric, 0 where it has none.
    widths: Vec<f32>,
}

impl FontMetrics {
    /// `getAverageCharacterWidth`: the mean of the positive widths, in `float` arithmetic.
    pub(super) fn average_character_width(&self) -> f32 {
        let mut total = 0f32;
        let mut count = 0f32;
        for &width in &self.widths {
            if width > 0.0 {
                total += width;
                count += 1.0;
            }
        }
        if total > 0.0 {
            total / count
        } else {
            0.0
        }
    }
}

/// The byte stream `AFMParser` reads, each byte a Latin-1 character and the end of the stream
/// `(char) -1`, U+FFFF.
struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}

const END: char = '\u{FFFF}';

fn is_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

impl Input<'_> {
    fn read(&mut self) -> Option<char> {
        let byte = self.bytes.get(self.at)?;
        self.at += 1;
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

/// `Float.parseFloat`: Java's syntax, the value rounded once to a `float`.
fn parse_float(text: &str) -> Result<f32, Thrown> {
    let number_format = || format!("Error parsing AFM document: {text}");
    let value = java::parse_double(text).ok_or_else(number_format)?;
    let plain = java::trim(text).trim_end_matches(['f', 'F', 'd', 'D']);
    Ok(plain.parse::<f32>().unwrap_or(value as f32))
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

/// `AFMParser.parse()`.
pub(super) fn parse(bytes: &[u8]) -> Result<FontMetrics, Thrown> {
    let mut input = Input { bytes, at: 0 };
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
            "Comment" => metrics.comments.push(input.line()),
            "StartCharMetrics" => {
                let count = input.int()?;
                for _ in 0..count.max(0) {
                    metrics.widths.push(char_metric(&input.line())?);
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
            _ => return Err(format!("Unknown AFM key '{command}'")),
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

/// `AdobeFontMetricParser.extractCreationDate`: the date of the first comment matching
/// `.*Creation\sDate.*`, after its first colon and the character after it, and the comments
/// without that one.
pub(super) fn creation_date(comments: &[String]) -> Result<(Option<String>, Vec<String>), Thrown> {
    let matches = |comment: &str| {
        !comment.chars().any(java::is_line_terminator)
            && comment.match_indices("Creation").any(|(at, _)| {
                let rest = &comment[at + "Creation".len()..];
                rest.chars().next().is_some_and(java::is_regex_space)
                    && rest[1..].starts_with("Date")
            })
    };
    let mut comments = comments.to_vec();
    let Some(index) = comments.iter().position(|comment| matches(comment)) else {
        return Ok((None, comments));
    };
    let characters: Vec<char> = comments.remove(index).chars().collect();
    let start = characters
        .iter()
        .position(|&character| character == ':')
        .map_or(1, |colon| colon + 2);
    let date: String = characters
        .get(start..)
        .ok_or_else(|| "StringIndexOutOfBoundsException".to_owned())?
        .iter()
        .collect();
    Ok((Some(date), comments))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_metrics_as_fontbox_does() {
        let metrics = parse(
            b"StartFontMetrics 4.1\r\nComment One\r\nFullName  A B \r\nStartCharMetrics 2\r\n\
              C 1 ; WX 10.5 ; N a ;\r\nC 2 ; WX 0 ; N b ;\r\nEndCharMetrics\r\nEndFontMetrics",
        )
        .unwrap();
        assert_eq!(metrics.afm_version, 4.1);
        assert_eq!(metrics.full_name.as_deref(), Some("A B "));
        assert_eq!(metrics.comments, ["One"]);
        assert_eq!(metrics.average_character_width(), 10.5);
        assert!(parse(b"StartFontMetrics 4.1\nC 1 ;\nEndFontMetrics").is_err());
        assert!(parse(b"StartFontMetrics x").is_err());
        assert!(parse(b"StartFontMetrics 1\nFontName x").is_err());
    }

    #[test]
    fn finds_the_creation_date_as_tika_does() {
        let comments = vec!["a".to_owned(), "Creation Date: Today".to_owned()];
        assert_eq!(
            creation_date(&comments),
            Ok((Some("Today".to_owned()), vec!["a".to_owned()]))
        );
        assert!(creation_date(&["Creation\tDate:".to_owned()]).is_err());
        assert_eq!(
            creation_date(&["CreationDate: x".to_owned()]),
            Ok((None, vec!["CreationDate: x".to_owned()]))
        );
    }
}
