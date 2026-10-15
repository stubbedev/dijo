//! Tika's `XFAExtractor`: what an XFA form's packets hold, read with a pull parser over the
//! form's XML. The text of its text elements becomes paragraphs as they come; the fields of
//! its template, named by their tooltips, are listed last with the values its data gives them.

use lopdf::Object;

use super::cos::Pdf;
use super::Unported;
use crate::formats::handlers::is_blank;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::Failure;

/// What `XFA_TEMPLATE_ANY_VERSION` matches the start of: the template namespace of any version.
const TEMPLATE_NAMESPACE: &str = "http://www.xfa.org/schema/xfa-template";
/// `XFA_DATA_NS`.
const DATA_NAMESPACE: &str = "http://www.xfa.org/schema/xfa-data/1.0/";
/// What `TEXT_PATTERN` matches: the local names of the elements whose text is written.
const TEXT_ELEMENTS: [&str; 5] = ["speak", "text", "contents-richtext", "toolTip", "exData"];

/// Whether `XFA_TEMPLATE_ANY_VERSION` finds the template namespace at the start of
/// `namespace`, each `.` of the pattern matching any character but a line terminator.
fn is_template(namespace: &str) -> bool {
    let mut characters = namespace.chars();
    TEMPLATE_NAMESPACE.chars().all(|expected| {
        characters.next().is_some_and(|character| match expected {
            '.' => !matches!(character, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'),
            _ => character == expected,
        })
    })
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
    document_type: bool,
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

    fn document_type(&mut self) -> Result<(), Failure> {
        self.document_type = true;
        Ok(())
    }
}

/// `XFAExtractor.extract` of the form's XML.
pub(super) fn extract(input: &[u8]) -> Result<Form, Unported> {
    let mut nodes = Nodes::default();
    if xml::parse_sax(input, "the XFA form", &mut nodes).is_err() {
        return Err(Unported(
            "an XFA form whose XML does not parse, where Tika records the failure and reads the \
             AcroForm"
                .to_owned(),
        ));
    }
    if nodes.document_type {
        return Err(Unported(
            "an XFA form with a document type declaration, which Woodstox reads without DTD \
             support"
                .to_owned(),
        ));
    }
    Ok(Reader {
        nodes: &nodes.nodes,
        next: 0,
    }
    .form())
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
    /// the end of the element unless it is blank.
    fn paragraphs(&mut self, namespace: &str, local_name: &str, paragraphs: &mut Vec<String>) {
        let mut buffer = String::new();
        while let Some(node) = self.next() {
            match node {
                Node::Text(text) => buffer.push_str(text),
                Node::End { .. } if node.ends(namespace, local_name) => break,
                Node::End { local_name, .. } if local_name == "p" => {
                    paragraphs.push(std::mem::take(&mut buffer));
                }
                _ => {}
            }
        }
        if !is_blank(&buffer) {
            paragraphs.push(buffer);
        }
    }

    /// `scrapeTextUntil` collecting text: a line break at the end of each `p`.
    fn text(&mut self, namespace: &str, local_name: &str) -> String {
        let mut buffer = String::new();
        while let Some(node) = self.next() {
            match node {
                Node::Text(text) => buffer.push_str(text),
                Node::End { .. } if node.ends(namespace, local_name) => break,
                Node::End { local_name, .. } if local_name == "p" => buffer.push('\n'),
                _ => {}
            }
        }
        buffer
    }

    /// `loadData`: the text gathered up to each end tag, by the end tag's local name, until the
    /// data ends.
    fn data(&mut self, values: &mut Vec<(String, Vec<String>)>) {
        let mut buffer = String::new();
        while let Some(node) = self.next() {
            match node {
                Node::Text(text) => buffer.push_str(text),
                Node::End { local_name, .. } => {
                    if !buffer.is_empty() {
                        let value = std::mem::take(&mut buffer);
                        match values.iter_mut().find(|(key, _)| key == local_name) {
                            Some((_, list)) => list.push(value),
                            None => values.push((local_name.clone(), vec![value])),
                        }
                    }
                    if node.ends(DATA_NAMESPACE, "data") {
                        return;
                    }
                }
                Node::Start { .. } => {}
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
    fn matches_template_namespaces_as_the_pattern_does() {
        assert!(is_template("http://www.xfa.org/schema/xfa-template/2.8/"));
        assert!(is_template("http://wwwXxfa-org/schema/xfa-template"));
        assert!(!is_template("http://www.xfa.org/schema/xfa-data/1.0/"));
        assert!(!is_template("http://www.xfa.org/schema/xfa-templ"));
    }

    #[test]
    fn defers_forms_whose_xml_fails() {
        assert!(extract(b"<a><b></a>").is_err());
        assert!(extract(b"").is_err());
    }

    #[test]
    fn reads_fields_data_and_text_as_xfaextractor_does() {
        let form = br#"<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/">
<template xmlns="http://www.xfa.org/schema/xfa-template/3.0/">
 <subform><draw><value><text>Ans&#248;gning</text></value></draw>
  <field name="navn"><assist><toolTip>Dit <p>fulde</p> navn</toolTip></assist></field>
  <field name="by"/>
 </subform>
</template>
<xfa:datasets xmlns:xfa="http://www.xfa.org/schema/xfa-data/1.0/">
 <xfa:data><form1><navn>Kari</navn><by>Oslo</by></form1></xfa:data>
</xfa:datasets>
</xdp:xdp>"#;
        assert_eq!(
            extract(form).unwrap(),
            Form {
                paragraphs: vec!["Ansøgning".to_owned()],
                fields: vec![
                    ("navn".to_owned(), "Dit fulde\n navn: Kari".to_owned()),
                    ("by".to_owned(), "by: Oslo".to_owned()),
                ],
            }
        );
    }
}
