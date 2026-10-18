//! Java's `java.net.URL`, as far as `HtmlHandler` takes it: parsing a URL, alone or against a
//! context, and its external form, with the protocol handlers Tika's native image has: `http`,
//! `https`, `file`, `resource` and `jar`. Every other protocol is unknown to it, so a URL of one
//! is malformed.
//!
//! A URL Java refuses as malformed is [`None`]; the JDK's checks of the host, the user
//! information and the authority under its default early URL parsing come with it.

use crate::formats::thrown::{Exception, Thrown};
use crate::java;
use crate::unicode;

/// The prefixes of the URLs `HtmlHandler.resolve` leaves as they are, in lower case.
const UNRESOLVED: [&str; 6] = ["urn:", "mailto:", "tel:", "data:", "javascript:", "about:"];

/// `HtmlHandler.resolve(base, url)`: `url`, trimmed, against the URL `base`, unless there is
/// none, `url` starts with a prefix of [`UNRESOLVED`] or either is malformed. A query alone
/// replaces the query of a base whose path does not end with `/`, its user information dropped.
/// Java throws a `NullPointerException` on a query alone against a base without a path.
pub(super) fn resolve(base: Option<&str>, url: &str) -> Result<String, Thrown> {
    let url = java::trim(url);
    let lower = java::to_lower_case(url);
    let Some(base) = base.filter(|_| !UNRESOLVED.iter().any(|p| lower.starts_with(p))) else {
        return Ok(url.to_owned());
    };
    let Some(base) = Url::parse(java::trim(base)) else {
        return Ok(url.to_owned());
    };
    let resolved = if url.starts_with('?') {
        let path = base
            .path
            .as_deref()
            .ok_or_else(|| Thrown::Runtime(Exception::bare("java.lang.NullPointerException")))?;
        if !path.is_empty() && !path.ends_with('/') {
            base.with_file(&format!("{path}{url}"))
        } else {
            base.join(url)
        }
    } else {
        base.join(url)
    };
    Ok(resolved.map_or_else(|| url.to_owned(), |resolved| resolved.external_form()))
}

/// Whether `new URL(text)` succeeds.
pub(super) fn is_url(text: &str) -> bool {
    Url::parse(text).is_some()
}

/// A protocol Tika's native image has a handler for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    Http,
    Https,
    File,
    Resource,
    Jar,
}

impl Protocol {
    /// `URL.getURLStreamHandler` of a protocol `lowerCaseProtocol` made.
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "http" => Self::Http,
            "https" => Self::Https,
            "file" => Self::File,
            "resource" => Self::Resource,
            "jar" => Self::Jar,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::File => "file",
            Self::Resource => "resource",
            Self::Jar => "jar",
        }
    }
}

/// A `java.net.URL` as its handler leaves it, its fields `None` where Java's are `null`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Url {
    protocol: Protocol,
    host: String,
    port: i32,
    file: Option<String>,
    query: Option<String>,
    authority: Option<String>,
    path: Option<String>,
    user_info: Option<String>,
    reference: Option<String>,
}

/// What a handler's `parseURL` starts from: the URL constructor's protocol, query and
/// reference, and the context whose fields it copied for a relative URL.
struct Parsing<'c> {
    protocol: Protocol,
    context: Option<&'c Url>,
    query: Option<String>,
    reference: Option<String>,
}

impl Url {
    /// `new URL(spec)`.
    fn parse(spec: &str) -> Option<Self> {
        Self::new(None, spec)
    }

    /// `new URL(this, spec)`.
    fn join(&self, spec: &str) -> Option<Self> {
        Self::new(Some(self), spec)
    }

    /// `new URL(getProtocol(), getHost(), getPort(), file)`.
    fn with_file(&self, file: &str) -> Option<Self> {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port < -1 {
            return None;
        }
        let authority = if self.port == -1 {
            host.clone()
        } else {
            format!("{host}:{}", self.port)
        };
        let (file, reference) = match file.split_once('#') {
            Some((file, reference)) => (file, Some(reference.to_owned())),
            None => (file, None),
        };
        let (path, query) = split_query(file);
        check_host(&host)?;
        if self.protocol == Protocol::Jar && is_jar(file) {
            return None;
        }
        Some(Self {
            protocol: self.protocol,
            host,
            port: self.port,
            file: Some(file.to_owned()),
            query,
            authority: Some(authority),
            path: Some(path),
            user_info: None,
            reference,
        })
    }

    /// `toExternalForm()`.
    fn external_form(&self) -> String {
        let mut form = format!("{}:", self.protocol.name());
        if let Some(authority) = self.authority.as_deref().filter(|a| !a.is_empty()) {
            form.push_str("//");
            form.push_str(authority);
        }
        if let Some(path) = &self.path {
            form.push_str(path);
        }
        if let Some(query) = &self.query {
            form.push('?');
            form.push_str(query);
        }
        if let Some(reference) = &self.reference {
            form.push('#');
            form.push_str(reference);
        }
        form
    }

    /// `new URL(context, spec, null)`.
    fn new(context: Option<&Self>, spec: &str) -> Option<Self> {
        let bytes = spec.as_bytes();
        let mut limit = bytes.len();
        while limit > 0 && bytes[limit - 1] <= b' ' {
            limit -= 1;
        }
        let mut start = 0;
        while start < limit && bytes[start] <= b' ' {
            start += 1;
        }
        if bytes
            .get(start..start + 4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"url:"))
        {
            start += 4;
        }
        let mut new_protocol = None;
        if bytes.get(start) != Some(&b'#') {
            if let Some(at) = spec[start..limit].find([':', '/']) {
                if bytes[start + at] == b':' {
                    let protocol = java::to_lower_case(&spec[start..start + at]);
                    if is_valid_protocol(&protocol) {
                        new_protocol = Some(protocol);
                        start += at + 1;
                    }
                }
            }
        }
        let mut relative_to = None;
        if let Some(context) = context {
            let matches = new_protocol
                .as_deref()
                .is_none_or(|name| java::equals_ignore_case(name, context.protocol.name()));
            if matches {
                if context
                    .path
                    .as_deref()
                    .is_some_and(|path| path.starts_with('/'))
                {
                    new_protocol = None;
                }
                if new_protocol.is_none() {
                    relative_to = Some(context);
                }
            }
        }
        let protocol = match (&new_protocol, relative_to) {
            (_, Some(context)) => context.protocol,
            (Some(name), None) => Protocol::named(name)?,
            (None, None) => return None,
        };
        let mut reference = None;
        if let Some(at) = spec[start..].find('#') {
            reference = Some(spec[start + at + 1..limit].to_owned());
            limit = start + at;
        }
        let mut query = None;
        if let Some(context) = relative_to.filter(|_| start == limit) {
            query.clone_from(&context.query);
            if reference.is_none() {
                reference.clone_from(&context.reference);
            }
        }
        let parsing = Parsing {
            protocol,
            context: relative_to,
            query,
            reference,
        };
        match protocol {
            Protocol::Jar => parsing.jar(spec, start, limit),
            Protocol::Http | Protocol::Https | Protocol::File | Protocol::Resource => {
                parsing.hierarchical(spec, start, limit)
            }
        }
    }

    /// `URLStreamHandler.setURL`, after its checks of a built-in handler's URL.
    #[allow(clippy::too_many_arguments)]
    fn checked(
        protocol: Protocol,
        host: String,
        port: i32,
        authority: Option<String>,
        user_info: Option<String>,
        path: Option<String>,
        query: Option<String>,
        reference: Option<String>,
    ) -> Option<Self> {
        check_host(&host)?;
        if let Some(user_info) = &user_info {
            scan(user_info, |c| is_excluded(c) && c != ':')?;
        }
        if let Some(authority) = &authority {
            scan(authority, |c| {
                is_excluded(c) && !matches!(c, '@' | '[' | ']' | ':')
            })?;
        }
        let file = path.as_ref().map(|path| match &query {
            Some(query) => format!("{path}?{query}"),
            None => path.clone(),
        });
        Some(Self {
            protocol,
            host,
            port,
            file,
            query,
            authority,
            path,
            user_info,
            reference,
        })
    }
}

impl Parsing<'_> {
    /// `URLStreamHandler.parseURL`, which the `file` handler takes with `/` as the separator.
    fn hierarchical(self, spec: &str, mut start: usize, mut limit: usize) -> Option<Url> {
        let context = self.context;
        let mut authority = context.and_then(|context| context.authority.clone());
        let mut user_info = context.and_then(|context| context.user_info.clone());
        let mut host = context.map(|context| context.host.clone());
        let mut port = context.map_or(-1, |context| context.port);
        let mut path = context.and_then(|context| context.path.clone());
        let mut query = self.query;
        let mut spec = spec;
        let mut is_relative_path = false;
        let mut query_only = false;
        if start < limit {
            if let Some(query_start) = spec.find('?') {
                query_only = query_start == start;
                if query_start < limit {
                    query = Some(spec[query_start + 1..limit].to_owned());
                    limit = query_start;
                    spec = &spec[..query_start];
                }
            }
        }
        let bytes = spec.as_bytes();
        let is_unc_name = start + 4 <= limit && bytes[start..start + 4] == *b"////";
        if !is_unc_name && start + 2 <= limit && bytes[start..start + 2] == *b"//" {
            start += 2;
            let end = match spec[start..].find('/').map(|at| start + at) {
                Some(end) if end <= limit => end,
                _ => match spec[start..].find('?').map(|at| start + at) {
                    Some(end) if end <= limit => end,
                    _ => limit,
                },
            };
            let whole = spec[start..end].to_owned();
            let named = match whole.find('@') {
                Some(at) if whole.rfind('@') != Some(at) => {
                    user_info = None;
                    None
                }
                Some(at) => {
                    user_info = Some(whole[..at].to_owned());
                    Some(&whole[at + 1..])
                }
                None => {
                    user_info = None;
                    Some(whole.as_str())
                }
            };
            host = Some(match named {
                Some(named) => {
                    let (named, named_port) = split_port(named)?;
                    port = named_port;
                    named
                }
                None => String::new(),
            });
            if port < -1 {
                return None;
            }
            start = end;
            if !whole.is_empty() {
                path = Some(String::new());
            }
            authority = Some(whole);
        }
        if start < limit {
            let tail = &spec[start..limit];
            path = Some(if tail.starts_with('/') {
                tail.to_owned()
            } else {
                match path.as_deref().filter(|path| !path.is_empty()) {
                    Some(base) => {
                        is_relative_path = true;
                        match base.rfind('/') {
                            None if authority.is_some() => format!("/{tail}"),
                            None => tail.to_owned(),
                            Some(at) => format!("{}{tail}", &base[..=at]),
                        }
                    }
                    None if authority.is_some() => format!("/{tail}"),
                    None => tail.to_owned(),
                }
            });
        } else if query_only {
            if let Some(base) = &path {
                path = Some(match base.rfind('/') {
                    Some(at) => base[..=at].to_owned(),
                    None => "/".to_owned(),
                });
            }
        }
        let path = path.unwrap_or_default();
        let path = if is_relative_path {
            remove_dot_segments(path)
        } else {
            path
        };
        Url::checked(
            self.protocol,
            host.unwrap_or_default(),
            port,
            authority,
            user_info,
            Some(path),
            query,
            self.reference,
        )
    }

    /// The `jar` handler's `parseURL`.
    fn jar(self, whole: &str, start: usize, limit: usize) -> Option<Url> {
        let context_file = self.context.and_then(|context| context.file.as_deref());
        let reference_at = whole[limit..].find('#').map(|at| limit + at);
        let reference_only = reference_at == Some(start);
        let reference = reference_at.map(|at| whole[at + 1..].to_owned());
        let spec = &whole[start..limit];
        if is_jar(spec) {
            return None;
        }
        let file = if is_jar(whole) {
            let bang_slash = index_of_bang_slash(spec)?;
            Url::parse(&spec[..bang_slash - 1])?;
            Some(spec.to_owned())
        } else if reference_only {
            context_file.map(str::to_owned)
        } else {
            let context = context_file?;
            let joined = if spec.starts_with('/') {
                format!("{}{spec}", &context[..index_of_bang_slash(context)?])
            } else {
                format!("{}{spec}", &context[..=context.rfind('/')?])
            };
            Some(canonicalize(joined)?)
        };
        let (path, query) = match file.as_deref().map(split_query) {
            Some((path, query)) => (Some(path), query),
            None => (None, None),
        };
        Url::checked(
            self.protocol,
            String::new(),
            -1,
            None,
            None,
            path,
            query,
            reference,
        )
    }
}

/// A file as `URL` splits it at its last `?`: its path and its query.
fn split_query(file: &str) -> (String, Option<String>) {
    match file.rfind('?') {
        Some(at) => (file[..at].to_owned(), Some(file[at + 1..].to_owned())),
        None => (file.to_owned(), None),
    }
}

/// `URL.isValidProtocol`: a letter, then letters, digits, `.`, `+` and `-`, each one `char`.
fn is_valid_protocol(protocol: &str) -> bool {
    let is_letter = |c: char| c.len_utf16() == 1 && unicode::is_letter(c);
    let mut chars = protocol.chars();
    chars.next().is_some_and(is_letter)
        && chars.all(|c| is_letter(c) || java::is_digit(c) || matches!(c, '.' | '+' | '-'))
}

/// The host and port `parseURL` reads from the host part of an authority, an IPv6 literal in
/// brackets among them; `None` where Java throws.
fn split_port(host: &str) -> Option<(String, i32)> {
    let port_of = |text: &str| -> Option<i32> {
        if text.is_empty() {
            Some(-1)
        } else {
            java::parse_int(text)
        }
    };
    if host.starts_with('[') {
        let close = host.find(']').filter(|&at| at > 2)?;
        if !is_ipv6_literal(&host[1..close]) {
            return None;
        }
        let port = match &host[close + 1..] {
            "" => -1,
            rest => port_of(rest.strip_prefix(':')?)?,
        };
        return Some((host[..=close].to_owned(), port));
    }
    match host.split_once(':') {
        Some((name, port)) => Some((name.to_owned(), port_of(port)?)),
        None => Some((host.to_owned(), -1)),
    }
}

/// `IPAddressUtil.checkHost`, under early URL parsing: an IPv6 literal in brackets with a scope
/// of allowed characters, or a host without excluded, unwise or delimiting characters.
fn check_host(host: &str) -> Option<()> {
    const OTHERS: [u16; 17] = [
        8263, 8264, 8265, 8448, 8449, 8453, 8454, 10868, 65109, 65110, 65119, 65131, 65283, 65295,
        65306, 65311, 65312,
    ];
    if let Some(literal) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        if !is_ipv6_literal(literal) {
            return None;
        }
        return match literal.find('%') {
            Some(at) => scan(&literal[at..], |c| {
                is_non_printable(c) || matches!(c, '[' | ']' | '/' | '\\' | '#' | '?')
            }),
            None => Some(()),
        };
    }
    scan(host, |c| {
        is_excluded(c)
            || matches!(c, '^' | '`' | '{' | '|' | '}' | ' ' | '"' | '<' | '>')
            || u16::try_from(u32::from(c)).is_ok_and(|unit| OTHERS.contains(&unit))
    })
}

/// `IPAddressUtil.scan`: `None` where `matches` finds a character of `text`.
fn scan(text: &str, matches: impl Fn(char) -> bool) -> Option<()> {
    (!text.chars().any(matches)).then_some(())
}

/// The characters `L_EXCLUDE` and `H_EXCLUDE` hold: the controls and `#/:?@[\]`.
fn is_excluded(c: char) -> bool {
    is_non_printable(c) || matches!(c, '#' | '/' | ':' | '?' | '@' | '[' | '\\' | ']')
}

/// The characters `L_NON_PRINTABLE` and `H_NON_PRINTABLE` hold.
fn is_non_printable(c: char) -> bool {
    c < ' ' || c == '\u{7f}'
}

/// `IPAddressUtil.isIPv6LiteralAddress`, which reads ASCII digits only and ignores a scope after
/// `%`.
fn is_ipv6_literal(text: &str) -> bool {
    const ADDRESS: usize = 16;
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() < 2 {
        return false;
    }
    let percent = units.iter().position(|&unit| unit == u16::from(b'%'));
    if percent == Some(units.len() - 1) {
        return false;
    }
    let length = percent.unwrap_or(units.len());
    let colon = u16::from(b':');
    let hex = |unit: u16| {
        u8::try_from(unit)
            .ok()
            .filter(u8::is_ascii)
            .and_then(|byte| char::from(byte).to_digit(16))
    };
    let mut filled = 0;
    let mut compressed = false;
    let mut i = 0;
    if units[0] == colon {
        i = 1;
        if units[1] != colon {
            return false;
        }
    }
    let mut token = i;
    let mut saw_digit = false;
    let mut value: u32 = 0;
    while i < length {
        let unit = units[i];
        i += 1;
        if let Some(digit) = hex(unit) {
            value = (value << 4) | digit;
            if value > 0xffff {
                return false;
            }
            saw_digit = true;
            continue;
        }
        if unit == colon {
            token = i;
            if !saw_digit {
                if compressed {
                    return false;
                }
                compressed = true;
                continue;
            } else if i == length {
                return false;
            }
            if filled + 2 > ADDRESS {
                return false;
            }
            filled += 2;
            saw_digit = false;
            value = 0;
            continue;
        }
        if unit == u16::from(b'.') && filled + 4 <= ADDRESS {
            let ipv4 = String::from_utf16_lossy(&units[token..length]);
            if ipv4.matches('.').count() != 3 || !is_ipv4_literal(&ipv4) {
                return false;
            }
            filled += 4;
            saw_digit = false;
            break;
        }
        return false;
    }
    if saw_digit {
        if filled + 2 > ADDRESS {
            return false;
        }
        filled += 2;
    }
    if compressed {
        if filled == ADDRESS {
            return false;
        }
        filled = ADDRESS;
    }
    filled == ADDRESS
}

/// `IPAddressUtil.textToNumericFormatV4` succeeding: up to four dotted decimal parts of ASCII
/// digits, the last filling the bytes the others leave.
fn is_ipv4_literal(text: &str) -> bool {
    if text.is_empty() || text.len() > 15 {
        return false;
    }
    let mut value: i64 = 0;
    let mut part = 0;
    let mut new_part = true;
    for c in text.chars() {
        if c == '.' {
            if new_part || value > 0xff || part == 3 {
                return false;
            }
            part += 1;
            value = 0;
            new_part = true;
        } else {
            let Some(digit) = c.to_digit(10).filter(|_| c.is_ascii()) else {
                return false;
            };
            value = value * 10 + i64::from(digit);
            new_part = false;
        }
    }
    !new_part && value < 1 << ((4 - part) * 8)
}

/// Whether `spec` starts with `jar:` in any case, which the `jar` handler reads as an absolute
/// URL and its `checkNestedProtocol` refuses after the protocol.
fn is_jar(spec: &str) -> bool {
    spec.as_bytes()
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"jar:"))
}

/// The `jar` handler's `indexOfBangSlash`: where the `/` of the last `!/` is.
fn index_of_bang_slash(spec: &str) -> Option<usize> {
    spec.match_indices("!/").last().map(|(at, _)| at + 1)
}

/// `URLStreamHandler.parseURL`'s removal of `.` and `..` segments from a relative path, which
/// keeps a `..` that would climb above the root.
fn remove_dot_segments(mut path: String) -> String {
    while let Some(at) = path.find("/./") {
        path.replace_range(at..at + 2, "");
    }
    let mut from = 0;
    while let Some(at) = path[from..].find("/../").map(|at| from + at) {
        let previous = path[..at].rfind('/').filter(|_| at > 0);
        match previous {
            Some(previous) if previous != 0 || !path.starts_with("/../") => {
                path.replace_range(previous..at + 3, "");
                from = 0;
            }
            _ => from = at + 3,
        }
    }
    while path.ends_with("/..") {
        let at = path.find("/..").expect("the path ends with /..");
        match path[..at].rfind('/') {
            Some(previous) => path.truncate(previous + 1),
            None => break,
        }
    }
    if path.starts_with("./") && path.len() > 2 {
        path.replace_range(..2, "");
    }
    if path.ends_with("/.") {
        path.pop();
    }
    path
}

/// The `jar` handler's `canonicalizeString` of a joined file: its entry after the last `!/`
/// without `.` and `..` segments; `None` where Java throws, for a file without `!/` that
/// needs it.
fn canonicalize(file: String) -> Option<String> {
    let bang_slash = index_of_bang_slash(&file);
    let from = bang_slash.unwrap_or(0);
    if bang_slash.is_some_and(|at| at >= file.len())
        || (!file[from..].contains("./") && !file.ends_with('.'))
    {
        return Some(file);
    }
    let at = bang_slash?;
    Some(format!(
        "{}{}",
        &file[..at],
        canonicalize_entry(file[at..].to_owned())
    ))
}

/// The `jar` handler's `doCanonicalize`.
fn canonicalize_entry(mut file: String) -> String {
    while let Some(at) = file.find("/../") {
        match file[..at].rfind('/') {
            Some(previous) => file.replace_range(previous..at + 3, ""),
            None => file.replace_range(..at + 3, ""),
        }
    }
    while let Some(at) = file.find("/./") {
        file.replace_range(at..at + 2, "");
    }
    while file.ends_with("/..") {
        let at = file.find("/..").expect("the file ends with /..");
        match file[..at].rfind('/') {
            Some(previous) => file.truncate(previous + 1),
            None => file.truncate(at),
        }
    }
    if file.ends_with("/.") {
        file.pop();
    }
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Tika's path writes for links against a `base`, each `(base, link, resolved)`.
    const RESOLVED: &[(&str, &str, &str)] = &[
        (
            "http://host/dir/page.html?q#f",
            "sub/x.html",
            "http://host/dir/sub/x.html",
        ),
        (
            "http://host/dir/page.html?q#f",
            "?p=1",
            "http://host/dir/page.html?p=1",
        ),
        (
            "http://host/dir/page.html?q#f",
            "#r",
            "http://host/dir/page.html?q#r",
        ),
        (
            "http://host/dir/page.html?q#f",
            "//other/y",
            "http://other/y",
        ),
        ("http://host/dir/page.html?q#f", "/abs", "http://host/abs"),
        ("http://host/dir/page.html?q#f", "../up", "http://host/up"),
        (
            "https://host/dir/page.html",
            "sub/x.html",
            "https://host/dir/sub/x.html",
        ),
        (
            "resource://host/dir/page.html",
            "../up",
            "resource://host/up",
        ),
        (
            "file://host/dir/page.html?q#f",
            "#r",
            "file://host/dir/page.html?q#r",
        ),
        ("ftp://host/dir/page.html", "sub/x.html", "sub/x.html"),
        ("mailto:me@x.org", "c", "c"),
        ("clsid://host/dir/", "x", "x"),
        ("dir/page", "x", "x"),
        ("file:x/y", "c", "file:x/c"),
        ("file:x/y", "../z", "file:x/../z"),
        ("file:x/y", "?q", "file:x/y?q"),
        ("file:/tmp/a/b.html", "c", "file:/tmp/a/c"),
        ("file:/tmp/a/b.html", "?q", "file:/tmp/a/b.html?q"),
        ("file:/tmp/a/b.html", "/d", "file:/d"),
        ("file:/tmp/a/b.html", "//h/e", "file://h/e"),
        ("http://host/d/p#frag", "", "http://host/d/p#frag"),
        ("http://host/d/p#frag", "#n", "http://host/d/p#n"),
        ("http://host/d/p#frag", "?b", "http://host/d/p?b"),
        ("http://host:80/a", "b", "http://host:80/b"),
        ("http://host:80/a", "http://host:/c", "http://host:/c"),
        ("http://host:80/a", "http://host:x/c", "http://host:x/c"),
        ("http://host:80/a", "http://h:99999/c", "http://h:99999/c"),
        ("http://host:80/a", "http://h:-1/x", "http://h:-1/x"),
        ("http://[::1]:8/a/b", "c", "http://[::1]:8/a/c"),
        (
            "http://[::1]:8/a/b",
            "//[fe80::1%25eth0]/x",
            "http://[fe80::1%25eth0]/x",
        ),
        ("http://[::1]:8/a/b", "http://[bad/x", "http://[bad/x"),
        (
            "jar:file:/x.jar!/a/b.html",
            "c.html",
            "jar:file:/x.jar!/a/c.html",
        ),
        ("jar:file:/x.jar!/a/b.html", "/d", "jar:file:/x.jar!/d"),
        ("jar:file:/x.jar!/a/b.html", "../e", "jar:file:/x.jar!/e"),
        (
            "jar:file:/x.jar!/a/b.html",
            "?q",
            "jar:file:/x.jar!/a/b.html?q",
        ),
        (
            "jar:file:/x.jar!/a/b.html",
            "#f",
            "jar:file:/x.jar!/a/b.html#f",
        ),
        ("jar://host/dir/page.html", "x", "x"),
        ("http://host", "x", "http://host/x"),
        ("http://host", "?q", "http://host/?q"),
        ("http://host", "#f", "http://host#f"),
        ("http://host", "../y", "http://host/../y"),
        ("http://host", "a/../../b", "http://host/a/../../b"),
        ("http://host/d/?a=1", "?b", "http://host/d/?b"),
        ("http://host/d/?a=1", "x", "http://host/d/x"),
        ("http://host/d/?a=1", "#f", "http://host/d/?a=1#f"),
        ("http://host/d/?a=1", "", "http://host/d/?a=1"),
        ("resource:/a/b", "c", "resource:/a/c"),
        ("resource:/a/b", "?q", "resource:/a/b?q"),
        ("resource://ho st/a/b", "c", "c"),
        ("http://host/a/b", "//ho st/c", "//ho st/c"),
        ("http://host/a/b", "//h^h/x", "//h^h/x"),
        ("http://host/a/b", "//u[@h/x", "//u[@h/x"),
        ("http://host/a/b", "//a@b@c/x", "http://a@b@c/x"),
        ("http://host/a/b", "//h:+80/x", "http://h:+80/x"),
        ("http://host/a/b", "//h:-2/x", "//h:-2/x"),
        ("http://host/a/b", "//h:1:2/x", "//h:1:2/x"),
        ("http://host/a/b", "//u:p@h:81/x?q", "http://u:p@h:81/x?q"),
        ("http://host/a/b", "\\\\x", "http://host/a/\\\\x"),
        ("http://u:p@h:81/a/b", "?q", "http://h:81/a/b?q"),
        ("http://u:p@h:81/a/b", "?q#r", "http://h:81/a/b?q#r"),
    ];

    /// Links against a base of upper case letters, as Tika's path writes them.
    const AGAINST_UPPER_CASE: &[(&str, &str)] = &[
        ("c", "http://Host:8080/A/c"),
        (" d ", "http://Host:8080/A/d"),
        ("http:rel", "http://Host:8080/A/rel"),
        ("HTTPS:abs", "https:abs"),
        ("http://u:p@h2/x", "http://u:p@h2/x"),
        ("", "http://Host:8080/A/B"),
        ("   ", "http://Host:8080/A/B"),
        ("#", "http://Host:8080/A/B#"),
        ("?", "http://Host:8080/A/B?"),
        ("a/./b/../c", "http://Host:8080/A/a/c"),
        ("../../../../z", "http://Host:8080/../z"),
        ("./", "http://Host:8080/A/"),
        ("..", "http://Host:8080/"),
        (".", "http://Host:8080/A/"),
        ("g;x", "http://Host:8080/A/g;x"),
        ("g?y/./x", "http://Host:8080/A/g?y/./x"),
        ("g#s/../x", "http://Host:8080/A/g#s/../x"),
        ("http:/abs", "http://Host:8080/abs"),
        ("http:", "http://Host:8080/A/B"),
        ("urn:x", "urn:x"),
        ("Mailto:x", "Mailto:x"),
        ("TEL:1", "TEL:1"),
        ("JavaScript:void(0)", "JavaScript:void(0)"),
        ("about:blank", "about:blank"),
        ("data:x", "data:x"),
        ("a b", "http://Host:8080/A/a b"),
        ("a\\b", "http://Host:8080/A/a\\b"),
        ("ftp://x/y", "ftp://x/y"),
        ("c:\\foo", "c:\\foo"),
        ("//[::1]:80/p", "http://[::1]:80/p"),
        ("http://[::1]/x", "http://[::1]/x"),
        ("q?a#b?c", "http://Host:8080/A/q?a#b?c"),
        ("url:http://x/y", "http://x/y"),
        ("URL:z", "http://Host:8080/A/z"),
    ];

    #[test]
    fn resolves_links_as_tikas_path_does() {
        for &(base, link, resolved) in RESOLVED {
            assert_eq!(
                resolve(Some(base), link).unwrap(),
                resolved,
                "{link:?} against {base:?}"
            );
        }
        for &(link, resolved) in AGAINST_UPPER_CASE {
            assert_eq!(
                resolve(Some(" HTTP://Host:8080/A/B "), link).unwrap(),
                resolved,
                "{link:?}"
            );
        }
        assert_eq!(resolve(None, " x ").unwrap(), "x");
    }

    #[test]
    fn fails_on_a_query_against_a_jar_url_without_a_file() {
        assert!(matches!(
            resolve(Some("url:jar:#x"), "?q"),
            Err(Thrown::Runtime(_))
        ));
        assert_eq!(resolve(Some("url:jar:#x"), "y").unwrap(), "y");
    }

    #[test]
    fn reads_ip_literals_as_java_does() {
        for literal in ["::1", "fe80::1%eth0", "::ffff:1.2.3.4", "1:2:3:4:5:6:7:8"] {
            assert!(is_ipv6_literal(literal), "{literal}");
        }
        for literal in [
            "1",
            ":1",
            "1::2::3",
            "1:2:3:4:5:6:7:8:9",
            "::1%",
            "::1.2.3",
            "g::1",
        ] {
            assert!(!is_ipv6_literal(literal), "{literal}");
        }
        assert!(is_ipv4_literal("1.2.3.4"));
        assert!(is_ipv4_literal("16909060"));
        assert!(!is_ipv4_literal("1.2.3.256"));
        assert!(!is_ipv4_literal("1..2"));
    }
}
