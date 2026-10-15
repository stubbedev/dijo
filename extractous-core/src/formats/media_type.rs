//! A port of the part of Tika's `MediaType` that TextAndCSVParser relies on: parsing a content
//! type into its type, subtype and parameters, and writing one back in Tika's canonical form.

use std::collections::BTreeMap;
use std::fmt;

/// A media type with its parameters, as Tika's `MediaType` holds one: type and subtype in lower
/// case, parameter names in lower case and sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MediaType {
    kind: String,
    subtype: String,
    parameters: BTreeMap<String, String>,
}

/// The characters Tika's `MediaType` neither allows in a type name nor writes unquoted in a
/// parameter value.
fn is_special(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '<' | '>' | '@' | ',' | ';' | ':' | '\\' | '"' | '/' | '[' | ']' | '?' | '='
    )
}

/// Java's `\s`: the ASCII white space of a regular expression without flags.
fn is_java_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{B}' | '\u{C}' | '\r')
}

/// A character of a type or subtype name in Tika's `VALID_CHARS`: none of the specials, white
/// space or U+001C (Java reads the class's `\c\` as that control character).
fn is_valid_name_char(c: char) -> bool {
    !is_special(c) && !is_java_space(c) && c != '\u{1C}'
}

/// Java's `String.trim`: every character up to U+0020 removed from both ends.
fn java_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c <= ' ')
}

impl MediaType {
    /// A media type without parameters.
    pub(crate) fn new(kind: &str, subtype: &str) -> Self {
        MediaType {
            kind: java_trim(kind).to_lowercase(),
            subtype: java_trim(subtype).to_lowercase(),
            parameters: BTreeMap::new(),
        }
    }

    /// The media type a content type names, as `MediaType.parse` reads it: `type/subtype`,
    /// optionally followed by `;`-separated `name=value` parameters whose values may be quoted,
    /// or a `charset=` parameter, a `;` and `type/subtype`; `None` for a content type it does
    /// not parse.
    pub(crate) fn parse(content_type: &str) -> Option<Self> {
        if let Some((kind, subtype, tail)) = type_and_subtype(content_type) {
            if tail.is_empty() || tail.starts_with(';') {
                return Some(MediaType::new(kind, subtype).with_parameters(tail));
            }
        }
        charset_first(content_type)
    }

    /// This media type with the parameters `MediaType.parseParameters` reads from `text`.
    fn with_parameters(mut self, text: &str) -> Self {
        for parameter in text.split(';') {
            let (name, value) = parameter.split_once('=').unwrap_or((parameter, ""));
            let name = java_trim(name);
            if !name.is_empty() {
                self.parameters
                    .insert(name.to_lowercase(), unquote(java_trim(value)).to_string());
            }
        }
        self
    }

    /// The type and subtype, without parameters: `getBaseType`.
    pub(crate) fn base(&self) -> MediaType {
        MediaType::new(&self.kind, &self.subtype)
    }

    /// This media type with a parameter set, replacing any of the same name.
    pub(crate) fn with(mut self, name: &str, value: &str) -> Self {
        self.parameters
            .insert(java_trim(name).to_lowercase(), value.to_string());
        self
    }
}

/// The type, subtype and what follows them at the start of `text`, as
/// `\s*VALID+\s*/\s*VALID+\s*` matches them: what follows starts after the white space.
fn type_and_subtype(text: &str) -> Option<(&str, &str, &str)> {
    let trimmed = text.trim_start_matches(is_java_space);
    let slash = trimmed.find('/')?;
    let kind = trimmed[..slash].trim_end_matches(is_java_space);
    let rest = trimmed[slash + 1..].trim_start_matches(is_java_space);
    let subtype_end = rest
        .find(|c: char| !is_valid_name_char(c))
        .unwrap_or(rest.len());
    let subtype = &rest[..subtype_end];
    let names_valid = [kind, subtype]
        .iter()
        .all(|name| !name.is_empty() && name.chars().all(is_valid_name_char));
    names_valid.then(|| {
        (
            kind,
            subtype,
            rest[subtype_end..].trim_start_matches(is_java_space),
        )
    })
}

/// Tika's form with the charset first, `(?is)\s*(charset\s*=\s*[^\c;\s]+)\s*;\s*TYPE\s*`, where
/// Java reads `\c;` as `{`: the value runs over any `;` but the last one the type can follow.
fn charset_first(text: &str) -> Option<MediaType> {
    let trimmed = text.trim_start_matches(is_java_space);
    let keyword = trimmed.get(..7)?;
    if !keyword.eq_ignore_ascii_case("charset") {
        return None;
    }
    let after = trimmed[7..].trim_start_matches(is_java_space);
    let value_start = after.strip_prefix('=')?.trim_start_matches(is_java_space);
    let run = value_start
        .find(|c: char| c == '{' || is_java_space(c))
        .unwrap_or(value_start.len());
    let value_offset = trimmed.len() - value_start.len();
    let ends: Vec<usize> = value_start[..run]
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .collect();
    ends.into_iter().rev().find_map(|end| {
        let rest = value_start[end..]
            .trim_start_matches(is_java_space)
            .strip_prefix(';')?;
        let (kind, subtype, tail) = type_and_subtype(rest)?;
        tail.is_empty()
            .then(|| MediaType::new(kind, subtype).with_parameters(&trimmed[..value_offset + end]))
    })
}

/// Tika's fuzzy unquoting: every leading and trailing `"` or `'` dropped.
fn unquote(value: &str) -> &str {
    value.trim_matches(['"', '\''])
}

/// Tika's canonical form: `type/subtype`, then `; name=value` for each parameter in name order,
/// a value with a special character or white space quoted and its specials escaped.
impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.subtype)?;
        for (name, value) in &self.parameters {
            if !value.chars().any(|c| is_special(c) || is_java_space(c)) {
                write!(f, "; {name}={value}")?;
                continue;
            }
            write!(f, "; {name}=\"")?;
            for c in value.chars() {
                if is_special(c) {
                    f.write_str("\\")?;
                }
                write!(f, "{c}")?;
            }
            f.write_str("\"")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_writes_as_tika_does() {
        let cases = [
            ("text/plain", "text/plain"),
            ("Text/Plain", "text/plain"),
            (" text / csv ;charset=UTF-8", "text/csv; charset=UTF-8"),
            (
                "text/csv; charset=\"utf-8\"; Header=present",
                "text/csv; charset=utf-8; header=present",
            ),
            ("text/x-a; b=c;; ;d", "text/x-a; b=c; d="),
            ("text/x-a; note=\"a b\"", "text/x-a; note=\"a b\""),
            ("text/x-a; path='/x'", "text/x-a; path=\"\\/x\""),
            (" Charset = utf-8 ; Text/HTML ", "text/html; charset=utf-8"),
            ("charset=a;b;text/html", "text/html; b=; charset=a"),
        ];
        for (input, expected) in cases {
            let parsed = MediaType::parse(input).unwrap_or_else(|| panic!("{input} parses"));
            assert_eq!(parsed.to_string(), expected, "{input}");
        }
        for invalid in ["text", "/plain", "text/", "text/pl ain", "te(xt/plain"] {
            assert_eq!(MediaType::parse(invalid), None, "{invalid}");
        }
        let quoted = MediaType::new("text", "x-a").with("q", "a=b\"c");
        assert_eq!(quoted.to_string(), "text/x-a; q=\"a\\=b\\\"c\"");
    }
}
