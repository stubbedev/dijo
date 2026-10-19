//! The links `HtmlHandler` resolves, as the WHATWG URL standard resolves them (#162), where
//! Tika's `java.net.URL` drops a base's user information from a query alone, keeps `..` above
//! the root, normalises no path against a base without one, fails on a query against a `jar`
//! base without a file, and resolves against no other base than `http`, `https`, `file`,
//! `resource` and `jar`.

use url::Url;

use crate::java;

/// The prefixes of the URLs `HtmlHandler.resolve` leaves as they are, in lower case.
const UNRESOLVED: [&str; 6] = ["urn:", "mailto:", "tel:", "data:", "javascript:", "about:"];

/// `HtmlHandler.resolve(base, url)`: `url`, trimmed, resolved against the URL `base` by the
/// WHATWG URL standard, unless there is no base, `url` starts with a prefix of [`UNRESOLVED`],
/// the base is no URL or the standard resolves nothing of `url` against it.
pub(super) fn resolve(base: Option<&str>, url: &str) -> String {
    let url = java::trim(url);
    let lower = java::to_lower_case(url);
    base.filter(|_| !UNRESOLVED.iter().any(|prefix| lower.starts_with(prefix)))
        .and_then(|base| Url::parse(java::trim(base)).ok())
        .and_then(|base| base.join(url).ok())
        .map_or_else(|| url.to_owned(), String::from)
}

/// Whether `text` is a URL links can be resolved against: one the WHATWG URL standard parses,
/// with a path that is no opaque string.
pub(super) fn is_base(text: &str) -> bool {
    Url::parse(text).is_ok_and(|url| !url.cannot_be_a_base())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Links against a `base`, each `(base, link, resolved)`, as the WHATWG URL standard
    /// resolves them.
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
        (
            "ftp://host/dir/page.html",
            "sub/x.html",
            "ftp://host/dir/sub/x.html",
        ),
        ("mailto:me@x.org", "c", "c"),
        ("clsid://host/dir/", "x", "clsid://host/dir/x"),
        ("dir/page", "x", "x"),
        ("file:x/y", "c", "file:///x/c"),
        ("file:x/y", "../z", "file:///z"),
        ("file:x/y", "?q", "file:///x/y?q"),
        ("file:/tmp/a/b.html", "c", "file:///tmp/a/c"),
        ("file:/tmp/a/b.html", "?q", "file:///tmp/a/b.html?q"),
        ("file:/tmp/a/b.html", "/d", "file:///d"),
        ("file:/tmp/a/b.html", "//h/e", "file://h/e"),
        ("http://host/d/p#frag", "", "http://host/d/p"),
        ("http://host/d/p#frag", "#n", "http://host/d/p#n"),
        ("http://host/d/p#frag", "?b", "http://host/d/p?b"),
        ("http://host:80/a", "b", "http://host/b"),
        ("http://host:80/a", "http://host:/c", "http://host/c"),
        ("http://host:80/a", "http://host:x/c", "http://host:x/c"),
        ("http://host:80/a", "http://h:99999/c", "http://h:99999/c"),
        ("http://host:80/a", "http://h:-1/x", "http://h:-1/x"),
        ("http://[::1]:8/a/b", "c", "http://[::1]:8/a/c"),
        (
            "http://[::1]:8/a/b",
            "//[fe80::1%25eth0]/x",
            "//[fe80::1%25eth0]/x",
        ),
        ("http://[::1]:8/a/b", "http://[bad/x", "http://[bad/x"),
        ("jar:file:/x.jar!/a/b.html", "c.html", "c.html"),
        ("jar:file:/x.jar!/a/b.html", "/d", "/d"),
        ("jar:file:/x.jar!/a/b.html", "../e", "../e"),
        ("jar:file:/x.jar!/a/b.html", "?q", "?q"),
        (
            "jar:file:/x.jar!/a/b.html",
            "#f",
            "jar:file:/x.jar!/a/b.html#f",
        ),
        ("jar://host/dir/page.html", "x", "jar://host/dir/x"),
        ("http://host", "x", "http://host/x"),
        ("http://host", "?q", "http://host/?q"),
        ("http://host", "#f", "http://host/#f"),
        ("http://host", "../y", "http://host/y"),
        ("http://host", "a/../../b", "http://host/b"),
        ("http://host/d/?a=1", "?b", "http://host/d/?b"),
        ("http://host/d/?a=1", "x", "http://host/d/x"),
        ("http://host/d/?a=1", "#f", "http://host/d/?a=1#f"),
        ("http://host/d/?a=1", "", "http://host/d/?a=1"),
        ("resource:/a/b", "c", "resource:/a/c"),
        ("resource:/a/b", "?q", "resource:/a/b?q"),
        ("resource://ho st/a/b", "c", "c"),
        ("http://host/a/b", "//ho st/c", "//ho st/c"),
        ("http://host/a/b", "//h^h/x", "//h^h/x"),
        ("http://host/a/b", "//u[@h/x", "http://u%5B@h/x"),
        ("http://host/a/b", "//a@b@c/x", "http://a%40b@c/x"),
        ("http://host/a/b", "//h:+80/x", "//h:+80/x"),
        ("http://host/a/b", "//h:-2/x", "//h:-2/x"),
        ("http://host/a/b", "//h:1:2/x", "//h:1:2/x"),
        ("http://host/a/b", "//u:p@h:81/x?q", "http://u:p@h:81/x?q"),
        ("http://host/a/b", "\\\\x", "http://x/"),
        ("http://u:p@h:81/a/b", "?q", "http://u:p@h:81/a/b?q"),
        ("http://u:p@h:81/a/b", "?q#r", "http://u:p@h:81/a/b?q#r"),
        ("url:jar:#x", "?q", "?q"),
        ("url:jar:#x", "y", "y"),
    ];

    /// Links against a base of upper case letters, as the WHATWG URL standard resolves them.
    const AGAINST_UPPER_CASE: &[(&str, &str)] = &[
        ("c", "http://host:8080/A/c"),
        (" d ", "http://host:8080/A/d"),
        ("http:rel", "http://host:8080/A/rel"),
        ("HTTPS:abs", "https://abs/"),
        ("http://u:p@h2/x", "http://u:p@h2/x"),
        ("", "http://host:8080/A/B"),
        ("   ", "http://host:8080/A/B"),
        ("#", "http://host:8080/A/B#"),
        ("?", "http://host:8080/A/B?"),
        ("a/./b/../c", "http://host:8080/A/a/c"),
        ("../../../../z", "http://host:8080/z"),
        ("./", "http://host:8080/A/"),
        ("..", "http://host:8080/"),
        (".", "http://host:8080/A/"),
        ("g;x", "http://host:8080/A/g;x"),
        ("g?y/./x", "http://host:8080/A/g?y/./x"),
        ("g#s/../x", "http://host:8080/A/g#s/../x"),
        ("http:/abs", "http://host:8080/abs"),
        ("http:", "http://host:8080/A/B"),
        ("urn:x", "urn:x"),
        ("Mailto:x", "Mailto:x"),
        ("TEL:1", "TEL:1"),
        ("JavaScript:void(0)", "JavaScript:void(0)"),
        ("about:blank", "about:blank"),
        ("data:x", "data:x"),
        ("a b", "http://host:8080/A/a%20b"),
        ("a\\b", "http://host:8080/A/a/b"),
        ("ftp://x/y", "ftp://x/y"),
        ("c:\\foo", "c:\\foo"),
        ("//[::1]:80/p", "http://[::1]/p"),
        ("http://[::1]/x", "http://[::1]/x"),
        ("q?a#b?c", "http://host:8080/A/q?a#b?c"),
        ("url:http://x/y", "url:http://x/y"),
        ("URL:z", "url:z"),
    ];

    #[test]
    fn resolves_links_as_the_whatwg_url_standard_does() {
        let upper_case = AGAINST_UPPER_CASE
            .iter()
            .map(|&(link, resolved)| (" HTTP://Host:8080/A/B ", link, resolved));
        let wrong: Vec<String> = RESOLVED
            .iter()
            .copied()
            .chain(upper_case)
            .filter_map(|(base, link, expected)| {
                let resolved = resolve(Some(base), link);
                (resolved != expected)
                    .then(|| format!("{link:?} against {base:?}: {resolved:?}, not {expected:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        assert_eq!(resolve(None, " x "), "x");
    }

    #[test]
    fn takes_hierarchical_urls_as_bases() {
        for base in [
            "http://host/a",
            "ftp://host/",
            "file:/tmp/x.html",
            "x-y://h/p",
        ] {
            assert!(is_base(base), "{base}");
        }
        for name in [
            "page.html",
            "mailto:me@x.org",
            "c:\\docs\\page.html",
            "a b:c",
        ] {
            assert!(!is_base(name), "{name}");
        }
    }
}
