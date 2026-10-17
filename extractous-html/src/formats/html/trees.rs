//! The tree builder against jsoup's own trees: `tests/fixtures/html/html-trees.tsv.gz`, written by
//! `just html-reference` (`HtmlReference.java` describes the form of a tree), for the documents of
//! html5lib's tree construction tests.

use std::io::Read;

use flate2::read::GzDecoder;

use super::dom::{Dom, NodeData, NodeId, DOCUMENT};
use super::reader::Source;
use super::tags::Namespace;
use super::tree_builder::parse;
use crate::formats::reference::digest;
use crate::text::charset::tests::base64;

const TREES: &[u8] = include_bytes!("../../../tests/fixtures/html/html-trees.tsv.gz");

/// A tree in the reference's form.
pub(super) fn tree(dom: &Dom) -> String {
    let mut out = String::new();
    node(dom, DOCUMENT, &mut out);
    out
}

fn text(kind: char, text: &str, out: &mut String) {
    out.push_str(&format!("{kind} {}:{text}\n", text.encode_utf16().count()));
}

fn node(dom: &Dom, id: NodeId, out: &mut String) {
    let element = match &dom.node(id).data {
        NodeData::Document { .. } => "#document".to_string(),
        NodeData::Element { .. } => dom.tag(id).name.clone(),
        NodeData::Text(content) => return text('T', content, out),
        NodeData::Data(content) => return text('D', content, out),
        NodeData::CData(content) => return text('X', content, out),
        NodeData::Comment(content) => return text('C', content, out),
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            out.push_str(&format!("Y {name}|{public_id}|{system_id}\n"));
            return;
        }
    };
    let namespace = match dom.namespace(id) {
        Namespace::Html => "html",
        Namespace::Svg => "svg",
        Namespace::MathMl => "math",
    };
    out.push_str(&format!("S {element} {namespace}"));
    for attribute in dom.attributes(id) {
        out.push(' ');
        out.push_str(&attribute.key);
        if let Some(value) = &attribute.value {
            out.push_str(&format!("={}:{value}", value.encode_utf16().count()));
        }
    }
    out.push('\n');
    for child in dom.children(id) {
        node(dom, child, out);
    }
    out.push_str("E\n");
}

#[test]
fn builds_the_trees_jsoup_builds() {
    let mut reference = String::new();
    GzDecoder::new(TREES)
        .read_to_string(&mut reference)
        .expect("the tree reference decompresses");
    let mut mismatches = Vec::new();
    let mut count = 0;
    for line in reference.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let input = String::from_utf8(base64(fields[1])).expect("the input is UTF-8");
        let units: Vec<u16> = input.encode_utf16().collect();
        let actual = digest(&tree(&parse(&units, Source::String)));
        count += 1;
        if actual != fields[2] {
            mismatches.push(format!("{} {input:?}", fields[0]));
        }
    }
    assert!(count > 1500, "the reference has {count} trees");
    assert!(
        mismatches.is_empty(),
        "{} of {count} trees differ:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
