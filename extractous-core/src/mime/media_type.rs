//! Tika's `MediaType`: a type, a subtype and parameters, parsed and printed as Tika parses and
//! prints them.

use std::collections::BTreeMap;
use std::fmt;

use crate::java;

/// A media type such as `application/pdf` or `application/x-quattro-pro; version=9`, held in
/// the form Tika's `MediaType.toString()` prints.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MediaType {
    string: String,
    slash: usize,
    semicolon: usize,
    parameters: Vec<(String, String)>,
}

const SPECIAL: &[char] = &[
    '(', ')', '<', '>', '@', ',', ';', ':', '\\', '"', '/', '[', ']', '?', '=',
];

/// Whether `c` may stand in a type or subtype outside the fast path: Java's
/// `[^\c\(\)<>@,;:\\"/\[\]\?=\s]`, where `\c\` is the control character U+001C.
fn is_valid(c: char) -> bool {
    !(c == '\u{1C}' || SPECIAL.contains(&c) || java::is_regex_space(c))
}

fn is_simple_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| matches!(c, '-' | '+' | '.' | '_' | '0'..='9' | 'a'..='z'))
}

impl MediaType {
    /// `MediaType.parse`: `None` where Tika returns null.
    pub(crate) fn parse(text: &str) -> Option<MediaType> {
        let slash = text.find('/')?;
        if is_simple_name(&text[..slash]) && is_simple_name(&text[slash + 1..]) {
            return Some(MediaType {
                string: text.to_owned(),
                slash,
                semicolon: text.len(),
                parameters: Vec::new(),
            });
        }
        if let Some((kind, subtype, rest)) = Self::match_type(text) {
            return Some(Self::with_parameters(kind, subtype, parse_parameters(rest)));
        }
        let (charset, kind, subtype) = Self::match_charset_first(text)?;
        Some(Self::with_parameters(
            kind,
            subtype,
            parse_parameters(charset),
        ))
    }

    /// A media type Tika's own code names, which must parse.
    pub(crate) fn named(text: &str) -> MediaType {
        Self::parse(text).unwrap_or_else(|| panic!("{text} is not a media type"))
    }

    /// `(?s)\s*VALID\s*/\s*VALID\s*($|;.*)`, matched in full.
    fn match_type(text: &str) -> Option<(&str, &str, &str)> {
        let rest = text.trim_start_matches(java::is_regex_space);
        let (kind, rest) = take_valid(rest)?;
        let rest = rest
            .trim_start_matches(java::is_regex_space)
            .strip_prefix('/')?;
        let rest = rest.trim_start_matches(java::is_regex_space);
        let (subtype, rest) = take_valid(rest)?;
        let rest = rest.trim_start_matches(java::is_regex_space);
        if rest.is_empty() || rest.starts_with(';') {
            Some((kind, subtype, rest))
        } else {
            None
        }
    }

    /// `(?is)\s*(charset\s*=\s*[^;\s]+)\s*;\s*VALID\s*/\s*VALID\s*`, matched in full: the
    /// charset's value ends at the first `;` or white space. Tika's pattern writes `[^\c;\s]`,
    /// which Java reads as "not `{` or white space", so its value runs over semicolons (#70).
    fn match_charset_first(text: &str) -> Option<(&str, &str, &str)> {
        let rest = text.trim_start_matches(java::is_regex_space);
        let start = text.len() - rest.len();
        let name = rest.get(..7)?;
        if !name.eq_ignore_ascii_case("charset") {
            return None;
        }
        let rest = rest[7..]
            .trim_start_matches(java::is_regex_space)
            .strip_prefix('=')?;
        let rest = rest.trim_start_matches(java::is_regex_space);
        let value_len = rest
            .find(|c: char| c == ';' || java::is_regex_space(c))
            .unwrap_or(rest.len());
        if value_len == 0 {
            return None;
        }
        let value_end = text.len() - rest.len() + value_len;
        let (kind, subtype) = Self::match_type_tail(&text[value_end..])?;
        Some((&text[start..value_end], kind, subtype))
    }

    /// `\s*;\s*VALID\s*/\s*VALID\s*`, matched in full.
    fn match_type_tail(text: &str) -> Option<(&str, &str)> {
        let rest = text
            .trim_start_matches(java::is_regex_space)
            .strip_prefix(';')?;
        let rest = rest.trim_start_matches(java::is_regex_space);
        let (kind, rest) = take_valid(rest)?;
        let rest = rest
            .trim_start_matches(java::is_regex_space)
            .strip_prefix('/')?;
        let rest = rest.trim_start_matches(java::is_regex_space);
        let (subtype, rest) = take_valid(rest)?;
        rest.trim_start_matches(java::is_regex_space)
            .is_empty()
            .then_some((kind, subtype))
    }

    /// `new MediaType(type, subtype, parameters)`.
    fn with_parameters(kind: &str, subtype: &str, parameters: Vec<(String, String)>) -> MediaType {
        let kind = java::to_lower_case(java::trim(kind));
        let subtype = java::to_lower_case(java::trim(subtype));
        let mut string = format!("{kind}/{subtype}");
        let slash = kind.len();
        let semicolon = string.len();
        let sorted: BTreeMap<java::JavaString, String> = parameters
            .into_iter()
            .map(|(key, value)| {
                (
                    java::JavaString(java::to_lower_case(java::trim(&key))),
                    value,
                )
            })
            .collect();
        for (key, value) in &sorted {
            string.push_str("; ");
            string.push_str(&key.0);
            string.push('=');
            if value
                .chars()
                .any(|c| SPECIAL.contains(&c) || java::is_regex_space(c))
            {
                string.push('"');
                for c in value.chars() {
                    if SPECIAL.contains(&c) {
                        string.push('\\');
                    }
                    string.push(c);
                }
                string.push('"');
            } else {
                string.push_str(value);
            }
        }
        MediaType {
            string,
            slash,
            semicolon,
            parameters: sorted
                .into_iter()
                .map(|(key, value)| (key.0, value))
                .collect(),
        }
    }

    /// This type with one more parameter, as `new MediaType(type, name, value)` makes it.
    pub(crate) fn with_parameter(&self, name: &str, value: &str) -> MediaType {
        let mut parameters = self.parameters.clone();
        parameters.push((name.to_owned(), value.to_owned()));
        Self::with_parameters(self.kind(), self.subtype(), parameters)
    }

    /// This type with the parameters of `other` as well, `other`'s winning a clash, as
    /// `new MediaType(type, other.getParameters())` makes it.
    pub(crate) fn with_parameters_of(&self, other: &MediaType) -> MediaType {
        let mut parameters = self.parameters.clone();
        parameters.extend(other.parameters.iter().cloned());
        Self::with_parameters(self.kind(), self.subtype(), parameters)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.string
    }

    /// The top-level type, such as `application`.
    pub(crate) fn kind(&self) -> &str {
        &self.string[..self.slash]
    }

    pub(crate) fn subtype(&self) -> &str {
        &self.string[self.slash + 1..self.semicolon]
    }

    /// The value of a parameter by its lower-case name, as `getParameters().get(name)`.
    pub(crate) fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn has_parameters(&self) -> bool {
        self.semicolon < self.string.len()
    }

    /// The type without its parameters.
    pub(crate) fn base_type(&self) -> MediaType {
        if self.has_parameters() {
            MediaType::named(&self.string[..self.semicolon])
        } else {
            self.clone()
        }
    }
}

impl Ord for MediaType {
    /// `MediaType.compareTo`: the printed forms in Java's UTF-16 order.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        java::compare(&self.string, &other.string)
    }
}

impl PartialOrd for MediaType {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.string)
    }
}

/// The longest run of characters a type may hold at the start of `text`, and the rest.
fn take_valid(text: &str) -> Option<(&str, &str)> {
    let end = text.find(|c: char| !is_valid(c)).unwrap_or(text.len());
    (end > 0).then(|| text.split_at(end))
}

/// `MediaType.parseParameters`: `k1=v1; k2=v2`, keys trimmed and values unquoted, a later key
/// replacing an equal earlier one as Java's `HashMap` does.
fn parse_parameters(text: &str) -> Vec<(String, String)> {
    let mut parameters: Vec<(String, String)> = Vec::new();
    for part in text.split(';') {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = java::trim(key);
        if key.is_empty() {
            continue;
        }
        let value = unquote(java::trim(value)).to_owned();
        match parameters.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => parameters.push((key.to_owned(), value)),
        }
    }
    parameters
}

fn unquote(text: &str) -> &str {
    text.trim_start_matches(['"', '\''])
        .trim_end_matches(['"', '\''])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Option<String> {
        MediaType::parse(text).map(|t| t.to_string())
    }

    #[test]
    fn parses_simple_names_as_they_are() {
        assert_eq!(parse("application/pdf").as_deref(), Some("application/pdf"));
        assert_eq!(parse("pdf"), None);
    }

    #[test]
    fn lowercases_and_trims_other_names() {
        assert_eq!(
            parse(" Application/VND.MS-Excel.Sheet.macroEnabled.12 ").as_deref(),
            Some("application/vnd.ms-excel.sheet.macroenabled.12")
        );
    }

    #[test]
    fn sorts_and_quotes_parameters() {
        assert_eq!(
            parse("text/plain; Zeta=1; charset=\"UTF-8\"; a=b c").as_deref(),
            Some("text/plain; a=\"b c\"; charset=UTF-8; zeta=1")
        );
        assert_eq!(
            MediaType::named("application/x-quattro-pro")
                .with_parameter("version", "7-8")
                .to_string(),
            "application/x-quattro-pro; version=7-8"
        );
    }

    #[test]
    fn reads_a_leading_charset() {
        assert_eq!(
            parse("charset=utf-8; text/html").as_deref(),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(parse("text/html junk"), None);
        assert_eq!(parse("charset=utf-8; x=1; text/html"), None);
        assert_eq!(
            parse("charset=a{b; text/html").as_deref(),
            Some("text/html; charset=a{b")
        );
    }

    #[test]
    fn parses_and_writes_content_types_as_tika_does() {
        let cases = [
            ("Text/Plain", "text/plain"),
            (" text / csv ;charset=UTF-8", "text/csv; charset=UTF-8"),
            (
                "text/csv; charset=\"utf-8\"; Header=present",
                "text/csv; charset=utf-8; header=present",
            ),
            ("text/x-a; b=c;; ;d", "text/x-a; b=c; d="),
            ("text/x-a; note=\"a b\"", "text/x-a; note=\"a b\""),
            ("text/x-a; path='/x'", "text/x-a; path=\"\\/x\""),
        ];
        for (input, expected) in cases {
            assert_eq!(parse(input).as_deref(), Some(expected), "{input}");
        }
        for invalid in ["text", "/plain", "text/", "text/pl ain", "te(xt/plain"] {
            assert_eq!(parse(invalid), None, "{invalid}");
        }
        let quoted = MediaType::named("text/x-a").with_parameter("q", "a=b\"c");
        assert_eq!(quoted.to_string(), "text/x-a; q=\"a\\=b\\\"c\"");
        let replaced = MediaType::named("text/csv; charset=a").with_parameter("charset", "b");
        assert_eq!(replaced.to_string(), "text/csv; charset=b");
    }

    #[test]
    fn splits_the_parts() {
        let t = MediaType::named("image/vnd.dgn; version=8");
        assert_eq!((t.kind(), t.subtype()), ("image", "vnd.dgn"));
        assert!(t.has_parameters());
        assert_eq!(t.base_type().as_str(), "image/vnd.dgn");
    }
}
