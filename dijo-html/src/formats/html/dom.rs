//! The document the WHATWG tree construction algorithm builds, held in an arena: each node
//! knows its parent and children, and moving a node detaches it from its old parent first.
//!
//! A document written as it is built lets go of each node once it is written, and its slot is
//! taken by a node made later: a node is known by its slot and by its ordinal, the order it was
//! made in, which no other node shares.

use std::collections::VecDeque;

pub(super) type NodeId = usize;

/// The order a node was made in, unique in its document.
pub(super) type Ordinal = u64;

/// The document node, the root of every tree.
pub(super) const DOCUMENT: NodeId = 0;

/// The ordinal of every node the tree builder never names, and of the document.
pub(super) const UNNAMED: Ordinal = 0;

/// The namespaces the HTML parser puts elements in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Namespace {
    Html,
    Svg,
    MathMl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    /// The attribute's name, with the prefix of an adjusted foreign attribute, as in
    /// `xlink:href`.
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub(super) enum NodeData {
    Document,
    Element {
        /// The local name: in lower case for an HTML element, adjusted for a foreign one, as in
        /// `foreignObject`.
        name: String,
        namespace: Namespace,
        attributes: Vec<Attribute>,
    },
    /// Text, never next to another text node it was made next to.
    Text(String),
    #[cfg_attr(not(test), expect(dead_code))]
    Comment(String),
    #[cfg_attr(not(test), expect(dead_code))]
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
    },
}

#[derive(Debug, Clone)]
pub(super) struct Node {
    pub(super) ordinal: Ordinal,
    pub(super) data: NodeData,
    pub(super) parent: Option<NodeId>,
    pub(super) children: VecDeque<NodeId>,
    /// Whether the node has had a place in the tree, so that placing it again moves it.
    pub(super) placed: bool,
    /// Whether the tree builder held the node open when it was last asked.
    pub(super) open: bool,
    /// Whether the node's start has been written.
    pub(super) started: bool,
    /// How many times the tree builder had been asked which nodes it holds open when the node
    /// was made.
    pub(super) epoch: u64,
}

pub(super) struct Dom {
    nodes: Vec<Option<Node>>,
    vacant: Vec<NodeId>,
    made: Ordinal,
}

impl Default for Dom {
    fn default() -> Self {
        Dom {
            nodes: vec![Some(Node {
                ordinal: UNNAMED,
                data: NodeData::Document,
                parent: None,
                children: VecDeque::new(),
                placed: true,
                open: false,
                started: false,
                epoch: 0,
            })],
            vacant: Vec::new(),
            made: 0,
        }
    }
}

impl Dom {
    /// Makes a node of `data` the tree builder names, without a place in the tree, in a slot of
    /// its own.
    pub(super) fn create(&mut self, data: NodeData, epoch: u64) -> NodeId {
        self.made += 1;
        self.insert(data, self.made, epoch)
    }

    /// Makes a node the tree builder never names, such as text, whose ordinal is
    /// [`UNNAMED`]: how many of them a build makes depends on what it has let go of.
    pub(super) fn create_unnamed(&mut self, data: NodeData) -> NodeId {
        self.insert(data, UNNAMED, 0)
    }

    fn insert(&mut self, data: NodeData, ordinal: Ordinal, epoch: u64) -> NodeId {
        let node = Node {
            ordinal,
            data,
            parent: None,
            children: VecDeque::new(),
            placed: false,
            open: false,
            started: false,
            epoch,
        };
        match self.vacant.pop() {
            Some(id) => {
                self.nodes[id] = Some(node);
                id
            }
            None => {
                self.nodes.push(Some(node));
                self.nodes.len() - 1
            }
        }
    }

    pub(super) fn node(&self, id: NodeId) -> &Node {
        self.nodes[id]
            .as_ref()
            .unwrap_or_else(|| panic!("node {id} was let go of"))
    }

    pub(super) fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.nodes[id]
            .as_mut()
            .unwrap_or_else(|| panic!("node {id} was let go of"))
    }

    /// The node made `ordinal`th in slot `id`, unless it has been let go of.
    pub(super) fn live(&self, id: NodeId, ordinal: Ordinal) -> Option<&Node> {
        self.nodes[id]
            .as_ref()
            .filter(|node| node.ordinal == ordinal)
    }

    /// [`Dom::live`], to change.
    pub(super) fn live_mut(&mut self, id: NodeId, ordinal: Ordinal) -> Option<&mut Node> {
        self.nodes[id]
            .as_mut()
            .filter(|node| node.ordinal == ordinal)
    }

    /// Lets go of `node`, which has no children left, detaching it from its parent.
    pub(super) fn release(&mut self, node: NodeId) {
        self.remove(node);
        let released = self.nodes[node]
            .take()
            .unwrap_or_else(|| panic!("node {node} was let go of twice"));
        assert!(
            released.children.is_empty(),
            "node {node} is let go of before its children"
        );
        self.vacant.push(node);
    }

    /// Detaches `node` from its parent, if it has one.
    pub(super) fn remove(&mut self, node: NodeId) {
        if let Some(parent) = self.node_mut(node).parent.take() {
            let index = self.index_in(parent, node);
            self.node_mut(parent).children.remove(index);
        }
    }

    fn index_in(&self, parent: NodeId, child: NodeId) -> usize {
        self.node(parent)
            .children
            .iter()
            .position(|&sibling| sibling == child)
            .unwrap_or_else(|| panic!("node {child} is missing from its parent's children"))
    }

    /// Makes `child` the last child of `parent`.
    pub(super) fn append(&mut self, parent: NodeId, child: NodeId) {
        self.remove(child);
        self.node_mut(parent).children.push_back(child);
        let child = self.node_mut(child);
        child.parent = Some(parent);
        child.placed = true;
    }

    /// Puts `node` right before `reference`.
    pub(super) fn insert_before(&mut self, reference: NodeId, node: NodeId) {
        self.remove(node);
        let parent = self
            .node(reference)
            .parent
            .unwrap_or_else(|| panic!("node {reference} has no parent to insert before it in"));
        let index = self.index_in(parent, reference);
        self.node_mut(parent).children.insert(index, node);
        let node = self.node_mut(node);
        node.parent = Some(parent);
        node.placed = true;
    }

    /// The sibling right before `node`.
    pub(super) fn previous_sibling(&self, node: NodeId) -> Option<NodeId> {
        let parent = self.node(node).parent?;
        let index = self.index_in(parent, node);
        index
            .checked_sub(1)
            .map(|previous| self.node(parent).children[previous])
    }

    pub(super) fn attributes(&self, node: NodeId) -> &[Attribute] {
        match &self.node(node).data {
            NodeData::Element { attributes, .. } => attributes,
            _ => &[],
        }
    }
}
