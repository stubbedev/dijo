//! Tika's `HSLFExtractor`: the slides with their headers, master text, text, tables, grouped
//! text, footers, comments, notes and embedded objects, then the pictures and the objects no
//! shape showed, as the events it writes, handed on a slide, a picture or an object at a time.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::embedded::{Wrapping, RELATIONSHIP_ID, RESOURCE_NAME};
use crate::formats::metadata::office::{
    COMMENT_PERSONS, HAS_ANIMATIONS, HAS_HIDDEN_SLIDES, NUM_HIDDEN_SLIDES,
    RESOURCE_NAME_EXTENSION_INFERRED,
};
use crate::formats::metadata::{apply_page_numbers, TikaMetadata};
use crate::formats::ole2::{office_document, Embedded, Handed};
use crate::formats::poi::{Thrown, Unread};
use crate::formats::{Decline, Difference, Unported};
use crate::java::is_blank;
use crate::mime::{self, MediaType, Origin};
use crate::ole2::Node;
use crate::ole2::{self, Ole2, Unreadable};

use super::headers::{notes_sheet, HeadersFooters, Part, Scope};
use super::pictures::{Object, Pictures, StoreEntry, UnreadData};
use super::records::kind;
use super::shapes::{sheet_shapes, Kind, Shape, BLIP_TO_DISPLAY, PICTURE_ID};
use super::show::{Records, Sheet, Show};
use super::text::Paragraph;

const CONTENT_TYPE: &str = "Content-Type";
/// `SSSlideInfoAtom.HIDDEN_BIT`.
const HIDDEN_BIT: i16 = 1 << 2;
/// The record of an extended time node of a slide's animations.
const EXT_TIME_NODE_CONTAINER: u16 = 0xF144;
const VERTICAL_TAB: u16 = 0x0B;
const CARRIAGE_RETURN: u16 = 0x0D;
const COMP_OBJ: &str = "application/x-tika-msoffice-embedded; format=comp_obj";
const MSOFFICE: &str = "application/x-tika-msoffice";

/// An event of the extractor's output.
#[derive(Debug)]
pub(super) enum Out<'a> {
    Start(&'static str),
    StartClass(&'static str, &'static str),
    StartWith(&'static str, Vec<(&'static str, String)>),
    End(&'static str),
    /// Characters, never empty.
    Text(Vec<u16>),
    Embedded(Handed<'a>, Wrapping),
    /// An object of a shape, handed on bare by `handleDataStream`, which catches what its parse
    /// throws and records it.
    Object(Handed<'a>),
    /// A value added to the presentation's metadata.
    Add(&'static str, String),
    /// A value set in the presentation's metadata.
    Set(&'static str, String),
    /// An exception Tika records as a warning before it goes on, as Java prints its first line.
    Warning(String),
}

fn recorded(reason: &str) -> Unported {
    Unported(format!(
        "an embedded object or picture Tika fails on and records with Java's stack trace: \
         {reason}"
    ))
}

/// Why the extraction stops: POI reads the presentation otherwise than Tika's path, or the
/// events handed on fail to be written.
pub(super) enum Halt<E> {
    Declined(Decline<'static>),
    Written(E),
}

impl<E> From<Unread> for Halt<E> {
    fn from(unread: Unread) -> Self {
        Halt::Declined(unread.into())
    }
}

impl<E> From<Unported> for Halt<E> {
    fn from(unported: Unported) -> Self {
        Halt::Declined(unported.into())
    }
}

impl<E> From<Decline<'static>> for Halt<E> {
    fn from(declined: Decline<'static>) -> Self {
        Halt::Declined(declined)
    }
}

/// Where the extractor hands its events on, whatever they borrow.
pub(super) type Flush<'f, E> = dyn for<'x> FnMut(Vec<Out<'x>>) -> Result<(), E> + 'f;

/// The extractor's state while it runs: its events not handed on yet, the comment authors met
/// and the objects shapes showed.
struct Extractor<'s, 'a> {
    show: &'s Show<'a>,
    records: &'s Records,
    pictures: &'s Pictures<'a>,
    objects: &'s [Object],
    config: &'s OfficeParserConfig,
    configs: &'s ParserConfigs,
    out: Vec<Out<'a>>,
    authors: HashSet<String>,
    processed: HashSet<i32>,
    has_animations: bool,
    /// The first picture or object whose data POI fails to read, which Tika records before it
    /// goes on and the native reading leaves out.
    difference: Option<Difference>,
}

/// `HSLFExtractor.parse`: the presentation's events, handed to `flush` a slide, a picture or an
/// object at a time, each slide's sheet built from its record as it is written; and where they
/// differ from Tika's, if they do.
pub(super) fn extract<'s, 'a, E>(
    show: &'s Show<'a>,
    records: &'s Records,
    objects: &'s [Object],
    pictures: &'s Pictures<'a>,
    configs: &'s ParserConfigs,
    flush: &mut Flush<'_, E>,
) -> Result<Option<Difference>, Halt<E>> {
    let mut extractor = Extractor {
        show,
        records,
        pictures,
        objects,
        config: &configs.office,
        configs,
        out: Vec::new(),
        authors: HashSet::new(),
        processed: HashSet::new(),
        has_animations: false,
        difference: None,
    };
    extractor.out.push(Out::StartClass("div", "slideShow"));
    let mut hidden = 0;
    for slide in &show.slides {
        let record = show.slide_record(slide)?;
        let sheet = show.slide_sheet(slide, &record)?;
        extractor.slide(&sheet, slide.notes)?;
        if is_hidden(sheet.container)? {
            hidden += 1;
        }
        extractor.find_animations(sheet.container);
        extractor.flush(flush)?;
    }
    if hidden > 0 {
        extractor
            .out
            .push(Out::Set(NUM_HIDDEN_SLIDES, hidden.to_string()));
        extractor
            .out
            .push(Out::Set(HAS_HIDDEN_SLIDES, "true".to_owned()));
    }
    extractor.pictures(flush)?;
    extractor.show_objects(flush)?;
    extractor.out.push(Out::End("div"));
    extractor.flush(flush)?;
    Ok(extractor.difference)
}

/// `HSLFSlide.isHidden`.
fn is_hidden(container: &super::records::Record) -> Result<bool, Unread> {
    let Some(info) = container.first(kind::SS_SLIDE_INFO_ATOM) else {
        return Ok(false);
    };
    let flags = info
        .short(10)
        .map_err(|_| Unported("a short slide show info atom".to_owned()))?;
    Ok(flags & HIDDEN_BIT != 0)
}

impl<'s, 'a> Extractor<'s, 'a> {
    /// Hands the events so far on.
    fn flush<E>(&mut self, flush: &mut Flush<'_, E>) -> Result<(), Halt<E>> {
        flush(std::mem::take(&mut self.out)).map_err(Halt::Written)
    }
    fn start_class(&mut self, tag: &'static str, class: &'static str) {
        self.out.push(Out::StartClass(tag, class));
    }

    fn end(&mut self, tag: &'static str) {
        self.out.push(Out::End(tag));
    }

    /// `xhtml.characters(String)`: nothing for empty text.
    fn characters(&mut self, text: &[u16]) {
        if !text.is_empty() {
            self.out.push(Out::Text(text.to_vec()));
        }
    }

    /// `xhtml.element(name, value)`: nothing for an empty value.
    fn element(&mut self, tag: &'static str, text: &[u16]) {
        if text.is_empty() {
            return;
        }
        self.out.push(Out::Start(tag));
        self.characters(text);
        self.end(tag);
    }

    fn slide(&mut self, sheet: &Sheet<'_>, notes: Option<usize>) -> Result<(), Unread> {
        let show = self.show;
        self.start_class("div", "slide");
        let headers = self.config.includes_headers_and_footers();
        let slide_headers = if headers {
            Some(HeadersFooters::new(show, sheet, Scope::Slide)?)
        } else {
            None
        };
        let first_notes = match show.notes.first().filter(|_| headers) {
            Some(entry) => Some((entry, show.notes_record(entry)?)),
            None => None,
        };
        let first_notes = match &first_notes {
            Some((entry, record)) => Some(show.notes_sheet(entry, record)?),
            None => None,
        };
        let notes_headers = if headers {
            Some(HeadersFooters::new(
                show,
                notes_sheet(show, first_notes.as_ref())?,
                Scope::Notes,
            )?)
        } else {
            None
        };
        if let Some(headers) = &slide_headers {
            self.header_or_footer(headers, Part::Header, "slide-header")?;
        }
        if self.config.includes_slide_master_content() {
            if let Some(master) = self.show.master_of(sheet) {
                self.master(master.sheet())?;
            }
        }
        self.start_class("div", "slide-content");
        let shapes = sheet_shapes(sheet)?;
        self.sheet_text(sheet, &shapes)?;
        self.end("div");
        if let Some(headers) = &slide_headers {
            self.header_or_footer(headers, Part::Footer, "slide-footer")?;
        }
        self.comments(sheet);
        if let Some(index) = notes.filter(|_| self.config.includes_slide_notes()) {
            let entry = &show.notes[index];
            let record = show.notes_record(entry)?;
            let notes = show.notes_sheet(entry, &record)?;
            self.notes(&notes, notes_headers.as_ref())?;
        }
        self.embedded_objects(&shapes)?;
        self.end("div");
        Ok(())
    }

    /// A header or footer in a paragraph of its class, where it is visible and has text.
    fn header_or_footer(
        &mut self,
        headers: &HeadersFooters<'_, '_>,
        part: Part,
        class: &'static str,
    ) -> Result<(), Unread> {
        if !headers.is_visible(part)? {
            return Ok(());
        }
        let Some(text) = headers.text(part)? else {
            return Ok(());
        };
        self.start_class("p", class);
        self.characters(&text);
        self.end("p");
        Ok(())
    }

    /// The text of a slide's or notes' lists, tables and groups.
    fn sheet_text<'r>(&mut self, sheet: &Sheet<'r>, shapes: &[Shape<'r>]) -> Result<(), Unread> {
        let lists: Vec<Vec<&Paragraph>> = sheet
            .lists
            .iter()
            .map(|list| list.iter().map(|&id| &sheet.paragraphs[id]).collect())
            .collect();
        self.runs_to_text(sheet, &lists)?;
        for shape in shapes {
            if shape.kind == Kind::Table {
                self.table(sheet, shape)?;
            }
        }
        self.group_text(sheet, shapes, 0)
    }

    /// `extractMaster`: the text of the master's shapes that are no placeholders.
    fn master(&mut self, master: &Sheet<'_>) -> Result<(), Unread> {
        let shapes = sheet_shapes(master)?;
        if shapes.is_empty() {
            return Ok(());
        }
        self.start_class("div", "slide-master-content");
        for shape in &shapes {
            if shape.kind.is_text() && !shape.is_placeholder() {
                let text = shape.text(master)?;
                self.element("p", &text);
            }
        }
        self.end("div");
        Ok(())
    }

    /// `extractTableText`.
    fn table<'r>(&mut self, sheet: &Sheet<'r>, table: &Shape<'r>) -> Result<(), Unread> {
        self.out.push(Out::Start("table"));
        let columns = table.cells.iter().map(Vec::len).max().unwrap_or(0);
        for row in &table.cells {
            self.out.push(Out::Start("tr"));
            for column in 0..columns {
                let text = match row.get(column).copied().flatten() {
                    Some(cell) => table.children[cell].text(sheet)?,
                    None => Vec::new(),
                };
                self.element("td", &text);
            }
            self.end("tr");
        }
        self.end("table");
        Ok(())
    }

    /// `extractGroupText`: the text of the text shapes in groups, the deepest first.
    fn group_text<'r>(
        &mut self,
        sheet: &Sheet<'r>,
        shapes: &[Shape<'r>],
        depth: usize,
    ) -> Result<(), Unread> {
        let mut lists = Vec::new();
        for shape in shapes {
            if shape.kind.is_group() {
                self.group_text(sheet, &shape.children, depth + 1)?;
            } else if shape.kind.is_text() && shape.kind != Kind::TableCell && depth > 0 {
                lists.push(shape.paragraphs(sheet));
            }
        }
        self.runs_to_text(sheet, &lists)
    }

    /// `textRunsToText`.
    fn runs_to_text(
        &mut self,
        sheet: &Sheet<'_>,
        lists: &[Vec<&Paragraph<'_>>],
    ) -> Result<(), Unread> {
        for list in lists {
            let mut is_bullet = false;
            for paragraph in list {
                let next_bullet = self.show.is_bullet(paragraph, sheet)?;
                if is_bullet != next_bullet {
                    is_bullet = next_bullet;
                    if is_bullet {
                        self.out.push(Out::Start("ul"));
                    } else {
                        self.end("ul");
                    }
                }
                let runs = &paragraph.runs;
                let first_line = remove_paragraph_break(&runs[0].text);
                let show_bullet = is_bullet && (runs.len() > 1 || !first_line.is_empty());
                let tag = if show_bullet { "li" } else { "p" };
                self.out.push(Out::Start(tag));
                for run in runs {
                    let mut linked = false;
                    if let Some(address) = run
                        .link
                        .as_ref()
                        .and_then(|link| link.address.as_ref())
                        .filter(|address| !address.starts_with("_ftn"))
                    {
                        self.out
                            .push(Out::StartWith("a", vec![("href", address.clone())]));
                        linked = true;
                    }
                    let line = &run.text;
                    for (index, fragment) in split_vertical_tabs(line).into_iter().enumerate() {
                        if index > 0 {
                            self.line_break();
                        }
                        self.characters(&remove_paragraph_break(fragment));
                    }
                    if line.last() == Some(&VERTICAL_TAB) {
                        self.line_break();
                    }
                    if linked {
                        self.end("a");
                    }
                }
                self.end(tag);
            }
            if is_bullet {
                self.end("ul");
            }
        }
        Ok(())
    }

    fn line_break(&mut self) {
        self.out.push(Out::Start("br"));
        self.end("br");
    }

    /// `handleComments`.
    fn comments(&mut self, sheet: &Sheet<'_>) {
        let comments: Vec<&super::records::Record> = sheet
            .container
            .first(kind::PROG_TAGS)
            .and_then(|tags| tags.first(kind::PROG_BINARY_TAG))
            .and_then(|tag| tag.first(kind::BINARY_TAG_DATA))
            .map(|data| {
                data.children()
                    .iter()
                    .filter(|child| child.kind == kind::COMMENT2000)
                    .collect()
            })
            .unwrap_or_default();
        if comments.is_empty() {
            return;
        }
        self.start_class("div", "slide-comments");
        for comment in comments {
            let string = |instance: i32| {
                comment
                    .children()
                    .iter()
                    .rev()
                    .find(|child| child.kind == kind::C_STRING && child.instance() == instance)
                    .map(|string| String::from_utf16_lossy(&string.utf16()))
            };
            let (author, text, initials) = (string(0), string(1), string(2));
            self.start_class("p", "slide-comment");
            let mut heading = String::new();
            if let Some(author) = author.filter(|author| !is_blank(author)) {
                heading.push_str(&author);
                if self.authors.insert(author.clone()) {
                    self.out.push(Out::Add(COMMENT_PERSONS, author));
                }
            }
            if let Some(initials) = initials {
                if !heading.is_empty() {
                    heading.push(' ');
                }
                heading.push('(');
                heading.push_str(&initials);
                heading.push(')');
            }
            if !heading.is_empty() {
                if text.is_some() {
                    heading.push_str(" - ");
                }
                self.out.push(Out::Start("b"));
                self.characters(&heading.encode_utf16().collect::<Vec<_>>());
                self.end("b");
            }
            if let Some(text) = text {
                self.characters(&text.encode_utf16().collect::<Vec<_>>());
            }
            self.end("p");
        }
        self.end("div");
    }

    /// `handleNotes`.
    fn notes(
        &mut self,
        notes: &Sheet<'_>,
        headers: Option<&HeadersFooters<'_, '_>>,
    ) -> Result<(), Unread> {
        self.start_class("div", "notes");
        if let Some(headers) = headers {
            self.header_or_footer(headers, Part::Header, "slide-note-header")?;
        }
        self.start_class("div", "notes-content");
        let shapes = sheet_shapes(notes)?;
        self.sheet_text(notes, &shapes)?;
        self.end("div");
        if let Some(headers) = headers {
            self.header_or_footer(headers, Part::Footer, "slide-note-footer")?;
        }
        self.embedded_objects(&shapes)?;
        self.end("div");
        Ok(())
    }

    /// `findAnimations`: whether the slide's binary tags hold an extended time node, once.
    fn find_animations(&mut self, container: &super::records::Record) {
        if self.has_animations {
            return;
        }
        let data = container
            .first(kind::PROG_TAGS)
            .and_then(|tags| tags.first(kind::PROG_BINARY_TAG))
            .and_then(|tag| tag.first(kind::BINARY_TAG_DATA));
        if data.is_some_and(|data| data.first(EXT_TIME_NODE_CONTAINER).is_some()) {
            self.has_animations = true;
            self.out.push(Out::Set(HAS_ANIMATIONS, "true".to_owned()));
        }
    }

    /// `handleSlideEmbeddedResources`: a marker for each OLE object shape, and its object.
    fn embedded_objects(&mut self, shapes: &[Shape<'_>]) -> Result<(), Unread> {
        for shape in shapes {
            if shape.kind != Kind::Object {
                continue;
            }
            let Some(object) = self.object_of(shape)? else {
                continue;
            };
            let id = shape.property(PICTURE_ID)?.to_string();
            self.processed.insert(self.objects[object].persist_id);
            self.out.push(Out::StartWith(
                "div",
                vec![("class", "embedded".to_owned()), ("id", id.clone())],
            ));
            self.end("div");
            let prog_id = self.prog_id(shape)?;
            let Some(data) = self.readable(self.objects[object].data(self.show)?)? else {
                continue;
            };
            self.data_stream(data, &id, prog_id.as_deref())?;
        }
        Ok(())
    }

    /// The data of a picture or an object, or none where POI fails reading it, which Tika
    /// records before it goes on, where the native reading leaves the picture or object out.
    fn readable(
        &mut self,
        data: Result<Cow<'a, [u8]>, UnreadData>,
    ) -> Result<Option<Cow<'a, [u8]>>, Unread> {
        match data {
            Ok(data) => Ok(Some(data)),
            Err(UnreadData::Thrown(exception)) => {
                self.out.push(Out::Warning(exception));
                Ok(None)
            }
            Err(UnreadData::Recorded(reason)) => {
                let Unported(reason) = super::pictures::recorded(reason);
                self.difference.get_or_insert(Difference(reason));
                Ok(None)
            }
            Err(UnreadData::Unported(unported)) => Err(unported.into()),
        }
    }

    /// `getExEmbed`: the last embedding of the document's objects with the shape's object id.
    fn embedding(
        &self,
        shape: &Shape<'_>,
    ) -> Result<Option<&'s super::records::Record<'a>>, Unread> {
        let Some(objects) = self.show.objects() else {
            return Ok(None);
        };
        let id = shape.property(PICTURE_ID)?;
        let mut found = None;
        for child in objects.children() {
            if child.kind != kind::EX_EMBED && child.kind != kind::EX_CONTROL {
                continue;
            }
            let atom = child
                .children()
                .get(1)
                .filter(|atom| atom.kind == kind::EX_OLE_OBJ_ATOM)
                .ok_or_else(|| recorded("an embedding without its OLE object atom"))?;
            if atom
                .int(8)
                .map_err(|_| recorded("a short OLE object atom"))?
                == id
            {
                found = Some(child);
            }
        }
        Ok(found)
    }

    /// `getObjectData`: the last object whose persist id the shape's embedding names.
    fn object_of(&self, shape: &Shape<'_>) -> Result<Option<usize>, Unread> {
        let Some(embedding) = self.embedding(shape)? else {
            return Ok(None);
        };
        let reference = embedding.children()[1]
            .int(16)
            .map_err(|_| recorded("a short OLE object atom"))?;
        Ok(self
            .objects
            .iter()
            .rposition(|object| object.persist_id == reference))
    }

    /// `getProgId`: the program id of the shape's embedding.
    fn prog_id(&self, shape: &Shape<'_>) -> Result<Option<String>, Unread> {
        Ok(self.embedding(shape)?.and_then(|embedding| {
            embedding
                .children()
                .iter()
                .skip(2)
                .rev()
                .find(|child| child.kind == kind::C_STRING && child.instance() == 2)
                .map(|string| String::from_utf16_lossy(&string.utf16()))
        }))
    }

    /// `handleDataStream`: an object handed on as the compound document Tika's detection names
    /// it, or as itself.
    fn data_stream(
        &mut self,
        data: Cow<'a, [u8]>,
        id: &str,
        prog_id: Option<&str>,
    ) -> Result<(), Unread> {
        let media_type = if prog_id == Some("Excel.Chart.8") {
            MediaType::named("application/vnd.ms-excel")
        } else {
            detect(&data)?
        };
        if media_type == MediaType::parse(COMP_OBJ).expect("a media type")
            || media_type == MediaType::named(MSOFFICE)
        {
            if let Some(embedded) =
                embedded_office_document(data, TikaMetadata::new(), id, self.configs)?
            {
                self.out.push(Out::Object(embedded));
            }
            return Ok(());
        }
        let mut metadata = TikaMetadata::new();
        metadata.set(RESOURCE_NAME, id);
        metadata.set(RELATIONSHIP_ID, id);
        metadata.set(CONTENT_TYPE, media_type.to_string());
        self.out.push(Out::Object(Handed::Document(Embedded {
            data: Node::from(data),
            metadata,
        })));
        Ok(())
    }

    /// `handleSlideEmbeddedPictures`: each picture, with the slides that show it, read from the
    /// stream and handed on one at a time.
    fn pictures<E>(&mut self, flush: &mut Flush<'_, E>) -> Result<(), Halt<E>> {
        let show = self.show;
        let pictures = self.pictures;
        let store = self.lookup_store()?;
        let mut slides_of: BTreeMap<usize, BTreeSet<i32>> = BTreeMap::new();
        for slide in &show.slides {
            let record = show.slide_record(slide)?;
            let sheet = show.slide_sheet(slide, &record)?;
            let shapes = sheet_shapes(&sheet)?;
            collect_picture_slides(&shapes, slide.number, pictures, &store, &mut slides_of)?;
        }
        for picture in &pictures.pictures {
            let media_type = picture.kind.media_type();
            let Some(data) = self.readable(pictures.data(picture))? else {
                continue;
            };
            let name = format!(
                "image-{}{}",
                picture.index,
                mime::registered_extension(&MediaType::named(media_type))
            );
            let mut metadata = TikaMetadata::new();
            metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
            if let Some(slides) = slides_of.get(&picture.index) {
                let pages: Vec<i32> = slides.iter().copied().collect();
                apply_page_numbers(&mut metadata, &pages);
            }
            metadata.set(RESOURCE_NAME, name);
            metadata.set(CONTENT_TYPE, media_type);
            self.out.push(Out::Embedded(
                Handed::Document(Embedded {
                    data: Node::from(data),
                    metadata,
                }),
                Wrapping::Bare,
            ));
            self.flush(flush)?;
        }
        Ok(())
    }

    /// The blip store picture shapes look their pictures up in: the current document's, which
    /// matching the pictures changed where it is the first document.
    fn lookup_store(&self) -> Result<LookupStore, Unread> {
        let first_document = self
            .records
            .tops
            .iter()
            .find(|top| top.kind == Some(kind::DOCUMENT));
        if self.pictures.store.is_some()
            && first_document.is_some_and(|first| first.offset == self.show.document_offset)
        {
            return Ok(LookupStore::Group(self.pictures.store.clone()));
        }
        let Some(group) = self
            .show
            .document
            .last_after_first(kind::PP_DRAWING_GROUP)
            .and_then(super::records::Record::drawing_group)
        else {
            return Ok(LookupStore::NoGroup);
        };
        Ok(LookupStore::Group(super::pictures::store_of(group)?))
    }

    /// `handleShowEmbeddedResources`: the objects no shape showed, read and handed on one at a
    /// time.
    fn show_objects<E>(&mut self, flush: &mut Flush<'_, E>) -> Result<(), Halt<E>> {
        let mut index = 0;
        for at in 0..self.objects.len() {
            if self.processed.contains(&self.objects[at].persist_id) {
                continue;
            }
            let Some(data) = self.readable(self.objects[at].data(self.show)?)? else {
                index += 1;
                continue;
            };
            let media_type = detect(&data)?;
            let name = format!(
                "embedded-{index}{}",
                mime::registered_extension(&media_type)
            );
            let mut metadata = TikaMetadata::new();
            metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
            if ole2::is_ole(&data) {
                let file = Ole2::open_stream(&data).map_err(|Unreadable(reason)| {
                    recorded(&format!(
                        "POI cannot open an embedded compound document: {reason}"
                    ))
                })?;
                if file.children(file.root()).next().is_none() {
                    return Ok(());
                }
                if let Some(embedded) =
                    embedded_office_document(data, metadata, &name, self.configs)?
                {
                    self.out
                        .push(Out::Embedded(embedded, Wrapping::PackageEntry));
                }
            } else if !data.is_empty() {
                metadata.set(RESOURCE_NAME, name);
                self.out.push(Out::Embedded(
                    Handed::Document(Embedded {
                        data: Node::from(data),
                        metadata,
                    }),
                    Wrapping::PackageEntry,
                ));
            }
            self.flush(flush)?;
            index += 1;
        }
        Ok(())
    }
}

/// `handleEmbeddedOfficeDoc` of the root of a compound document read from a stream: the document
/// Tika hands on, if any, its streams in place where the compound document is.
fn embedded_office_document<'a>(
    data: Cow<'a, [u8]>,
    metadata: TikaMetadata,
    name: &str,
    configs: &ParserConfigs,
) -> Result<Option<Handed<'a>>, Unread> {
    match data {
        Cow::Borrowed(data) => office_document_of(data, metadata, name, configs),
        Cow::Owned(data) => {
            Ok(office_document_of(&data, metadata, name, configs)?.map(Handed::into_owned))
        }
    }
}

fn office_document_of<'a>(
    data: &'a [u8],
    metadata: TikaMetadata,
    name: &str,
    configs: &ParserConfigs,
) -> Result<Option<Handed<'a>>, Unread> {
    let file = Ole2::open_stream(data).map_err(|Unreadable(reason)| {
        recorded(&format!(
            "POI cannot open an embedded compound document: {reason}"
        ))
    })?;
    Ok(office_document(
        &file,
        file.root(),
        metadata,
        Some(name),
        configs,
    )?)
}

/// The blip store picture shapes look their pictures up in, or none where the document has no
/// drawing group, on which POI's lookup throws.
enum LookupStore {
    NoGroup,
    Group(Option<Vec<StoreEntry>>),
}

/// `collectPictureSlides`: the slide number of each picture the shapes show, in groups too.
fn collect_picture_slides(
    shapes: &[Shape<'_>],
    slide: i32,
    pictures: &Pictures<'_>,

    store: &LookupStore,
    slides_of: &mut BTreeMap<usize, BTreeSet<i32>>,
) -> Result<(), Unread> {
    for shape in shapes {
        if shape.kind.is_picture() {
            let LookupStore::Group(store) = store else {
                return Err(
                    Thrown::new("a picture shape of a document without a drawing group").into(),
                );
            };
            let index = shape.property(BLIP_TO_DISPLAY)?;
            if let Some(picture) = pictures.of_entry(store.as_deref(), index)? {
                slides_of.entry(picture.index).or_default().insert(slide);
            }
        } else if shape.kind.is_group() {
            collect_picture_slides(&shape.children, slide, pictures, store, slides_of)?;
        }
    }
    Ok(())
}

/// Tika's detection of an embedded object.
fn detect(data: &[u8]) -> Result<MediaType, Unread> {
    mime::detect(data, Origin::Bytes)
        .map(|detection| detection.media_type)
        .map_err(|error| {
            recorded(&format!("Tika's detection fails on an object: {error:?}")).into()
        })
}

/// `removePBreak`: a fragment without the carriage return that ends it, or that a final line
/// terminator follows, as `replaceFirst("\r$", "")` removes it.
fn remove_paragraph_break(fragment: &[u16]) -> Cow<'_, [u16]> {
    match fragment {
        [rest @ .., CARRIAGE_RETURN] => Cow::Borrowed(rest),
        [rest @ .., CARRIAGE_RETURN, terminator @ (0x0085 | 0x2028 | 0x2029)] => {
            let mut removed = rest.to_vec();
            removed.push(*terminator);
            Cow::Owned(removed)
        }
        _ => Cow::Borrowed(fragment),
    }
}

/// `line.split("\u000b")`: the parts between vertical tabs, without trailing empty ones, one
/// empty part for an empty line.
fn split_vertical_tabs(line: &[u16]) -> Vec<&[u16]> {
    if line.is_empty() {
        return vec![line];
    }
    let mut parts: Vec<&[u16]> = line.split(|&unit| unit == VERTICAL_TAB).collect();
    while parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn splits_at_vertical_tabs_as_java_does() {
        assert_eq!(
            split_vertical_tabs(&units("a\u{b}b")),
            [&units("a")[..], &units("b")]
        );
        assert_eq!(split_vertical_tabs(&units("a\u{b}")), [&units("a")[..]]);
        assert!(split_vertical_tabs(&units("\u{b}\u{b}")).is_empty());
        assert_eq!(split_vertical_tabs(&[]), [&[] as &[u16]]);
    }

    #[test]
    fn removes_a_closing_carriage_return() {
        assert_eq!(&*remove_paragraph_break(&units("a\r")), &units("a")[..]);
        assert_eq!(&*remove_paragraph_break(&units("a\rb")), &units("a\rb")[..]);
        assert_eq!(
            &*remove_paragraph_break(&units("a\r\u{2028}")),
            &units("a\u{2028}")[..]
        );
    }
}
