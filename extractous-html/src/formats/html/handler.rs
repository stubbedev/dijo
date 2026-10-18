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
//! The parser leaves to Tika the documents this would hand to an embedded document extractor
//! or resolve links against a URL (see [`deferral`]), so it never does either.

use super::dom::{Attribute, Dom, NodeData, NodeId, DOCUMENT};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::java;
use crate::java::is_blank;
use crate::mime::MediaType;

const URI_ATTRIBUTES: [&str; 4] = ["src", "href", "longdesc", "cite"];

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

/// Whether `text` starts with a URL scheme, which Java's `URL` might read as a protocol.
pub(super) fn has_scheme(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
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

struct HtmlHandler<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    title: String,
    body_level: usize,
    discard_level: usize,
    title_level: usize,
    /// The first `title` element's text, trimmed, once it has ended.
    element_title: Option<String>,
    /// The content of each `meta` named `title`, in order.
    meta_titles: Vec<String>,
}

/// Writes the document through Tika's handlers.
pub(super) fn write(dom: &Dom, xhtml: &mut Xhtml) -> Result<(), Stop> {
    let mut handler = HtmlHandler {
        xhtml,
        title: String::new(),
        body_level: 0,
        discard_level: 0,
        title_level: 0,
        element_title: None,
        meta_titles: Vec::new(),
    };
    handler.node(dom, DOCUMENT)
}

impl HtmlHandler<'_, '_> {
    fn node(&mut self, dom: &Dom, node: NodeId) -> Result<(), Stop> {
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

    fn start_element(&mut self, name: &str, attributes: &[&Attribute]) -> Result<(), Stop> {
        if name == "HTML" {
            if let Some(lang) = value(attributes, "lang") {
                self.xhtml.metadata_mut().set("Content-Language", lang);
            }
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
                    self.xhtml
                        .metadata_mut()
                        .set("Content-Location", java::trim(href));
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
        Ok(())
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
    ) -> Result<(), Stop> {
        if attributes.is_empty() {
            return self.xhtml.start_element(name);
        }
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
                java::trim(value).to_string()
            } else if name == "object"
                && matches!(attribute.key.as_str(), "codebase" | "data" | "classid")
            {
                java::trim(&attribute.value).to_string()
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
        self.xhtml.start_element_with(name, &pairs)
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
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

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
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

/// Why Tika's path should read a document rather than this parser: Tika would hand a data URI
/// or an `iframe`'s `srcdoc` to its embedded document extractor (with OCR, under extractous's
/// defaults), or resolve links against a URL (a `base` in the head, an `object`'s codebase or
/// the resource name), which takes Java's `URL`. Decided over the tree, as `HtmlHandler` meets
/// the nodes.
pub(super) fn deferral(dom: &Dom, resource_name: Option<&str>) -> Option<&'static str> {
    if resource_name.is_some_and(|name| has_scheme(java::trim(name))) {
        return Some("the resource name is a URL that links would be resolved against");
    }
    let mut scan = Scan::default();
    scan.node(dom, DOCUMENT)
}

#[derive(Default)]
struct Scan {
    script_level: usize,
    body_level: usize,
    discard_level: usize,
}

impl Scan {
    fn node(&mut self, dom: &Dom, node: NodeId) -> Option<&'static str> {
        let name = event_name(dom, node)?;
        let attributes = sax_attributes(dom.attributes(node));
        if name == "SCRIPT" {
            self.script_level += 1;
        }
        if name == "BODY" || name == "FRAMESET" || self.body_level > 0 {
            self.body_level += 1;
        }
        if is_discarded(&name) || self.discard_level > 0 {
            self.discard_level += 1;
        }
        if self.body_level == 0
            && self.discard_level == 0
            && name == "BASE"
            && value(&attributes, "href").is_some_and(|href| has_scheme(java::trim(href)))
        {
            return Some("a base element sets a URL that links would be resolved against");
        }
        if self.body_level > 0
            && self.discard_level == 0
            && name == "OBJECT"
            && value(&attributes, "codebase").is_some_and(|base| has_scheme(java::trim(base)))
        {
            return Some("an object's codebase is a URL that its data would be resolved against");
        }
        if self.script_level == 0
            && value(&attributes, "src").is_some_and(|src| src.starts_with("data:"))
        {
            return Some("a data URI would be parsed as an embedded document");
        }
        if name == "IFRAME" && value(&attributes, "srcdoc").is_some_and(|doc| !is_blank(doc)) {
            return Some("an iframe's srcdoc would be parsed as an embedded document");
        }
        for &child in &dom.node(node).children {
            if let Some(reason) = self.node(dom, child) {
                return Some(reason);
            }
        }
        if name == "SCRIPT" {
            self.script_level -= 1;
        }
        self.body_level = self.body_level.saturating_sub(1);
        self.discard_level = self.discard_level.saturating_sub(1);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn recognises_schemes() {
        assert!(has_scheme("http://example.com/"));
        assert!(has_scheme("file:x"));
        assert!(!has_scheme("docs/"));
        assert!(!has_scheme("1http:"));
        assert!(!has_scheme("a b:"));
    }
}
