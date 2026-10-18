//! The shapes of a sheet as POI's `HSLFShapeFactory` makes them from its drawing, with what
//! setting their sheet reads: a text shape's paragraphs, a table's cells by their anchors.

use std::collections::BTreeSet;

use crate::formats::escher::{kind as escher_kind, Content};
use crate::formats::poi::{Thrown, Unread};
use crate::formats::Unported;

use super::drawing::{textbox_records, Node, SPGR_CONTAINER};
use super::records::{kind, Record};
use super::show::{Found, Sheet};
use super::text::{external, placeholder, raw_text, Paragraph, Props, Run};

/// `EscherSpRecord.FLAG_CHILD`.
const FLAG_CHILD: i32 = 0x0002;
/// `GROUPSHAPE__TABLEPROPERTIES`.
const TABLE_PROPERTIES: u16 = 0x039F;
/// `BLIP__BLIPTODISPLAY`.
pub(super) const BLIP_TO_DISPLAY: u16 = 0x0104;
/// `BLIP__PICTUREID`.
pub(super) const PICTURE_ID: u16 = 0x010B;
/// `InteractiveInfoAtom.ACTION_OLE` and `ACTION_MEDIA`.
const ACTION_OLE: u8 = 5;
const ACTION_MEDIA: u8 = 6;

fn poi_throws(reason: &str) -> Unread {
    Thrown(format!("reading the presentation's shapes: {reason}")).into()
}

/// What `HSLFShapeFactory` makes of a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Group,
    Table,
    /// A text box, auto shape or freeform.
    Text,
    TableCell,
    Line,
    Picture,
    Object,
    Movie,
}

impl Kind {
    pub(super) fn is_text(self) -> bool {
        matches!(self, Kind::Text | Kind::Line | Kind::TableCell)
    }

    pub(super) fn is_group(self) -> bool {
        matches!(self, Kind::Group | Kind::Table)
    }

    /// Whether it is an `HSLFPictureShape`, which object and movie shapes are too.
    pub(super) fn is_picture(self) -> bool {
        matches!(self, Kind::Picture | Kind::Object | Kind::Movie)
    }
}

/// The paragraphs of a text shape (`getTextParagraphs`).
#[derive(Debug, Clone)]
pub(super) enum Text<'a> {
    /// A list of the sheet's, by its paragraphs' indexes.
    Sheet(Vec<usize>),
    /// Paragraphs of its own.
    Own(Vec<Paragraph<'a>>),
}

/// A shape.
#[derive(Debug)]
pub(super) struct Shape<'a> {
    pub(super) node: &'a Node,
    pub(super) kind: Kind,
    /// A text shape's paragraphs, and its run type (`getRunType`), `None` where POI throws
    /// finding it.
    pub(super) text: Option<(Text<'a>, Option<i32>)>,
    /// A table's cells by row and column, as indexes of its shapes.
    pub(super) cells: Vec<Vec<Option<usize>>>,
    /// A group's or table's shapes.
    pub(super) children: Vec<Shape<'a>>,
}

impl<'a> Shape<'a> {
    /// `isPlaceholder`: whether a text shape's client data holds a placeholder.
    pub(super) fn is_placeholder(&self) -> bool {
        self.kind.is_text()
            && (self.node.client_record(kind::OE_PLACEHOLDER_ATOM).is_some()
                || self
                    .node
                    .client_record(kind::ROUND_TRIP_HF_PLACEHOLDER)
                    .is_some())
    }

    /// The paragraphs of a text shape.
    pub(super) fn paragraphs<'s>(&'s self, sheet: &'s Sheet<'a>) -> Vec<&'s Paragraph<'a>> {
        match &self.text {
            Some((Text::Sheet(list), _)) => list.iter().map(|&id| &sheet.paragraphs[id]).collect(),
            Some((Text::Own(paragraphs), _)) => paragraphs.iter().collect(),
            None => Vec::new(),
        }
    }

    /// `HSLFTextShape.getText`: the raw text of its paragraphs as `toExternalString` gives it.
    pub(super) fn text(&self, sheet: &Sheet<'a>) -> Result<Vec<u16>, Unread> {
        let run_type = match &self.text {
            Some((_, Some(run_type))) => *run_type,
            Some((_, None)) => {
                return Err(poi_throws(
                    "a text box refers to outline text its sheet lacks, whose run type POI \
                     fails to find",
                ))
            }
            None => -1,
        };
        Ok(external(&raw_text(&self.paragraphs(sheet)), run_type))
    }

    /// An integer property of its options, as `getEscherProperty(type)` reads it: 0 where it
    /// has none, failing where it is complex.
    pub(super) fn property(&self, number: u16) -> Result<i32, Unread> {
        let Some(property) = self
            .node
            .options()
            .and_then(|options| options.lookup(number))
        else {
            return Ok(0);
        };
        if property.complex.is_some() {
            return Err(poi_throws("a complex property read as a simple one"));
        }
        Ok(property.value)
    }
}

/// `HSLFSheet.getShapes`: the shapes of a sheet's first group, after its own shape.
pub(super) fn sheet_shapes<'a>(sheet: &Sheet<'a>) -> Result<Vec<Shape<'a>>, Unread> {
    let Some((_, group)) = sheet.drawing.first_group() else {
        return Err(poi_throws("spgr not found"));
    };
    if !group.is_container() {
        return Err(poi_throws("Did not have a EscherContainerRecord"));
    }
    group
        .children
        .iter()
        .skip(1)
        .map(|child| {
            if !child.is_container() {
                return Err(poi_throws("Did not have a EscherContainerRecord"));
            }
            create(child, false, sheet)
        })
        .collect()
}

/// `HSLFShapeFactory.createShape`, then `setSheet`.
fn create<'a>(node: &'a Node, in_table: bool, sheet: &Sheet<'a>) -> Result<Shape<'a>, Unread> {
    if node.record_id == SPGR_CONTAINER {
        return group(node, sheet);
    }
    let shape = node
        .shape()
        .ok_or_else(|| poi_throws("Could not read EscherSpRecord"))?;
    let kind = match shape.kind {
        202 => Kind::Text,
        201 | 75 => frame(node),
        20 => Kind::Line,
        0 | 0x0FFF => Kind::Text,
        1..=202 if in_table => Kind::TableCell,
        1..=202 => Kind::Text,
        _ => return Err(poi_throws("Unknown shape type")),
    };
    let text = if kind.is_text() {
        Some(text_of(node, sheet)?)
    } else {
        None
    };
    Ok(Shape {
        node,
        kind,
        text,
        cells: Vec::new(),
        children: Vec::new(),
    })
}

/// `createFrame`: an OLE object or movie by the shape's interactive record, else an object
/// where it refers to one, else a picture.
fn frame(node: &Node) -> Kind {
    if let Some(info) = node.client_record(kind::INTERACTIVE_INFO) {
        let atom = info
            .children()
            .first()
            .filter(|child| child.kind == kind::INTERACTIVE_INFO_ATOM);
        match atom.and_then(|atom| atom.byte(8).ok()) {
            Some(ACTION_OLE) => return Kind::Object,
            Some(ACTION_MEDIA) => return Kind::Movie,
            _ => {}
        }
    }
    if node.client_record(kind::EX_OBJ_REF_ATOM).is_some() {
        Kind::Object
    } else {
        Kind::Picture
    }
}

/// `createShapeGroup`: a table where its group shape's tertiary options say so, else a group,
/// with its shapes.
fn group<'a>(node: &'a Node, sheet: &Sheet<'a>) -> Result<Shape<'a>, Unread> {
    let first = node
        .children
        .first()
        .filter(|child| child.is_container())
        .ok_or_else(|| poi_throws("Did not have a EscherContainerRecord"))?;
    let is_table = first
        .child(escher_kind::TERTIARY_OPT)
        .and_then(|options| match &options.content {
            Content::TertiaryOptions(options) => Some(options),
            _ => None,
        })
        .is_some_and(|options| {
            options.0.iter().any(|property| {
                property.number == TABLE_PROPERTIES
                    && property.complex.is_none()
                    && property.value & 1 == 1
            })
        });
    let children = group_shapes(node, is_table, sheet)?;
    let mut shape = Shape {
        node,
        kind: if is_table { Kind::Table } else { Kind::Group },
        text: None,
        cells: Vec::new(),
        children,
    };
    if is_table {
        shape.cells = cells(&shape.children)?;
    }
    Ok(shape)
}

/// `HSLFGroupShape.getShapes`: the containers after the group's own shape.
fn group_shapes<'a>(
    node: &'a Node,
    is_table: bool,
    sheet: &Sheet<'a>,
) -> Result<Vec<Shape<'a>>, Unread> {
    node.children
        .iter()
        .skip(1)
        .filter(|child| child.is_container())
        .map(|child| create(child, is_table, sheet))
        .collect()
}

/// `HSLFShape.getAnchor`'s left and top, in points as `Units.masterToPoints` gives them.
fn anchor_origin(node: &Node) -> Result<(f64, f64), Unread> {
    let flags = node.shape().map_or(0, |shape| shape.flags);
    let child = node.child(escher_kind::CHILD_ANCHOR);
    let (x, y) = match child.map(|child| &child.content) {
        Some(Content::ChildAnchor([x1, y1, _, _])) if flags & FLAG_CHILD != 0 => (*x1, *y1),
        _ => match node
            .child(escher_kind::CLIENT_ANCHOR)
            .map(|anchor| &anchor.content)
        {
            Some(Content::ClientAnchor(corners)) => {
                let [flag, col1, _, _] = corners.unwrap_or([0; 4]);
                (i32::from(col1), i32::from(flag))
            }
            _ => {
                return Err(poi_throws(
                    "Could not read record 'CLIENT_ANCHOR' with record-id: -4080",
                ))
            }
        },
    };
    let points = |master: i32| {
        if master == -1 {
            -1.0
        } else {
            f64::from(master.wrapping_mul(72)) / 576.0
        }
    };
    Ok((points(x), points(y)))
}

/// A coordinate as a key `TreeSet<Double>` and `indexOf` compare alike: points are eighths of
/// master units, never NaN.
fn key(value: f64) -> i64 {
    (value * 8.0) as i64
}

/// `HSLFTable.cellListToArray`: the cells by the distinct tops and lefts of their anchors.
fn cells(children: &[Shape]) -> Result<Vec<Vec<Option<usize>>>, Unread> {
    let mut anchored = Vec::new();
    for (index, child) in children.iter().enumerate() {
        if child.kind == Kind::TableCell {
            let (x, y) = anchor_origin(child.node)?;
            anchored.push((index, key(x), key(y)));
        }
    }
    if anchored.is_empty() {
        return Err(poi_throws("HSLFTable without HSLFTableCells"));
    }
    let columns: Vec<i64> = anchored
        .iter()
        .map(|&(_, x, _)| x)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let rows: Vec<i64> = anchored
        .iter()
        .map(|&(_, _, y)| y)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut grid = vec![vec![None; columns.len()]; rows.len()];
    for (index, x, y) in anchored {
        let row = rows
            .iter()
            .position(|&top| top == y)
            .expect("a row of the set");
        let column = columns
            .iter()
            .position(|&left| left == x)
            .expect("a column of the set");
        grid[row][column] = Some(index);
    }
    Ok(grid)
}

/// `HSLFTextShape.getTextParagraphs` of a shape on `sheet`, then `getRunType`, `None` where it
/// throws.
fn text_of<'a>(node: &'a Node, sheet: &Sheet<'a>) -> Result<(Text<'a>, Option<i32>), Unread> {
    let records = match textbox_records(node) {
        Some(Ok(records)) => Some(records),
        Some(Err(unread)) => return Err(unread),
        None => None,
    };
    let found = match records {
        None => Found::Nothing,
        Some(_) => sheet.find_in_sheet(node)?,
    };
    let first_of = |paragraph: Option<&Paragraph>| {
        let (index, run_type) = paragraph.map_or((-1, -1), |first| (first.index, first.run_type));
        Some(if index == -1 { -1 } else { run_type })
    };
    Ok(match found {
        Found::List(list) => {
            let run_type = first_of(Some(&sheet.paragraphs[list[0]]));
            (Text::Sheet(list), run_type)
        }
        Found::New(paragraphs) => {
            let run_type = first_of(paragraphs.first());
            (Text::Own(paragraphs), run_type)
        }
        Found::Nothing => {
            let records = records.unwrap_or_default();
            let references_outline = records
                .iter()
                .any(|record| record.kind == kind::OUTLINE_TEXT_REF_ATOM);
            let run_type = if references_outline { None } else { Some(-1) };
            (Text::Own(vec![empty_paragraph(records)?]), run_type)
        }
    })
}

/// `createEmptyParagraph`: one paragraph of the text of the text box's first text atom, or of
/// none.
fn empty_paragraph<'a>(records: &'a [Record]) -> Result<Paragraph<'a>, Unread> {
    if records
        .iter()
        .any(|record| record.kind == kind::STYLE_TEXT_PROP_ATOM)
    {
        return Err(Unported(
            "an empty paragraph made with a style atom POI has not read, which this port does \
             not read"
                .to_owned(),
        )
        .into());
    }
    let bytes = records
        .iter()
        .find(|record| record.kind == kind::TEXT_BYTES_ATOM);
    let chars = records
        .iter()
        .find(|record| record.kind == kind::TEXT_CHARS_ATOM);
    let text = match (bytes, chars) {
        (Some(bytes), _) => bytes.latin1(),
        (None, Some(chars)) => chars.utf16(),
        (None, None) => Vec::new(),
    };
    let header = records
        .iter()
        .position(|record| record.kind == kind::TEXT_HEADER_ATOM);
    let run_type = match header {
        Some(at) => records[at]
            .int(0)
            .map_err(|_| poi_throws("a short text header"))?,
        None => placeholder::OTHER,
    };
    Ok(Paragraph {
        parent: records,
        header: header.unwrap_or(0),
        index: -1,
        run_type,
        style: Props {
            covered: text.len() as i32 + 1,
            indent: 0,
            flags: None,
        },
        runs: vec![Run {
            text: super::text::internal(&text),
            link: None,
        }],
    })
}
