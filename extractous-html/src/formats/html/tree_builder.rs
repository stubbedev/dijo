//! The document html5ever builds from decoded text: the WHATWG tokeniser and tree construction
//! algorithm, as a user agent that runs no scripts applies them (so `noscript` holds markup),
//! into a [`Dom`].
//!
//! Three choices go beyond the algorithm, for a text extractor:
//! - a `template` element's contents are its children, as they are text of the document;
//! - past [`MAX_DEPTH`] open elements, each new element is closed at once ([`Guard`]);
//! - an element nested deeper than [`MAX_DEPTH`] all the same keeps its descendants' text and
//!   comments but not their elements, which bounds the depth every walk over the tree recurses
//!   to.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use html5ever::interface::{ElementFlags, NodeOrText, QuirksMode, Tracer, TreeSink};
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use html5ever::tree_builder::{TreeBuilder, TreeBuilderOpts};
use html5ever::{ns, Attribute as Html5Attribute, ExpandedName, QualName, TokenizerResult};

use super::dom::{Attribute, Dom, Namespace, NodeData, NodeId, DOCUMENT};

/// How deep the elements of a document nest at most, counting the document as 0, and how many
/// elements the tree builder holds open before it closes each new one at once.
pub(super) const MAX_DEPTH: usize = 512;

/// The start tags the guard never closes: void elements, which are never open, the elements the
/// document has one of, and those whose content the tokeniser reads otherwise.
const NEVER_CLOSED: [&str; 31] = [
    "area",
    "base",
    "basefont",
    "bgsound",
    "body",
    "br",
    "col",
    "embed",
    "frame",
    "frameset",
    "head",
    "hr",
    "html",
    "iframe",
    "image",
    "img",
    "input",
    "keygen",
    "link",
    "meta",
    "noembed",
    "noframes",
    "param",
    "plaintext",
    "script",
    "source",
    "style",
    "textarea",
    "title",
    "track",
    "wbr",
];

/// Builds the document `html` holds.
pub(super) fn parse(html: &str) -> Dom {
    let builder = TreeBuilder::new(
        Sink::default(),
        TreeBuilderOpts {
            scripting_enabled: false,
            ..TreeBuilderOpts::default()
        },
    );
    let tokenizer = Tokenizer::new(Guard { builder }, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from(html));
    while !matches!(tokenizer.feed(&input), TokenizerResult::Done) {}
    tokenizer.end();
    let mut dom = tokenizer.sink.builder.sink.finish();
    flatten_beyond_max_depth(&mut dom);
    dom
}

/// The tree builder behind a guard on the elements it holds open, whose every scope check scans
/// them, so that a document nesting deeper than [`MAX_DEPTH`] is built in linear time: past it,
/// an element opened by a start tag is closed by its end tag at once, and what follows is its
/// sibling.
struct Guard {
    builder: TreeBuilder<Handle, Sink>,
}

impl Guard {
    /// The handles the tree builder holds: the document, the open elements and the active
    /// formatting elements, at least as many as the open elements.
    fn held(&self) -> usize {
        let counter = Counter(Cell::new(0));
        self.builder.trace_handles(&counter);
        counter.0.get()
    }
}

struct Counter(Cell<usize>);

impl Tracer for Counter {
    type Handle = Handle;

    fn trace_handle(&self, _node: &Handle) {
        self.0.set(self.0.get() + 1);
    }
}

impl TokenSink for Guard {
    type Handle = Handle;

    fn process_token(&self, token: Token, line_number: u64) -> TokenSinkResult<Handle> {
        let opened = match &token {
            Token::TagToken(tag)
                if tag.kind == TagKind::StartTag && !NEVER_CLOSED.contains(&&*tag.name) =>
            {
                Some(tag.name.clone())
            }
            _ => None,
        };
        let result = self.builder.process_token(token, line_number);
        let Some(name) = opened else {
            return result;
        };
        if !matches!(result, TokenSinkResult::Continue) || self.held() <= MAX_DEPTH {
            return result;
        }
        let end = Tag {
            kind: TagKind::EndTag,
            name,
            self_closing: false,
            attrs: Vec::new(),
            had_duplicate_attributes: false,
        };
        match self
            .builder
            .process_token(Token::TagToken(end), line_number)
        {
            TokenSinkResult::Continue => result,
            other => panic!("the tree builder answered an end tag with {other:?}"),
        }
    }

    fn end(&self) {
        self.builder.end();
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.builder
            .adjusted_current_node_present_but_not_in_html_namespace()
    }
}

/// A node as html5ever holds it: its id, and for an element its name, which html5ever borrows.
#[derive(Debug, Clone)]
struct Handle {
    id: NodeId,
    element: Option<Rc<Element>>,
}

#[derive(Debug)]
struct Element {
    name: QualName,
    /// Whether the element is a MathML `annotation-xml` that is an HTML integration point.
    integration_point: bool,
}

#[derive(Default)]
struct Sink {
    dom: RefCell<Dom>,
}

impl Sink {
    fn handle(&self, data: NodeData) -> Handle {
        Handle {
            id: self.dom.borrow_mut().create(data),
            element: None,
        }
    }
}

fn namespace(name: &QualName) -> Namespace {
    if name.ns == ns!(html) {
        Namespace::Html
    } else if name.ns == ns!(svg) {
        Namespace::Svg
    } else if name.ns == ns!(mathml) {
        Namespace::MathMl
    } else {
        panic!("the HTML parser made an element {name:?} in another namespace")
    }
}

fn attribute(attribute: Html5Attribute) -> Attribute {
    let local = &attribute.name.local;
    Attribute {
        key: match &attribute.name.prefix {
            Some(prefix) => format!("{prefix}:{local}"),
            None => local.to_string(),
        },
        value: attribute.value.to_string(),
    }
}

/// What is appended to a node: a node, or text.
enum Child<'t> {
    Node(NodeId),
    Text(&'t str),
}

impl<'t> From<&'t NodeOrText<Handle>> for Child<'t> {
    fn from(child: &'t NodeOrText<Handle>) -> Self {
        match child {
            NodeOrText::AppendNode(node) => Child::Node(node.id),
            NodeOrText::AppendText(text) => Child::Text(text),
        }
    }
}

/// Makes `child` the last child of `parent`, text merged into a text node that is the last
/// child already.
fn append(dom: &mut Dom, parent: NodeId, child: Child) {
    let last_text = dom
        .node(parent)
        .children
        .last()
        .copied()
        .filter(|&last| is_text(dom, last));
    match child {
        Child::Node(node) => match (last_text, &dom.node(node).data) {
            (Some(last), NodeData::Text(text)) => {
                let text = text.clone();
                dom.remove(node);
                push_text(dom, last, &text);
            }
            _ => dom.append(parent, node),
        },
        Child::Text(text) => match last_text {
            Some(last) => push_text(dom, last, text),
            None => {
                let node = dom.create(NodeData::Text(text.to_owned()));
                dom.append(parent, node);
            }
        },
    }
}

fn push_text(dom: &mut Dom, node: NodeId, more: &str) {
    match &mut dom.node_mut(node).data {
        NodeData::Text(text) => text.push_str(more),
        other => panic!("node {node} is not text: {other:?}"),
    }
}

fn is_text(dom: &Dom, node: NodeId) -> bool {
    matches!(dom.node(node).data, NodeData::Text(_))
}

fn is_html(dom: &Dom, node: NodeId, local: &str) -> bool {
    matches!(
        &dom.node(node).data,
        NodeData::Element { name, namespace: Namespace::Html, .. } if name == local
    )
}

fn ancestors(dom: &Dom, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(dom.node(node).parent, |&node| dom.node(node).parent)
}

/// The descendants of `node`, in tree order.
fn descendants(dom: &Dom, node: NodeId) -> Vec<NodeId> {
    let mut found = Vec::new();
    let mut pending: Vec<NodeId> = dom.node(node).children.iter().rev().copied().collect();
    while let Some(next) = pending.pop() {
        found.push(next);
        pending.extend(dom.node(next).children.iter().rev());
    }
    found
}

/// A copy of `node` and its descendants, without a parent.
fn deep_copy(dom: &mut Dom, node: NodeId) -> NodeId {
    let copy = dom.create(dom.node(node).data.clone());
    let mut pending = vec![(node, copy)];
    while let Some((original, copy)) = pending.pop() {
        for child in dom.node(original).children.clone() {
            let child_copy = dom.create(dom.node(child).data.clone());
            dom.append(copy, child_copy);
            pending.push((child, child_copy));
        }
    }
    copy
}

impl TreeSink for Sink {
    type Handle = Handle;
    type Output = Dom;
    type ElemName<'a> = ExpandedName<'a>;

    fn finish(self) -> Dom {
        self.dom.into_inner()
    }

    fn parse_error(&self, _message: Cow<'static, str>) {}

    fn get_document(&self) -> Handle {
        Handle {
            id: DOCUMENT,
            element: None,
        }
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> ExpandedName<'a> {
        match &target.element {
            Some(element) => element.name.expanded(),
            None => panic!("html5ever asked for the name of node {}", target.id),
        }
    }

    fn create_element(
        &self,
        name: QualName,
        attributes: Vec<Html5Attribute>,
        flags: ElementFlags,
    ) -> Handle {
        let mut handle = self.handle(NodeData::Element {
            name: name.local.to_string(),
            namespace: namespace(&name),
            attributes: attributes.into_iter().map(attribute).collect(),
        });
        handle.element = Some(Rc::new(Element {
            name,
            integration_point: flags.mathml_annotation_xml_integration_point,
        }));
        handle
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        self.handle(NodeData::Comment(text.to_string()))
    }

    fn create_pi(&self, target: StrTendril, _data: StrTendril) -> Handle {
        panic!("the HTML parser made a processing instruction {target}")
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        append(&mut self.dom.borrow_mut(), parent.id, Child::from(&child));
    }

    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        let has_parent = self.dom.borrow().node(element.id).parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let doctype = self.handle(NodeData::Doctype {
            name: name.to_string(),
            public_id: public_id.to_string(),
            system_id: system_id.to_string(),
        });
        self.dom.borrow_mut().append(DOCUMENT, doctype.id);
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        target.clone()
    }

    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        x.id == y.id
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &Handle, child: NodeOrText<Handle>) {
        let mut dom = self.dom.borrow_mut();
        let previous_text = dom
            .previous_sibling(sibling.id)
            .filter(|&previous| is_text(&dom, previous));
        let node = match child {
            NodeOrText::AppendText(text) => {
                if let Some(previous) = previous_text {
                    push_text(&mut dom, previous, &text);
                    return;
                }
                dom.create(NodeData::Text(text.to_string()))
            }
            NodeOrText::AppendNode(node) => node.id,
        };
        dom.insert_before(sibling.id, node);
    }

    fn add_attrs_if_missing(&self, target: &Handle, attributes: Vec<Html5Attribute>) {
        let mut dom = self.dom.borrow_mut();
        let NodeData::Element {
            attributes: existing,
            ..
        } = &mut dom.node_mut(target.id).data
        else {
            panic!(
                "html5ever added attributes to node {}, not an element",
                target.id
            );
        };
        for attribute in attributes.into_iter().map(attribute) {
            if !existing.iter().any(|present| present.key == attribute.key) {
                existing.push(attribute);
            }
        }
    }

    fn remove_from_parent(&self, target: &Handle) {
        self.dom.borrow_mut().remove(target.id);
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let mut dom = self.dom.borrow_mut();
        let children = std::mem::take(&mut dom.node_mut(node.id).children);
        for child in children {
            dom.node_mut(child).parent = None;
            append(&mut dom, new_parent.id, Child::Node(child));
        }
    }

    fn maybe_clone_an_option_into_selectedcontent(&self, option: &Handle) {
        let mut dom = self.dom.borrow_mut();
        let Some(select) = ancestors(&dom, option.id).find(|&node| is_html(&dom, node, "select"))
        else {
            return;
        };
        let descendants = descendants(&dom, select);
        let options: Vec<NodeId> = descendants
            .iter()
            .copied()
            .filter(|&node| is_html(&dom, node, "option"))
            .collect();
        let has_selected = |node: NodeId| {
            dom.attributes(node)
                .iter()
                .any(|attribute| attribute.key == "selected")
        };
        let selected = has_selected(option.id)
            || (!options.iter().any(|&node| has_selected(node))
                && options.first() == Some(&option.id));
        let Some(selectedcontent) = descendants
            .iter()
            .copied()
            .find(|&node| is_html(&dom, node, "selectedcontent"))
        else {
            return;
        };
        if !selected {
            return;
        }
        for child in std::mem::take(&mut dom.node_mut(selectedcontent).children) {
            dom.node_mut(child).parent = None;
        }
        for child in dom.node(option.id).children.clone() {
            let copy = deep_copy(&mut dom, child);
            dom.append(selectedcontent, copy);
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Handle) -> bool {
        handle
            .element
            .as_ref()
            .is_some_and(|element| element.integration_point)
    }
}

/// Replaces the descendants of each element at [`MAX_DEPTH`] with their text and comments, in
/// document order, adjacent texts merged.
fn flatten_beyond_max_depth(dom: &mut Dom) {
    let mut pending = vec![(DOCUMENT, 0)];
    while let Some((node, depth)) = pending.pop() {
        if depth == MAX_DEPTH {
            flatten(dom, node);
            continue;
        }
        pending.extend(
            dom.node(node)
                .children
                .iter()
                .map(|&child| (child, depth + 1)),
        );
    }
}

fn flatten(dom: &mut Dom, element: NodeId) {
    let mut pending: Vec<NodeId> = std::mem::take(&mut dom.node_mut(element).children);
    pending.reverse();
    while let Some(node) = pending.pop() {
        dom.node_mut(node).parent = None;
        if let NodeData::Element { .. } = dom.node(node).data {
            let children = std::mem::take(&mut dom.node_mut(node).children);
            pending.extend(children.into_iter().rev());
            continue;
        }
        append(dom, element, Child::Node(node));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(dom: &mut Dom) -> NodeId {
        dom.create(NodeData::Element {
            name: "div".to_owned(),
            namespace: Namespace::Html,
            attributes: Vec::new(),
        })
    }

    #[test]
    fn flattens_what_nests_deeper_than_the_limit_into_its_text() {
        let mut dom = Dom::default();
        let mut parent = DOCUMENT;
        for level in 1..=MAX_DEPTH + 3 {
            let child = element(&mut dom);
            dom.append(parent, child);
            let text = dom.create(NodeData::Text(level.to_string()));
            dom.append(child, text);
            parent = child;
        }
        let comment = dom.create(NodeData::Comment("c".to_owned()));
        dom.append(parent, comment);
        let tail = dom.create(NodeData::Text("!".to_owned()));
        dom.append(parent, tail);
        flatten_beyond_max_depth(&mut dom);
        let mut deepest = DOCUMENT;
        for _ in 0..MAX_DEPTH {
            deepest = *dom
                .node(deepest)
                .children
                .iter()
                .find(|&&child| matches!(dom.node(child).data, NodeData::Element { .. }))
                .unwrap();
        }
        let data: Vec<String> = dom
            .node(deepest)
            .children
            .iter()
            .map(|&child| format!("{:?}", dom.node(child).data))
            .collect();
        let deeper = format!("{MAX_DEPTH}{}{}", MAX_DEPTH + 1, MAX_DEPTH + 2);
        assert_eq!(
            data,
            [
                format!("Text({:?})", format!("{deeper}{}", MAX_DEPTH + 3)),
                "Comment(\"c\")".to_owned(),
                "Text(\"!\")".to_owned()
            ]
        );
        assert!(dom
            .node(deepest)
            .children
            .iter()
            .all(|&child| dom.node(child).parent == Some(deepest)));
    }
}
