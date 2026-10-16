//! The structure tree of a tagged document, as Tika's `StructureIndex` reads it: its elements,
//! the marked content each element owns, by content stream and identifier, and what marks
//! each glyph's place in it.

use std::cell::OnceCell;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use lopdf::{Dictionary, Object};

use super::cos::{as_dict, int_value, name_string, Identity, Pdf};

const MAX_NODES: usize = 500_000;
const MAX_LEAVES: usize = 1_000_000;
const MAX_DEPTH: usize = 4096;
const MAX_ROLE_HOPS: usize = 16;

/// What a glyph was drawn under (`PDFMarkedContent2XHTML.Label`): a marked-content identifier
/// in a content stream, nothing, or an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Label {
    Untagged,
    Artifact,
    Tagged { scope: Identity, mcid: i32 },
}

/// The standard structure types PDFBox lists, and those of PDF 2.0 Tika adds.
const STANDARD_TYPES: [&str; 57] = [
    "Document",
    "Part",
    "Art",
    "Sect",
    "Div",
    "BlockQuote",
    "Caption",
    "TOC",
    "TOCI",
    "Index",
    "NonStruct",
    "Private",
    "P",
    "H",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "L",
    "LI",
    "Lbl",
    "LBody",
    "Table",
    "TR",
    "TH",
    "TD",
    "THead",
    "TBody",
    "TFoot",
    "Span",
    "Quote",
    "Note",
    "Reference",
    "BibEntry",
    "Code",
    "Link",
    "Annot",
    "Ruby",
    "RB",
    "RT",
    "RP",
    "Warichu",
    "WT",
    "WP",
    "Formula",
    "Form",
    "Figure",
    "Title",
    "FENote",
    "Sub",
    "Em",
    "Strong",
    "Aside",
    "DocumentFragment",
    "Artifact",
];

pub(super) fn is_standard_type(structure_type: &str) -> bool {
    STANDARD_TYPES.contains(&structure_type)
}

/// A structure element.
#[derive(Debug)]
pub(super) struct Node<'p, S> {
    pub(super) parent: Option<usize>,
    pub(super) structure_type: String,
    pub(super) dict: &'p Dictionary,
    /// The first object an `OBJR` child of the element references.
    pub(super) objr: Option<&'p Dictionary>,
    /// How the element is written, once Tika's writer has worked it out.
    pub(super) spec: OnceCell<S>,
}

/// A marked-content sequence or an object the tree owns, with the element owning it and its
/// order in the tree.
#[derive(Debug, Clone, Copy)]
pub(super) struct Leaf {
    pub(super) node: usize,
    pub(super) order: usize,
}

/// `StructureIndex`: the tree's elements, and its leaves by scope.
#[derive(Debug)]
pub(super) struct StructureIndex<'p, S> {
    pub(super) nodes: Vec<Node<'p, S>>,
    leaves: HashMap<Identity, HashMap<i32, Leaf>>,
    object_leaves: HashMap<Identity, Vec<Leaf>>,
    role_map: HashMap<String, String>,
    leaf_count: usize,
    /// Why the tree cannot be used, if it cannot.
    pub(super) reason: Option<&'static str>,
}

/// A frame of the walk: an array being walked, or one kid.
enum Frame<'p> {
    Array {
        items: &'p [Object],
        next: usize,
        parent: Option<usize>,
        depth: usize,
        page: Option<Identity>,
    },
    Kid {
        kid: &'p Object,
        parent: Option<usize>,
        depth: usize,
        page: Option<Identity>,
    },
}

impl<'p, S> StructureIndex<'p, S> {
    fn empty(reason: Option<&'static str>) -> Self {
        Self {
            nodes: Vec::new(),
            leaves: HashMap::new(),
            object_leaves: HashMap::new(),
            role_map: HashMap::new(),
            leaf_count: 0,
            reason,
        }
    }

    /// `StructureIndex.load`.
    pub(super) fn load(pdf: &'p Pdf) -> Self {
        let Some(root) = pdf
            .catalog()
            .and_then(|catalog| pdf.dict(catalog, b"StructTreeRoot"))
        else {
            return Self::empty(Some("no-structure-tree"));
        };
        let Ok(k) = root.get(b"K") else {
            return Self::empty(Some("empty-structure-tree"));
        };
        let mut index = Self::empty(None);
        if let Some(role_map) = pdf.dict(root, b"RoleMap") {
            index.role_map = role_map_of(pdf, role_map);
        }
        index.walk(pdf, k);
        index
    }

    /// `hasMarkedContent`: the structure tree root has kids.
    pub(super) fn has_marked_content(pdf: &Pdf) -> bool {
        let Some(root) = pdf
            .catalog()
            .and_then(|catalog| pdf.dict(catalog, b"StructTreeRoot"))
        else {
            return false;
        };
        match pdf.get(root, b"K") {
            Some(Object::Array(items)) => !items.is_empty(),
            Some(other) => as_dict(other).is_some_and(|dict| !dict.is_empty()),
            None => false,
        }
    }

    pub(super) fn leaves(&self, scope: Identity) -> Option<&HashMap<i32, Leaf>> {
        self.leaves.get(&scope)
    }

    pub(super) fn leaf(&self, scope: Identity, mcid: i32) -> Option<Leaf> {
        self.leaves.get(&scope)?.get(&mcid).copied()
    }

    pub(super) fn object_leaves(&self, page: Identity) -> &[Leaf] {
        self.object_leaves.get(&page).map_or(&[], Vec::as_slice)
    }

    fn fail(&mut self, why: &'static str) {
        self.reason = Some(why);
        self.leaves.clear();
        self.object_leaves.clear();
    }

    fn push_kids(
        stack: &mut Vec<Frame<'p>>,
        kids: &'p Object,
        parent: Option<usize>,
        depth: usize,
        page: Option<Identity>,
    ) {
        match kids {
            Object::Array(items) => stack.push(Frame::Array {
                items,
                next: 0,
                parent,
                depth,
                page,
            }),
            kid => stack.push(Frame::Kid {
                kid,
                parent,
                depth,
                page,
            }),
        }
    }

    /// `StructureIndex.walk`: the tree in document order, depth first.
    fn walk(&mut self, pdf: &'p Pdf, root_kids: &'p Object) {
        let mut visited: HashSet<Identity> = HashSet::new();
        let mut stack: Vec<Frame<'p>> = Vec::new();
        Self::push_kids(&mut stack, root_kids, None, 0, None);
        while let Some(frame) = stack.pop() {
            let (kid, parent, depth, page) = match frame {
                Frame::Array {
                    items,
                    next,
                    parent,
                    depth,
                    page,
                } => {
                    if next < items.len() {
                        stack.push(Frame::Array {
                            items,
                            next: next + 1,
                            parent,
                            depth,
                            page,
                        });
                        stack.push(Frame::Kid {
                            kid: &items[next],
                            parent,
                            depth,
                            page,
                        });
                    }
                    continue;
                }
                Frame::Kid {
                    kid,
                    parent,
                    depth,
                    page,
                } => (kid, parent, depth, page),
            };
            let Some((identity, kid)) = pdf.resolve_identified(kid) else {
                continue;
            };
            match kid {
                Object::Integer(mcid) => {
                    if let (Some(parent), Some(page)) = (parent, page) {
                        self.add_leaf(parent, page, *mcid as i32);
                    }
                }
                Object::Array(_) => Self::push_kids(&mut stack, kid, parent, depth, page),
                other => {
                    let Some(dict) = as_dict(other) else {
                        continue;
                    };
                    let kind = pdf.name_as_string(dict, b"Type");
                    match kind.as_deref() {
                        None | Some("StructElem") => {
                            if !visited.insert(identity) {
                                continue;
                            }
                            if depth > MAX_DEPTH {
                                self.fail("structure-tree-depth");
                                return;
                            }
                            if self.nodes.len() + 1 > MAX_NODES {
                                self.fail("structure-tree-nodes");
                                return;
                            }
                            let own_page = pdf
                                .get_identified(dict, b"Pg")
                                .filter(|(_, object)| as_dict(object).is_some())
                                .map(|(identity, _)| identity);
                            let page = own_page.or(page);
                            let raw_type = pdf.name_as_string(dict, b"S");
                            let structure_type = self.resolve_type(raw_type.as_deref());
                            let node = self.nodes.len();
                            self.nodes.push(Node {
                                parent,
                                structure_type,
                                dict,
                                objr: None,
                                spec: OnceCell::new(),
                            });
                            if let Ok(kids) = dict.get(b"K") {
                                Self::push_kids(&mut stack, kids, Some(node), depth + 1, page);
                            }
                        }
                        Some("MCR") => {
                            let Some(parent) = parent else {
                                continue;
                            };
                            let stream = pdf.get_identified(dict, b"Stm").map(|(id, _)| id);
                            let own_page = pdf
                                .get_identified(dict, b"Pg")
                                .filter(|(_, object)| as_dict(object).is_some())
                                .map(|(identity, _)| identity);
                            let scope = stream.or(own_page).or(page);
                            let mcid = pdf.int(dict, b"MCID", -1);
                            if let (Some(scope), true) = (scope, mcid >= 0) {
                                self.add_leaf(parent, scope, mcid);
                            }
                        }
                        Some("OBJR") => {
                            let Some(parent) = parent else {
                                continue;
                            };
                            if self.nodes[parent].objr.is_none() {
                                self.nodes[parent].objr = pdf.dict(dict, b"Obj");
                            }
                            let own_page = pdf
                                .get_identified(dict, b"Pg")
                                .filter(|(_, object)| as_dict(object).is_some())
                                .map(|(identity, _)| identity);
                            let structure_type = &self.nodes[parent].structure_type;
                            if let (Some(page), true) = (
                                own_page.or(page),
                                matches!(structure_type.as_str(), "Figure" | "Formula"),
                            ) {
                                self.add_object_leaf(parent, page);
                            }
                        }
                        _ => {}
                    }
                }
            }
            if self.reason.is_some() {
                return;
            }
        }
    }

    fn add_leaf(&mut self, node: usize, scope: Identity, mcid: i32) {
        if self.leaf_count >= MAX_LEAVES {
            self.fail("structure-tree-leaves");
            return;
        }
        if let Entry::Vacant(vacant) = self.leaves.entry(scope).or_default().entry(mcid) {
            self.leaf_count += 1;
            vacant.insert(Leaf {
                node,
                order: self.leaf_count,
            });
        }
    }

    fn add_object_leaf(&mut self, node: usize, page: Identity) {
        if self.leaf_count >= MAX_LEAVES {
            self.fail("structure-tree-leaves");
            return;
        }
        self.leaf_count += 1;
        self.object_leaves.entry(page).or_default().push(Leaf {
            node,
            order: self.leaf_count,
        });
    }

    /// `resolveType`: a type through the role map, up to a standard type.
    fn resolve_type(&self, raw: Option<&str>) -> String {
        let Some(raw) = raw else {
            return String::new();
        };
        let mut current = raw.to_owned();
        for _ in 0..MAX_ROLE_HOPS {
            if is_standard_type(&current) {
                break;
            }
            match self.role_map.get(&current) {
                Some(mapped) if *mapped != current => current = mapped.clone(),
                _ => break,
            }
        }
        current
    }

    /// The number of ancestors of a node of the given type.
    pub(super) fn count_ancestors(&self, node: usize, structure_type: &str) -> usize {
        let mut count = 0;
        let mut current = self.nodes[node].parent;
        while let Some(parent) = current {
            if self.nodes[parent].structure_type == structure_type {
                count += 1;
            }
            current = self.nodes[parent].parent;
        }
        count
    }
}

/// `PDStructureTreeRoot.getRoleMap`: the names the role map maps to names or strings. PDFBox
/// converts every value and drops the whole map when one is not a basic type.
fn role_map_of(pdf: &Pdf, role_map: &Dictionary) -> HashMap<String, String> {
    let mut mapped = HashMap::new();
    for (key, value) in role_map.iter() {
        let target = match pdf.resolve(value) {
            Some(Object::Name(name)) => name_string(name),
            Some(Object::String(bytes, _)) => super::cos::text_string(bytes),
            Some(Object::Integer(_) | Object::Real(_) | Object::Boolean(_)) => continue,
            _ => return HashMap::new(),
        };
        mapped.insert(name_string(key), target);
    }
    mapped
}

/// `StructureIndex.linkUri`: the URI of the object a link element references, if it is a URI
/// action.
pub(super) fn link_uri(pdf: &Pdf, objr: Option<&Dictionary>) -> Option<String> {
    let action = pdf.dict(objr?, b"A")?;
    if pdf.name(action, b"S") != Some(b"URI") {
        return None;
    }
    pdf.text(action, b"URI")
        .filter(|uri| !crate::formats::handlers::is_blank(uri))
}

/// `PDStructureElement.getAttributes`: the element's attribute objects, by owner.
pub(super) fn attributes<'p>(pdf: &'p Pdf, element: &'p Dictionary) -> Vec<&'p Dictionary> {
    match pdf.get(element, b"A") {
        Some(Object::Array(items)) => items
            .iter()
            .filter_map(|item| pdf.resolve(item))
            .filter_map(as_dict)
            .collect(),
        Some(other) => as_dict(other).into_iter().collect(),
        None => Vec::new(),
    }
}

/// `PDTableAttributeObject.getInteger` and the like: an attribute's integer, or `default`.
pub(super) fn attribute_int(pdf: &Pdf, attribute: &Dictionary, key: &[u8], default: i32) -> i32 {
    pdf.get(attribute, key)
        .and_then(int_value)
        .unwrap_or(default)
}
