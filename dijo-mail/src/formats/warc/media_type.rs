//! jwarc's `MediaType`, as `parseLeniently` reads one and `toString` writes it: the type, the
//! subtype and the parameters, kept case-insensitively by their first spelling and listed in
//! `String.CASE_INSENSITIVE_ORDER`.

use super::ragel::{Flow, ERROR};
use super::tables::{MEDIA_TYPE, MEDIA_TYPE_EN_LENIENT};
use crate::java;

/// What `parseLeniently` throws on: an `IllegalArgumentException` or a
/// `StringIndexOutOfBoundsException`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unparsable(pub(super) String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MediaType {
    kind: String,
    subtype: String,
    /// The parameters in the order of a `TreeMap` with `CASE_INSENSITIVE_ORDER`.
    parameters: Vec<(String, String)>,
}

/// The characters `validToken` allows unquoted.
fn is_token_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(character)
}

impl MediaType {
    /// `MediaType.parseLeniently`.
    pub(super) fn parse_leniently(text: &str) -> Result<Self, Unparsable> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let keys: Vec<i32> = units.iter().map(|&unit| i32::from(unit)).collect();
        let substring = |start: usize, end: usize| -> Result<String, Unparsable> {
            if start > end || end > units.len() {
                return Err(Unparsable(format!(
                    "begin {start}, end {end}, length {}",
                    units.len()
                )));
            }
            String::from_utf16(&units[start..end])
                .map_err(|_| Unparsable("a surrogate pair split by the parse".to_owned()))
        };
        let mut parameters: Vec<(String, String)> = Vec::new();
        let put_if_absent =
            |parameters: &mut Vec<(String, String)>, name: String, value: String| {
                if let Err(at) = parameters
                    .binary_search_by(|(existing, _)| java::compare_ignore_case(existing, &name))
                {
                    parameters.insert(at, (name, value));
                }
            };
        let (mut type_end, mut subtype_end) = (units.len(), units.len());
        let (mut name_start, mut name_end, mut value_start) = (0, 0, 0);
        let mut quoted: Vec<u16> = Vec::new();
        let mut valid = true;
        let mut cs = MEDIA_TYPE_EN_LENIENT;
        MEDIA_TYPE.exec(&mut cs, keys.as_slice(), 0, true, |action, p| {
            match action {
                0 => {
                    let name = substring(name_start, name_end)?;
                    let value = substring(value_start, p)?;
                    put_if_absent(&mut parameters, name, value);
                }
                1 => {
                    let name = substring(name_start, name_end)?;
                    let value = String::from_utf16(&quoted).map_err(|_| {
                        Unparsable("a surrogate pair split by the parse".to_owned())
                    })?;
                    put_if_absent(&mut parameters, name, value);
                }
                2 => return Err(Unparsable(format!("parse error at position {p}"))),
                3 => valid = false,
                4 | 10 => value_start = p,
                5 => quoted.extend_from_slice(&units[value_start..p]),
                6 => quoted.push(units[p]),
                7 => quoted.clear(),
                8 => name_start = p,
                9 => name_end = p,
                11 | 13 => type_end = p,
                12 | 14 => subtype_end = p,
                _ => unreachable!("the media type machine has no action {action}"),
            }
            Ok(Flow::Continue)
        })?;
        debug_assert_ne!(cs, ERROR, "an error runs the parse_error action");
        let kind = substring(0, type_end)?;
        let subtype = if !valid && type_end + 1 >= units.len() {
            String::new()
        } else {
            substring(type_end + 1, subtype_end)?
        };
        Ok(Self {
            kind,
            subtype,
            parameters,
        })
    }

    /// `base().equals(other)` for a parameterless `other`: the type and subtype the same,
    /// ignoring case.
    pub(super) fn has_base(&self, kind: &str, subtype: &str) -> bool {
        java::equals_ignore_case(&self.kind, kind)
            && java::equals_ignore_case(&self.subtype, subtype)
    }
}

impl std::fmt::Display for MediaType {
    /// `MediaType.toString`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.kind)?;
        if !self.subtype.is_empty() {
            write!(f, "/{}", self.subtype)?;
        }
        for (name, value) in &self.parameters {
            if value.chars().all(is_token_char) {
                write!(f, ";{name}={value}")?;
            } else {
                let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
                write!(f, ";{name}=\"{escaped}\"")?;
            }
        }
        Ok(())
    }
}

/// The `MediaType.OCTET_STREAM` a message without a `Content-Type` has.
pub(super) fn octet_stream() -> MediaType {
    MediaType {
        kind: "application".to_owned(),
        subtype: "octet-stream".to_owned(),
        parameters: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(text: &str) -> String {
        MediaType::parse_leniently(text).unwrap().to_string()
    }

    #[test]
    fn reads_and_writes_media_types_as_jwarc_does() {
        assert_eq!(written("text/html"), "text/html");
        assert_eq!(
            written("text/html; Charset=\"utf-8\" ;charset=x;A=\"b c\""),
            "text/html;A=\"b c\";Charset=utf-8"
        );
        assert_eq!(
            written("application/http;msgtype=response"),
            "application/http;msgtype=response"
        );
        assert_eq!(written("nonsense"), "nonsense");
        assert_eq!(written("text/"), "text");
        assert_eq!(written("text/html; bogus"), "text/html");
        assert!(MediaType::parse_leniently("text html/x").is_err());
        assert!(MediaType::parse_leniently("application/HTTP")
            .unwrap()
            .has_base("application", "http"));
    }
}
