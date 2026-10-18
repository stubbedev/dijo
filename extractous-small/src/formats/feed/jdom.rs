//! The JDOM 2.0.6.1 tree ROME reads a feed into: built from the document as JDOM's `SAXHandler`
//! builds it from Xerces' events with ROME's settings (no document type declaration allowed),
//! read as `Element`'s accessors read it, and written out as a raw-format `XMLOutputter` writes
//! it. Elements a reader picks are left out of the tree and handed to it as they end, so that
//! the tree holds no more than the rest.

use crate::xml::{self, Event, Flow, TextKind, XmlError, XMLNS};

/// A node of an element's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Node {
    Text(String),
    CData(String),
    Element(Element),
    Comment(String),
    Instruction { target: String, data: String },
}

/// A namespace: a prefix bound to a URI. JDOM caches one instance per pair, so two are the
/// same object exactly when both parts are equal; `Namespace.equals` compares the URIs alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Namespace {
    pub(super) prefix: String,
    pub(super) uri: String,
}

/// An attribute other than a namespace declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Attribute {
    namespace: String,
    name: String,
    value: String,
}

/// An element: its name, namespace, attributes, the namespaces it declares besides its own,
/// and its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Element {
    pub(super) namespace: Namespace,
    pub(super) name: String,
    qualified_name: String,
    attributes: Vec<Attribute>,
    pub(super) additional_namespaces: Vec<Namespace>,
    pub(super) content: Vec<Node>,
}

impl Element {
    /// `getNamespaceURI()`.
    pub(super) fn namespace_uri(&self) -> &str {
        &self.namespace.uri
    }

    /// `getChildren()`: the child elements.
    pub(super) fn children(&self) -> impl Iterator<Item = &Element> {
        self.content.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    /// `getChildren(name, namespace)`: the child elements of that name in the namespace with
    /// that URI.
    pub(super) fn children_named<'e>(
        &'e self,
        name: &'e str,
        namespace: &'e str,
    ) -> impl Iterator<Item = &'e Element> {
        self.children()
            .filter(move |child| child.name == name && child.namespace.uri == namespace)
    }

    /// `getChild(name, namespace)`: the first child element of that name in the namespace with
    /// that URI.
    pub(super) fn child(&self, name: &str, namespace: &str) -> Option<&Element> {
        self.children()
            .find(|child| child.name == name && child.namespace.uri == namespace)
    }

    /// `getAttributeValue(name, namespace)`: the value of the attribute of that name in the
    /// namespace with that URI, the empty URI for none.
    pub(super) fn attribute(&self, name: &str, namespace: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name == name && attribute.namespace == namespace)
            .map(|attribute| attribute.value.as_str())
    }

    /// `getText()`: the text and CDATA content run together, without what child elements
    /// hold.
    pub(super) fn text(&self) -> String {
        let mut text = String::new();
        for node in &self.content {
            if let Node::Text(value) | Node::CData(value) = node {
                text.push_str(value);
            }
        }
        text
    }
}

/// Why JDOM gives no tree: what was wrong where.
pub(super) type Invalid = String;

/// `SAXBuilder.build` of `input` with ROME's settings: the root element, or why Xerces or
/// JDOM fails on it. ROME disallows document type declarations.
///
/// Each element `picks` picks as it starts, by its open ancestors, the root first, and itself
/// without its content, is left out of the tree and handed to `each` with its ancestors once it
/// ends, whole; what an element `picks` picks holds is not asked about.
pub(super) fn build(
    input: &[u8],
    picks: &mut dyn FnMut(&[Element], &Element) -> bool,
    each: &mut dyn FnMut(&[Element], Element),
) -> Result<Element, Invalid> {
    let mut builder = Builder::default();
    let mut disallowed = false;
    let result = xml::parse(input, &mut |event| {
        if matches!(event, Event::DocumentType) {
            disallowed = true;
            return Flow::Stop;
        }
        builder.event(event, picks);
        if let Some(picked) = builder.picked.take() {
            each(&builder.open, picked);
        }
        Flow::Continue
    });
    if disallowed {
        return Err("DOCTYPE is disallowed when the feature \
                    \"http://apache.org/xml/features/disallow-doctype-decl\" set to true"
            .to_owned());
    }
    result.map_err(|error| match error {
        XmlError::CharConversion => "a byte sequence its encoding does not allow".to_owned(),
        XmlError::Malformed(reason) => reason,
        XmlError::EntityExpansionLimit => "more entity expansions than allowed".to_owned(),
    })?;
    builder
        .root
        .ok_or_else(|| "the document has no root element".to_owned())
}

/// `SAXHandler`'s state: the open elements, and the character data not yet added as a node,
/// with whether it is CDATA, which JDOM decides as Xerces' events come; how deep the open
/// elements are into one picked to be left out, if they are, and the one that last ended.
#[derive(Default)]
struct Builder {
    open: Vec<Element>,
    root: Option<Element>,
    text: String,
    previous_cdata: bool,
    in_cdata: bool,
    picking: Option<usize>,
    picked: Option<Element>,
}

impl Builder {
    fn event(&mut self, event: Event<'_>, picks: &mut dyn FnMut(&[Element], &Element) -> bool) {
        match event {
            Event::Start(name, attributes) => {
                self.start(name, attributes);
                match &mut self.picking {
                    Some(depth) => *depth += 1,
                    None => {
                        let (element, ancestors) =
                            self.open.split_last().expect("an element was just opened");
                        if picks(ancestors, element) {
                            self.picking = Some(0);
                        }
                    }
                }
            }
            Event::End(_) => self.end(),
            Event::Text(text, TextKind::Data | TextKind::Reference) => self.characters(text),
            Event::Text(text, TextKind::CData) => {
                self.in_cdata = true;
                if !text.is_empty() {
                    self.characters(text);
                }
                self.previous_cdata = true;
                self.flush();
                self.previous_cdata = false;
                self.in_cdata = false;
            }
            Event::Comment(text) => {
                self.flush();
                if !text.is_empty() {
                    self.add(Node::Comment(text.to_owned()));
                }
            }
            Event::ProcessingInstruction(target, data) => {
                self.flush();
                self.add(Node::Instruction {
                    target: target.to_owned(),
                    data: data.to_owned(),
                });
            }
            Event::DocumentType => {}
        }
    }

    /// Adds a node to the open element; the document's own comments and instructions, outside
    /// the root, are not kept.
    fn add(&mut self, node: Node) {
        if let Some(element) = self.open.last_mut() {
            element.content.push(node);
        }
    }

    fn characters(&mut self, text: &str) {
        if self.previous_cdata != self.in_cdata {
            self.flush();
        }
        self.text.push_str(text);
    }

    /// `flushCharacters`: the pending character data as a text or CDATA node.
    fn flush(&mut self) {
        let data = std::mem::take(&mut self.text);
        if data.is_empty() && !self.in_cdata {
            self.previous_cdata = self.in_cdata;
            return;
        }
        if self.previous_cdata {
            self.add(Node::CData(data));
        } else if !data.is_empty() {
            self.add(Node::Text(data));
        }
        self.previous_cdata = self.in_cdata;
    }

    fn start(&mut self, name: &xml::Name, attributes: &[xml::Attribute]) {
        let qualified_name = name.qualified_name();
        let prefix = qualified_name
            .split_once(':')
            .map_or("", |(prefix, _)| prefix);
        let namespace = Namespace {
            prefix: prefix.to_owned(),
            uri: name.namespace().to_owned(),
        };
        let additional_namespaces = attributes
            .iter()
            .filter(|attribute| attribute.namespace == XMLNS)
            .map(|declaration| Namespace {
                prefix: declaration
                    .qualified_name
                    .strip_prefix("xmlns:")
                    .unwrap_or_default()
                    .to_owned(),
                uri: declaration.value.clone(),
            })
            .filter(|declared| *declared != namespace)
            .collect();
        let attributes = attributes
            .iter()
            .filter(|attribute| attribute.namespace != XMLNS)
            .map(|attribute| Attribute {
                namespace: attribute.namespace.clone(),
                name: attribute.local_name.clone(),
                value: attribute.value.clone(),
            })
            .collect();
        self.flush();
        self.open.push(Element {
            namespace,
            name: name.local_name().to_owned(),
            qualified_name: qualified_name.to_owned(),
            attributes,
            additional_namespaces,
            content: Vec::new(),
        });
    }

    fn end(&mut self) {
        self.flush();
        let element = self.open.pop().expect("an end tag closes an open element");
        match &mut self.picking {
            Some(0) => {
                self.picking = None;
                self.picked = Some(element);
                return;
            }
            Some(depth) => *depth -= 1,
            None => {}
        }
        match self.open.last_mut() {
            Some(parent) => parent.content.push(Node::Element(element)),
            None => self.root = Some(element),
        }
    }
}

/// The line separator JDOM's raw `Format` writes for a line feed in text.
const LINE_SEPARATOR: &str = "\r\n";

/// What `new XMLOutputter().outputString(contents)` writes of `contents`, but for the inside
/// of each tag, which it writes as the bare name: text escaped, CDATA, comments and processing
/// instructions as they are written in XML, elements as tags. ROME's output reaches Tika only
/// through `FeedParser.stripTags`, which removes everything from a `<` to the next `>`, and a
/// tag's attributes and namespace declarations, which `XMLOutputter` escapes, never hold a `>`,
/// so they never change what it leaves.
pub(super) fn output(contents: &[Node], out: &mut String) {
    for node in contents {
        match node {
            Node::Text(text) => escape_text(text, out),
            Node::CData(text) => {
                out.push_str("<![CDATA[");
                out.push_str(text);
                out.push_str("]]>");
            }
            Node::Element(element) => output_element(element, out),
            Node::Comment(text) => {
                out.push_str("<!--");
                out.push_str(text);
                out.push_str("-->");
            }
            Node::Instruction { target, data } => {
                out.push_str("<?");
                out.push_str(target);
                if !data.is_empty() {
                    out.push(' ');
                    out.push_str(data);
                }
                out.push_str("?>");
            }
        }
    }
}

/// What `XMLOutputter.outputString(element)` writes of `element`, its tags as [`output`]
/// writes them.
pub(super) fn output_element(element: &Element, out: &mut String) {
    out.push('<');
    out.push_str(&element.qualified_name);
    if element.content.is_empty() {
        out.push_str(" />");
        return;
    }
    out.push('>');
    output(&element.content, out);
    out.push_str("</");
    out.push_str(&element.qualified_name);
    out.push('>');
}

/// `Format.escapeText` with the UTF-8 escape strategy and the raw format's line separator:
/// markup characters as entities, a carriage return as a character reference, a line feed as
/// the separator, and a character beyond the Basic Multilingual Plane, a surrogate pair to
/// Java, as a hexadecimal character reference.
fn escape_text(text: &str, out: &mut String) {
    for character in text.chars() {
        match character {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\r' => out.push_str("&#xD;"),
            '\n' => out.push_str(LINE_SEPARATOR),
            _ if u32::from(character) > 0xFFFF => {
                out.push_str(&format!("&#x{:x};", u32::from(character)));
            }
            _ => out.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(xml: &[u8]) -> Result<Element, Invalid> {
        build(xml, &mut |_, _| false, &mut |_, _| {})
    }

    fn root(xml: &str) -> Element {
        whole(xml.as_bytes()).unwrap()
    }

    #[test]
    fn hands_on_the_elements_picked_as_they_end() {
        let mut picked = Vec::new();
        let root = build(
            b"<r>a<i><j>1</j></i>b<x><i>2</i></x><i>3</i></r>",
            &mut |ancestors, element| ancestors.len() == 1 && element.name == "i",
            &mut |ancestors, element| picked.push((ancestors.len(), element.text())),
        )
        .unwrap();
        assert_eq!(picked, [(1, String::new()), (1, "3".to_owned())]);
        assert_eq!(root.text(), "ab");
        assert_eq!(root.children().count(), 1);
    }

    #[test]
    fn builds_text_and_cdata_nodes_as_jdom_does() {
        let element = root("<r>a<![CDATA[b]]>c<![CDATA[]]><!--x--><!----><?p d?><e/>f</r>");
        assert_eq!(
            element.content,
            [
                Node::Text("a".into()),
                Node::CData("b".into()),
                Node::CData("c".into()),
                Node::Comment("x".into()),
                Node::Instruction {
                    target: "p".into(),
                    data: "d".into()
                },
                Node::Element(root("<e/>")),
                Node::Text("f".into()),
            ]
        );
        assert_eq!(element.text(), "abcf");
        assert_eq!(
            root("<r><![CDATA[]]></r>").content,
            [Node::CData(String::new())]
        );
    }

    #[test]
    fn keeps_the_namespaces_an_element_declares_besides_its_own() {
        let element =
            root("<rdf:RDF xmlns:rdf='urn:r' xmlns='urn:d' xmlns:x='urn:x' x:a='1' b='2'/>");
        assert_eq!(element.namespace_uri(), "urn:r");
        assert_eq!(
            element.additional_namespaces,
            [
                Namespace {
                    prefix: String::new(),
                    uri: "urn:d".into()
                },
                Namespace {
                    prefix: "x".into(),
                    uri: "urn:x".into()
                }
            ]
        );
        assert_eq!(element.attribute("a", "urn:x"), Some("1"));
        assert_eq!(element.attribute("a", ""), None);
        assert_eq!(element.attribute("b", ""), Some("2"));
    }

    #[test]
    fn writes_content_as_xml_outputter_does() {
        let element = root("<r>a &amp; b\r\n<i>x<![CDATA[<y>]]></i><j/>\u{1F600}<!--c--><?p?></r>");
        let mut out = String::new();
        output(&element.content, &mut out);
        assert_eq!(
            out,
            "a &amp; b\r\n<i>x<![CDATA[<y>]]></i><j />&#x1f600;<!--c--><?p?>"
        );
    }

    #[test]
    fn refuses_document_type_declarations() {
        assert!(whole(b"<!DOCTYPE r><r/>").is_err());
        assert!(whole(b"<r>").is_err());
    }
}
