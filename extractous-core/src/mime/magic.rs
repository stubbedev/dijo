//! The magic of `tika-mimetypes.xml`: Tika's `MagicMatch` clauses, combined into `and`, `or`
//! and `minShouldMatch` clauses, each evaluated as Tika's `MagicDetector` evaluates it.

use std::cell::OnceCell;
use std::fmt;

use super::java_regex::{Anchor, JavaRegex};
use super::media_type::MediaType;

/// How far a regex clause reads past the start of each offset it tries.
const REGEX_WINDOW: usize = 8 * 1024;

/// The bytes clauses are evaluated over, and the same bytes as ISO-8859-1 text, decoded once for
/// every regex clause that reads them.
pub(crate) struct Haystack<'a> {
    bytes: &'a [u8],
    latin1: OnceCell<Latin1>,
}

/// Bytes as the characters U+0000 to U+00FF, with where each byte's character starts in the text
/// and, last, the text's length.
struct Latin1 {
    text: String,
    offsets: Vec<usize>,
}

impl<'a> Haystack<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Haystack {
            bytes,
            latin1: OnceCell::new(),
        }
    }

    /// The bytes from `start` to `end` as ISO-8859-1 text.
    fn text(&self, start: usize, end: usize) -> &str {
        let latin1 = self.latin1.get_or_init(|| {
            let text: String = self.bytes.iter().map(|&b| char::from(b)).collect();
            let mut offsets = Vec::with_capacity(self.bytes.len() + 1);
            offsets.extend(text.char_indices().map(|(offset, _)| offset));
            offsets.push(text.len());
            Latin1 { text, offsets }
        });
        &latin1.text[latin1.offsets[start]..latin1.offsets[end]]
    }
}

/// One `<match>`: a pattern, under a mask, at an offset or in a range of offsets.
#[derive(Debug)]
pub(crate) struct MagicMatch {
    /// `MagicMatch.toString()`, which orders clauses of equal priority and size.
    string: String,
    pattern: Vec<u8>,
    mask: Vec<u8>,
    /// `MagicDetector.getLength()`: the longer of the pattern and the mask.
    pattern_length: usize,
    kind: Kind,
    begin: usize,
    end: usize,
}

#[derive(Debug)]
enum Kind {
    Bytes,
    IgnoreCase,
    Regex(JavaRegex),
}

/// The attributes of a `<match>` element.
pub(crate) struct MatchAttributes<'a> {
    pub(crate) kind: Option<&'a str>,
    pub(crate) offset: Option<&'a str>,
    pub(crate) value: Option<&'a str>,
    pub(crate) mask: Option<&'a str>,
}

/// Java's `Integer.parseInt` and `Long.parseLong`: an optional sign, then digits of the radix.
fn parse_java_integer(text: &str, radix: u32) -> Result<i64, String> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return Err(format!("{text} is not a number in radix {radix}"));
    }
    i64::from_str_radix(text, radix).map_err(|error| format!("{text}: {error}"))
}

/// `MagicDetector.decodeString`: backslash escapes for `\\`, `\xHH`, `\r`, `\n` and up to three
/// octal digits, then each character truncated to a byte, or written as two bytes for the
/// `unicode` kinds.
fn decode_string(value: &str, kind: &str) -> Result<Vec<u8>, String> {
    if let Some(hex) = value.strip_prefix("0x") {
        let units: Vec<char> = hex.chars().collect();
        return units
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| {
                let text: String = pair.iter().collect();
                u8::from_str_radix(&text, 16).map_err(|error| format!("{value}: {error}"))
            })
            .collect();
    }
    let units: Vec<u16> = value.encode_utf16().collect();
    let mut decoded: Vec<u16> = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        if units[i] != u16::from(b'\\') {
            decoded.push(units[i]);
            i += 1;
            continue;
        }
        let next = units
            .get(i + 1)
            .copied()
            .ok_or_else(|| format!("{value} ends in a backslash"))?;
        match next {
            0x5C => {
                decoded.push(0x5C);
                i += 2;
            }
            0x78 => {
                let hex = String::from_utf16(units.get(i + 2..i + 4).unwrap_or_default())
                    .map_err(|error| format!("{value}: {error}"))?;
                decoded.push(
                    u16::from_str_radix(&hex, 16).map_err(|error| format!("{value}: {error}"))?,
                );
                i += 4;
            }
            0x72 => {
                decoded.push(u16::from(b'\r'));
                i += 2;
            }
            0x6E => {
                decoded.push(u16::from(b'\n'));
                i += 2;
            }
            _ => {
                let mut j = i + 1;
                while j < i + 4 && j < units.len() && (0x30..=0x39).contains(&units[j]) {
                    j += 1;
                }
                let digits = String::from_utf16(&units[i + 1..j])
                    .map_err(|error| format!("{value}: {error}"))?;
                let octal = if digits.is_empty() {
                    0
                } else {
                    i64::from_str_radix(&digits, 8).map_err(|error| format!("{value}: {error}"))?
                };
                decoded.push(((octal as u8) as i8) as i16 as u16);
                i = j;
            }
        }
    }
    Ok(match kind {
        "unicodeLE" => decoded.iter().flat_map(|unit| unit.to_le_bytes()).collect(),
        "unicodeBE" => decoded.iter().flat_map(|unit| unit.to_be_bytes()).collect(),
        _ => decoded.iter().map(|&unit| unit as u8).collect(),
    })
}

/// `MagicDetector.decodeValue`.
fn decode_value(value: &str, kind: &str) -> Result<Vec<u8>, String> {
    let (digits, radix) = match value.strip_prefix("0x") {
        Some(hex) => (hex, 16),
        None => (value, 8),
    };
    Ok(match kind {
        "string" | "regex" | "unicodeLE" | "unicodeBE" => decode_string(value, kind)?,
        "stringignorecase" => decode_string(&value.to_lowercase(), kind)?,
        "byte" => digits.as_bytes().to_vec(),
        "host16" | "little16" => {
            let i = parse_java_integer(digits, radix)? as i32;
            vec![(i & 0xFF) as u8, (i >> 8) as u8]
        }
        "big16" => {
            let i = parse_java_integer(digits, radix)? as i32;
            vec![(i >> 8) as u8, (i & 0xFF) as u8]
        }
        "host32" | "little32" => parse_java_integer(digits, radix)?.to_le_bytes()[..4].to_vec(),
        "big32" => parse_java_integer(digits, radix)?.to_be_bytes()[4..].to_vec(),
        _ => return Err(format!("unknown magic type {kind}")),
    })
}

impl MagicMatch {
    /// `MagicMatch` with its `MagicDetector`, as `MagicDetector.parse` builds it.
    pub(crate) fn parse(
        media_type: &MediaType,
        attributes: &MatchAttributes<'_>,
    ) -> Result<MagicMatch, String> {
        let kind = attributes.kind.unwrap_or("string");
        let string = format!(
            "{media_type} {kind} {} {} {}",
            attributes.offset.unwrap_or("null"),
            attributes.value.unwrap_or("null"),
            attributes.mask.unwrap_or("null")
        );
        let (begin, end) = match attributes.offset {
            None => (0, 0),
            Some(offset) => match offset.split_once(':') {
                None => {
                    let at = parse_java_integer(offset, 10)?;
                    (at, at)
                }
                Some((begin, end)) => {
                    (parse_java_integer(begin, 10)?, parse_java_integer(end, 10)?)
                }
            },
        };
        if begin < 0 || end < begin {
            return Err(format!("invalid offset range in {string}"));
        }
        let value = attributes
            .value
            .ok_or_else(|| format!("no pattern in {string}"))?;
        let raw_pattern = decode_value(value, kind)?;
        let raw_mask = attributes
            .mask
            .map(|mask| decode_value(mask, kind))
            .transpose()?;
        let pattern_length = raw_pattern.len().max(raw_mask.as_ref().map_or(0, Vec::len));
        let mask: Vec<u8> = (0..pattern_length)
            .map(|i| {
                raw_mask
                    .as_ref()
                    .and_then(|m| m.get(i))
                    .copied()
                    .unwrap_or(0xFF)
            })
            .collect();
        let pattern: Vec<u8> = (0..pattern_length)
            .map(|i| raw_pattern.get(i).map_or(0, |&b| b & mask[i]))
            .collect();
        let kind = match kind {
            "regex" => {
                let source = String::from_utf8_lossy(&pattern);
                Kind::Regex(
                    JavaRegex::compile(&source, Anchor::LookingAt)
                        .map_err(|error| format!("{string}: {error}"))?,
                )
            }
            "stringignorecase" => Kind::IgnoreCase,
            _ => Kind::Bytes,
        };
        Ok(MagicMatch {
            string,
            pattern,
            mask,
            pattern_length,
            kind,
            begin: usize::try_from(begin).map_err(|error| error.to_string())?,
            end: usize::try_from(end).map_err(|error| error.to_string())?,
        })
    }

    /// `MagicDetector.getLength()`.
    pub(crate) fn size(&self) -> usize {
        self.pattern_length
    }

    /// The bytes the comparison window spans past each offset.
    fn length(&self) -> usize {
        match self.kind {
            Kind::Regex(_) => REGEX_WINDOW,
            _ => self.pattern_length,
        }
    }

    /// `MagicDetector.matches`.
    pub(crate) fn matches(&self, haystack: &Haystack) -> bool {
        let data = haystack.bytes;
        match &self.kind {
            Kind::Regex(regex) => {
                if data.len() <= self.begin {
                    return false;
                }
                let last = self.end.min(data.len() - 1);
                let span = last - self.begin;
                let buffer_end = self.begin + (data.len() - self.begin).min(REGEX_WINDOW + span);
                (self.begin..=last).any(|start| {
                    let region_end = (start + REGEX_WINDOW).min(buffer_end);
                    start < region_end && regex.is_match(haystack.text(start, region_end))
                })
            }
            Kind::Bytes | Kind::IgnoreCase => {
                let length = self.length();
                if data.len() < self.begin + length {
                    return false;
                }
                if length == 0 {
                    return true;
                }
                let last = self.end.min(data.len() - length);
                (self.begin..=last).any(|i| self.matches_at(&data[i..i + length]))
            }
        }
    }

    fn matches_at(&self, window: &[u8]) -> bool {
        let ignore_case = matches!(self.kind, Kind::IgnoreCase);
        window
            .iter()
            .zip(&self.mask)
            .zip(&self.pattern)
            .all(|((&byte, &mask), &pattern)| {
                let masked = byte & mask;
                let masked = if ignore_case && masked < 0x80 {
                    masked.to_ascii_lowercase()
                } else {
                    masked
                };
                masked == pattern
            })
    }
}

/// A clause of a `<magic>`: a match, or matches combined.
#[derive(Debug)]
pub(crate) enum Clause {
    Match(Box<MagicMatch>),
    /// A match and what its nested matches require.
    And(Vec<Clause>),
    Or(Vec<Clause>),
    MinShouldMatch(usize, Vec<Clause>),
}

impl Clause {
    pub(crate) fn eval(&self, haystack: &Haystack) -> bool {
        match self {
            Clause::Match(m) => m.matches(haystack),
            Clause::And(clauses) => clauses.iter().all(|c| c.eval(haystack)),
            Clause::Or(clauses) => clauses.iter().any(|c| c.eval(haystack)),
            Clause::MinShouldMatch(min, clauses) => {
                clauses
                    .iter()
                    .filter(|c| c.eval(haystack))
                    .take(*min)
                    .count()
                    >= *min
            }
        }
    }

    pub(crate) fn size(&self) -> usize {
        match self {
            Clause::Match(m) => m.size(),
            Clause::And(clauses) => clauses.iter().map(Clause::size).sum(),
            Clause::Or(clauses) | Clause::MinShouldMatch(_, clauses) => {
                clauses.iter().map(Clause::size).max().unwrap_or(0)
            }
        }
    }
}

fn write_list(f: &mut fmt::Formatter<'_>, clauses: &[Clause]) -> fmt::Result {
    f.write_str("[")?;
    for (i, clause) in clauses.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{clause}")?;
    }
    f.write_str("]")
}

impl fmt::Display for Clause {
    /// The clauses' `toString()`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Clause::Match(m) => f.write_str(&m.string),
            Clause::And(clauses) => {
                f.write_str("and")?;
                write_list(f, clauses)
            }
            Clause::Or(clauses) => {
                f.write_str("or")?;
                write_list(f, clauses)
            }
            Clause::MinShouldMatch(min, clauses) => {
                write!(f, "minShouldMatch (min: {min}) ")?;
                write_list(f, clauses)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clause(kind: &str, offset: &str, value: &str, mask: Option<&str>) -> MagicMatch {
        MagicMatch::parse(
            &MediaType::named("application/x-test"),
            &MatchAttributes {
                kind: Some(kind),
                offset: Some(offset),
                value: Some(value),
                mask,
            },
        )
        .unwrap()
    }

    #[test]
    fn decodes_values_as_java_does() {
        assert_eq!(decode_value("\\x1F\\213", "string").unwrap(), [0x1F, 0x8B]);
        assert_eq!(decode_value("0x1f8b", "string").unwrap(), [0x1F, 0x8B]);
        assert_eq!(decode_value("12", "little16").unwrap(), [10, 0]);
        assert_eq!(
            decode_value("0xcafebabe", "big32").unwrap(),
            [0xCA, 0xFE, 0xBA, 0xBE]
        );
        assert_eq!(
            decode_value("0xcafebabe", "little32").unwrap(),
            [0xBE, 0xBA, 0xFE, 0xCA]
        );
        assert_eq!(
            decode_value("a\\tb", "string").unwrap(),
            [b'a', 0, b't', b'b']
        );
        assert_eq!(decode_value("AB", "unicodeLE").unwrap(), [b'A', 0, b'B', 0]);
    }

    #[test]
    fn matches_at_any_offset_of_a_range() {
        let m = clause("string", "0:4", "PK", None);
        assert!(m.matches(&Haystack::new(b"xxxPKyy")));
        assert!(!m.matches(&Haystack::new(b"xxxxxPK")));
        assert!(!m.matches(&Haystack::new(b"P")));
    }

    #[test]
    fn applies_masks_and_case() {
        let m = clause("string", "0", "!BDN....SO", Some("0xFFFFFFFF00000000FFFF"));
        assert!(m.matches(&Haystack::new(b"!BDNabcdSO")));
        let m = clause("stringignorecase", "0", "<HTML", None);
        assert!(m.matches(&Haystack::new(b"<hTmL")));
        assert!(!m.matches(&Haystack::new(b"<htm")));
    }

    #[test]
    fn matches_regexes_at_each_offset() {
        let m = clause("regex", "0:3", "BZh[1-9]", None);
        assert!(m.matches(&Haystack::new(b"..BZh9")));
        assert!(!m.matches(&Haystack::new(b"....BZh9")));
        assert_eq!(m.size(), 8);
    }

    #[test]
    fn prints_as_java_does() {
        let a = Clause::Match(Box::new(clause("string", "0", "%PDF-", None)));
        let b = Clause::Match(Box::new(clause("string", "1", "X", None)));
        let and = Clause::And(vec![a, Clause::Or(vec![b])]);
        assert_eq!(
            and.to_string(),
            "and[application/x-test string 0 %PDF- null, or[application/x-test string 1 X null]]"
        );
    }
}
