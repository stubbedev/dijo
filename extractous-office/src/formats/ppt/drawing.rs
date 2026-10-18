//! A slide's drawing as POI's `PPDrawing` reads it: the Escher records of its `DgContainer`, the
//! slide records the client data of its shapes holds, and the text box wrappers of the shapes
//! of its groups, whose paragraphs are the sheet's.

use crate::formats::escher::{self, kind as escher_kind, Content, Escher, Factory};
use crate::formats::poi::{i32_at, Thrown, Unread};

use super::records::{build_at, children_at, Built, Depth, Record};

const DG_CONTAINER: u16 = 0xF002;
pub(super) const SPGR_CONTAINER: u16 = 0xF003;
pub(super) const SP_CONTAINER: u16 = 0xF004;

/// An Escher record of a drawing.
#[derive(Debug)]
pub(super) struct Node {
    pub(super) record_id: u16,
    pub(super) content: Content<'static>,
    pub(super) children: Vec<Node>,
    /// The slide records of a shape's client data (`HSLFEscherClientDataRecord`).
    pub(super) client: Vec<Record>,
    /// The records of a text box (`EscherTextboxWrapper`), or why POI throws building them.
    pub(super) textbox: Option<Built<Vec<Record>>>,
}

impl Node {
    /// Converts an Escher record, reading the slide records its client data and text box hold.
    fn of(escher: Escher<'_>, depth: Depth) -> Built<Node> {
        let inner = depth.deeper()?;
        let client = match &escher.content {
            Content::ClientData(data) => client_records(data, inner)?,
            _ => Vec::new(),
        };
        let textbox = match &escher.content {
            Content::Textbox(data) if escher.record_id == escher_kind::CLIENT_TEXTBOX => {
                Some(children_at(data, 0, data.len() as i64, inner))
            }
            _ => None,
        };
        let children = escher
            .children
            .into_iter()
            .map(|child| Node::of(child, inner))
            .collect::<Built<Vec<_>>>()?;
        Ok(Node {
            record_id: escher.record_id,
            content: escher.content.into_owned(),
            children,
            client,
            textbox,
        })
    }

    pub(super) fn is_container(&self) -> bool {
        self.content == Content::Container
    }

    /// `getChildById`: the first child of the type.
    pub(super) fn child(&self, record_id: u16) -> Option<&Node> {
        self.children
            .iter()
            .find(|child| child.record_id == record_id)
    }

    /// The first slide record of the client data of the type (`getClientDataRecord`).
    pub(super) fn client_record(&self, kind: u16) -> Option<&Record> {
        self.child(escher_kind::CLIENT_DATA)?
            .client
            .iter()
            .find(|record| record.kind == kind)
    }

    /// The shape record (`EscherSpRecord`) of a shape's container.
    pub(super) fn shape(&self) -> Option<escher::Shape> {
        self.children.iter().find_map(|child| match child.content {
            Content::Shape(shape) if child.record_id == escher_kind::SP => Some(shape),
            _ => None,
        })
    }

    /// `getEscherOptRecord`: the options of a shape, its tertiary ones where it has none.
    pub(super) fn options(&self) -> Option<&escher::Options> {
        let options = self
            .child(escher_kind::OPT)
            .and_then(|node| match &node.content {
                Content::Options(options) => Some(options),
                _ => None,
            });
        options.or_else(|| {
            self.child(escher_kind::TERTIARY_OPT)
                .and_then(|node| match &node.content {
                    Content::TertiaryOptions(options) => Some(options),
                    _ => None,
                })
        })
    }
}

/// `HSLFEscherClientDataRecord.setRemainingData`: the slide records of client data.
fn client_records(data: &[u8], depth: Depth) -> Built<Vec<Record>> {
    let mut records = Vec::new();
    let mut offset: i64 = 0;
    while offset < data.len() as i64 {
        if let Some(record) = build_at(data, offset, depth)? {
            records.push(record);
        }
        let length = i64::from(i32_at(data, offset as usize + 4)? as u32);
        offset += 8 + length;
        if offset > i64::from(i32::MAX) {
            return Err(Thrown::new("integer overflow").into());
        }
    }
    Ok(records)
}

/// A slide's drawing.
#[derive(Debug)]
pub(super) struct Drawing {
    /// The `DgContainer`.
    pub(super) root: Node,
    /// `getTextboxWrappers`: the text boxes of the shapes of the drawing's groups, each by the
    /// group's index among the root's children, the shape's among the group's, and its shape id.
    pub(super) wrappers: Vec<Wrapper>,
}

/// A text box `PPDrawing` wraps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Wrapper {
    pub(super) group: usize,
    pub(super) shape: usize,
    pub(super) shape_id: i32,
}

impl Drawing {
    /// `new PPDrawing(source, start, length)`.
    pub(super) fn read(source: &[u8], start: i64, depth: Depth) -> Built<Drawing> {
        let at = usize::try_from(start + 8)
            .map_err(|_| Unread::Thrown(Thrown::new("a drawing starts before its data")))?;
        let (escher, _) = escher::container_made_at(source, at, Factory::Slides)?;
        if escher.record_id != DG_CONTAINER {
            return Err(Thrown::new(format!(
                "Unexpected record type: {}",
                escher.record_id as i16
            ))
            .into());
        }
        let root = Node::of(escher, depth)?;
        let mut wrappers = Vec::new();
        for (group_index, group) in root.children.iter().enumerate() {
            if !(group.is_container() && group.record_id == SPGR_CONTAINER) {
                continue;
            }
            for (shape_index, shape) in group.children.iter().enumerate() {
                if !(shape.is_container() && shape.record_id == SP_CONTAINER) {
                    continue;
                }
                let Some(textbox) = shape.child(escher_kind::CLIENT_TEXTBOX) else {
                    continue;
                };
                if let Some(Err(fault)) = &textbox.textbox {
                    return Err(fault.clone());
                }
                wrappers.push(Wrapper {
                    group: group_index,
                    shape: shape_index,
                    shape_id: shape.shape().map_or(0, |shape| shape.id),
                });
            }
        }
        Ok(Drawing { root, wrappers })
    }

    /// The shape container a wrapper wraps the text box of.
    pub(super) fn wrapped(&self, wrapper: Wrapper) -> &Node {
        &self.root.children[wrapper.group].children[wrapper.shape]
    }

    /// The first group of the drawing, whose shapes are the sheet's (`getShapes`), or `None`.
    pub(super) fn first_group(&self) -> Option<(usize, &Node)> {
        self.root
            .children
            .iter()
            .enumerate()
            .find(|(_, child)| child.record_id == SPGR_CONTAINER)
    }
}

/// The records of the text box of a shape container, as its wrapper holds them.
pub(super) fn textbox_records(shape: &Node) -> Option<Result<&[Record], Unread>> {
    let textbox = shape.child(escher_kind::CLIENT_TEXTBOX)?;
    match textbox.textbox.as_ref()? {
        Ok(records) => Some(Ok(records)),
        Err(fault) => Some(Err(fault.clone())),
    }
}
