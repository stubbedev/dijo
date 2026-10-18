//! The walk over a document in tree order, as `TikaNodeFilter` traverses jsoup's tree: each
//! node started, its children walked, and the node ended, text as characters. A document written
//! as it is built is walked as far as its tree is settled, a node let go of once it is ended:
//!
//! - a node is started once every node before it is ended, unless the [`Holds`] keep it back;
//! - a node is ended once it has no children left and the tree builder no longer holds it open,
//!   unless the holds keep it back;
//! - an open `select` keeps its children back, which the tree builder reads again as it copies
//!   an option into a `selectedcontent`.
//!
//! An element nested deeper than [`MAX_DEPTH`] is walked for its text and comments alone, as
//! though its descendants were its text and comments in document order, which bounds the depth
//! the walk reports.

use std::collections::HashMap;

use super::dom::{Attribute, Dom, NodeData, NodeId, Ordinal, DOCUMENT};
use super::tree_builder::MAX_DEPTH;

/// A node the walk reports.
pub enum ParsedNode<'d> {
    Document,
    /// An element, by its local name, with its attributes in their order.
    Element {
        name: &'d str,
        attributes: &'d [Attribute],
    },
    Comment,
    Doctype,
}

/// What a walk reports each node of a document to.
pub trait Visitor {
    /// Why the walk stops.
    type Stop;

    fn start(&mut self, node: &ParsedNode<'_>) -> Result<(), Self::Stop>;

    /// The text of a text node, or of part of one, never empty.
    fn text(&mut self, text: &str) -> Result<(), Self::Stop>;

    fn end(&mut self, node: &ParsedNode<'_>) -> Result<(), Self::Stop>;
}

/// What surveying a document as it is built finds that walking it as it is built must know
/// ahead: the times, counted in changes the tree builder makes, before which a node may not be
/// started or ended, since a change up to then puts nodes before it, moves it or its children,
/// or adds children to it after it could be ended; and the attributes a node ends with where the
/// tree builder adds attributes to it after it is made.
#[derive(Debug, Default)]
pub(super) struct Holds {
    pub(super) starts: HashMap<Ordinal, u64>,
    pub(super) ends: HashMap<Ordinal, u64>,
    pub(super) attributes: HashMap<Ordinal, Vec<Attribute>>,
}

/// What may be walked of a document: what its holds let be walked by the time `now`, or all that
/// is settled, where nothing holds it back.
#[derive(Clone, Copy)]
pub(super) enum Gate<'h> {
    Free,
    Held { holds: &'h Holds, now: u64 },
}

impl<'h> Gate<'h> {
    fn may_start(&self, ordinal: Ordinal) -> bool {
        match self {
            Gate::Free => true,
            Gate::Held { holds, now } => holds.starts.get(&ordinal).is_none_or(|t| now >= t),
        }
    }

    fn may_end(&self, ordinal: Ordinal) -> bool {
        match self {
            Gate::Free => true,
            Gate::Held { holds, now } => holds.ends.get(&ordinal).is_none_or(|t| now >= t),
        }
    }

    fn attributes<'d>(self, ordinal: Ordinal, own: &'d [Attribute]) -> &'d [Attribute]
    where
        'h: 'd,
    {
        match self {
            Gate::Free => own,
            Gate::Held { holds, .. } => holds.attributes.get(&ordinal).map_or(own, Vec::as_slice),
        }
    }
}

/// Whether the walk lets go of each node it has ended, as a walk of a document as it is built
/// does, or keeps the tree whole, to walk it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Release {
    Ended,
    Nothing,
}

/// A node the walk has started and not yet ended.
struct Step {
    node: NodeId,
    depth: usize,
    /// Whether its start was reported, as it is unless it is an element nested too deep.
    reported: bool,
    /// How many of its children the walk has ended and kept.
    kept: usize,
}

/// Where a walk stands in its document.
#[derive(Default)]
pub(super) struct Cursor {
    path: Vec<Step>,
    finished: bool,
}

impl Cursor {
    /// Walks on as far as the tree is settled, as `gate` lets it.
    pub(super) fn advance<V: Visitor>(
        &mut self,
        dom: &mut Dom,
        gate: Gate<'_>,
        release: Release,
        visitor: &mut V,
    ) -> Result<(), V::Stop> {
        if self.finished {
            return Ok(());
        }
        if self.path.is_empty() {
            visitor.start(&ParsedNode::Document)?;
            dom.node_mut(DOCUMENT).started = true;
            self.path.push(Step {
                node: DOCUMENT,
                depth: 0,
                reported: true,
                kept: 0,
            });
        }
        while let Some(step) = self.path.last() {
            let (node, depth, kept) = (step.node, step.depth, step.kept);
            let current = dom.node(node);
            let barrier = current.open && is_select(&current.data);
            if let Some(&child) = current.children.get(kept).filter(|_| !barrier) {
                let next = dom.node(child);
                if !gate.may_start(next.ordinal) {
                    return Ok(());
                }
                if let NodeData::Text(text) = &next.data {
                    if !text.is_empty() {
                        visitor.text(text)?;
                    }
                    self.ended(dom, child, release);
                    continue;
                }
                let reported = !matches!(next.data, NodeData::Element { .. }) || depth < MAX_DEPTH;
                if reported {
                    visitor.start(&parsed(dom, child, gate))?;
                }
                dom.node_mut(child).started = true;
                self.path.push(Step {
                    node: child,
                    depth: depth + 1,
                    reported,
                    kept: 0,
                });
                continue;
            }
            if current.open || !gate.may_end(current.ordinal) {
                return Ok(());
            }
            if step.reported {
                visitor.end(&parsed(dom, node, gate))?;
            }
            self.path.pop();
            if node == DOCUMENT {
                self.finished = true;
                return Ok(());
            }
            self.ended(dom, node, release);
        }
        Ok(())
    }

    /// Whether the walk has ended the document.
    pub(super) fn finished(&self) -> bool {
        self.finished
    }

    fn ended(&mut self, dom: &mut Dom, node: NodeId, release: Release) {
        match release {
            Release::Ended => dom.release(node),
            Release::Nothing => {
                self.path
                    .last_mut()
                    .expect("an ended node has a parent the walk started")
                    .kept += 1
            }
        }
    }
}

/// Walks all of a document built whole, keeping it, to walk it again.
pub(super) fn walk_whole<V: Visitor>(dom: &mut Dom, visitor: &mut V) -> Result<(), V::Stop> {
    let mut cursor = Cursor::default();
    cursor.advance(dom, Gate::Free, Release::Nothing, visitor)?;
    assert!(
        cursor.finished(),
        "a document built whole is walked to its end"
    );
    Ok(())
}

fn is_select(data: &NodeData) -> bool {
    matches!(
        data,
        NodeData::Element { name, namespace: super::dom::Namespace::Html, .. } if name == "select"
    )
}

/// `node` as the walk reports it, with the attributes it ends with.
fn parsed<'d>(dom: &'d Dom, node: NodeId, gate: Gate<'d>) -> ParsedNode<'d> {
    let current = dom.node(node);
    match &current.data {
        NodeData::Document => ParsedNode::Document,
        NodeData::Element {
            name, attributes, ..
        } => ParsedNode::Element {
            name,
            attributes: gate.attributes(current.ordinal, attributes),
        },
        NodeData::Comment(_) => ParsedNode::Comment,
        NodeData::Doctype { .. } => ParsedNode::Doctype,
        NodeData::Text(_) => panic!("node {node} is text, which the walk reports as characters"),
    }
}
