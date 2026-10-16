//! Tika's `HSLFExtractor`: the slides with their headers, master text, text, tables, grouped
//! text, footers, comments, notes and embedded objects, then the pictures and the objects no
//! shape showed, as the events it writes, decided before any is written.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::embedded::{Wrapping, RELATIONSHIP_ID, RESOURCE_NAME};
use crate::formats::handlers::is_blank;
use crate::formats::metadata::office::{
    COMMENT_PERSONS, HAS_ANIMATIONS, HAS_HIDDEN_SLIDES, NUM_HIDDEN_SLIDES,
    RESOURCE_NAME_EXTENSION_INFERRED,
};
use crate::formats::metadata::{apply_page_numbers, TikaMetadata};
use crate::formats::ole2::{office_document, Embedded, Handed};
use crate::formats::Unported;
use crate::mime::{self, MediaType, Origin};
use crate::ole2::{self, Ole2, Unreadable};

use super::headers::{notes_sheet, HeadersFooters, Part, Scope};
use super::pictures::{Object, Pictures, StoreEntry};
use super::records::kind;
use super::shapes::{sheet_shapes, Kind, Shape, BLIP_TO_DISPLAY, PICTURE_ID};
use super::show::{Sheet, Show};
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
pub(super) enum Out {
    Start(&'static str),
    StartClass(&'static str, &'static str),
    StartWith(&'static str, Vec<(&'static str, String)>),
    End(&'static str),
    /// Characters, never empty.
    Text(Vec<u16>),
    Embedded(Handed, Wrapping),
    /// An object of a shape, handed on bare by `handleDataStream`, which catches what its parse
    /// throws and records it.
    Object(Handed),
    /// A value added to the presentation's metadata.
    Add(&'static str, String),
    /// A value set in the presentation's metadata.
    Set(&'static str, String),
}

fn recorded(reason: &str) -> Unported {
    Unported(format!(
        "an embedded object or picture Tika fails on and records with Java's stack trace: \
         {reason}"
    ))
}

/// The extractor's state while it runs: its events, the comment authors met and the objects
/// shapes showed.
struct Extractor<'s, 'a> {
    show: &'s Show<'a>,
    config: &'s OfficeParserConfig,
    configs: &'s ParserConfigs,
    out: Vec<Out>,
    authors: HashSet<String>,
    processed: HashSet<i32>,
    objects: Vec<Object<'a>>,
    has_animations: bool,
}

/// `HSLFExtractor.parse`: the presentation's events.
pub(super) fn extract<'a>(
    show: &Show<'a>,
    objects: Vec<Object<'a>>,
    pictures: &Pictures,
    configs: &ParserConfigs,
) -> Result<Vec<Out>, Unported> {
    let config = &configs.office;
    let mut extractor = Extractor {
        show,
        config,
        configs,
        out: Vec::new(),
        authors: HashSet::new(),
        processed: HashSet::new(),
        objects,
        has_animations: false,
    };
    extractor.out.push(Out::StartClass("div", "slideShow"));
    let mut hidden = 0;
    for slide in &show.slides {
        extractor.slide(&slide.sheet, slide.notes)?;
        if is_hidden(slide.sheet.container)? {
            hidden += 1;
        }
        extractor.find_animations(slide.sheet.container);
    }
    if hidden > 0 {
        extractor
            .out
            .push(Out::Set(NUM_HIDDEN_SLIDES, hidden.to_string()));
        extractor
            .out
            .push(Out::Set(HAS_HIDDEN_SLIDES, "true".to_owned()));
    }
    extractor.pictures(pictures)?;
    extractor.show_objects()?;
    extractor.out.push(Out::End("div"));
    Ok(extractor.out)
}

/// `HSLFSlide.isHidden`.
fn is_hidden(container: &super::records::Record) -> Result<bool, Unported> {
    let Some(info) = container.first(kind::SS_SLIDE_INFO_ATOM) else {
        return Ok(false);
    };
    let flags = info
        .short(10)
        .map_err(|_| Unported("a short slide show info atom".to_owned()))?;
    Ok(flags & HIDDEN_BIT != 0)
}

impl<'s, 'a> Extractor<'s, 'a> {
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

    fn slide(&mut self, sheet: &Sheet<'a>, notes: Option<usize>) -> Result<(), Unported> {
        self.start_class("div", "slide");
        let headers = self.config.includes_headers_and_footers();
        let (slide_headers, notes_headers) = if headers {
            (
                Some(HeadersFooters::new(self.show, sheet, Scope::Slide)?),
                Some(HeadersFooters::new(
                    self.show,
                    notes_sheet(self.show)?,
                    Scope::Notes,
                )?),
            )
        } else {
            (None, None)
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
            self.notes(&self.show.notes[index], notes_headers.as_ref())?;
        }
        self.embedded_objects(&shapes)?;
        self.end("div");
        Ok(())
    }

    /// A header or footer in a paragraph of its class, where it is visible and has text.
    fn header_or_footer(
        &mut self,
        headers: &HeadersFooters,
        part: Part,
        class: &'static str,
    ) -> Result<(), Unported> {
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
    fn sheet_text(&mut self, sheet: &Sheet<'a>, shapes: &[Shape<'a>]) -> Result<(), Unported> {
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
    fn master(&mut self, master: &Sheet<'a>) -> Result<(), Unported> {
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
    fn table(&mut self, sheet: &Sheet<'a>, table: &Shape<'a>) -> Result<(), Unported> {
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
    fn group_text(
        &mut self,
        sheet: &Sheet<'a>,
        shapes: &[Shape<'a>],
        depth: usize,
    ) -> Result<(), Unported> {
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
        sheet: &Sheet<'a>,
        lists: &[Vec<&Paragraph>],
    ) -> Result<(), Unported> {
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
    fn comments(&mut self, sheet: &Sheet<'a>) {
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
        notes: &Sheet<'a>,
        headers: Option<&HeadersFooters>,
    ) -> Result<(), Unported> {
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
    fn embedded_objects(&mut self, shapes: &[Shape<'a>]) -> Result<(), Unported> {
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
            let data = self.objects[object].data()?;
            self.data_stream(data, &id, prog_id.as_deref())?;
        }
        Ok(())
    }

    /// `getExEmbed`: the last embedding of the document's objects with the shape's object id.
    fn embedding(&self, shape: &Shape<'a>) -> Result<Option<&'a super::records::Record>, Unported> {
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
    fn object_of(&self, shape: &Shape<'a>) -> Result<Option<usize>, Unported> {
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
    fn prog_id(&self, shape: &Shape<'a>) -> Result<Option<String>, Unported> {
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
        data: Vec<u8>,
        id: &str,
        prog_id: Option<&str>,
    ) -> Result<(), Unported> {
        let media_type = if prog_id == Some("Excel.Chart.8") {
            MediaType::named("application/vnd.ms-excel")
        } else {
            detect(&data)?
        };
        if media_type == MediaType::parse(COMP_OBJ).expect("a media type")
            || media_type == MediaType::named(MSOFFICE)
        {
            if let Some(embedded) =
                embedded_office_document(&data, TikaMetadata::new(), id, self.configs)?
            {
                self.out.push(Out::Object(embedded));
            }
            return Ok(());
        }
        let mut metadata = TikaMetadata::new();
        metadata.set(RESOURCE_NAME, id);
        metadata.set(RELATIONSHIP_ID, id);
        metadata.set(CONTENT_TYPE, media_type.to_string());
        self.out
            .push(Out::Object(Handed::Document(Embedded { data, metadata })));
        Ok(())
    }

    /// `handleSlideEmbeddedPictures`: each picture, with the slides that show it.
    fn pictures(&mut self, pictures: &Pictures) -> Result<(), Unported> {
        let store = self.lookup_store(pictures)?;
        let mut slides_of: BTreeMap<usize, BTreeSet<i32>> = BTreeMap::new();
        for slide in &self.show.slides {
            let shapes = sheet_shapes(&slide.sheet)?;
            collect_picture_slides(&shapes, slide.number, pictures, &store, &mut slides_of)?;
        }
        for picture in &pictures.pictures {
            let media_type = picture.kind.media_type();
            let data = picture.data()?;
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
                Handed::Document(Embedded { data, metadata }),
                Wrapping::Bare,
            ));
        }
        Ok(())
    }

    /// The blip store picture shapes look their pictures up in: the current document's, which
    /// matching the pictures changed where it is the first document.
    fn lookup_store(&self, pictures: &Pictures) -> Result<LookupStore, Unported> {
        let first_document = self
            .show
            .records
            .iter()
            .filter_map(|top| top.record.as_ref())
            .find(|record| record.kind == kind::DOCUMENT);
        if pictures.store.is_some()
            && first_document.is_some_and(|first| std::ptr::eq(first, self.show.document))
        {
            return Ok(LookupStore::Group(pictures.store.clone()));
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

    /// `handleShowEmbeddedResources`: the objects no shape showed.
    fn show_objects(&mut self) -> Result<(), Unported> {
        let mut index = 0;
        for at in 0..self.objects.len() {
            if self.processed.contains(&self.objects[at].persist_id) {
                continue;
            }
            let data = self.objects[at].data()?;
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
                    embedded_office_document(&data, metadata, &name, self.configs)?
                {
                    self.out
                        .push(Out::Embedded(embedded, Wrapping::PackageEntry));
                }
            } else if !data.is_empty() {
                metadata.set(RESOURCE_NAME, name);
                self.out.push(Out::Embedded(
                    Handed::Document(Embedded { data, metadata }),
                    Wrapping::PackageEntry,
                ));
            }
            index += 1;
        }
        Ok(())
    }
}

/// `handleEmbeddedOfficeDoc` of the root of a compound document read from a stream: the document
/// Tika hands on, if any.
fn embedded_office_document(
    data: &[u8],
    metadata: TikaMetadata,
    name: &str,
    configs: &ParserConfigs,
) -> Result<Option<Handed>, Unported> {
    let file = Ole2::open_stream(data).map_err(|Unreadable(reason)| {
        recorded(&format!(
            "POI cannot open an embedded compound document: {reason}"
        ))
    })?;
    office_document(&file, file.root(), metadata, Some(name), configs)
}

/// The blip store picture shapes look their pictures up in, or none where the document has no
/// drawing group, on which POI's lookup throws.
enum LookupStore {
    NoGroup,
    Group(Option<Vec<StoreEntry>>),
}

/// `collectPictureSlides`: the slide number of each picture the shapes show, in groups too.
fn collect_picture_slides(
    shapes: &[Shape],
    slide: i32,
    pictures: &Pictures,
    store: &LookupStore,
    slides_of: &mut BTreeMap<usize, BTreeSet<i32>>,
) -> Result<(), Unported> {
    for shape in shapes {
        if shape.kind.is_picture() {
            let LookupStore::Group(store) = store else {
                return Err(Unported(
                    "a picture shape of a document without a drawing group, on which POI throws"
                        .to_owned(),
                ));
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
fn detect(data: &[u8]) -> Result<MediaType, Unported> {
    mime::detect(data, Origin::Bytes)
        .map(|detection| detection.media_type)
        .map_err(|error| recorded(&format!("Tika's detection fails on an object: {error:?}")))
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
