//! The tree builder against html5lib's tree construction tests:
//! `tests/fixtures/html/html5lib-trees.tsv.gz`, written by `just html-reference`, holds the trees
//! they expect without scripting. Also the form `HtmlReference.java` gives jsoup's trees in, which
//! [`super::reference`] compares with.

use std::io::Read;

use flate2::read::GzDecoder;

use super::dom::{Dom, Namespace, NodeData, NodeId, DOCUMENT};
use super::tree_builder::parse;
use crate::text::charset::tests::base64;

const TREES: &[u8] = include_bytes!("../../../tests/fixtures/html/html5lib-trees.tsv.gz");

/// A tree in the form of `HtmlReference.java`.
pub(super) fn tree(dom: &Dom) -> String {
    let mut out = String::new();
    reference_node(dom, DOCUMENT, &mut out);
    out
}

fn reference_text(kind: char, text: &str, out: &mut String) {
    out.push_str(&format!("{kind} {}:{text}\n", text.encode_utf16().count()));
}

fn reference_node(dom: &Dom, id: NodeId, out: &mut String) {
    let (name, namespace) = match &dom.node(id).data {
        NodeData::Document => ("#document", Namespace::Html),
        NodeData::Element {
            name, namespace, ..
        } => (name.as_str(), *namespace),
        NodeData::Text(content) => return reference_text('T', content, out),
        NodeData::Comment(content) => return reference_text('C', content, out),
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            out.push_str(&format!("Y {name}|{public_id}|{system_id}\n"));
            return;
        }
    };
    let namespace = match namespace {
        Namespace::Html => "html",
        Namespace::Svg => "svg",
        Namespace::MathMl => "math",
    };
    out.push_str(&format!("S {name} {namespace}"));
    for attribute in dom.attributes(id) {
        let value = &attribute.value;
        out.push_str(&format!(
            " {}={}:{value}",
            attribute.key,
            value.encode_utf16().count()
        ));
    }
    out.push('\n');
    for &child in &dom.node(id).children {
        reference_node(dom, child, out);
    }
    out.push_str("E\n");
}

/// The attributes the parser adjusts to a namespace on a foreign element, by the name it gives
/// them, which html5lib's form writes with a space for the colon.
const ADJUSTED_FOREIGN_ATTRIBUTES: [&str; 10] = [
    "xlink:actuate",
    "xlink:arcrole",
    "xlink:href",
    "xlink:role",
    "xlink:show",
    "xlink:title",
    "xlink:type",
    "xml:lang",
    "xml:space",
    "xmlns:xlink",
];

/// A tree in html5lib's form.
fn html5lib(dom: &Dom) -> String {
    let mut lines = Vec::new();
    for &child in &dom.node(DOCUMENT).children {
        html5lib_node(dom, child, 0, &mut lines);
    }
    lines.join("\n")
}

fn html5lib_node(dom: &Dom, id: NodeId, depth: usize, lines: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    let (name, namespace) = match &dom.node(id).data {
        NodeData::Text(text) => return lines.push(format!("| {indent}\"{text}\"")),
        NodeData::Comment(text) => return lines.push(format!("| {indent}<!-- {text} -->")),
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            let ids = if public_id.is_empty() && system_id.is_empty() {
                String::new()
            } else {
                format!(" \"{public_id}\" \"{system_id}\"")
            };
            return lines.push(format!("| {indent}<!DOCTYPE {name}{ids}>"));
        }
        NodeData::Document => panic!("the document is inside node {id}"),
        NodeData::Element {
            name, namespace, ..
        } => (name, *namespace),
    };
    let prefix = match namespace {
        Namespace::Html => "",
        Namespace::Svg => "svg ",
        Namespace::MathMl => "math ",
    };
    lines.push(format!("| {indent}<{prefix}{name}>"));
    let mut attributes: Vec<String> = dom
        .attributes(id)
        .iter()
        .map(|attribute| {
            let key = if namespace != Namespace::Html
                && ADJUSTED_FOREIGN_ATTRIBUTES.contains(&attribute.key.as_str())
            {
                attribute.key.replacen(':', " ", 1)
            } else {
                attribute.key.clone()
            };
            format!("| {indent}  {key}=\"{}\"", attribute.value)
        })
        .collect();
    attributes.sort();
    lines.extend(attributes);
    let mut depth = depth + 1;
    if namespace == Namespace::Html && name == "template" {
        lines.push(format!("| {indent}  content"));
        depth += 1;
    }
    for &child in &dom.node(id).children {
        html5lib_node(dom, child, depth, lines);
    }
}

/// The tests whose `option`, closed by another tag or the end of the input rather than by its own
/// end tag, is not copied into the `selectedcontent` of its `select`: html5ever copies an option
/// only at `</option>` (servo/html5ever#712).
const IMPLICITLY_CLOSED_SELECTED_OPTIONS: [&str; 4] = [
    "html5lib/webkit02.dat#45",
    "html5lib/webkit02.dat#46",
    "html5lib/webkit02.dat#47",
    "html5lib/webkit02.dat#48",
];

/// The tests of html5lib's reference, each its name, its input and the tree it expects.
fn html5lib_tests() -> Vec<(String, String, String)> {
    let mut reference = String::new();
    GzDecoder::new(TREES)
        .read_to_string(&mut reference)
        .expect("the tree reference decompresses");
    reference
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (
                fields[0].to_owned(),
                String::from_utf8(base64(fields[1])).expect("the input is UTF-8"),
                String::from_utf8(base64(fields[2])).expect("the tree is UTF-8"),
            )
        })
        .collect()
}

/// The inputs of html5lib's tree construction tests, each by its name.
pub(super) fn html5lib_inputs() -> Vec<(String, String)> {
    html5lib_tests()
        .into_iter()
        .map(|(name, input, _)| (name, input))
        .collect()
}

#[test]
fn builds_the_trees_html5lib_expects() {
    let mut mismatches = Vec::new();
    let mut unmet = Vec::new();
    let mut count = 0;
    for (name, input, expected) in html5lib_tests() {
        let actual = html5lib(&parse(&input));
        count += 1;
        if actual == expected {
            continue;
        }
        if IMPLICITLY_CLOSED_SELECTED_OPTIONS.contains(&name.as_str()) {
            unmet.push(name);
            continue;
        }
        mismatches.push(format!(
            "{name} {input:?}\nexpected\n{expected}\nactual\n{actual}"
        ));
    }
    assert_eq!(unmet, IMPLICITLY_CLOSED_SELECTED_OPTIONS);
    assert!(count > 1400, "the reference has {count} trees");
    assert!(
        mismatches.is_empty(),
        "{} of {count} trees differ:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// The tree of `html` in html5lib's form.
fn parsed(html: &str) -> String {
    html5lib(&parse(html))
}

#[test]
fn copies_the_selected_option_closed_by_its_end_tag_into_selectedcontent() {
    assert_eq!(
        parsed(
            "<select><button><selectedcontent></selectedcontent></button><option>X</option>\
             <option selected>Y<b>!</b></option><option>Z</option></select>"
        ),
        "| <html>\n|   <head>\n|   <body>\n|     <select>\n|       <button>\n\
         |         <selectedcontent>\n|           \"Y\"\n|           <b>\n|             \"!\"\n\
         |       <option>\n|         \"X\"\n|       <option>\n|         selected=\"\"\n\
         |         \"Y\"\n|         <b>\n|           \"!\"\n|       <option>\n|         \"Z\""
    );
}

/// Where jsoup's tree builder departed from the WHATWG algorithm, as #84 lists.
mod whatwg {
    use super::{parse, parsed};
    use crate::formats::html::dom::{NodeData, DOCUMENT};
    use crate::formats::html::tree_builder::MAX_DEPTH;

    const HEAD_BODY: &str = "| <html>\n|   <head>\n|   <body>\n";

    #[test]
    fn normalises_line_breaks() {
        assert_eq!(
            parsed("<p>a\r\nb\rc</p>"),
            format!("{HEAD_BODY}|     <p>\n|       \"a\nb\nc\"")
        );
    }

    #[test]
    fn drops_or_replaces_nul_as_the_state_requires() {
        assert_eq!(
            parsed("<p>a\0b</p><textarea>c\0d</textarea>"),
            format!(
                "{HEAD_BODY}|     <p>\n|       \"ab\"\n|     <textarea>\n|       \"c\u{FFFD}d\""
            )
        );
    }

    #[test]
    fn reads_u_ffff_as_a_character() {
        assert_eq!(
            parsed("<p>before \u{FFFF} after</p>"),
            format!("{HEAD_BODY}|     <p>\n|       \"before \u{FFFF} after\"")
        );
    }

    #[test]
    fn ignores_the_self_closing_flag_of_a_non_void_element() {
        assert_eq!(
            parsed("<div/>a<span/>b"),
            format!("{HEAD_BODY}|     <div>\n|       \"a\"\n|       <span>\n|         \"b\"")
        );
    }

    #[test]
    fn fosters_text_out_of_a_table() {
        assert_eq!(
            parsed("<table>foster<tr><td>x</td></tr>after</table>"),
            format!(
                "{HEAD_BODY}|     \"fosterafter\"\n|     <table>\n|       <tbody>\n\
                 |         <tr>\n|           <td>\n|             \"x\""
            )
        );
    }

    #[test]
    fn drops_white_space_before_head_and_keeps_it_after_body_in_body() {
        assert_eq!(
            parsed("<html> \n <head></head><body>x</body>\n</html>\n"),
            "| <html>\n|   <head>\n|   <body>\n|     \"x\n\n\""
        );
    }

    #[test]
    fn reconstructs_every_formatting_element() {
        let formatting = [
            "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong",
            "tt", "u",
        ];
        let open: String = formatting.iter().map(|name| format!("<{name}>")).collect();
        let tree = parsed(&format!("<p>{open}x<p>y"));
        let y = tree.lines().last().unwrap();
        assert_eq!(y.trim_start_matches(['|', ' ']), "\"y\"", "{tree}");
        let depth = (y.len() - y.trim_start_matches(['|', ' ']).len() - 2) / 2;
        assert_eq!(depth, 3 + formatting.len(), "{tree}");
    }

    #[test]
    fn closes_an_applet_at_its_end_tag() {
        assert_eq!(
            parsed("<applet>a</applet>b"),
            format!("{HEAD_BODY}|     <applet>\n|       \"a\"\n|     \"b\"")
        );
    }

    #[test]
    fn integrates_foreign_object_whatever_its_case() {
        assert_eq!(
            parsed("<svg><foreignobject><p>x</p></foreignobject></svg>"),
            format!(
                "{HEAD_BODY}|     <svg svg>\n|       <svg foreignObject>\n|         <p>\n\
                 |           \"x\""
            )
        );
    }

    /// Nesting deeper than jsoup's cap keeps its elements down to the resource guard, every
    /// element, and every character of text in order.
    #[test]
    fn keeps_deep_nesting_down_to_the_resource_guard() {
        let divs = 2 * MAX_DEPTH;
        let html = format!("{}x{}", "<div>a".repeat(divs), "</div>".repeat(divs));
        let dom = parse(&html);
        let mut text = String::new();
        let mut elements = 0;
        let mut deepest = 0;
        let mut pending = vec![(DOCUMENT, 0)];
        while let Some((node, depth)) = pending.pop() {
            match &dom.node(node).data {
                NodeData::Text(more) => text.push_str(more),
                NodeData::Element { name, .. } if name == "div" => elements += 1,
                _ => {}
            }
            deepest = deepest.max(depth);
            let children = &dom.node(node).children;
            pending.extend(children.iter().rev().map(|&child| (child, depth + 1)));
        }
        assert_eq!(text, format!("{}x", "a".repeat(divs)));
        assert_eq!(elements, divs);
        assert!((MAX_DEPTH - 8..=MAX_DEPTH).contains(&deepest), "{deepest}");
    }

    /// The guard keeps the tree builder linear in the depth of nesting a document asks for.
    #[test]
    fn builds_a_deeply_nested_document_quickly() {
        let html = "<div>".repeat(200_000);
        let start = std::time::Instant::now();
        parse(&html);
        assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
    }
}
