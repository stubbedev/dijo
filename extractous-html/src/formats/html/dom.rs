//! The document jsoup builds (`Node` and its kinds), held in an arena: each node knows its
//! parent and children, and moving a node detaches it from its old parent first, as jsoup's
//! `appendChild` and `before` do.

use super::tags::{Namespace, Tag, TagId, TagSet};

pub(super) type NodeId = usize;

/// The document node, the root of every tree.
pub(super) const DOCUMENT: NodeId = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Attribute {
    pub(super) key: String,
    /// `None` for a boolean attribute, which reads as "".
    pub(super) value: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum NodeData {
    /// The document, an element of tag `#root`.
    Document {
        tag: TagId,
    },
    Element {
        tag: TagId,
        attributes: Vec<Attribute>,
    },
    Text(String),
    /// The text of an element whose content is data, such as `script` or `style`.
    Data(String),
    CData(String),
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
    pub(super) nodes: Vec<Node>,
    pub(super) tags: TagSet,
    pub(super) quirks: bool,
}

impl Dom {
    pub(super) fn new(mut tags: TagSet) -> Self {
        let root = tags.value_of("#root", Some("#root"), Namespace::Html, false);
        Dom {
            nodes: vec![Node {
                data: NodeData::Document { tag: root },
                parent: None,
                children: Vec::new(),
            }],
            tags,
            quirks: false,
        }
    }

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

    fn detach(&mut self, child: NodeId) {
        if let Some(parent) = self.nodes[child].parent.take() {
            let siblings = &mut self.nodes[parent].children;
            let index = siblings
                .iter()
                .position(|&sibling| sibling == child)
                .unwrap_or_else(|| panic!("node {child} is missing from its parent's children"));
            siblings.remove(index);
        }
    }

    /// Makes `child` the last child of `parent` (`appendChild`).
    pub(super) fn append(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        self.nodes[parent].children.push(child);
        self.nodes[child].parent = Some(parent);
    }

    /// Puts `node` right before `reference` (`before`).
    pub(super) fn insert_before(&mut self, reference: NodeId, node: NodeId) {
        self.detach(node);
        let parent = self.nodes[reference]
            .parent
            .unwrap_or_else(|| panic!("node {reference} has no parent to insert before it in"));
        let index = self.nodes[parent]
            .children
            .iter()
            .position(|&sibling| sibling == reference)
            .unwrap_or_else(|| panic!("node {reference} is missing from its parent's children"));
        self.nodes[parent].children.insert(index, node);
        self.nodes[node].parent = Some(parent);
    }

    pub(super) fn remove(&mut self, node: NodeId) {
        self.detach(node);
    }

    pub(super) fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.nodes[node].parent
    }

    pub(super) fn children(&self, node: NodeId) -> Vec<NodeId> {
        self.nodes[node].children.clone()
    }

    /// The tag of an element or the document.
    pub(super) fn tag(&self, node: NodeId) -> &Tag {
        match &self.nodes[node].data {
            NodeData::Document { tag } | NodeData::Element { tag, .. } => self.tags.tag(*tag),
            other => panic!("node {node} is not an element: {other:?}"),
        }
    }

    pub(super) fn tag_id(&self, node: NodeId) -> TagId {
        match &self.nodes[node].data {
            NodeData::Document { tag } | NodeData::Element { tag, .. } => *tag,
            other => panic!("node {node} is not an element: {other:?}"),
        }
    }

    pub(super) fn normal_name(&self, node: NodeId) -> &str {
        &self.tag(node).normal
    }

    pub(super) fn namespace(&self, node: NodeId) -> Namespace {
        self.tag(node).namespace
    }

    /// Whether an element's normal name is `name`, in any namespace (`nameIs`).
    pub(super) fn name_is(&self, node: NodeId, name: &str) -> bool {
        self.normal_name(node) == name
    }

    /// Whether an element is `name` in the HTML namespace (`elementIs`).
    pub(super) fn is_html(&self, node: NodeId, name: &str) -> bool {
        let tag = self.tag(node);
        tag.normal == name && tag.namespace == Namespace::Html
    }

    pub(super) fn attributes(&self, node: NodeId) -> &[Attribute] {
        match &self.nodes[node].data {
            NodeData::Element { attributes, .. } => attributes,
            _ => &[],
        }
    }

    pub(super) fn attributes_mut(&mut self, node: NodeId) -> &mut Vec<Attribute> {
        match &mut self.nodes[node].data {
            NodeData::Element { attributes, .. } => attributes,
            other => panic!("node {node} has no attributes: {other:?}"),
        }
    }

    /// The value of an attribute, its key ignoring case, or "" (`attr`).
    pub(super) fn attr(&self, node: NodeId, key: &str) -> &str {
        let key: Vec<u16> = key.encode_utf16().collect();
        self.attributes(node)
            .iter()
            .find(|attribute| {
                super::reader::equals_ignore_case(
                    &attribute.key.encode_utf16().collect::<Vec<_>>(),
                    &key,
                )
            })
            .and_then(|attribute| attribute.value.as_deref())
            .unwrap_or("")
    }
}
