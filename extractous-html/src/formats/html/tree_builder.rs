//! The document html5ever builds from decoded text: the WHATWG tokeniser and tree construction
//! algorithm, as a user agent that runs no scripts applies them (so `noscript` holds markup),
//! into a [`Dom`].
//!
//! Two choices go beyond the algorithm, for a text extractor:
//! - a `template` element's contents are its children, as they are text of the document;
//! - past [`MAX_DEPTH`] open elements, each new element is closed at once ([`Guard`]).
//!
//! A document is built whole ([`build`]), or, to hold a bounded part of it however large it is,
//! built twice a piece at a time: a [`survey`] finds where the tree builder changes what is
//! already built, and the second build ([`stream`]) walks each part once nothing can change it
//! any more, and lets go of it.

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

use super::dom::{Attribute, Dom, Namespace, Node, NodeData, NodeId, Ordinal, DOCUMENT, UNNAMED};
use super::handler::reads_otherwise;
use super::walk::{Cursor, Gate, Holds, ParsedNode, Release, Visitor};

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

/// How many documents [`build`] and [`survey`] have built in this process, for the parity
/// harness, which compares the documents whose native extraction built one by the HTML parser's
/// [`DEPARTURES`] and runs one test per process.
///
/// [`DEPARTURES`]: super::DEPARTURES
#[cfg(all(feature = "tika", any(test, feature = "testing")))]
pub(super) static BUILT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn count_built() {
    #[cfg(all(feature = "tika", any(test, feature = "testing")))]
    BUILT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A document built whole, and whether an element of it holds a data URI the parser may read
/// otherwise than Tika ([`super::handler::reads_otherwise`]).
pub(super) struct Built {
    pub(super) dom: Dom,
    pub(super) reads_otherwise: bool,
}

/// Builds the document `html` holds, whole.
pub(super) fn build(html: &str) -> Built {
    count_built();
    let sink = drive(std::iter::once(html), Plan::Whole, &mut Unwalked)
        .unwrap_or_else(|stop| match stop {});
    Built {
        reads_otherwise: sink.reads_otherwise.get(),
        dom: sink.dom.into_inner(),
    }
}

/// [`build`]'s tree alone.
#[cfg(test)]
pub(super) fn parse(html: &str) -> Dom {
    build(html).dom
}

/// What [`survey`] finds of a document: what walking it as it is built must know ahead, and
/// whether an element of it holds a data URI the parser may read otherwise than Tika.
pub(super) struct Survey {
    pub(super) holds: Holds,
    pub(super) reads_otherwise: bool,
}

/// Builds the document of the text `pieces`, a piece at a time, letting go of what is settled as
/// it goes, to find what walking it as it is built must know ahead.
pub(super) fn survey(pieces: impl Iterator<Item = impl AsRef<str>>) -> Survey {
    count_built();
    let sink = drive(pieces, Plan::Survey, &mut Unwalked).unwrap_or_else(|stop| match stop {});
    Survey {
        holds: sink.holds.into_inner(),
        reads_otherwise: sink.reads_otherwise.get(),
    }
}

/// Builds the document of the text `pieces` again, a piece at a time, and walks it with
/// `visitor` as far as it is settled after each, as `holds`, its [`survey`]'s, let it.
pub(super) fn stream<V: Visitor>(
    pieces: impl Iterator<Item = impl AsRef<str>>,
    holds: &Holds,
    visitor: &mut V,
) -> Result<(), V::Stop> {
    drive(pieces, Plan::Held(holds), visitor).map(|_| ())
}

/// A visitor of nothing, for a build no walk reports.
struct Unwalked;

impl Visitor for Unwalked {
    type Stop = std::convert::Infallible;

    fn start(&mut self, _node: &ParsedNode<'_>) -> Result<(), Self::Stop> {
        Ok(())
    }

    fn text(&mut self, _text: &str) -> Result<(), Self::Stop> {
        Ok(())
    }

    fn end(&mut self, _node: &ParsedNode<'_>) -> Result<(), Self::Stop> {
        Ok(())
    }
}

/// How a document is built: whole; surveyed, settled nodes let go of as they are walked by
/// nothing; or walked as it is built, by its survey's holds.
#[derive(Clone, Copy)]
enum Plan<'h> {
    Whole,
    Survey,
    Held(&'h Holds),
}

/// Feeds the tree builder the `pieces`, and unless the plan builds the document whole, walks it
/// with `visitor` after each piece; the sink once the document is built and walked.
fn drive<'h, V: Visitor>(
    pieces: impl Iterator<Item = impl AsRef<str>>,
    plan: Plan<'h>,
    visitor: &mut V,
) -> Result<Sink<'h>, V::Stop> {
    let builder = TreeBuilder::new(
        Sink::new(plan),
        TreeBuilderOpts {
            scripting_enabled: false,
            ..TreeBuilderOpts::default()
        },
    );
    let tokenizer = Tokenizer::new(Guard { builder }, TokenizerOpts::default());
    let input = BufferQueue::default();
    let mut cursor = Cursor::default();
    for piece in pieces {
        input.push_back(StrTendril::from(piece.as_ref()));
        while !matches!(tokenizer.feed(&input), TokenizerResult::Done) {}
        if !matches!(plan, Plan::Whole) {
            tokenizer.sink.refresh();
            tokenizer.sink.builder.sink.walk(&mut cursor, visitor)?;
        }
    }
    tokenizer.end();
    let sink = &tokenizer.sink.builder.sink;
    sink.close();
    if !matches!(plan, Plan::Whole) {
        sink.walk(&mut cursor, visitor)?;
        assert!(cursor.finished(), "the walk ends with the document");
    }
    Ok(tokenizer.sink.builder.sink)
}

/// The tree builder behind a guard on the elements it holds open, whose every scope check scans
/// them, so that a document nesting deeper than [`MAX_DEPTH`] is built in linear time: past it,
/// an element opened by a start tag is closed by its end tag at once, and what follows is its
/// sibling.
struct Guard<'h> {
    builder: TreeBuilder<Handle, Sink<'h>>,
}

impl Guard<'_> {
    /// The handles the tree builder holds: the document, the open elements, the active
    /// formatting elements, at least as many as the open elements, and the `head` and `form`
    /// element pointers.
    fn held(&self) -> Vec<Handle> {
        let collector = Collector(RefCell::new(Vec::new()));
        self.builder.trace_handles(&collector);
        collector.0.into_inner()
    }

    /// How many handles the tree builder holds, as [`Guard::held`] lists them.
    fn count_held(&self) -> usize {
        let counter = Counter(Cell::new(0));
        self.builder.trace_handles(&counter);
        counter.0.get()
    }

    /// Marks open the nodes the tree builder holds, but by its `head` and `form` element
    /// pointers alone.
    fn refresh(&self) {
        self.builder.sink.refresh(&self.held());
    }
}

struct Counter(Cell<usize>);

impl Tracer for Counter {
    type Handle = Handle;

    fn trace_handle(&self, _node: &Handle) {
        self.0.set(self.0.get() + 1);
    }
}

struct Collector(RefCell<Vec<Handle>>);

impl Tracer for Collector {
    type Handle = Handle;

    fn trace_handle(&self, node: &Handle) {
        self.0.borrow_mut().push(node.clone());
    }
}

impl TokenSink for Guard<'_> {
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
        if !matches!(result, TokenSinkResult::Continue) || self.count_held() <= MAX_DEPTH {
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

/// A node as html5ever holds it: its slot and ordinal, and for an element its name, which
/// html5ever borrows.
#[derive(Debug, Clone)]
struct Handle {
    id: NodeId,
    ordinal: Ordinal,
    element: Option<Rc<Element>>,
}

#[derive(Debug)]
struct Element {
    name: QualName,
    /// Whether the element is a MathML `annotation-xml` that is an HTML integration point.
    integration_point: bool,
}

struct Sink<'h> {
    dom: RefCell<Dom>,
    plan: Plan<'h>,
    /// What a survey finds.
    holds: RefCell<Holds>,
    /// How many changes the tree builder has made.
    time: Cell<u64>,
    /// How many times the nodes held open have been marked.
    epoch: Cell<u64>,
    /// The `head` element, which the tree builder keeps a pointer to.
    head: Cell<Option<(NodeId, Ordinal)>>,
    /// The nodes last marked open.
    opened: RefCell<Vec<(NodeId, Ordinal)>>,
    reads_otherwise: Cell<bool>,
}

impl<'h> Sink<'h> {
    fn new(plan: Plan<'h>) -> Self {
        Self {
            dom: RefCell::default(),
            plan,
            holds: RefCell::default(),
            time: Cell::new(0),
            epoch: Cell::new(0),
            head: Cell::new(None),
            opened: RefCell::default(),
            reads_otherwise: Cell::new(false),
        }
    }

    /// Counts a change, and its time.
    fn changed(&self) -> u64 {
        let time = self.time.get() + 1;
        self.time.set(time);
        time
    }

    fn handle(&self, data: NodeData) -> Handle {
        self.changed();
        let mut dom = self.dom.borrow_mut();
        let id = dom.create(data, self.epoch.get());
        Handle {
            id,
            ordinal: dom.node(id).ordinal,
            element: None,
        }
    }

    /// Marks open the nodes of `held`, the handles the tree builder holds after the document,
    /// but those it holds as its `head` and `form` element pointers alone, traced last.
    fn refresh(&self, held: &[Handle]) {
        let mut dom = self.dom.borrow_mut();
        let mut opened = self.opened.borrow_mut();
        for (id, ordinal) in opened.drain(..) {
            if let Some(node) = dom.live_mut(id, ordinal) {
                node.open = false;
            }
        }
        let is_head = |handle: &Handle| self.head.get() == Some((handle.id, handle.ordinal));
        let pointers = match held {
            _ if self.head.get().is_none() => 0,
            [.., last] if is_head(last) => 1,
            [.., head, _] if is_head(head) => 2,
            _ => panic!("the tree builder holds no head element pointer"),
        };
        for handle in &held[1..held.len() - pointers] {
            if let Some(node) = dom.live_mut(handle.id, handle.ordinal) {
                node.open = true;
                opened.push((handle.id, handle.ordinal));
            }
        }
        dom.node_mut(DOCUMENT).open = true;
        self.epoch.set(self.epoch.get() + 1);
    }

    /// Marks every node closed, once the document is built.
    fn close(&self) {
        let mut dom = self.dom.borrow_mut();
        for (id, ordinal) in self.opened.borrow_mut().drain(..) {
            if let Some(node) = dom.live_mut(id, ordinal) {
                node.open = false;
            }
        }
        dom.node_mut(DOCUMENT).open = false;
    }

    /// Walks the document as far as it is settled.
    fn walk<V: Visitor>(&self, cursor: &mut Cursor, visitor: &mut V) -> Result<(), V::Stop> {
        let mut dom = self.dom.borrow_mut();
        match self.plan {
            Plan::Whole => cursor.advance(&mut dom, Gate::Free, Release::Nothing, visitor),
            Plan::Survey => cursor.advance(&mut dom, Gate::Free, Release::Ended, visitor),
            Plan::Held(holds) => {
                let gate = Gate::Held {
                    holds,
                    now: self.time.get(),
                };
                cursor.advance(&mut dom, gate, Release::Ended, visitor)
            }
        }
    }

    /// The node `handle` names, which the tree builder still holds.
    fn held_node<'d>(&self, dom: &'d Dom, handle: &Handle) -> &'d Node {
        dom.live(handle.id, handle.ordinal).unwrap_or_else(|| {
            panic!(
                "the tree builder holds node {}, which was let go of",
                handle.ordinal
            )
        })
    }

    /// Notes that the change at `time` puts nodes before `handle`, moves it or moves its
    /// children: it may not be started before then.
    fn unsettles_start(&self, dom: &Dom, handle: &Handle, time: u64) {
        match self.plan {
            Plan::Whole => {}
            Plan::Survey => {
                self.holds.borrow_mut().starts.insert(handle.ordinal, time);
            }
            Plan::Held(_) => assert!(
                dom.live(handle.id, handle.ordinal)
                    .is_some_and(|node| !node.started),
                "node {} was started before the change at {time} that the survey of its \
                 document holds it back for",
                handle.ordinal
            ),
        }
    }

    /// Notes that the change at `time` adds children to `handle`: it may not be ended before
    /// then, where it could have been ended already.
    fn unsettles_end(&self, dom: &Dom, handle: &Handle, time: u64) {
        match self.plan {
            Plan::Whole => {}
            Plan::Survey => {
                let settled = dom
                    .live(handle.id, handle.ordinal)
                    .is_none_or(|node| node.epoch < self.epoch.get() && !node.open);
                if settled {
                    self.holds.borrow_mut().ends.insert(handle.ordinal, time);
                }
            }
            Plan::Held(_) => assert!(
                dom.live(handle.id, handle.ordinal).is_some(),
                "node {} was ended before the change at {time} that the survey of its document \
                 holds it back for",
                handle.ordinal
            ),
        }
    }

    /// Notes that `child` is put in the tree at `time`, which moves it if it had a place.
    fn places(&self, dom: &Dom, child: &NodeOrText<Handle>, time: u64) {
        if let NodeOrText::AppendNode(node) = child {
            if dom
                .live(node.id, node.ordinal)
                .is_none_or(|node| node.placed)
            {
                self.unsettles_start(dom, node, time);
            }
        }
    }

    /// Notes that `attributes` may hold a data URI the parser reads otherwise than Tika.
    fn note_attributes(&self, attributes: &[Attribute]) {
        if reads_otherwise(attributes) {
            self.reads_otherwise.set(true);
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
/// child already, and the text node it came in let go of.
fn append(dom: &mut Dom, parent: NodeId, child: Child) {
    let last_text = dom
        .node(parent)
        .children
        .back()
        .copied()
        .filter(|&last| is_text(dom, last));
    match child {
        Child::Node(node) => match (last_text, &dom.node(node).data) {
            (Some(last), NodeData::Text(text)) => {
                let text = text.clone();
                dom.release(node);
                push_text(dom, last, &text);
            }
            _ => dom.append(parent, node),
        },
        Child::Text(text) => match last_text {
            Some(last) => push_text(dom, last, text),
            None => {
                let node = dom.create_unnamed(NodeData::Text(text.to_owned()));
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
    let copy = dom.create_unnamed(dom.node(node).data.clone());
    let mut pending = vec![(node, copy)];
    while let Some((original, copy)) = pending.pop() {
        for child in dom.node(original).children.clone() {
            let child_copy = dom.create_unnamed(dom.node(child).data.clone());
            dom.append(copy, child_copy);
            pending.push((child, child_copy));
        }
    }
    copy
}

impl TreeSink for Sink<'_> {
    type Handle = Handle;
    type Output = Self;
    type ElemName<'a>
        = ExpandedName<'a>
    where
        Self: 'a;

    fn finish(self) -> Self {
        self
    }

    fn parse_error(&self, _message: Cow<'static, str>) {}

    fn get_document(&self) -> Handle {
        Handle {
            id: DOCUMENT,
            ordinal: UNNAMED,
            element: None,
        }
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> ExpandedName<'a> {
        match &target.element {
            Some(element) => element.name.expanded(),
            None => panic!("html5ever asked for the name of node {}", target.ordinal),
        }
    }

    fn create_element(
        &self,
        name: QualName,
        attributes: Vec<Html5Attribute>,
        flags: ElementFlags,
    ) -> Handle {
        let attributes: Vec<Attribute> = attributes.into_iter().map(attribute).collect();
        self.note_attributes(&attributes);
        let namespace = namespace(&name);
        let is_head = namespace == Namespace::Html && &*name.local == "head";
        let mut handle = self.handle(NodeData::Element {
            name: name.local.to_string(),
            namespace,
            attributes,
        });
        if is_head && self.head.get().is_none() {
            self.head.set(Some((handle.id, handle.ordinal)));
        }
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
        let time = self.changed();
        let mut dom = self.dom.borrow_mut();
        self.unsettles_end(&dom, parent, time);
        self.places(&dom, &child, time);
        let live = |handle: &Handle| dom.live(handle.id, handle.ordinal).is_some();
        let moved = match &child {
            NodeOrText::AppendNode(node) => Some(live(node)),
            NodeOrText::AppendText(_) => None,
        };
        if !live(parent) {
            if let (NodeOrText::AppendNode(node), Some(true)) = (&child, moved) {
                dom.remove(node.id);
            }
            return;
        }
        if moved == Some(false) {
            return;
        }
        append(&mut dom, parent.id, Child::from(&child));
    }

    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        let has_parent = self.held_node(&self.dom.borrow(), element).parent.is_some();
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
        self.append(&self.get_document(), NodeOrText::AppendNode(doctype));
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        target.clone()
    }

    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        x.ordinal == y.ordinal
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &Handle, child: NodeOrText<Handle>) {
        let time = self.changed();
        let mut dom = self.dom.borrow_mut();
        self.unsettles_start(&dom, sibling, time);
        self.places(&dom, &child, time);
        if dom.live(sibling.id, sibling.ordinal).is_none() {
            if let NodeOrText::AppendNode(node) = &child {
                if dom.live(node.id, node.ordinal).is_some() {
                    dom.remove(node.id);
                }
            }
            return;
        }
        let previous_text = dom
            .previous_sibling(sibling.id)
            .filter(|&previous| is_text(&dom, previous));
        let node = match child {
            NodeOrText::AppendText(text) => {
                if let Some(previous) = previous_text {
                    push_text(&mut dom, previous, &text);
                    return;
                }
                dom.create_unnamed(NodeData::Text(text.to_string()))
            }
            NodeOrText::AppendNode(node) => {
                if dom.live(node.id, node.ordinal).is_none() {
                    return;
                }
                node.id
            }
        };
        dom.insert_before(sibling.id, node);
    }

    fn add_attrs_if_missing(&self, target: &Handle, attributes: Vec<Html5Attribute>) {
        self.changed();
        let mut dom = self.dom.borrow_mut();
        self.held_node(&dom, target);
        let node = dom
            .live_mut(target.id, target.ordinal)
            .expect("the tree builder holds the node");
        let NodeData::Element {
            attributes: existing,
            ..
        } = &mut node.data
        else {
            panic!(
                "html5ever added attributes to node {}, not an element",
                target.ordinal
            );
        };
        let before = existing.len();
        for attribute in attributes.into_iter().map(attribute) {
            if !existing.iter().any(|present| present.key == attribute.key) {
                existing.push(attribute);
            }
        }
        if existing.len() == before {
            return;
        }
        let all = existing.clone();
        self.note_attributes(&all);
        if matches!(self.plan, Plan::Survey) {
            self.holds
                .borrow_mut()
                .attributes
                .insert(target.ordinal, all);
        }
    }

    fn remove_from_parent(&self, target: &Handle) {
        let time = self.changed();
        let mut dom = self.dom.borrow_mut();
        self.unsettles_start(&dom, target, time);
        if dom.live(target.id, target.ordinal).is_some() {
            dom.remove(target.id);
        }
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let time = self.changed();
        let mut dom = self.dom.borrow_mut();
        self.unsettles_start(&dom, node, time);
        if dom.live(node.id, node.ordinal).is_none() {
            return;
        }
        self.held_node(&dom, new_parent);
        let children = std::mem::take(&mut dom.node_mut(node.id).children);
        for child in children {
            dom.node_mut(child).parent = None;
            append(&mut dom, new_parent.id, Child::Node(child));
        }
    }

    fn maybe_clone_an_option_into_selectedcontent(&self, option: &Handle) {
        let time = self.changed();
        let mut dom = self.dom.borrow_mut();
        self.held_node(&dom, option);
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
        let handle = Handle {
            id: selectedcontent,
            ordinal: dom.node(selectedcontent).ordinal,
            element: None,
        };
        self.unsettles_start(&dom, &handle, time);
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

#[cfg(test)]
mod tests {
    use super::super::walk::ParsedNode;
    use super::*;

    /// The walk as a string: each element as `<name>` and `</name>`, a comment as `<!>`, text
    /// as itself.
    #[derive(Default)]
    struct Log(String);

    impl Visitor for Log {
        type Stop = std::convert::Infallible;

        fn start(&mut self, node: &ParsedNode<'_>) -> Result<(), Self::Stop> {
            match node {
                ParsedNode::Element { name, .. } => self.0.push_str(&format!("<{name}>")),
                ParsedNode::Comment => self.0.push_str("<!>"),
                ParsedNode::Document | ParsedNode::Doctype => {}
            }
            Ok(())
        }

        fn text(&mut self, text: &str) -> Result<(), Self::Stop> {
            self.0.push_str(text);
            Ok(())
        }

        fn end(&mut self, node: &ParsedNode<'_>) -> Result<(), Self::Stop> {
            if let ParsedNode::Element { name, .. } = node {
                self.0.push_str(&format!("</{name}>"));
            }
            Ok(())
        }
    }

    fn element(dom: &mut Dom) -> NodeId {
        dom.create(
            NodeData::Element {
                name: "div".to_owned(),
                namespace: Namespace::Html,
                attributes: Vec::new(),
            },
            0,
        )
    }

    #[test]
    fn walks_what_nests_deeper_than_the_limit_as_its_text() {
        let mut dom = Dom::default();
        let mut parent = DOCUMENT;
        for level in 1..=MAX_DEPTH + 3 {
            let child = element(&mut dom);
            dom.append(parent, child);
            let text = dom.create(NodeData::Text(level.to_string()), 0);
            dom.append(child, text);
            parent = child;
        }
        let comment = dom.create(NodeData::Comment("c".to_owned()), 0);
        dom.append(parent, comment);
        let tail = dom.create(NodeData::Text("!".to_owned()), 0);
        dom.append(parent, tail);
        let mut log = Log::default();
        let walked = Cursor::default().advance(&mut dom, Gate::Free, Release::Nothing, &mut log);
        walked.unwrap_or_else(|stop| match stop {});
        let deepest = format!("<div>{}", MAX_DEPTH);
        let at = log
            .0
            .rfind(&deepest)
            .expect("the deepest element is walked");
        assert_eq!(
            &log.0[at..],
            format!(
                "{deepest}{}{}{}<!>!{}",
                MAX_DEPTH + 1,
                MAX_DEPTH + 2,
                MAX_DEPTH + 3,
                "</div>".repeat(MAX_DEPTH)
            )
        );
        assert_eq!(log.0.matches("<div>").count(), MAX_DEPTH);
    }

    /// A document walked as it is built, a character at a time, is walked as it is once built
    /// whole: misnested formatting, text fostered out of tables, attributes a later `html` or
    /// `body` tag adds, a `selectedcontent`, and a `head` written to after it ends.
    #[test]
    fn walks_a_document_as_it_is_built_as_once_it_is_built() {
        for html in [
            "<p>plain <b>bold</b> text</p>",
            "<a href=x><p>one<a href=y>two</a>three</p></a>after",
            "<b>1<p>2</b>3</p><i><div>4<i>5</div>6</i>",
            "<table>foster<tr><td>x</td></tr>after<a><div>y</a>z</table>tail",
            "<p>x</p><html lang=da><body data-x=y>more<html class=c>",
            "<select><button><selectedcontent></selectedcontent></button><option>X</option>\
             <option selected>Y<b>!</b></option><option>Z</option></select>end",
            "<head><title>t</title></head><meta name=a content=b><link href=c>body",
            "<table><tr><td><table><tr><td>in</td></tr>out</table></td></tr>more</table>",
            "<frameset><frame src=x></frameset>",
            "<body>text<frameset><frame></frameset>",
            "<svg><desc><b>x</svg>y</b>z",
            "<!DOCTYPE html><!--c--><html><body><p>a<!--d-->b",
        ] {
            let whole = {
                let mut dom = parse(html);
                let mut log = Log::default();
                Cursor::default()
                    .advance(&mut dom, Gate::Free, Release::Nothing, &mut log)
                    .unwrap_or_else(|stop| match stop {});
                log.0
            };
            let pieces: Vec<String> = html.chars().map(String::from).collect();
            let survey = survey(pieces.iter().map(String::as_str));
            let mut log = Log::default();
            stream(pieces.iter().map(String::as_str), &survey.holds, &mut log)
                .unwrap_or_else(|stop| match stop {});
            assert_eq!(log.0, whole, "{html}");
        }
    }
}
