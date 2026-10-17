//! The document the WHATWG tree construction algorithm builds, held in an arena: each node
//! knows its parent and children, and moving a node detaches it from its old parent first.

pub(super) type NodeId = usize;

/// The document node, the root of every tree.
pub(super) const DOCUMENT: NodeId = 0;

/// The namespaces the HTML parser puts elements in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Namespace {
    Html,
    Svg,
    MathMl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Attribute {
    /// The attribute's name, with the prefix of an adjusted foreign attribute, as in
    /// `xlink:href`.
    pub(super) key: String,
    pub(super) value: String,
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
    /// Text, never next to another text node.
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
    pub(super) data: NodeData,
    pub(super) parent: Option<NodeId>,
    pub(super) children: Vec<NodeId>,
}

pub(super) struct Dom {
    nodes: Vec<Node>,
}

impl Default for Dom {
    fn default() -> Self {
        Dom {
            nodes: vec![Node {
                data: NodeData::Document,
                parent: None,
                children: Vec::new(),
            }],
        }
    }
}

impl Dom {
    pub(super) fn create(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node {
            data,
            parent: None,
            children: Vec::new(),
        });
        self.nodes.len() - 1
    }

    pub(super) fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub(super) fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id]
    }

    /// Detaches `node` from its parent, if it has one.
    pub(super) fn remove(&mut self, node: NodeId) {
        if let Some(parent) = self.nodes[node].parent.take() {
            let index = self.index_in(parent, node);
            self.nodes[parent].children.remove(index);
        }
    }

    fn index_in(&self, parent: NodeId, child: NodeId) -> usize {
        self.nodes[parent]
            .children
            .iter()
            .position(|&sibling| sibling == child)
            .unwrap_or_else(|| panic!("node {child} is missing from its parent's children"))
    }

    /// Makes `child` the last child of `parent`.
    pub(super) fn append(&mut self, parent: NodeId, child: NodeId) {
        self.remove(child);
        self.nodes[parent].children.push(child);
        self.nodes[child].parent = Some(parent);
    }

    /// Puts `node` right before `reference`.
    pub(super) fn insert_before(&mut self, reference: NodeId, node: NodeId) {
        self.remove(node);
        let parent = self.nodes[reference]
            .parent
            .unwrap_or_else(|| panic!("node {reference} has no parent to insert before it in"));
        let index = self.index_in(parent, reference);
        self.nodes[parent].children.insert(index, node);
        self.nodes[node].parent = Some(parent);
    }

    /// The sibling right before `node`.
    pub(super) fn previous_sibling(&self, node: NodeId) -> Option<NodeId> {
        let parent = self.nodes[node].parent?;
        let index = self.index_in(parent, node);
        index
            .checked_sub(1)
            .map(|previous| self.nodes[parent].children[previous])
    }

    pub(super) fn attributes(&self, node: NodeId) -> &[Attribute] {
        match &self.nodes[node].data {
            NodeData::Element { attributes, .. } => attributes,
            _ => &[],
        }
    }
}
