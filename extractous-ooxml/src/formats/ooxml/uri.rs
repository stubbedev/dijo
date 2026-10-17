//! The URIs of a package's relationships as POI reads them: `PackagingURIHelper.toURI` prepares
//! the target, `java.net.URI` accepts or rejects it, and an internal target is resolved against
//! the part that names it.

use crate::unicode::is_whitespace;
use crate::unicode::{is_iso_control, is_space_char};

/// What POI uses for a target `java.net.URI` rejects.
pub(super) const INVALID_TARGET: &str = "http://invalid.uri";

/// `PackagingURIHelper.toURI(value).toString()`, or `None` where `new URI` throws.
pub(super) fn to_uri(value: &str) -> Option<String> {
    let mut value = value.replace('\\', "/");
    if let Some(index) = value.find('#') {
        let fragment = encode(&value[index + 1..]);
        value.truncate(index + 1);
        value.push_str(&fragment);
    }
    let trailing = value
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_whitespace(c) || c == '\u{a0}')
        .last()
        .map(|(index, _)| index);
    if let Some(index) = trailing {
        let encoded = encode(&value[index..]);
        value.truncate(index);
        value.push_str(&encoded);
    }
    if is_missing_authority(&value) {
        value.push('/');
    }
    is_valid(&value).then_some(value)
}

/// `PackagingURIHelper.encode`: the UTF-8 bytes from 0x80, `|` and whitespace percent-encoded.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if byte >= 0x80 || byte == b'|' || is_whitespace(char::from(byte)) {
            encoded.push_str(&format!("%{byte:02X}"));
        } else {
            encoded.push(char::from(byte));
        }
    }
    encoded
}

/// `\w+://`, the whole value: a scheme with an empty authority, to which POI adds a slash.
fn is_missing_authority(value: &str) -> bool {
    value.strip_suffix("://").is_some_and(|scheme| {
        !scheme.is_empty()
            && scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

const ALPHA: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const DIGIT: &str = "0123456789";
const MARK: &str = "-_.!~*'()";
const RESERVED: &str = ";/?:@&=+$,[]";

/// The character classes of `java.net.URI`'s parser.
#[derive(Clone, Copy)]
enum Class {
    /// `uric`: reserved, unreserved and escaped characters.
    Uric,
    /// The characters of a path.
    Path,
    /// The characters of a registry-based authority.
    RegName,
    /// The characters of a scheme after its first letter.
    Scheme,
}

impl Class {
    fn contains(self, c: char) -> bool {
        let unreserved = |c: char| ALPHA.contains(c) || DIGIT.contains(c) || MARK.contains(c);
        match self {
            Class::Uric => unreserved(c) || RESERVED.contains(c),
            Class::Path => unreserved(c) || ":@&=+$,;/".contains(c),
            Class::RegName => unreserved(c) || "$,;:@&=+".contains(c),
            Class::Scheme => ALPHA.contains(c) || DIGIT.contains(c) || "+-.".contains(c),
        }
    }

    fn escapes(self) -> bool {
        !matches!(self, Class::Scheme)
    }
}

/// Whether every character of `text` belongs to `class`, an escape `%XX` and, where escapes
/// are allowed, a non-ASCII character other than a space or a control counting as one.
fn all_in(text: &str, class: Class) -> bool {
    let mut characters = text.chars();
    while let Some(c) = characters.next() {
        if class.contains(c) {
            continue;
        }
        if !class.escapes() {
            return false;
        }
        if c == '%' {
            let mut hex = characters.clone().take(2);
            if !(hex.next().is_some_and(|h| h.is_ascii_hexdigit())
                && hex.next().is_some_and(|h| h.is_ascii_hexdigit()))
            {
                return false;
            }
            characters.nth(1);
            continue;
        }
        if u32::from(c) > 128 && !is_space_char(c) && !is_iso_control(c) {
            continue;
        }
        return false;
    }
    true
}

/// Whether `new java.net.URI(value)` accepts `value`. An authority holding an IPv6 literal, the
/// one form a server-based authority has that a registry-based one lacks, is not accepted.
fn is_valid(value: &str) -> bool {
    let (body, fragment) = match value.find('#') {
        Some(index) => (&value[..index], Some(&value[index + 1..])),
        None => (value, None),
    };
    if fragment.is_some_and(|fragment| !all_in(fragment, Class::Uric)) {
        return false;
    }
    let scheme_end = body
        .find(['/', '?', '#', ':'])
        .filter(|&index| body[index..].starts_with(':'));
    let Some(scheme_end) = scheme_end else {
        return is_valid_hierarchical(body);
    };
    let scheme = &body[..scheme_end];
    let mut letters = scheme.chars();
    if !letters.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !all_in(&scheme[1..], Class::Scheme)
    {
        return false;
    }
    let rest = &body[scheme_end + 1..];
    if rest.starts_with('/') {
        return is_valid_hierarchical(rest);
    }
    !rest.is_empty() && all_in(rest, Class::Uric)
}

fn is_valid_hierarchical(text: &str) -> bool {
    let mut rest = text;
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        if end == 0 && end == after.len() {
            return false;
        }
        let authority = &after[..end];
        if authority.contains('[') || authority.contains(']') || !all_in(authority, Class::RegName)
        {
            return false;
        }
        rest = &after[end..];
    }
    let (path, query) = match rest.find('?') {
        Some(index) => (&rest[..index], Some(&rest[index + 1..])),
        None => (rest, None),
    };
    all_in(path, Class::Path) && query.is_none_or(|query| all_in(query, Class::Uric))
}

/// Where an internal target leads, resolved against the path of the part naming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Resolution {
    /// The part name the target resolves to.
    Name(String),
    /// POI throws on the target: it is absolute, or it resolves to no valid part name.
    Fails,
    /// A target with characters, queries or fragments this port does not resolve.
    Unported,
}

/// The characters this port resolves part names of: those a package's own names use.
pub(super) fn is_plain_name_character(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~' | '/')
}

/// `PackageRelationship.getTargetURI` of an internal relationship from `source` (`/` for the
/// package), with `target` already through [`to_uri`], then the part name POI makes of it.
pub(super) fn resolve(source: &str, target: Option<&str>) -> Resolution {
    let Some(target) = target else {
        return Resolution::Fails;
    };
    if !target.chars().all(is_plain_name_character) {
        return if is_valid(target) && has_scheme(target) {
            Resolution::Fails
        } else {
            Resolution::Unported
        };
    }
    if target.is_empty() || target.contains("//") {
        return Resolution::Unported;
    }
    let path = if target.starts_with('/') {
        target.to_owned()
    } else {
        let directory = &source[..source.rfind('/').map_or(0, |index| index + 1)];
        normalize(&format!("{directory}{target}"))
    };
    if is_valid_part_name(&path) {
        Resolution::Name(path)
    } else {
        Resolution::Fails
    }
}

fn has_scheme(value: &str) -> bool {
    value
        .find(['/', '?', '#', ':'])
        .is_some_and(|index| value[index..].starts_with(':'))
}

/// `java.net.URI`'s normalization of an absolute path: redundant slashes and `.` segments
/// removed, a segment followed by `..` removed with it; leading `..` segments stay.
fn normalize(path: &str) -> String {
    let trailing = path.ends_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        match segment {
            "." => {}
            ".." if segments.last().is_some_and(|last| *last != "..") => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    let mut normalized = String::with_capacity(path.len());
    for segment in &segments {
        normalized.push('/');
        normalized.push_str(segment);
    }
    if trailing || normalized.is_empty() {
        normalized.push('/');
    }
    normalized
}

/// POI's rules for a part name of plain characters: it starts with a slash, and has segments,
/// none of them empty or ending with a dot.
pub(super) fn is_valid_part_name(name: &str) -> bool {
    name.strip_prefix('/').is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .split('/')
                .all(|segment| !segment.is_empty() && !segment.ends_with('.'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepares_targets_as_poi_does() {
        assert_eq!(to_uri("http://x.org/a b").as_deref(), None);
        assert_eq!(
            to_uri("http://x.org/a#b c|d").as_deref(),
            Some("http://x.org/a#b%20c%7Cd")
        );
        assert_eq!(
            to_uri("http://x.org/a  ").as_deref(),
            Some("http://x.org/a%20%20")
        );
        assert_eq!(to_uri("http://").as_deref(), Some("http:///"));
        assert_eq!(
            to_uri("C:\\dir\\file.doc").as_deref(),
            Some("C:/dir/file.doc")
        );
        assert_eq!(to_uri("mailto:a@b.dk").as_deref(), Some("mailto:a@b.dk"));
        assert_eq!(to_uri("http://x.org/ø").as_deref(), Some("http://x.org/ø"));
        assert_eq!(to_uri("http://x.org/%zz").as_deref(), None);
        assert_eq!(to_uri("http://[::1]/").as_deref(), None);
        assert_eq!(to_uri("1abc:x").as_deref(), None);
        assert_eq!(to_uri("abc1:x").as_deref(), Some("abc1:x"));
        assert_eq!(to_uri("a{b").as_deref(), None);
        assert_eq!(to_uri("").as_deref(), Some(""));
    }

    #[test]
    fn resolves_internal_targets_as_poi_does() {
        let resolve = |source, target: &str| resolve(source, Some(target));
        assert_eq!(
            resolve("/word/document.xml", "styles.xml"),
            Resolution::Name("/word/styles.xml".to_owned())
        );
        assert_eq!(
            resolve("/word/document.xml", "../customXml/item1.xml"),
            Resolution::Name("/customXml/item1.xml".to_owned())
        );
        assert_eq!(
            resolve("/", "word/document.xml"),
            Resolution::Name("/word/document.xml".to_owned())
        );
        assert_eq!(
            resolve("/word/document.xml", "../../x.xml"),
            Resolution::Fails
        );
        assert_eq!(
            resolve("/word/document.xml", "http://x.org/"),
            Resolution::Fails
        );
        assert_eq!(
            resolve("/word/document.xml", "media/a%20b.png"),
            Resolution::Unported
        );
        assert_eq!(resolve("/word/document.xml", "dir/"), Resolution::Fails);
    }
}
