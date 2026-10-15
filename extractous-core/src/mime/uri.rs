//! The file name `MimeTypes` takes from a resource name: the last segment of the path that
//! `java.net.URI` parses it into, or the whole name when it is not a URI.
//!
//! The parse follows `java.net.URI`'s for everything a file name can hold. An authority
//! (`//host/...`) is accepted when it is a registry name; one that only parses as a server
//! address, an IPv6 literal for instance, is treated as unparseable.

use unicode_general_category::{get_general_category, GeneralCategory};

/// The name to look up by its extension, and whether the resource name is an HTTP URL, for
/// which Tika ignores the names of server-side scripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameHint {
    pub(crate) name: String,
    pub(crate) http: bool,
}

/// What `MimeTypes.detect` looks a resource name up by, or `None` when it takes no name from
/// it, as for an opaque URI or a path ending in a slash.
pub(crate) fn name_hint(resource_name: &str) -> Option<NameHint> {
    if resource_name.contains(' ') {
        return Some(NameHint {
            name: resource_name.to_owned(),
            http: false,
        });
    }
    let Some(parsed) = parse(resource_name) else {
        return Some(NameHint {
            name: resource_name.to_owned(),
            http: false,
        });
    };
    let http = parsed
        .scheme
        .is_some_and(|scheme| scheme.starts_with("http"));
    let path = decode(parsed.path?);
    let last = match path.rfind('/') {
        Some(slash) => &path[slash + 1..],
        None => path.as_str(),
    };
    (!last.is_empty()).then(|| NameHint {
        name: last.to_owned(),
        http,
    })
}

/// What `java.net.URI` parses a string into, as far as Tika and POI look at it.
pub(crate) struct Parsed<'a> {
    pub(crate) scheme: Option<&'a str>,
    /// The raw path of a hierarchical URI; `None` for an opaque one.
    pub(crate) path: Option<&'a str>,
    pub(crate) query: Option<&'a str>,
    pub(crate) fragment: Option<&'a str>,
}

fn is_mark(c: char) -> bool {
    "-_.!~*'()".contains(c)
}

fn is_unreserved(c: char) -> bool {
    c.is_ascii_alphanumeric() || is_mark(c)
}

fn is_reserved(c: char) -> bool {
    ";/?:@&=+$,[]".contains(c)
}

fn is_uric(c: char) -> bool {
    is_reserved(c) || is_unreserved(c)
}

fn is_path(c: char) -> bool {
    is_unreserved(c) || ":@&=+$,;/".contains(c)
}

fn is_scheme(c: char) -> bool {
    c.is_ascii_alphanumeric() || "+-.".contains(c)
}

fn is_reg_name(c: char) -> bool {
    is_unreserved(c) || "$,;:@&=+".contains(c)
}

/// `Character.isSpaceChar`: the separators.
fn is_space_char(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::SpaceSeparator
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    )
}

/// `Parser.checkChars` with escapes allowed: every character is in the class, a `%XX` escape,
/// or a visible character beyond ASCII.
fn check(text: &str, class: fn(char) -> bool) -> Option<()> {
    let bytes = text.as_bytes();
    let mut chars = text.char_indices();
    while let Some((index, c)) = chars.next() {
        if c.is_ascii() && class(c) {
            continue;
        }
        if c == '%' {
            let hex = bytes.get(index + 1..index + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            chars.next();
            chars.next();
            continue;
        }
        if u32::from(c) > 128 && !is_space_char(c) && !c.is_control() {
            continue;
        }
        return None;
    }
    Some(())
}

/// `URI.Parser.parse`: `None` where it throws `URISyntaxException`.
pub(crate) fn parse(input: &str) -> Option<Parsed<'_>> {
    let first_stop = input.find(['/', '?', '#', ':']);
    let (scheme, rest) = match first_stop {
        Some(colon) if input[colon..].starts_with(':') => {
            let scheme = &input[..colon];
            let mut chars = scheme.chars();
            if !chars.next()?.is_ascii_alphabetic() || !chars.all(is_scheme) {
                return None;
            }
            (Some(scheme), &input[colon + 1..])
        }
        _ => (None, input),
    };
    if scheme.is_some() && !rest.starts_with('/') {
        let opaque_end = rest.find('#').unwrap_or(rest.len());
        if opaque_end == 0 {
            return None;
        }
        check(&rest[..opaque_end], is_uric)?;
        let fragment = rest.get(opaque_end + 1..);
        check(fragment.unwrap_or(""), is_uric)?;
        return Some(Parsed {
            scheme,
            path: None,
            query: None,
            fragment,
        });
    }
    let mut rest = rest;
    if let Some(after) = rest.strip_prefix("//") {
        let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
        if authority_end == 0 && authority_end == after.len() {
            return None;
        }
        if authority_end > 0 {
            check(&after[..authority_end], is_reg_name)?;
        }
        rest = &after[authority_end..];
    }
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let path = &rest[..path_end];
    check(path, is_path)?;
    let mut rest = &rest[path_end..];
    let mut query = None;
    if let Some(after) = rest.strip_prefix('?') {
        let query_end = after.find('#').unwrap_or(after.len());
        check(&after[..query_end], is_uric)?;
        query = Some(&after[..query_end]);
        rest = &after[query_end..];
    }
    let fragment = rest.strip_prefix('#');
    if let Some(fragment) = fragment {
        check(fragment, is_uric)?;
    }
    Some(Parsed {
        scheme,
        path: Some(path),
        query,
        fragment,
    })
}

/// `URI.toASCIIString` of a URI parsed from `input`: its characters beyond ASCII, normalized
/// to NFC, percent-encoded as UTF-8.
pub(crate) fn to_ascii_string(input: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    if input.is_ascii() {
        return input.to_owned();
    }
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.nfc().collect::<String>().bytes() {
        if byte >= 0x80 {
            out.push_str(&format!("%{byte:02X}"));
        } else {
            out.push(char::from(byte));
        }
    }
    out
}

/// `URI.decode`: `%XX` escapes as UTF-8, malformed sequences replaced.
pub(crate) fn decode(path: &str) -> String {
    if !path.contains('%') {
        return path.to_owned();
    }
    let mut out = String::with_capacity(path.len());
    let mut bytes = Vec::new();
    let mut rest = path;
    while let Some(percent) = rest.find('%') {
        out.push_str(&rest[..percent]);
        let mut escapes = &rest[percent..];
        bytes.clear();
        while let Some(hex) = escapes.strip_prefix('%') {
            bytes.push(u8::from_str_radix(&hex[..2], 16).expect("the parse checked the escape"));
            escapes = &hex[2..];
        }
        out.push_str(&String::from_utf8_lossy(&bytes));
        rest = escapes;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(name: &str) -> Option<(String, bool)> {
        name_hint(name).map(|hint| (hint.name, hint.http))
    }

    #[test]
    fn takes_the_last_path_segment() {
        assert_eq!(hint("report.pdf"), Some(("report.pdf".into(), false)));
        assert_eq!(hint("a%20b.pdf"), Some(("a b.pdf".into(), false)));
        assert_eq!(hint("file#1.txt"), Some(("file".into(), false)));
        assert_eq!(hint("a?b.pdf"), Some(("a".into(), false)));
        assert_eq!(
            hint("http://example.com/x/page.php?a=1"),
            Some(("page.php".into(), true))
        );
        assert_eq!(hint("caf\u{e9}.txt"), Some(("caf\u{e9}.txt".into(), false)));
    }

    #[test]
    fn takes_no_name_from_opaque_uris_or_directories() {
        assert_eq!(hint("c:foo.txt"), None);
        assert_eq!(hint("dir/"), None);
        assert_eq!(hint(""), None);
    }

    #[test]
    fn keeps_names_that_are_not_uris() {
        assert_eq!(hint("my file.doc"), Some(("my file.doc".into(), false)));
        assert_eq!(hint("x[1].doc"), Some(("x[1].doc".into(), false)));
        assert_eq!(hint("100%.doc"), Some(("100%.doc".into(), false)));
        assert_eq!(hint("1a:b.doc"), Some(("1a:b.doc".into(), false)));
        assert_eq!(hint(":b.doc"), Some((":b.doc".into(), false)));
        assert_eq!(hint("a{b}.doc"), Some(("a{b}.doc".into(), false)));
    }
}
