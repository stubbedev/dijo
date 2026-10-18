//! What Tika writes for a parsed document: JSoupParser's traversal (`TikaNodeFilter`), which
//! reports every node, the document, comments and doctypes among them, as an element and every
//! text or data node as characters; `XHTMLDowngradeHandler`, which names elements in upper case
//! and drops `xmlns` attributes; and `HtmlHandler` with the `DefaultHtmlMapper`, which writes the
//! safe elements of the body with their safe attributes, drops scripts and styles with their
//! content, keeps a line break for other block elements, and reads the head's `meta`, `base`,
//! `link` and `title` and the `lang` of `html`.
//!
//! The title is recorded as `dc:title`: the first `title` element's text, then the content of
//! each `meta` named `title`, wherever it stands. Tika records a meta title after the `title`
//! element as `html:dc:title` and drops one before it (#86).
//!
//! The links of the safe elements, and an `object`'s data and class against its codebase, are
//! resolved against the document's URL as Java's `URL` resolves them ([`url`]); the document of
//! a data URI an element's `src` holds outside a script, and the `srcdoc` of an `iframe`, are
//! parsed as inline embedded documents. The parser reads a document differently from Tika's
//! where the text of a data URI is in a charset it does not encode ([`difference`]).

use super::dom::{Attribute, Dom, NodeData, NodeId, DOCUMENT};
use super::url;
use super::JSoupParser;
use crate::formats::data_uri;
use crate::formats::embedded::{ResourceType, Wrapping, PARSER_OVERRIDE};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Difference, Failure, Parser};
use crate::java;
use crate::java::is_blank;
use crate::mime::MediaType;

/// The attributes whose links `HtmlHandler` resolves against the base.
pub(super) const URI_ATTRIBUTES: [&str; 4] = ["src", "href", "longdesc", "cite"];

/// The attribute of an `object` whose link `HtmlHandler` resolves against the base, and which
/// [`OBJECT_LINKS`] resolve against.
pub(super) const CODEBASE: &str = "codebase";

/// The attributes of an `object` whose links `HtmlHandler` resolves against its [`CODEBASE`].
pub(super) const OBJECT_LINKS: [&str; 2] = ["data", "classid"];

/// The elements the default mapper keeps, by their upper case names (`SAFE_ELEMENTS`).
fn safe_element(name: &str) -> Option<&'static str> {
    Some(match name {
        "H1" => "h1",
        "H2" => "h2",
        "H3" => "h3",
        "H4" => "h4",
        "H5" => "h5",
        "H6" => "h6",
        "P" => "p",
        "PRE" => "pre",
        "BLOCKQUOTE" => "blockquote",
        "Q" => "q",
        "UL" => "ul",
        "OL" => "ol",
        "MENU" => "ul",
        "LI" => "li",
        "DL" => "dl",
        "DT" => "dt",
        "DD" => "dd",
        "TABLE" => "table",
        "THEAD" => "thead",
        "TBODY" => "tbody",
        "TR" => "tr",
        "TH" => "th",
        "TD" => "td",
        "ADDRESS" => "address",
        "A" => "a",
        "MAP" => "map",
        "AREA" => "area",
        "IMG" => "img",
        "FRAMESET" => "frameset",
        "FRAME" => "frame",
        "IFRAME" => "iframe",
        "OBJECT" => "object",
        "PARAM" => "param",
        "INS" => "ins",
        "DEL" => "del",
        _ => return None,
    })
}

/// The attributes the default mapper keeps on a safe element (`SAFE_ATTRIBUTES`).
fn safe_attribute(element: &str, attribute: &str) -> bool {
    let safe: &[&str] = match element {
        "a" => &[
            "charset", "type", "name", "href", "hreflang", "rel", "rev", "shape", "coords",
        ],
        "img" => &[
            "src", "alt", "longdesc", "height", "width", "usemap", "ismap",
        ],
        "frame" => &[
            "longdesc",
            "name",
            "src",
            "frameborder",
            "marginwidth",
            "marginheight",
            "noresize",
            "scrolling",
        ],
        "iframe" => &[
            "longdesc",
            "name",
            "src",
            "frameborder",
            "marginwidth",
            "marginheight",
            "scrolling",
            "align",
            "height",
            "width",
        ],
        "link" => &["charset", "href", "hreflang", "type", "rel", "rev", "media"],
        "map" => &["id", "class", "style", "title", "name"],
        "area" => &["shape", "coords", "href", "nohref", "alt"],
        "object" => &[
            "declare", "classid", "codebase", "data", "type", "codetype", "archive", "standby",
            "height", "width", "usemap", "name", "tabindex", "align", "border", "hspace", "vspace",
        ],
        "param" => &["id", "name", "value", "valuetype", "type"],
        "blockquote" | "q" => &["cite"],
        "ins" | "del" => &["cite", "datetime"],
        _ => &[],
    };
    safe.contains(&attribute)
}

fn is_discarded(name: &str) -> bool {
    name == "STYLE" || name == "SCRIPT"
}

/// A node's attributes as `XHTMLDowngradeHandler` passes them on: without `xmlns` and
/// `xmlns:*` declarations.
fn sax_attributes(attributes: &[Attribute]) -> Vec<&Attribute> {
    attributes
        .iter()
        .filter(|attribute| attribute.key != "xmlns" && !attribute.key.starts_with("xmlns:"))
        .collect()
}

fn value<'a>(attributes: &[&'a Attribute], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.key == name)
        .map(|attribute| attribute.value.as_str())
}

/// The name a node takes in the SAX events, in upper case as `XHTMLDowngradeHandler` writes it.
fn event_name(dom: &Dom, node: NodeId) -> Option<String> {
    let name = match &dom.node(node).data {
        NodeData::Document => "#document",
        NodeData::Element { name, .. } => name,
        NodeData::Comment(_) => "#comment",
        NodeData::Doctype { .. } => "#doctype",
        NodeData::Text(_) => return None,
    };
    Some(name.to_uppercase())
}

/// The URL `HtmlHandler` resolves links against before a `base` element sets one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Base {
    /// The metadata it is given holds no `Content-Location`, and its resource name is no URL.
    Unknown,
    /// The `Content-Location` of the metadata it is given.
    Located(String),
    /// Its resource name, trimmed, a URL links resolve against, which it records as the
    /// `Content-Location`.
    Named(String),
}

impl Base {
    /// What `HtmlHandler`'s constructor takes from the metadata the parser is given.
    pub(super) fn of(metadata: &TikaMetadata) -> Self {
        if let Some(location) = metadata.get("Content-Location") {
            return Self::Located(location.to_owned());
        }
        match metadata.get("tk:resource-name").map(java::trim) {
            Some(name) if url::is_base(name) => Self::Named(name.to_owned()),
            Some(_) | None => Self::Unknown,
        }
    }
}

struct HtmlHandler<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    /// The `Content-Location` links are resolved against.
    location: Option<String>,
    title: String,
    body_level: usize,
    discard_level: usize,
    title_level: usize,
    script_level: usize,
    /// The first `title` element's text, trimmed, once it has ended.
    element_title: Option<String>,
    /// The content of each `meta` named `title`, in order.
    meta_titles: Vec<String>,
}

/// Writes the document through Tika's handlers, links resolved against `base`.
pub(super) fn write(dom: &Dom, base: Base, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let location = match base {
        Base::Unknown => None,
        Base::Located(location) => Some(location),
        Base::Named(name) => {
            xhtml.metadata_mut().set("Content-Location", name.as_str());
            Some(name)
        }
    };
    let mut handler = HtmlHandler {
        xhtml,
        location,
        title: String::new(),
        body_level: 0,
        discard_level: 0,
        title_level: 0,
        script_level: 0,
        element_title: None,
        meta_titles: Vec::new(),
    };
    handler.node(dom, DOCUMENT)
}

impl HtmlHandler<'_, '_> {
    fn node(&mut self, dom: &Dom, node: NodeId) -> Result<(), Failure> {
        if let NodeData::Text(text) = &dom.node(node).data {
            if !text.is_empty() {
                self.characters(text)?;
            }
            return Ok(());
        }
        let name = event_name(dom, node).unwrap_or_else(|| panic!("node {node} has no name"));
        let attributes = sax_attributes(dom.attributes(node));
        self.start_element(&name, &attributes)?;
        for &child in &dom.node(node).children {
            self.node(dom, child)?;
        }
        self.end_element(&name)
    }

    fn start_element(&mut self, name: &str, attributes: &[&Attribute]) -> Result<(), Failure> {
        if name == "HTML" {
            if let Some(lang) = value(attributes, "lang") {
                self.xhtml.metadata_mut().set("Content-Language", lang);
            }
        }
        if name == "SCRIPT" {
            self.script_level += 1;
        }
        if name == "TITLE" && self.title_level == 0 {
            self.title.clear();
        }
        if name == "TITLE" || self.title_level > 0 {
            self.title_level += 1;
        }
        if name == "BODY" || name == "FRAMESET" || self.body_level > 0 {
            self.body_level += 1;
        }
        if is_discarded(name) || self.discard_level > 0 {
            self.discard_level += 1;
        }
        if self.body_level == 0 && self.discard_level == 0 {
            if name == "META" {
                if let Some(content) = value(attributes, "content") {
                    if let Some(equiv) = value(attributes, "http-equiv") {
                        self.add_html_metadata(equiv, content);
                    } else if let Some(meta_name) = value(attributes, "name") {
                        self.add_html_metadata(meta_name, content);
                    } else if let Some(property) = value(attributes, "property") {
                        self.xhtml
                            .metadata_mut()
                            .add(&format!("html:{property}"), content);
                    }
                }
            } else if name == "BASE" {
                if let Some(href) = value(attributes, "href") {
                    self.start_element_with_mapped_attributes("base", attributes)?;
                    self.xhtml.end_element("base")?;
                    let location = self.resolve(href);
                    self.xhtml
                        .metadata_mut()
                        .set("Content-Location", location.as_str());
                    self.location = Some(location);
                }
            } else if name == "LINK" {
                self.start_element_with_mapped_attributes("link", attributes)?;
                self.xhtml.end_element("link")?;
            }
        }
        if self.body_level > 0 && self.discard_level == 0 {
            if let Some(safe) = safe_element(name) {
                self.start_element_with_mapped_attributes(safe, attributes)?;
            }
        }
        if self.script_level == 0 {
            if let Some(uri) = embedded_data_uri(attributes) {
                uri.embed(self.xhtml, JSoupParser.tika_name())?;
            }
        }
        if name == "IFRAME" {
            if let Some(document) = value(attributes, "srcdoc").filter(|doc| !is_blank(doc)) {
                self.embed_srcdoc(document)?;
            }
        }
        Ok(())
    }

    /// `handleSrcDoc`: an `iframe`'s `srcdoc`, in UTF-8, parsed as an inline HTML document.
    fn embed_srcdoc(&mut self, document: &str) -> Result<(), Failure> {
        let mut metadata = TikaMetadata::new();
        ResourceType::Inline.set(&mut metadata);
        metadata.set("Content-Type", "text/html");
        metadata.set(PARSER_OVERRIDE, "text/html");
        self.xhtml
            .parse_embedded(document.as_bytes(), &metadata, Wrapping::PackageEntry)
    }

    /// `resolve(url)`: `url` against the `Content-Location`.
    fn resolve(&self, url: &str) -> String {
        url::resolve(self.location.as_deref(), url)
    }

    fn add_html_metadata(&mut self, name: &str, value: &str) {
        if is_blank(name) || is_blank(value) {
            return;
        }
        let metadata = self.xhtml.metadata_mut();
        if java::equals_ignore_case(name, "ICBM") {
            match icbm(value) {
                Some((latitude, longitude)) => {
                    metadata.set("ICBM", format!("{latitude}, {longitude}"));
                    metadata.set("geo:lat", latitude);
                    metadata.set("geo:long", longitude);
                }
                None => metadata.set("ICBM", value),
            }
            return;
        }
        if java::equals_ignore_case(name, "Content-Type") {
            let hint = MediaType::parse(value)
                .map(|media_type| media_type.to_string())
                .unwrap_or_else(|| value.to_string());
            metadata.set("tk:content-type-hint", hint);
            return;
        }
        match name.to_lowercase().as_str() {
            "author" => metadata.add("dc:creator", value),
            "title" => {
                self.meta_titles.push(value.to_owned());
                self.set_title();
            }
            "subject" => metadata.add("dc:subject", value),
            "keywords" => {
                metadata.add("meta:keyword", value);
                metadata.add("dc:subject", value);
            }
            "description" => metadata.add("dc:description", value),
            _ => metadata.add(&format!("html:{name}"), value),
        }
    }

    fn start_element_with_mapped_attributes(
        &mut self,
        name: &str,
        attributes: &[&Attribute],
    ) -> Result<(), Failure> {
        if attributes.is_empty() {
            return Ok(self.xhtml.start_element(name)?);
        }
        let codebase = if name == "object" {
            match value(attributes, CODEBASE) {
                Some(codebase) => Some(self.resolve(codebase)),
                None => self.location.clone(),
            }
        } else {
            None
        };
        let mut mapped: Vec<(String, String)> = Vec::with_capacity(attributes.len() + 1);
        for attribute in attributes {
            if !safe_attribute(name, &attribute.key) {
                continue;
            }
            let value = if URI_ATTRIBUTES.contains(&attribute.key.as_str()) {
                let value = if attribute.key == "src" && attribute.value.starts_with("data:") {
                    "data:"
                } else {
                    &attribute.value
                };
                self.resolve(value)
            } else if name == "object" && attribute.key == CODEBASE {
                codebase.clone().expect("an object with a codebase has one")
            } else if name == "object" && OBJECT_LINKS.contains(&attribute.key.as_str()) {
                url::resolve(codebase.as_deref(), &attribute.value)
            } else {
                attribute.value.clone()
            };
            mapped.push((attribute.key.clone(), value));
        }
        if name == "img" && !mapped.iter().any(|(key, _)| key == "alt") {
            mapped.push(("alt".to_string(), String::new()));
        }
        let pairs: Vec<(&str, &str)> = mapped
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        Ok(self.xhtml.start_element_with(name, &pairs)?)
    }

    fn end_element(&mut self, name: &str) -> Result<(), Failure> {
        if name == "SCRIPT" {
            self.script_level -= 1;
        }
        if self.body_level > 0 && self.discard_level == 0 {
            if let Some(safe) = safe_element(name) {
                self.xhtml.end_element(safe)?;
            } else if Xhtml::is_endline(&name.to_lowercase()) {
                self.xhtml.newline()?;
            }
        }
        if self.title_level > 0 {
            self.title_level -= 1;
            if self.title_level == 0 && self.element_title.is_none() {
                self.element_title = Some(java::trim(&self.title).to_string());
                self.set_title();
            }
        }
        self.body_level = self.body_level.saturating_sub(1);
        self.discard_level = self.discard_level.saturating_sub(1);
        Ok(())
    }

    /// Records the title as it stands: the `title` element's text, then the meta titles.
    fn set_title(&mut self) {
        let titles = self
            .element_title
            .iter()
            .chain(&self.meta_titles)
            .cloned()
            .collect();
        self.xhtml.metadata_mut().set_values("dc:title", titles);
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.title_level > 0 && self.body_level == 0 {
            self.title.push_str(text);
        }
        if self.body_level > 0 && self.discard_level == 0 {
            self.xhtml.characters(text)?;
        }
        Ok(())
    }
}

/// The latitude and longitude `\s*(-?\d+\.\d+)[,\s]+(-?\d+\.\d+)\s*` reads from a whole value.
fn icbm(value: &str) -> Option<(&str, &str)> {
    let is_space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\u{B}' | '\u{C}' | '\r');
    let number = |text: &str| -> Option<usize> {
        let bytes = text.as_bytes();
        let mut i = usize::from(bytes.first() == Some(&b'-'));
        let whole = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        if whole == 0 || bytes.get(i + whole) != Some(&b'.') {
            return None;
        }
        i += whole + 1;
        let fraction = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        (fraction > 0).then_some(i + fraction)
    };
    let text = value.trim_start_matches(is_space);
    let first_end = number(text)?;
    let latitude = &text[..first_end];
    let rest = &text[first_end..];
    let separators = rest
        .find(|c: char| c != ',' && !is_space(c))
        .unwrap_or(rest.len());
    if separators == 0 {
        return None;
    }
    let rest = &rest[separators..];
    let second_end = number(rest)?;
    if !rest[second_end..].chars().all(is_space) {
        return None;
    }
    Some((latitude, &rest[..second_end]))
}

/// The data URI of an element's `src`, which `HtmlHandler` hands to the embedded document
/// extractor outside a script.
fn embedded_data_uri(attributes: &[&Attribute]) -> Option<data_uri::DataUri> {
    value(attributes, "src")
        .filter(|src| src.starts_with("data:"))
        .and_then(data_uri::parse)
}

/// Where this parser reads a document differently from Tika's: where the text of a data URI it
/// embeds is in a charset it does not encode, which it encodes in UTF-8.
pub(super) fn difference(dom: &Dom) -> Option<Difference> {
    let mut scan = Scan::default();
    scan.node(dom, DOCUMENT).map(Difference::new)
}

#[derive(Default)]
struct Scan {
    script_level: usize,
}

impl Scan {
    fn node(&mut self, dom: &Dom, node: NodeId) -> Option<String> {
        let name = event_name(dom, node)?;
        let attributes = sax_attributes(dom.attributes(node));
        if name == "SCRIPT" {
            self.script_level += 1;
        }
        if self.script_level == 0 {
            if let Some(reason) = embedded_data_uri(&attributes).and_then(|uri| uri.difference()) {
                return Some(reason);
            }
        }
        for &child in &dom.node(node).children {
            if let Some(reason) = self.node(dom, child) {
                return Some(reason);
            }
        }
        if name == "SCRIPT" {
            self.script_level -= 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xhtml_of(html: &[u8]) -> (String, crate::Metadata) {
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(html)
            .unwrap()
    }

    /// The document of a data URI outside a script, and an `iframe`'s `srcdoc`, are embedded
    /// inline in the element that holds them, as `HtmlHandler` hands them to the extractor.
    #[test]
    fn embeds_data_uris_and_srcdocs() {
        let (xhtml, _) = xhtml_of(
            b"<html><head><script src=\"data:text/plain,Skjult\"></script></head><body>\
              <p>Ude</p><img alt=\"a\" src=\"data:text/plain;base64,S8O4YmVuaGF2bg==\">\
              <iframe srcdoc=\"<p>Indre</p>\"></iframe><iframe srcdoc=\" \"></iframe>\
              <img src=\"data:text\"></body></html>",
        );
        assert!(
            xhtml.contains(
                "<img alt=\"a\" src=\"data:\"><div class=\"package-entry\"><p>K\u{f8}benhavn</p>"
            ),
            "{xhtml}"
        );
        assert!(
            xhtml.contains("<iframe><div class=\"package-entry\"><p>Indre</p>"),
            "{xhtml}"
        );
        assert!(!xhtml.contains("Skjult"), "{xhtml}");
        assert_eq!(xhtml.matches("package-entry").count(), 2, "{xhtml}");
    }

    /// Links are resolved as the WHATWG URL standard resolves them, where `java.net.URL`, and so
    /// Tika's path, departs from it (#162).
    #[test]
    fn resolves_links_by_the_url_standard() {
        let page = |base: &str, links: &[&str]| {
            let anchors: String = links
                .iter()
                .map(|link| format!("<a href=\"{link}\">l</a>"))
                .collect();
            xhtml_of(
                format!("<html><head><base href=\"{base}\"></head><body>{anchors}</body></html>")
                    .as_bytes(),
            )
            .0
        };
        let cases = [
            (page("http://u:p@h/a/b", &["?q"]), "http://u:p@h/a/b?q"),
            (page("http://h/a/", &["../../z"]), "http://h/z"),
            (page("http://h", &["a/../../b"]), "http://h/b"),
            (page("ftp://h/d/", &["x"]), "ftp://h/d/x"),
            (page("url:jar:#x", &["?q"]), "?q"),
        ];
        for (xhtml, link) in cases {
            assert!(
                xhtml.contains(&format!("<a href=\"{link}\">")),
                "{link}: {xhtml}"
            );
        }
    }

    /// Links are resolved against the `base`, which becomes the `Content-Location`, an object's
    /// data and class against its codebase.
    #[test]
    fn resolves_links_against_the_base_and_the_codebase() {
        let (xhtml, metadata) = xhtml_of(
            b"<html><head><base href=\" http://example.com/a/b.html \">\
              <link rel=\"stylesheet\" href=\"../s.css\"></head><body>\
              <a href=\"c.html?x#y\">c</a><a href=\"mailto:me@example.com\">m</a>\
              <object codebase=\"http://cdn.example.com/lib/\" data=\"film.swf\" \
              classid=\"clsid:D27CDB6E\"></object><object data=\"d.swf\"></object>\
              </body></html>",
        );
        assert_eq!(
            metadata.get("Content-Location"),
            Some(&vec!["http://example.com/a/b.html".to_owned()])
        );
        for resolved in [
            "href=\"http://example.com/s.css\"",
            "href=\"http://example.com/a/c.html?x#y\"",
            "href=\"mailto:me@example.com\"",
            "codebase=\"http://cdn.example.com/lib/\" data=\"http://cdn.example.com/lib/film.swf\" \
             classid=\"clsid:D27CDB6E\"",
            "<object data=\"http://example.com/a/d.swf\"",
        ] {
            assert!(xhtml.contains(resolved), "{resolved}: {xhtml}");
        }
    }

    #[test]
    fn takes_a_resource_name_that_is_a_url_for_the_base() {
        let mut metadata = TikaMetadata::new();
        metadata.set("tk:resource-name", " http://example.com/page.html ");
        assert_eq!(
            Base::of(&metadata),
            Base::Named("http://example.com/page.html".to_owned())
        );
        metadata.set("tk:resource-name", "page.html");
        assert_eq!(Base::of(&metadata), Base::Unknown);
        metadata.set("Content-Location", "ftp://example.com/");
        assert_eq!(
            Base::of(&metadata),
            Base::Located("ftp://example.com/".to_owned())
        );
    }

    /// A data URI of text in a charset the parser does not encode is read differently from Tika,
    /// unless it stands in a script, whose data Tika does not embed.
    #[test]
    fn reads_differently_a_data_uri_of_text_it_does_not_encode() {
        let tree = |html: &str| super::super::tree_builder::parse(html);
        let windows = "data:text/plain;charset=windows-1252,hej";
        assert!(difference(&tree(&format!("<img src=\"{windows}\">"))).is_some());
        assert!(difference(&tree(&format!("<script src=\"{windows}\"></script>"))).is_none());
        assert!(difference(&tree("<img src=\"data:text/plain;charset=UTF-8,hej\">")).is_none());
    }

    #[test]
    fn reads_icbm_as_tika_does() {
        assert_eq!(icbm(" 55.68, 12.57 "), Some(("55.68", "12.57")));
        assert_eq!(icbm("-1.5 ,, -2.25"), Some(("-1.5", "-2.25")));
        assert_eq!(icbm("1.5,2"), None);
        assert_eq!(icbm("1.5 2.5x"), None);
        assert_eq!(icbm("north"), None);
    }

    /// The title element's text is the title, the meta titles before and after it further
    /// titles (#86).
    #[test]
    fn records_meta_titles_as_further_titles_wherever_they_stand() {
        let html = b"<html><head><meta name=\"title\" content=\"Early\"><title> Element title \
                     </title><meta name=\"title\" content=\"Late\"></head><body><p>x</p></body>\
                     </html>";
        let (xhtml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(html)
            .unwrap();
        assert_eq!(
            metadata.get("dc:title"),
            Some(&vec![
                "Element title".to_owned(),
                "Early".to_owned(),
                "Late".to_owned()
            ])
        );
        assert_eq!(metadata.get("html:dc:title"), None);
        assert!(xhtml.contains("<title>Element title</title>"), "{xhtml}");
        let (_, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(
                b"<html><head><meta name=\"title\" content=\"Only\"></head></html>",
            )
            .unwrap();
        assert_eq!(metadata.get("dc:title"), Some(&vec!["Only".to_owned()]));
    }
}
