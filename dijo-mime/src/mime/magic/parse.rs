//! The magic as the generator reads it from `<match>` elements: `MagicDetector.parse`, with each
//! clause's `toString()`, which orders clauses of equal priority and size.

use std::sync::OnceLock;

use super::{Anchor, Clause, Kind, MagicMatch, Run};
use crate::mime::java_regex::translate::{self, Text};
use crate::mime::java_regex::JavaRegex;
use crate::mime::media_type::MediaType;

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
pub(crate) fn decode_value(value: &str, kind: &str) -> Result<Vec<u8>, String> {
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

/// What a window that matches `pattern` under `mask` must hold at a place in it, as
/// [`Anchor`] describes.
fn anchor(pattern: &'static [u8], mask: &[u8], kind: &Kind) -> Option<Anchor> {
    match kind {
        Kind::Bytes => {
            let mut best: Option<(usize, usize)> = None;
            let mut start = 0;
            for (index, &mask) in mask.iter().chain([&0]).enumerate() {
                if mask == 0xFF {
                    continue;
                }
                if index > start && best.is_none_or(|(_, length)| index - start > length) {
                    best = Some((start, index - start));
                }
                start = index + 1;
            }
            let (offset, length) = best?;
            let bytes = &pattern[offset..offset + length];
            Some(match bytes {
                [byte] => Anchor::Byte {
                    offset,
                    byte: *byte,
                },
                _ => Anchor::Run {
                    offset,
                    run: Run::new(bytes, Box::leak(Box::new(OnceLock::new()))),
                },
            })
        }
        Kind::IgnoreCase => {
            let offset = mask.iter().position(|&mask| mask == 0xFF)?;
            let byte = pattern[offset];
            if byte.is_ascii_lowercase() {
                Some(Anchor::EitherByte {
                    offset,
                    bytes: [byte, byte.to_ascii_uppercase()],
                })
            } else if byte.is_ascii_uppercase() {
                None
            } else {
                Some(Anchor::Byte { offset, byte })
            }
        }
        Kind::Regex(_) => None,
    }
}

impl MagicMatch {
    /// `MagicMatch` with its `MagicDetector`, as `MagicDetector.parse` builds it.
    pub(crate) fn parse(
        media_type: &MediaType,
        attributes: &MatchAttributes<'_>,
    ) -> Result<Read, String> {
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
                    JavaRegex::compile(&source, translate::Anchor::LookingAt, Text::Latin1)
                        .map_err(|error| format!("{string}: {error}"))?,
                )
            }
            "stringignorecase" => Kind::IgnoreCase,
            _ => Kind::Bytes,
        };
        let pattern: &'static [u8] = pattern.leak();
        let anchor = if begin < end {
            anchor(pattern, &mask, &kind)
        } else {
            None
        };
        let found = MagicMatch {
            pattern,
            mask: mask.leak(),
            kind,
            begin: usize::try_from(begin).map_err(|error| error.to_string())?,
            end: usize::try_from(end).map_err(|error| error.to_string())?,
            anchor,
        };
        Ok(Read {
            clause: Clause::Match(found),
            string,
        })
    }
}

/// A clause as the generator reads it, and its `toString()`.
#[derive(Debug)]
pub(crate) struct Read {
    pub(crate) clause: Clause,
    pub(crate) string: String,
}

impl Read {
    /// `AndClause`: a match and what its nested matches require.
    pub(crate) fn and(clauses: Vec<Read>) -> Read {
        Self::combined("and", clauses, Clause::And)
    }

    /// `OrClause`.
    pub(crate) fn or(clauses: Vec<Read>) -> Read {
        Self::combined("or", clauses, Clause::Or)
    }

    /// `MinShouldMatchClause`.
    pub(crate) fn min_should_match(min: usize, clauses: Vec<Read>) -> Read {
        Self::combined(
            &format!("minShouldMatch (min: {min}) "),
            clauses,
            |clauses| Clause::MinShouldMatch(min, clauses),
        )
    }

    fn combined(
        name: &str,
        clauses: Vec<Read>,
        combine: impl FnOnce(&'static [Clause]) -> Clause,
    ) -> Read {
        let strings: Vec<String> = clauses.iter().map(|read| read.string.clone()).collect();
        let clauses: Vec<Clause> = clauses.into_iter().map(|read| read.clause).collect();
        Read {
            clause: combine(clauses.leak()),
            string: format!("{name}[{}]", strings.join(", ")),
        }
    }
}

impl Clause {
    /// `getLength()`: the pattern's length for a match, the sum of an and's, the longest of an
    /// or's.
    pub(crate) fn size(&self) -> usize {
        match self {
            Clause::Match(m) => m.pattern.len(),
            Clause::And(clauses) => clauses.iter().map(Clause::size).sum(),
            Clause::Or(clauses) | Clause::MinShouldMatch(_, clauses) => {
                clauses.iter().map(Clause::size).max().unwrap_or(0)
            }
        }
    }
}
