//! How Tika's `OOXMLTikaBodyPartHandler` writes what the text handler finds as XHTML:
//! paragraphs as their style's element, tables, list numbers, links and bookmarks, notes inline
//! where they are referenced and comments after the paragraph that refers to them, with
//! `FormattingTagManager` keeping bold, italic, strike-through, underline and links nested.

use std::collections::{HashMap, HashSet};

use super::notes::InlineParts;
use super::notes::Recorded;
use super::numbering::ListManager;
use super::styles::{paragraph_tag_and_style, Styles};
use super::text::{RunProperties, TextHandler, TextOptions};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Failure;

/// The flags `OOXMLTikaBodyPartHandler` sets on the metadata.
pub(super) const HAS_LINKED_OLE_OBJECTS: &str = "msoffice:has-linked-ole-objects";
pub(super) const HAS_FIELD_HYPERLINKS: &str = "msoffice:has-field-hyperlinks";
const HAS_HOVER_HYPERLINKS: &str = "msoffice:has-hover-hyperlinks";
const HAS_VML_HYPERLINKS: &str = "msoffice:has-vml-hyperlinks";

/// What a part says of an OLE object it embeds, as `embeddedOLERef` records it in an
/// `EmbeddedPartMetadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OleReference {
    /// The program the object belongs to.
    pub(super) prog_id: Option<String>,
    /// The id of the relationship to the picture that shows the object, last in the part
    /// before it.
    pub(super) icon: Option<String>,
}

/// What a part says of the OLE objects it embeds, by the id of the relationship to each.
pub(super) type OleReferences = HashMap<String, OleReference>;

/// What kind of note a reference inlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoteKind {
    Footnote,
    Endnote,
    Comment,
}

impl NoteKind {
    pub(super) fn class(self) -> &'static str {
        match self {
            NoteKind::Footnote => "footnote",
            NoteKind::Endnote => "endnote",
            NoteKind::Comment => "comment",
        }
    }
}

/// The paragraph information the text handler gathers before a paragraph starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ParagraphProperties {
    pub(super) style_id: Option<String>,
    pub(super) level: i32,
    pub(super) num_id: i32,
}

impl ParagraphProperties {
    pub(super) fn new() -> Self {
        Self {
            style_id: None,
            level: -1,
            num_id: -1,
        }
    }
}

/// What a body handler writes besides the text itself, which the handlers of inlined notes and
/// comments do without.
pub(crate) struct Context<'c> {
    pub(super) styles: Option<&'c Styles>,
    pub(super) lists: &'c mut ListManager,
    pub(super) inline: &'c InlineParts,
    /// Whether the handler records features of the document in its metadata.
    pub(super) records_metadata: bool,
    pub(super) include_deleted: bool,
    pub(super) include_move_from: bool,
}

impl<'c> Context<'c> {
    /// What `new OOXMLTikaBodyPartHandler(xhtml)` writes with, or with `(xhtml, metadata)` when
    /// it `records_metadata`: no styles, list numbers or notes, and no deleted or moved text.
    pub(crate) fn plain(
        lists: &'c mut ListManager,
        inline: &'c InlineParts,
        records_metadata: bool,
    ) -> Self {
        Self {
            styles: None,
            lists,
            inline,
            records_metadata,
            include_deleted: false,
            include_move_from: false,
        }
    }
}

/// `OOXMLTikaBodyPartHandler`.
pub(crate) struct Body<'x, 'h, 'c> {
    xhtml: &'x mut Xhtml<'h>,
    context: Context<'c>,
    formatting: Formatting,
    paragraph_depth: i32,
    table_depth: i32,
    cell_depth: i32,
    paragraphs_in_cell: i32,
    paragraph_tag: String,
    /// The paragraphs, tables, rows and cells written and not closed yet, innermost last.
    open: Vec<String>,
    pending_comments: Vec<String>,
    emitted_comments: HashSet<String>,
    ole_references: OleReferences,
}

impl<'x, 'h, 'c> Body<'x, 'h, 'c> {
    pub(crate) fn new(xhtml: &'x mut Xhtml<'h>, context: Context<'c>) -> Self {
        Self {
            xhtml,
            context,
            formatting: Formatting::default(),
            paragraph_depth: 0,
            table_depth: 0,
            cell_depth: 0,
            paragraphs_in_cell: 0,
            paragraph_tag: "p".to_owned(),
            open: Vec::new(),
            pending_comments: Vec::new(),
            emitted_comments: HashSet::new(),
            ole_references: OleReferences::new(),
        }
    }

    /// The comments this handler inlined, by id, and what the part said of the OLE objects it
    /// embeds.
    pub(super) fn finish(self) -> (HashSet<String>, OleReferences) {
        (self.emitted_comments, self.ole_references)
    }

    pub(super) fn include_deleted(&self) -> bool {
        self.context.include_deleted
    }

    pub(super) fn include_move_from(&self) -> bool {
        self.context.include_move_from
    }

    pub(super) fn run(&mut self, properties: &RunProperties, text: &str) -> Result<(), Stop> {
        self.formatting.apply(self.xhtml, properties)?;
        self.xhtml.characters(text)
    }

    pub(super) fn hyperlink_start(&mut self, link: Option<&str>) -> Result<(), Stop> {
        self.formatting.open_hyperlink(self.xhtml, link)
    }

    pub(super) fn hyperlink_end(&mut self) -> Result<(), Stop> {
        self.formatting.close_hyperlink(self.xhtml)
    }

    pub(super) fn start_paragraph(&mut self, properties: &ParagraphProperties) -> Result<(), Stop> {
        if self.cell_depth > 0 && self.paragraphs_in_cell > 0 {
            self.xhtml.characters("\n")?;
        }
        if self.paragraph_depth == 0 && self.table_depth == 0 {
            let mut tag = "p".to_owned();
            let mut class = None;
            if let (Some(id), Some(styles)) = (&properties.style_id, self.context.styles) {
                if let Some(name) = styles.name(id) {
                    let tag_and_style = paragraph_tag_and_style(name);
                    tag = tag_and_style.tag;
                    class = tag_and_style.class;
                }
            }
            match &class {
                Some(class) => self.xhtml.start_element_with(&tag, &[("class", class)])?,
                None => self.xhtml.start_element(&tag)?,
            }
            self.open.push(tag.clone());
            self.paragraph_tag = tag;
        }
        if properties.level >= 0 && properties.num_id >= 0 {
            let number = self
                .context
                .lists
                .formatted_number(properties.num_id, properties.level);
            self.xhtml.characters(&number)?;
        }
        self.paragraph_depth += 1;
        Ok(())
    }

    pub(super) fn end_paragraph(&mut self) -> Result<(), Failure> {
        self.formatting.close_all(self.xhtml)?;
        if self.paragraph_depth == 1 && self.table_depth == 0 {
            let tag = self.paragraph_tag.clone();
            self.xhtml.end_element(&tag)?;
            self.close_expected(&tag);
        } else if self.cell_depth == 0 || self.paragraphs_in_cell > 0 {
            self.xhtml.characters("\n")?;
        }
        self.emit_pending_comments()?;
        if self.cell_depth > 0 {
            self.paragraphs_in_cell += 1;
        }
        self.paragraph_depth -= 1;
        Ok(())
    }

    fn emit_pending_comments(&mut self) -> Result<(), Failure> {
        let pending = std::mem::take(&mut self.pending_comments);
        let inline = self.context.inline;
        for id in pending {
            if let Some(events) = inline.comments.get(&id) {
                inline_note(self.xhtml, inline, events, NoteKind::Comment)?;
                self.emitted_comments.insert(id);
            }
        }
        Ok(())
    }

    /// `popExpected`: forgets the innermost open element if it is `tag`.
    fn close_expected(&mut self, tag: &str) {
        if self.open.last().is_some_and(|open| open == tag) {
            self.open.pop();
        }
    }

    /// `closeAnyPending`: closes the formatting and the elements left open, innermost first.
    fn close_any_pending(&mut self) -> Result<(), Stop> {
        self.formatting.close_all(self.xhtml)?;
        while let Some(tag) = self.open.pop() {
            self.xhtml.end_element(&tag)?;
        }
        self.table_depth = 0;
        self.cell_depth = 0;
        self.paragraph_depth = 0;
        self.paragraphs_in_cell = 0;
        Ok(())
    }

    pub(super) fn start_table(&mut self) -> Result<(), Stop> {
        self.formatting.close_all(self.xhtml)?;
        self.xhtml.start_element("table")?;
        self.open.push("table".to_owned());
        self.table_depth += 1;
        Ok(())
    }

    pub(super) fn end_table(&mut self) -> Result<(), Stop> {
        self.xhtml.end_element("table")?;
        self.close_expected("table");
        self.table_depth -= 1;
        Ok(())
    }

    pub(super) fn start_row(&mut self) -> Result<(), Stop> {
        self.xhtml.start_element("tr")?;
        self.open.push("tr".to_owned());
        Ok(())
    }

    pub(super) fn end_row(&mut self) -> Result<(), Stop> {
        self.xhtml.end_element("tr")?;
        self.close_expected("tr");
        Ok(())
    }

    pub(super) fn start_cell(&mut self) -> Result<(), Stop> {
        self.xhtml.start_element("td")?;
        self.open.push("td".to_owned());
        self.cell_depth += 1;
        Ok(())
    }

    pub(super) fn end_cell(&mut self) -> Result<(), Stop> {
        self.xhtml.end_element("td")?;
        self.close_expected("td");
        self.paragraphs_in_cell = 0;
        self.cell_depth -= 1;
        Ok(())
    }

    pub(super) fn footnote_reference(&mut self, id: Option<&str>) -> Result<(), Failure> {
        let inline = self.context.inline;
        self.note_reference(&inline.footnotes, NoteKind::Footnote, id)
    }

    pub(super) fn endnote_reference(&mut self, id: Option<&str>) -> Result<(), Failure> {
        let inline = self.context.inline;
        self.note_reference(&inline.endnotes, NoteKind::Endnote, id)
    }

    /// A note inline where it is referenced, or its id in brackets when the document has no
    /// such note.
    fn note_reference(
        &mut self,
        notes: &HashMap<String, Vec<Recorded>>,
        kind: NoteKind,
        id: Option<&str>,
    ) -> Result<(), Failure> {
        let Some(id) = id else {
            return Ok(());
        };
        let inline = self.context.inline;
        match notes.get(id) {
            Some(events) => inline_note(self.xhtml, inline, events, kind),
            None => {
                self.xhtml.characters("[")?;
                self.xhtml.characters(id)?;
                Ok(self.xhtml.characters("]")?)
            }
        }
    }

    pub(super) fn comment_reference(&mut self, id: Option<&str>) {
        if let Some(id) = id {
            self.pending_comments.push(id.to_owned());
        }
    }

    /// `embeddedOLERef`: a marker of the object, which the program and the picture showing it,
    /// when the part gives either, describe.
    pub(super) fn embedded_ole_reference(
        &mut self,
        relationship: Option<&str>,
        prog_id: Option<&str>,
        icon: Option<&str>,
    ) -> Result<(), Stop> {
        let Some(relationship) = relationship else {
            return Ok(());
        };
        let given =
            |value: Option<&str>| value.filter(|value| !value.is_empty()).map(str::to_owned);
        let (prog_id, icon) = (given(prog_id), given(icon));
        if prog_id.is_some() || icon.is_some() {
            self.ole_references
                .insert(relationship.to_owned(), OleReference { prog_id, icon });
        }
        self.xhtml
            .start_element_with("div", &[("class", "embedded"), ("id", relationship)])?;
        self.xhtml.end_element("div")
    }

    pub(super) fn linked_ole_reference(&mut self, relationship: Option<&str>) -> Result<(), Stop> {
        let Some(relationship) = relationship else {
            return Ok(());
        };
        if self.context.records_metadata {
            self.xhtml
                .metadata_mut()
                .set(HAS_LINKED_OLE_OBJECTS, "true");
        }
        self.xhtml.start_element_with(
            "a",
            &[("class", "external-ref-linkedOle"), ("id", relationship)],
        )?;
        self.xhtml.end_element("a")
    }

    pub(super) fn embedded_picture(
        &mut self,
        file_name: Option<&str>,
        description: Option<&str>,
    ) -> Result<(), Stop> {
        let source = file_name.map(|name| format!("embedded:{name}"));
        let mut attributes: Vec<(&str, &str)> = Vec::with_capacity(2);
        if let Some(source) = &source {
            attributes.push(("src", source));
        }
        if let Some(description) = description {
            attributes.push(("alt", description));
        }
        self.xhtml.start_element_with("img", &attributes)?;
        self.xhtml.end_element("img")
    }

    pub(super) fn field_hyperlink_start(&mut self, link: &str) -> Result<(), Stop> {
        if self.context.records_metadata {
            self.xhtml.metadata_mut().set(HAS_FIELD_HYPERLINKS, "true");
        }
        self.hyperlink_start(Some(link))
    }

    pub(super) fn external_reference(&mut self, kind: &str, url: Option<&str>) -> Result<(), Stop> {
        let Some(url) = url.filter(|url| !url.is_empty()) else {
            return Ok(());
        };
        if self.context.records_metadata {
            let flag = match kind {
                "hlinkHover" => HAS_HOVER_HYPERLINKS,
                "vml-shape-href" => HAS_VML_HYPERLINKS,
                _ => HAS_FIELD_HYPERLINKS,
            };
            self.xhtml.metadata_mut().set(flag, "true");
        }
        let class = format!("external-ref-{kind}");
        self.xhtml
            .start_element_with("a", &[("class", &class), ("href", url)])?;
        self.xhtml.end_element("a")
    }

    pub(super) fn bookmark(&mut self, name: Option<&str>) -> Result<(), Stop> {
        if let Some(name) = name {
            if !self.formatting.hyperlink_active() {
                self.xhtml.start_element_with("a", &[("name", name)])?;
                self.xhtml.end_element("a")?;
            }
        }
        Ok(())
    }
}

/// Writes a note or comment where it is referenced, as `inlineNoteContent` parses it with a
/// handler of its own, without styles, lists or notes of its own. Tika catches the limit of a
/// string extraction being reached in the note, closes what the note left open and goes on.
pub(super) fn inline_note(
    xhtml: &mut Xhtml,
    inline: &InlineParts,
    events: &[Recorded],
    kind: NoteKind,
) -> Result<(), Failure> {
    xhtml.start_element_with("div", &[("class", kind.class())])?;
    if play_note(xhtml, inline, events, true)? == Played::LimitReached {
        xhtml.characters(&format!("[{} parse error]", kind.class()))?;
    }
    Ok(xhtml.end_element("div")?)
}

/// Writes a comment no paragraph refers to, as `handleUnreferencedComments` does: on reaching
/// the limit of a string extraction, Tika records the exception and leaves the comment open.
pub(super) fn unreferenced_comment(
    xhtml: &mut Xhtml,
    inline: &InlineParts,
    events: &[Recorded],
) -> Result<(), Failure> {
    xhtml.start_element_with("div", &[("class", NoteKind::Comment.class())])?;
    if play_note(xhtml, inline, events, false)? == Played::Done {
        xhtml.end_element("div")?;
    }
    Ok(())
}

/// How playing a note ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Played {
    Done,
    /// The limit of a string extraction was reached, which Tika catches.
    LimitReached,
}

/// Plays the events of a note to a handler of its own; on reaching the limit, closes what the
/// note left open when `close`.
fn play_note(
    xhtml: &mut Xhtml,
    inline: &InlineParts,
    events: &[Recorded],
    close: bool,
) -> Result<Played, Failure> {
    let mut lists = ListManager::new(None);
    let empty = InlineParts::default();
    let body = Body::new(xhtml, Context::plain(&mut lists, &empty, false));
    let mut handler = TextHandler::new(body, &inline.relationships, TextOptions::default());
    match events.iter().try_for_each(|event| event.play(&mut handler)) {
        Ok(()) => Ok(Played::Done),
        Err(Failure::Stopped(Stop::LimitReached)) => {
            if close {
                handler.into_body().close_any_pending()?;
            }
            Ok(Played::LimitReached)
        }
        Err(failure) => Err(failure),
    }
}

/// `FormattingTagManager`: the open link and formatting, nested link, bold, italic,
/// strike-through, underline from the outside in.
#[derive(Debug, Default)]
struct Formatting {
    hyperlink: Option<String>,
    wrapper_hyperlink: bool,
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
}

impl Formatting {
    fn open_hyperlink(&mut self, xhtml: &mut Xhtml, url: Option<&str>) -> Result<(), Stop> {
        let Some(url) = url else {
            return Ok(());
        };
        self.close_formatting(xhtml)?;
        if self.hyperlink.is_some() {
            xhtml.end_element("a")?;
        }
        xhtml.start_element_with("a", &[("href", url)])?;
        self.hyperlink = Some(url.to_owned());
        self.wrapper_hyperlink = true;
        Ok(())
    }

    fn close_hyperlink(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if self.hyperlink.is_some() && self.wrapper_hyperlink {
            self.close_formatting(xhtml)?;
            xhtml.end_element("a")?;
            self.hyperlink = None;
            self.wrapper_hyperlink = false;
        }
        Ok(())
    }

    fn hyperlink_active(&self) -> bool {
        self.hyperlink.is_some()
    }

    fn apply(&mut self, xhtml: &mut Xhtml, run: &RunProperties) -> Result<(), Stop> {
        if !self.wrapper_hyperlink && run.hyperlink != self.hyperlink {
            self.close_formatting(xhtml)?;
            if self.hyperlink.is_some() {
                xhtml.end_element("a")?;
            }
            if let Some(url) = &run.hyperlink {
                xhtml.start_element_with("a", &[("href", url)])?;
            }
            self.hyperlink.clone_from(&run.hyperlink);
        }
        if run.bold != self.bold {
            self.close_underline(xhtml)?;
            self.close_strike(xhtml)?;
            self.close_italic(xhtml)?;
            toggle(xhtml, "b", run.bold)?;
            self.bold = run.bold;
        }
        if run.italic != self.italic {
            self.close_underline(xhtml)?;
            self.close_strike(xhtml)?;
            toggle(xhtml, "i", run.italic)?;
            self.italic = run.italic;
        }
        if run.strike != self.strike {
            self.close_underline(xhtml)?;
            toggle(xhtml, "s", run.strike)?;
            self.strike = run.strike;
        }
        if run.underline != self.underline {
            toggle(xhtml, "u", run.underline)?;
            self.underline = run.underline;
        }
        Ok(())
    }

    fn close_all(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        self.close_formatting(xhtml)?;
        if self.hyperlink.is_some() {
            xhtml.end_element("a")?;
            self.hyperlink = None;
            self.wrapper_hyperlink = false;
        }
        Ok(())
    }

    fn close_formatting(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        self.close_underline(xhtml)?;
        self.close_strike(xhtml)?;
        self.close_italic(xhtml)?;
        if self.bold {
            xhtml.end_element("b")?;
            self.bold = false;
        }
        Ok(())
    }

    fn close_underline(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if self.underline {
            xhtml.end_element("u")?;
            self.underline = false;
        }
        Ok(())
    }

    fn close_strike(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if self.strike {
            xhtml.end_element("s")?;
            self.strike = false;
        }
        Ok(())
    }

    fn close_italic(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if self.italic {
            xhtml.end_element("i")?;
            self.italic = false;
        }
        Ok(())
    }
}

fn toggle(xhtml: &mut Xhtml, element: &str, on: bool) -> Result<(), Stop> {
    if on {
        xhtml.start_element(element)
    } else {
        xhtml.end_element(element)
    }
}
