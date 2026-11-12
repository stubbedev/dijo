//! The text of a Word Pro document, as LibreOffice's `lwpfilter` finds it: the root document
//! and the divisions its sockets plug in (`LwpDocument`, `LwpDocSock`), each division's
//! initial layout (`LwpDivInfo`), a page layout whose content is the division's story, and
//! the stories' paragraphs (`LwpStory`, `LwpPara`), each a list of fribs (`LwpFrib`): text, in
//! the code page its modifiers give, and the tables, frames, notes and footnotes it anchors,
//! each a layout with stories of its own.
//!
//! A division's information names the division it belongs to; it is found by that name, as the
//! document object places it after its foundry, which this reader does not read. Every story
//! and layout is read once: one reached again, as a story two layouts show, is not repeated. An
//! id the index does not list names no object, as LibreOffice's `QueryObject` finds none: a
//! list ends there, and a reference to it is passed over.

use std::collections::{BTreeMap, BTreeSet};

use super::objects::{packed, Fields, ObjectId, Objects, WINDOWS_1252};
use super::Malformed;
use crate::text::charset::Charset;

/// The object types read (`VO_TYPE`).
mod tag {
    pub(super) const PARAGRAPH: u16 = 0;
    pub(super) const DOCUMENT: u16 = 5;
    pub(super) const DIVISION_INFO: u16 = 7;
    pub(super) const STORY: u16 = 13;
    pub(super) const FOOTNOTE: u16 = 53;
    pub(super) const OBJECT_HOLDER: u16 = 98;
    pub(super) const DOC_DATA: u16 = 137;
    pub(super) const DOC_SOCK: u16 = 155;
}

/// The frib types read (`FRIB_TAG_*`), and the bits of a frib's tag beside its type.
mod frib {
    pub(super) const END: u8 = 0;
    pub(super) const TEXT: u8 = 2;
    pub(super) const TABLE: u8 = 3;
    pub(super) const TAB: u8 = 4;
    pub(super) const FRAME: u8 = 6;
    pub(super) const FOOTNOTE: u8 = 7;
    pub(super) const LINE_BREAK: u8 = 9;
    pub(super) const HARD_SPACE: u8 = 10;
    pub(super) const UNICODE: [u8; 3] = [13, 14, 15];
    pub(super) const NOTE: u8 = 20;
    pub(super) const MAXIMUM: u8 = 30;
    pub(super) const NO_UNICODE: u8 = 0x40;
    pub(super) const MODIFIED: u8 = 0x80;
    pub(super) const MODIFIER_END: u8 = 0;
    pub(super) const MODIFIER_CODE_PAGE: u8 = 8;
}

/// The flags of a paragraph: one that keeps only the fields of a simple paragraph, one with
/// notify lists.
const SIMPLE: u8 = 1;
const NOTIFY: u8 = 2;
/// The division flags `IsGotoable` reads, and the class of an OLE division.
const HAS_CONTENTS: u16 = 0x02;
const SCROLLABLE: u16 = 0x01;
const OLE_DIVISION: &str = "OLEDivision";
/// The classes of the divisions that hold endnotes.
const ENDNOTE_DIVISIONS: [&str; 3] = ["DivisionEndnote", "DivisionGroupEndnote", "DocumentEndnote"];
/// The bit of a footnote's type that makes it an endnote.
const ENDNOTE: u16 = 0x80;
/// How deep layouts and stories nest in one another before the document is taken to be
/// malformed.
const MAX_DEPTH: usize = 64;

/// What a document's text is made of, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Block {
    Paragraph(String),
    /// Rows of cells, each cell's blocks.
    Table(Vec<Vec<Vec<Block>>>),
    Division(Class, Vec<Block>),
}

/// What a division's blocks are, beside the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Header,
    Footer,
    TextBox,
    Note,
    Footnote,
    Endnote,
}

impl Class {
    pub(super) fn name(self) -> &'static str {
        match self {
            Class::Header => "header",
            Class::Footer => "footer",
            Class::TextBox => "textbox",
            Class::Note => "annotation",
            Class::Footnote => "footnote",
            Class::Endnote => "endnote",
        }
    }
}

/// The kinds of layouts, by their types (`VO_*LAYOUT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Page,
    Header,
    Footer,
    /// A frame, a group of frames, a drop cap or a ruby.
    Frame,
    Note,
    /// A table's layout, whose children are its rows.
    Table,
    Row,
    Cell,
    /// A cell another cell spans, which shows none of its own.
    HiddenCell,
    /// Any other layout with content of its own: a super table, a viewport, a note's parts.
    Other,
    /// A layout without content: a column, the head layout.
    Contentless,
}

impl Kind {
    /// The kind of layout of type `tag`; none for an object that is no layout.
    fn of(tag: u16) -> Option<Kind> {
        Some(match tag {
            143 => Kind::Page,
            139 => Kind::Header,
            140 => Kind::Footer,
            141 | 108 | 217 | 180 => Kind::Frame,
            159 => Kind::Note,
            31 | 117 | 65 | 138 | 206 => Kind::Table,
            32 | 118 | 59 => Kind::Row,
            18 | 28 | 61 | 207 => Kind::Cell,
            29 => Kind::HiddenCell,
            149 | 109 | 113 | 111 | 145 | 146 | 178 | 166 | 132 | 174 | 175 => Kind::Other,
            142 | 35 => Kind::Contentless,
            _ => return None,
        })
    }

    /// Whether the layout is an `LwpMiddleLayout`, which has content.
    fn has_content(self) -> bool {
        !matches!(self, Kind::Row | Kind::Contentless)
    }

    /// The class of the division the layout's blocks are written in, if any.
    fn class(self) -> Option<Class> {
        match self {
            Kind::Header => Some(Class::Header),
            Kind::Footer => Some(Class::Footer),
            Kind::Frame => Some(Class::TextBox),
            Kind::Note => Some(Class::Note),
            _ => None,
        }
    }
}

/// A layout as far as it is read: its place in its parent's children, its children, and its
/// content.
#[derive(Debug, Clone, Copy)]
struct Layout {
    id: ObjectId,
    kind: Kind,
    next: Option<ObjectId>,
    first_child: Option<ObjectId>,
    content: Option<ObjectId>,
    /// A row's number in its table.
    row: Option<u16>,
}

/// A story as far as it is read: the layouts that show it and its paragraphs.
struct Story {
    layouts: Vec<ObjectId>,
    first: Option<ObjectId>,
    last: Option<ObjectId>,
}

/// What a division's information says of it (`LwpDivInfo`).
struct Division {
    name: Option<String>,
    flags: u16,
    class: Option<String>,
    initial_layout: Option<ObjectId>,
}

impl Division {
    /// `LwpDivInfo::IsGotoable`.
    fn gotoable(&self) -> bool {
        self.flags & HAS_CONTENTS != 0
            && (self.flags & SCROLLABLE != 0 || self.class.as_deref() == Some(OLE_DIVISION))
    }
}

/// A paragraph as far as it is read: the next paragraph of its story and its fribs' pieces.
struct Paragraph {
    next: Option<ObjectId>,
    pieces: Vec<Piece>,
}

/// What a frib of a paragraph holds.
enum Piece {
    Text(String),
    /// A table, frame or note: a layout.
    Layout(ObjectId),
    Footnote(ObjectId),
}

/// The text of the document `objects` holds, and the document data objects, which hold its
/// properties.
pub(super) struct Text {
    pub(super) blocks: Vec<Block>,
    pub(super) doc_data: Vec<ObjectId>,
}

/// Reads the text of the document `objects` holds.
pub(super) fn read(objects: &Objects<'_>) -> Result<Text, Malformed> {
    let mut divisions = BTreeMap::new();
    let mut doc_data = Vec::new();
    for id in objects.ids() {
        match objects.tag(id)? {
            Some(tag::DIVISION_INFO) => {
                let (document, division) = division(objects, id)
                    .map_err(|malformed| malformed.within(&format!("division info {id}")))?;
                if let Some(document) = document {
                    divisions.insert(document, division);
                }
            }
            Some(tag::DOC_DATA) => doc_data.push(id),
            _ => {}
        }
    }
    let mut reading = Reading {
        objects,
        divisions,
        seen: BTreeSet::new(),
        depth: 0,
    };
    let mut blocks = Vec::new();
    reading.parse(objects.root(), &mut blocks)?;
    Ok(Text { blocks, doc_data })
}

/// The document a division's information `id` belongs to, and what it says of the division
/// (`LwpDivInfo::Read`).
fn division(
    objects: &Objects<'_>,
    id: ObjectId,
) -> Result<(Option<ObjectId>, Division), Malformed> {
    let object = objects.indexed(id)?;
    let mut fields = objects.fields(&object.body);
    for _ in 0..3 {
        fields.indexed_id()?;
    }
    let parent = fields.indexed_id()?;
    let name = fields.atom()?;
    fields.indexed_id()?;
    let flags = fields.u16()?;
    fields.atom()?;
    fields.atom()?;
    let class = fields.atom()?;
    let initial_layout = fields.indexed_id()?;
    Ok((
        parent,
        Division {
            name,
            flags,
            class,
            initial_layout,
        },
    ))
}

/// The walk through a document's objects.
struct Reading<'o, 'a> {
    objects: &'o Objects<'a>,
    divisions: BTreeMap<ObjectId, Division>,
    seen: BTreeSet<ObjectId>,
    depth: usize,
}

impl Reading<'_, '_> {
    /// Adds the blocks of the object `id` to `out`, as `DoParse` parses it: a document or a
    /// socket of documents; any other object holds no text of its own.
    fn parse(&mut self, id: ObjectId, out: &mut Vec<Block>) -> Result<(), Malformed> {
        if !self.seen.insert(id) {
            return Ok(());
        }
        self.nested(|reading| match reading.objects.tag(id)? {
            Some(tag::DOCUMENT) => reading
                .document(id, out)
                .map_err(|malformed| malformed.within(&format!("document {id}"))),
            Some(tag::DOC_SOCK) => reading
                .socket(id, out)
                .map_err(|malformed| malformed.within(&format!("document socket {id}"))),
            _ => Ok(()),
        })
    }

    /// Runs `work` a level deeper into the document, unless that is too deep.
    fn nested<R>(
        &mut self,
        work: impl FnOnce(&mut Self) -> Result<R, Malformed>,
    ) -> Result<R, Malformed> {
        if self.depth == MAX_DEPTH {
            return Err(Malformed(format!(
                "its objects nest more than {MAX_DEPTH} deep"
            )));
        }
        self.depth += 1;
        let result = work(self);
        self.depth -= 1;
        result
    }

    /// `LwpDocument::Parse`: the division's content, unless it is skipped, then the divisions
    /// its socket plugs in.
    fn document(&mut self, id: ObjectId, out: &mut Vec<Block>) -> Result<(), Malformed> {
        let object = self.objects.indexed(id)?;
        let mut fields = self.objects.fields(&object.body);
        list_header(&mut fields)?;
        if fields.u8()? != 0 {
            fields.indexed_id()?;
        }
        fields.skip_extra()?;
        let socket = fields.indexed_id()?;
        if let Some(initial_layout) = self.content_layout(id)? {
            self.division_content(initial_layout, out)?;
        }
        if let Some(socket) = socket {
            self.parse(socket, out)?;
        }
        Ok(())
    }

    /// `LwpDocSock::Parse`: the document plugged in, then the next.
    fn socket(&mut self, id: ObjectId, out: &mut Vec<Block>) -> Result<(), Malformed> {
        let object = self.objects.indexed(id)?;
        let mut fields = self.objects.fields(&object.body);
        let next = fields.indexed_id()?;
        fields.indexed_id()?;
        let child = fields.indexed_id()?;
        for id in [child, next].into_iter().flatten() {
            self.parse(id, out)?;
        }
        Ok(())
    }

    /// The initial layout of the division `document`, unless it has none or is skipped
    /// (`LwpDocument::IsSkippedDivision`): one that is named but cannot be gone to, as a script
    /// division, or an endnote division whose story holds a single paragraph, the endnotes'
    /// table.
    fn content_layout(&self, document: ObjectId) -> Result<Option<ObjectId>, Malformed> {
        let Some(division) = self.divisions.get(&document) else {
            return Ok(None);
        };
        let Some(initial) = division.initial_layout else {
            return Ok(None);
        };
        if division
            .name
            .as_deref()
            .is_some_and(|name| !name.is_empty())
            && !division.gotoable()
        {
            return Ok(None);
        }
        if division
            .class
            .as_deref()
            .is_some_and(|class| ENDNOTE_DIVISIONS.contains(&class))
        {
            if let Some(layout) = self
                .layout_head(initial)?
                .filter(|layout| layout.kind == Kind::Page)
            {
                if let Some(story) = self.story_of(&layout)? {
                    let story = self.story_head(story)?;
                    if story.first == story.last {
                        return Ok(None);
                    }
                }
            }
        }
        Ok(Some(initial))
    }

    /// The blocks of a division whose initial layout is `initial`: for a page layout, the
    /// headers of the page layouts that show its story, the story, and their other layouts.
    fn division_content(
        &mut self,
        initial: ObjectId,
        out: &mut Vec<Block>,
    ) -> Result<(), Malformed> {
        let Some(layout) = self.layout_head(initial)? else {
            return Ok(());
        };
        if layout.kind != Kind::Page {
            return self.layout(layout, out);
        }
        let story = self.story_of(&layout)?;
        let mut pages = vec![layout];
        if let Some(story) = story {
            for id in self.story_head(story)?.layouts {
                if id != initial {
                    pages.extend(
                        self.layout_head(id)?
                            .filter(|other| other.kind == Kind::Page),
                    );
                }
            }
        }
        let mut children = Vec::new();
        for page in &pages {
            self.seen.insert(page.id);
            children.extend(self.children(page)?);
        }
        let (headers, others): (Vec<Layout>, Vec<Layout>) = children
            .into_iter()
            .partition(|child| child.kind == Kind::Header);
        for header in headers {
            self.layout(header, out)?;
        }
        if let Some(story) = story {
            self.story(story, out)?;
        }
        for other in others {
            self.layout(other, out)?;
        }
        Ok(())
    }

    /// The story the layout shows, if its content is one.
    fn story_of(&self, layout: &Layout) -> Result<Option<ObjectId>, Malformed> {
        match layout.content {
            Some(content) if self.objects.tag(content)? == Some(tag::STORY) => Ok(Some(content)),
            _ => Ok(None),
        }
    }

    /// The layout `id` as far as it is read (`LwpVirtualLayout::Read`,
    /// `LwpMiddleLayout::Read`, `LwpRowLayout::Read`); none if the index does not list it.
    fn layout_head(&self, id: ObjectId) -> Result<Option<Layout>, Malformed> {
        let within = |malformed: Malformed| malformed.within(&format!("layout {id}"));
        let Some(object) = self.objects.object(id)? else {
            return Ok(None);
        };
        let kind = Kind::of(object.tag)
            .ok_or_else(|| Malformed(format!("object {id} of type {} is no layout", object.tag)))?;
        let mut fields = self.objects.fields(&object.body);
        let (next, first_child) = list_header(&mut fields).map_err(within)?;
        let read = |fields: &mut Fields<'_>| -> Result<(Option<ObjectId>, Option<u16>), Malformed> {
            if fields.u8()? != 0 {
                fields.indexed_id()?;
            }
            fields.skip_extra()?;
            fields.skip(4 * 4 + 2 + 2)?;
            fields.indexed_id()?;
            fields.indexed_id()?;
            fields.skip_extra()?;
            fields.atom()?;
            fields.skip_extra()?;
            if kind.has_content() {
                return Ok((fields.indexed_id()?, None));
            }
            let row = (kind == Kind::Row).then(|| fields.u16()).transpose()?;
            Ok((None, row))
        };
        let (content, row) = read(&mut fields).map_err(within)?;
        Ok(Some(Layout {
            id,
            kind,
            next,
            first_child,
            content,
            row,
        }))
    }

    /// The children of `layout`, in their order.
    fn children(&self, layout: &Layout) -> Result<Vec<Layout>, Malformed> {
        let mut children = Vec::new();
        let mut listed = BTreeSet::new();
        let mut next = layout.first_child;
        while let Some(id) = next {
            if !listed.insert(id) {
                return Err(Malformed(format!(
                    "the children of layout {} list layout {id} twice",
                    layout.id
                )));
            }
            let Some(child) = self.layout_head(id)? else {
                break;
            };
            next = child.next;
            children.push(child);
        }
        Ok(children)
    }

    /// Adds the blocks of `layout` to `out`, unless they are read already: a table's rows and
    /// cells, or else the layout's story and its children's blocks, in a division of the
    /// layout's class if it has one.
    fn layout(&mut self, layout: Layout, out: &mut Vec<Block>) -> Result<(), Malformed> {
        if !self.seen.insert(layout.id) {
            return Ok(());
        }
        self.nested(|reading| {
            if layout.kind == Kind::Table {
                return reading.table(&layout, out);
            }
            match layout.kind.class() {
                Some(class) => {
                    let mut blocks = Vec::new();
                    reading.contents(&layout, &mut blocks)?;
                    if !blocks.is_empty() {
                        out.push(Block::Division(class, blocks));
                    }
                    Ok(())
                }
                None => reading.contents(&layout, out),
            }
        })
    }

    /// Adds the layout's story, then its children's blocks, to `out`.
    fn contents(&mut self, layout: &Layout, out: &mut Vec<Block>) -> Result<(), Malformed> {
        if let Some(story) = self.story_of(layout)? {
            self.story(story, out)?;
        }
        for child in self.children(layout)? {
            self.layout(child, out)?;
        }
        Ok(())
    }

    /// Adds a table to `out`: its rows in the order of their numbers, each row's cells but the
    /// ones another spans, a row without cells of its own left out; then the blocks of its
    /// children that are no rows.
    fn table(&mut self, layout: &Layout, out: &mut Vec<Block>) -> Result<(), Malformed> {
        let (mut rows, others): (Vec<Layout>, Vec<Layout>) = self
            .children(layout)?
            .into_iter()
            .partition(|child| child.kind == Kind::Row);
        rows.sort_by_key(|row| row.row);
        let mut table = Vec::with_capacity(rows.len());
        for row in rows {
            if !self.seen.insert(row.id) {
                continue;
            }
            let mut cells = Vec::new();
            for cell in self.children(&row)? {
                if cell.kind == Kind::HiddenCell {
                    self.seen.insert(cell.id);
                    continue;
                }
                let mut blocks = Vec::new();
                self.layout(cell, &mut blocks)?;
                cells.push(blocks);
            }
            if !cells.is_empty() {
                table.push(cells);
            }
        }
        if table.iter().flatten().any(|cell| !cell.is_empty()) {
            out.push(Block::Table(table));
        }
        for other in others {
            self.layout(other, out)?;
        }
        Ok(())
    }

    /// The story `id` as far as it is read (`LwpContent::Read`, `LwpStory::Read`), with the
    /// layouts that show it (`LwpAssociatedLayouts`).
    fn story_head(&self, id: ObjectId) -> Result<Story, Malformed> {
        let within = |malformed: Malformed| malformed.within(&format!("story {id}"));
        let object = self.objects.indexed(id)?;
        let mut fields = self.objects.fields(&object.body);
        let read = |fields: &mut Fields<'_>| -> Result<_, Malformed> {
            list_header(fields)?;
            let only = fields.indexed_id()?;
            let holders = head_tail(fields)?.0;
            fields.skip_extra()?;
            fields.u16()?;
            fields.atom()?;
            fields.indexed_id()?;
            fields.indexed_id()?;
            if fields.u8()? != 0 {
                fields.indexed_id()?;
                fields.skip_extra()?;
            }
            fields.skip_extra()?;
            let (first, last) = head_tail(fields)?;
            Ok((only, holders, first, last))
        };
        let (only, holders, first, last) = read(&mut fields).map_err(within)?;
        let layouts = match only {
            Some(only) => vec![only],
            None => self.held(holders).map_err(within)?,
        };
        Ok(Story {
            layouts,
            first,
            last,
        })
    }

    /// The objects a list of holders, from `first` on, holds (`LwpObjectHolder`).
    fn held(&self, first: Option<ObjectId>) -> Result<Vec<ObjectId>, Malformed> {
        let mut held = Vec::new();
        let mut listed = BTreeSet::new();
        let mut next = first;
        while let Some(id) = next {
            if !listed.insert(id) {
                return Err(Malformed(format!("the holder {id} is listed twice")));
            }
            let Some(object) = self.objects.object(id)? else {
                break;
            };
            if object.tag != tag::OBJECT_HOLDER {
                return Err(Malformed(format!(
                    "object {id} of type {} in a list of holders is no holder",
                    object.tag
                )));
            }
            let mut fields = self.objects.fields(&object.body);
            next = fields.indexed_id()?;
            fields.indexed_id()?;
            held.extend(fields.indexed_id()?);
        }
        Ok(held)
    }

    /// Adds the paragraphs of the story `id` to `out`, unless they are read already, with the
    /// blocks of what they anchor.
    fn story(&mut self, id: ObjectId, out: &mut Vec<Block>) -> Result<(), Malformed> {
        if !self.seen.insert(id) {
            return Ok(());
        }
        self.nested(|reading| {
            let story = reading.story_head(id)?;
            let mut read = BTreeSet::new();
            let mut next = story.first;
            while let Some(paragraph) = next {
                if !read.insert(paragraph) {
                    return Err(Malformed(format!(
                        "story {id} lists paragraph {paragraph} twice"
                    )));
                }
                let Some(Paragraph {
                    next: following,
                    pieces,
                }) = reading
                    .paragraph(paragraph)
                    .map_err(|malformed| malformed.within(&format!("paragraph {paragraph}")))?
                else {
                    break;
                };
                reading.write_paragraph(pieces, out)?;
                next = following;
            }
            Ok(())
        })
    }

    /// Adds a paragraph's `pieces` to `out`: its text, broken where it anchors a layout, which
    /// is added there; then its footnotes.
    fn write_paragraph(
        &mut self,
        pieces: Vec<Piece>,
        out: &mut Vec<Block>,
    ) -> Result<(), Malformed> {
        let mut text = String::new();
        let mut footnotes = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Text(more) => text.push_str(&more),
                Piece::Layout(id) => {
                    push_paragraph(&mut text, out);
                    if !self.seen.contains(&id) {
                        if let Some(layout) = self.layout_head(id)? {
                            self.layout(layout, out)?;
                        }
                    }
                }
                Piece::Footnote(id) => footnotes.push(id),
            }
        }
        push_paragraph(&mut text, out);
        for id in footnotes {
            self.footnote(id, out)?;
        }
        Ok(())
    }

    /// Adds the story of the footnote `id` to `out`, in a division of a footnote or an endnote
    /// (`LwpFootnote::Read`).
    fn footnote(&mut self, id: ObjectId, out: &mut Vec<Block>) -> Result<(), Malformed> {
        let Some(object) = self.objects.object(id)? else {
            return Ok(());
        };
        if object.tag != tag::FOOTNOTE {
            return Err(Malformed(format!(
                "a footnote mark points to object {id} of type {}",
                object.tag
            )));
        }
        let mut fields = self.objects.fields(&object.body);
        let read = |fields: &mut Fields<'_>| -> Result<_, Malformed> {
            list_header(fields)?;
            fields.indexed_id()?;
            fields.indexed_id()?;
            fields.skip_extra()?;
            let kind = fields.u16()?;
            fields.u16()?;
            Ok((kind, fields.indexed_id()?))
        };
        let (kind, content) =
            read(&mut fields).map_err(|malformed| malformed.within(&format!("footnote {id}")))?;
        let Some(content) = content else {
            return Ok(());
        };
        if self.objects.tag(content)? != Some(tag::STORY) {
            return Ok(());
        }
        let mut blocks = Vec::new();
        self.story(content, &mut blocks)?;
        if !blocks.is_empty() {
            let class = if kind & ENDNOTE != 0 {
                Class::Endnote
            } else {
                Class::Footnote
            };
            out.push(Block::Division(class, blocks));
        }
        Ok(())
    }

    /// The paragraph `id`, none if the index does not list it (`LwpPara::Read`,
    /// `LwpFribPtr::ReadPara`).
    fn paragraph(&self, id: ObjectId) -> Result<Option<Paragraph>, Malformed> {
        let Some(object) = self.objects.object(id)? else {
            return Ok(None);
        };
        if object.tag != tag::PARAGRAPH {
            return Err(Malformed(format!(
                "object {id} of type {} is no paragraph",
                object.tag
            )));
        }
        let mut fields = self.objects.fields(&object.body);
        let next = fields.indexed_id()?;
        fields.indexed_id()?;
        let flags = fields.u8()?;
        let simple = flags & SIMPLE != 0;
        if !simple {
            fields.u32()?;
            if flags & NOTIFY != 0 {
                for _ in 0..2 {
                    fields.indexed_id()?;
                    fields.skip_extra()?;
                }
            }
        }
        fields.u16()?;
        fields.indexed_id()?;
        if !simple {
            fields.skip(8)?;
        }
        fields.indexed_id()?;
        if !simple {
            fields.u16()?;
        }
        let mut pieces = Vec::new();
        while let Some(piece) = self.frib(&mut fields)? {
            pieces.extend(piece);
        }
        Ok(Some(Paragraph { next, pieces }))
    }

    /// The next frib of a paragraph and what it holds, if anything; none at the paragraph's
    /// end (`LwpFrib::CreateFrib`, `LwpFrib::ReadModifiers`).
    fn frib(&self, fields: &mut Fields<'_>) -> Result<Option<Option<Piece>>, Malformed> {
        let tag = fields.u8()?;
        let kind = tag & !(frib::NO_UNICODE | frib::MODIFIED);
        if kind == frib::END {
            return Ok(None);
        }
        fields.u8()?;
        if kind == frib::MAXIMUM {
            return Ok(Some(None));
        }
        let mut code_page = None;
        if tag & frib::MODIFIED != 0 {
            loop {
                let modifier = fields.u8()?;
                if modifier == frib::MODIFIER_END {
                    break;
                }
                let length = usize::from(fields.u8()?);
                let value = fields.take(length)?;
                if modifier == frib::MODIFIER_CODE_PAGE && length == 2 {
                    code_page = Some(u16::from_le_bytes([value[0], value[1]]));
                }
            }
        }
        let length = usize::from(fields.u16()?);
        let data = fields.take(length)?;
        let text = |charset: Charset| packed(data, charset).map(|text| Some(Piece::Text(text)));
        let layout = |data: &[u8]| -> Result<Option<Piece>, Malformed> {
            Ok(self.objects.fields(data).indexed_id()?.map(Piece::Layout))
        };
        Ok(Some(match kind {
            frib::TEXT if tag & frib::NO_UNICODE != 0 => text(Charset::named("ISO-8859-1"))?,
            frib::TEXT => text(code_page_charset(code_page))?,
            kind if frib::UNICODE.contains(&kind) && length > 1 => {
                text(code_page_charset(code_page))?
            }
            frib::TAB => Some(Piece::Text("\t".into())),
            frib::LINE_BREAK => Some(Piece::Text("\n".into())),
            frib::HARD_SPACE => Some(Piece::Text("\u{a0}".into())),
            frib::TABLE | frib::FRAME | frib::NOTE => layout(data)?,
            frib::FOOTNOTE => self.objects.fields(data).indexed_id()?.map(Piece::Footnote),
            _ => None,
        }))
    }
}

/// Ends the paragraph `text` holds, adding it to `out` unless it is blank.
fn push_paragraph(text: &mut String, out: &mut Vec<Block>) {
    if text.trim().is_empty() {
        text.clear();
    } else {
        out.push(Block::Paragraph(std::mem::take(text)));
    }
}

/// The fields of a list element with children and a name (`LwpDLNFVList::Read`): the next
/// element and the first child.
fn list_header(fields: &mut Fields<'_>) -> Result<(Option<ObjectId>, Option<ObjectId>), Malformed> {
    let next = fields.indexed_id()?;
    fields.indexed_id()?;
    let (first_child, _) = head_tail(fields)?;
    fields.indexed_id()?;
    fields.atom()?;
    Ok((next, first_child))
}

/// A list's head and tail, the tail there only after a head (`LwpDLVListHeadTail::Read`).
fn head_tail(fields: &mut Fields<'_>) -> Result<(Option<ObjectId>, Option<ObjectId>), Malformed> {
    let head = fields.indexed_id()?;
    let tail = match head {
        Some(_) => fields.indexed_id()?,
        None => None,
    };
    Ok((head, tail))
}

/// The charset of a Word Pro code page (`LwpCharSetMgr`), code page 1252 for none or one it
/// does not map.
fn code_page_charset(code_page: Option<u16>) -> Charset {
    let name = match code_page {
        Some(437) => "IBM437",
        Some(850) => "IBM850",
        Some(852) => "IBM852",
        Some(857) => "IBM857",
        Some(860) => "IBM860",
        Some(863) => "IBM863",
        Some(865) => "IBM865",
        Some(866) => "IBM866",
        Some(869) => "IBM869",
        Some(874) => "x-windows-874",
        Some(932) => "windows-31j",
        Some(936) => "GBK",
        Some(949) => "x-windows-949",
        Some(950) => "Big5",
        Some(1250) => "windows-1250",
        Some(1251) => "windows-1251",
        Some(1253) => "windows-1253",
        Some(1254) => "windows-1254",
        Some(1255) => "windows-1255",
        Some(1256) => "windows-1256",
        Some(1257) => "windows-1257",
        _ => WINDOWS_1252,
    };
    Charset::named(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_page_is_a_charset_that_is_decoded() {
        let code_pages = [
            437, 850, 852, 857, 860, 863, 865, 866, 869, 874, 932, 936, 949, 950, 1250, 1251, 1252,
            1253, 1254, 1255, 1256, 1257,
        ];
        for code_page in code_pages.into_iter().map(Some).chain([None, Some(1)]) {
            assert!(code_page_charset(code_page).decodes(), "{code_page:?}");
        }
        assert!(Charset::named("ISO-8859-1").decodes());
        assert_eq!(
            code_page_charset(Some(1251)).decode(b"\xe4").unwrap(),
            "\u{434}"
        );
    }
}
