//! Tika's `XFAExtractor`: what an XFA form's packets hold, read with a pull parser over the
//! form's XML. The text of its text elements becomes paragraphs as they come; the fields of
//! its template, named by their tooltips, are listed last with the values its data gives them.

use lopdf::Object;

use super::cos::Pdf;
use super::Unported;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java::is_blank;

/// What `XFA_TEMPLATE_ANY_VERSION` matches the start of: the template namespace of any version.
const TEMPLATE_NAMESPACE: &str = "http://www.xfa.org/schema/xfa-template";
/// `XFA_DATA_NS`.
const DATA_NAMESPACE: &str = "http://www.xfa.org/schema/xfa-data/1.0/";
/// What `TEXT_PATTERN` matches: the local names of the elements whose text is written.
const TEXT_ELEMENTS: [&str; 5] = ["speak", "text", "contents-richtext", "toolTip", "exData"];

/// Whether `namespace` is the template namespace of some version. Tika's
/// `XFA_TEMPLATE_ANY_VERSION` leaves the pattern's dots unescaped, so each matches any
/// character (#109).
fn is_template(namespace: &str) -> bool {
    namespace.starts_with(TEMPLATE_NAMESPACE)
}

/// `PDXFAResource.getBytes`: the stream, or the streams of a packet array's odd entries one
/// after another.
pub(super) fn bytes(pdf: &Pdf, xfa: &Object) -> Result<Vec<u8>, Unported> {
    match xfa {
        Object::Array(packets) => {
            let mut bytes = Vec::new();
            for packet in packets.iter().skip(1).step_by(2) {
                if let Some(Object::Stream(stream)) = pdf.resolve(packet) {
                    bytes.extend(pdf.decode(stream)?);
                }
            }
            Ok(bytes)
        }
        Object::Stream(stream) => pdf.decode(stream),
        _ => Ok(Vec::new()),
    }
}

/// What `XFAExtractor.extract` writes: the paragraphs inside its `xfa_content` div, then, if
/// the template has fields, the list of them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Form {
    pub(super) paragraphs: Vec<String>,
    /// Each field's name and the text of its item.
    pub(super) fields: Vec<(String, String)>,
}

/// An event of the pull parser `XFAExtractor` reads with.
#[derive(Debug)]
enum Node {
    Start {
        namespace: String,
        local_name: String,
        /// The value of the first attribute of the local name `name`.
        name: Option<String>,
    },
    End {
        namespace: String,
        local_name: String,
    },
    Text(String),
}

impl Node {
    fn ends(&self, namespace: &str, local_name: &str) -> bool {
        matches!(self, Self::End { namespace: n, local_name: l } if n == namespace && l == local_name)
    }
}

/// The events of an XFA form's XML.
#[derive(Default)]
struct Nodes {
    nodes: Vec<Node>,
}

impl SaxHandler for Nodes {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.nodes.push(Node::Start {
            namespace: namespace.to_owned(),
            local_name: local_name.to_owned(),
            name: attributes
                .iter()
                .find(|attribute| attribute.local_name == "name")
                .map(|attribute| attribute.value.clone()),
        });
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.nodes.push(Node::End {
            namespace: namespace.to_owned(),
            local_name: local_name.to_owned(),
        });
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        match self.nodes.last_mut() {
            Some(Node::Text(buffer)) => buffer.push_str(text),
            _ => self.nodes.push(Node::Text(text.to_owned())),
        }
        Ok(())
    }
}

/// What `XFAExtractor.extract` makes of a form's XML.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Extracted {
    Read(Form),
    /// XML the pull parser fails on: the paragraphs written before the failure, which ends the
    /// `xfa_content` div there, and the failure, which `extractAcroForm` records before it reads
    /// the AcroForm's fields instead.
    Failed {
        paragraphs: Vec<String>,
        failure: String,
    },
}

/// `XFAExtractor.extract` of the form's XML, read as the JDK's StAX reader reads it without DTD
/// support: a document type declaration is read past, and an entity reference other than the
/// predefined ones fails the XML.
pub(super) fn extract(input: &[u8]) -> Extracted {
    let mut nodes = Nodes::default();
    let parsed = xml::parse_without_dtd(input, "the XFA form", &mut nodes);
    let form = Reader {
        nodes: &nodes.nodes,
        next: 0,
    }
    .form();
    let reason = match parsed {
        Ok(()) => return Extracted::Read(form),
        Err(Failure::Malformed(reason)) => reason,
        Err(Failure::Extraction(error)) => error.to_string(),
        Err(failure @ (Failure::Stopped(_) | Failure::Embedded(_))) => {
            unreachable!("the XFA form's events are only gathered: {failure:?}")
        }
    };
    Extracted::Failed {
        paragraphs: form.paragraphs,
        failure: format!("javax.xml.stream.XMLStreamException: {reason}"),
    }
}

/// The pull parser over the form's events.
struct Reader<'n> {
    nodes: &'n [Node],
    next: usize,
}

impl<'n> Reader<'n> {
    fn next(&mut self) -> Option<&'n Node> {
        let node = self.nodes.get(self.next)?;
        self.next += 1;
        Some(node)
    }

    fn form(&mut self) -> Form {
        let mut form = Form::default();
        let mut values: Vec<(String, Vec<String>)> = Vec::new();
        let mut fields: Vec<(String, String)> = Vec::new();
        while let Some(node) = self.next() {
            let Node::Start {
                namespace,
                local_name,
                name,
            } = node
            else {
                continue;
            };
            if is_template(namespace) && local_name == "field" {
                self.field(name.clone().unwrap_or_default(), &mut fields);
            } else if namespace == DATA_NAMESPACE && local_name == "data" {
                self.data(&mut values);
            } else if TEXT_ELEMENTS.contains(&local_name.as_str()) {
                self.paragraphs(namespace, local_name, &mut form.paragraphs);
            }
        }
        for (name, tooltip) in fields {
            let display = if is_blank(&tooltip) { &name } else { &tooltip };
            let field_values = values
                .iter()
                .find(|(key, _)| *key == name)
                .map_or(&[][..], |(_, values)| values.as_slice());
            if field_values.is_empty() {
                form.fields.push((name.clone(), format!("{display}: ")));
            }
            for value in field_values {
                form.fields
                    .push((name.clone(), format!("{display}: {value}")));
            }
        }
        form
    }

    /// `scrapeTextUntil` writing paragraphs: one at the end of each `p`, and what is left at
    /// the end of the element unless it is blank, or unless the XML fails before it ends.
    fn paragraphs(&mut self, namespace: &str, local_name: &str, paragraphs: &mut Vec<String>) {
        let mut buffer = String::new();
        let mut ended = false;
        while let Some(node) = self.next() {
            match node {
                Node::Text(text) => buffer.push_str(text),
                Node::End { .. } if node.ends(namespace, local_name) => {
                    ended = true;
                    break;
                }
                Node::End { local_name, .. } if local_name == "p" => {
                    paragraphs.push(std::mem::take(&mut buffer));
                }
                _ => {}
            }
        }
        if ended && !is_blank(&buffer) {
            paragraphs.push(buffer);
        }
    }

    /// `scrapeTextUntil` collecting text: a line break between `p`s. Tika ends each `p` with
    /// one, the last included (#109).
    fn text(&mut self, namespace: &str, local_name: &str) -> String {
        let mut buffer = String::new();
        let mut paragraph_ended = false;
        while let Some(node) = self.next() {
            match node {
                Node::Text(text) => {
                    if paragraph_ended && !text.is_empty() {
                        buffer.push('\n');
                        paragraph_ended = false;
                    }
                    buffer.push_str(text);
                }
                Node::End { .. } if node.ends(namespace, local_name) => break,
                Node::End { local_name, .. } if local_name == "p" => paragraph_ended = true,
                _ => {}
            }
        }
        buffer
    }

    /// `loadData`: the text of each element holding text and no elements, by its local name,
    /// until the data ends, blank text left out. Tika takes all the text since the last end
    /// tag, whitespace and the text around child elements included, as the value of the next
    /// end tag (#109).
    fn data(&mut self, values: &mut Vec<(String, Vec<String>)>) {
        let mut buffer = String::new();
        let mut leaf = false;
        while let Some(node) = self.next() {
            match node {
                Node::Text(text) => buffer.push_str(text),
                Node::Start { .. } => {
                    buffer.clear();
                    leaf = true;
                }
                Node::End { local_name, .. } => {
                    if node.ends(DATA_NAMESPACE, "data") {
                        return;
                    }
                    if leaf && !is_blank(&buffer) {
                        let value = std::mem::take(&mut buffer);
                        match values.iter_mut().find(|(key, _)| key == local_name) {
                            Some((_, list)) => list.push(value),
                            None => values.push((local_name.clone(), vec![value])),
                        }
                    }
                    buffer.clear();
                    leaf = false;
                }
            }
        }
    }

    /// `handleField`: the field's tooltip, kept under its name where the field ends; a later
    /// field of the name replaces it in its place.
    fn field(&mut self, name: String, fields: &mut Vec<(String, String)>) {
        let mut tooltip = String::new();
        while let Some(node) = self.next() {
            match node {
                Node::Start {
                    namespace,
                    local_name,
                    ..
                } if local_name == "toolTip" => {
                    tooltip = self.text(namespace, local_name);
                }
                Node::End {
                    namespace,
                    local_name,
                } if is_template(namespace) && local_name == "field" => {
                    match fields.iter_mut().find(|(key, _)| *key == name) {
                        Some((_, existing)) => *existing = tooltip,
                        None => fields.push((name, tooltip)),
                    }
                    return;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_template_namespaces_of_any_version() {
        assert!(is_template("http://www.xfa.org/schema/xfa-template/2.8/"));
        assert!(!is_template("http://wwwXxfa-org/schema/xfa-template"));
        assert!(!is_template("http://www.xfa.org/schema/xfa-data/1.0/"));
        assert!(!is_template("http://www.xfa.org/schema/xfa-templ"));
    }

    #[test]
    fn keeps_the_paragraphs_written_before_the_xml_fails() {
        let failed = |input: &[u8]| match extract(input) {
            Extracted::Failed { paragraphs, .. } => paragraphs,
            Extracted::Read(form) => panic!("{form:?}"),
        };
        assert_eq!(failed(b""), Vec::<String>::new());
        let form = b"<form><text>En<p>to</p>tre</text><field name=\"f\"/>\
                     <text>fire<p>fem</p>seks</tex></form>";
        assert_eq!(failed(form), ["Ento", "tre", "firefem"]);
    }

    #[test]
    fn reads_fields_data_and_text() {
        let form = br#"<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/">
<template xmlns="http://www.xfa.org/schema/xfa-template/3.0/">
 <subform><draw><value><text>Ans&#248;gning</text></value></draw>
  <field name="navn"><assist><toolTip>Dit <p>fulde</p> navn</toolTip></assist></field>
  <field name="by"><assist><toolTip><p>Byen</p><p>du bor i</p></toolTip></assist></field>
  <field name="adresse"/><field name="gate"/>
 </subform>
</template>
<xfa:datasets xmlns:xfa="http://www.xfa.org/schema/xfa-data/1.0/">
 <xfa:data><form1>
  <navn>Kari</navn><by>Oslo</by><adresse> <gate>Storgata</gate> 1</adresse>
 </form1></xfa:data>
</xfa:datasets>
</xdp:xdp>"#;
        assert_eq!(
            extract(form),
            Extracted::Read(Form {
                paragraphs: vec!["Ansøgning".to_owned()],
                fields: vec![
                    ("navn".to_owned(), "Dit fulde\n navn: Kari".to_owned()),
                    ("by".to_owned(), "Byen\ndu bor i: Oslo".to_owned()),
                    ("adresse".to_owned(), "adresse: ".to_owned()),
                    ("gate".to_owned(), "gate: Storgata".to_owned()),
                ],
            })
        );
    }
}
