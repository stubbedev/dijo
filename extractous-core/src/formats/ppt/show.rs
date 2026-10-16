//! A presentation as POI's `HSLFSlideShowImpl` and `HSLFSlideShow` read it: the document stream's
//! records reached from the current user's edit through every user edit and its persist
//! pointers, the most recent record of each persist id, and the masters, notes and slides of the
//! document's slide lists with their text.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::formats::Unported;
use crate::ole2::{EntryId, EntryKind, Ole2, Unreadable};

use super::drawing::{textbox_records, Drawing, Wrapper};
use super::records::{build_at, kind, persist_pointers, Depth, Fault, Record};
use super::text::{apply_hyperlinks, find_paragraphs, placeholder, MasterStyles, Paragraph};

pub(super) const POWERPOINT_DOCUMENT: &str = "PowerPoint Document";
const DUAL_STORAGE: &str = "PP97_DUALSTORAGE";
const POWERPOINT_95: &str = "PP40";
const CURRENT_USER: &str = "Current User";
/// The longest "Current User" stream POI reads.
const MAX_CURRENT_USER: usize = 131_072;
/// The longest document stream POI reads.
const MAX_DOCUMENT: usize = 100_000_000;
/// How many user edits this port follows before it leaves a document whose edits POI repairs
/// in a loop to Tika.
const MAX_USER_EDITS: usize = 100_000;
/// The slide lists' instances: of slides, masters and notes.
const SLIDES: i32 = 0;
const MASTERS: i32 = 1;
const NOTES: i32 = 2;
/// The text types a master's styles are kept for.
const MASTER_TEXT_TYPES: usize = 9;

fn unported(reason: impl Into<String>) -> Unported {
    Unported(reason.into())
}

fn poi_throws(reason: &str) -> Unported {
    Unported(format!(
        "POI throws reading the presentation, which fails Tika's parse: {reason}"
    ))
}

/// A record of the document stream at the top level.
#[derive(Debug)]
pub(super) struct Top {
    pub(super) offset: i32,
    /// The record, `None` where it claims more bytes than the stream holds.
    pub(super) record: Option<Record>,
    /// The persist id a persist pointer gives its offset.
    pub(super) persist_id: Option<i32>,
}

/// The directory POI reads the presentation from: the dual storage's where there is one.
pub(super) fn presentation_directory(file: &Ole2<'_>, root: EntryId) -> Result<EntryId, Unported> {
    match file.child_ignoring_case(root, DUAL_STORAGE) {
        None => Ok(root),
        Some(entry) if file.kind(entry) != EntryKind::Document => Ok(entry),
        Some(_) => Err(poi_throws("the dual storage is no directory")),
    }
}

/// `CurrentUserAtom`'s offset of the current edit, as `readCurrentUserStream` reads it.
fn current_edit_offset(file: &Ole2<'_>, directory: EntryId) -> Result<i64, Unported> {
    let Some(entry) = file.child(directory, CURRENT_USER) else {
        return Ok(0);
    };
    if file.kind(entry) != EntryKind::Document {
        return Err(poi_throws("the Current User entry is no document"));
    }
    let Ok(contents) = file.read(entry) else {
        return Ok(0);
    };
    if contents.len() > MAX_CURRENT_USER {
        return Err(poi_throws("The Current User stream is implausibly long"));
    }
    if contents.len() < 28 {
        return Err(poi_throws(
            "the Current User stream is too short, of a PowerPoint 95 or corrupt document",
        ));
    }
    Ok(i64::from(u32::from_le_bytes([
        contents[16],
        contents[17],
        contents[18],
        contents[19],
    ])))
}

/// `HSLFSlideShowImpl`'s records: the document stream read and its records built.
pub(super) fn read_records(
    file: &Ole2<'_>,
    directory: EntryId,
) -> Result<(Vec<u8>, Vec<Top>), Unported> {
    let edit = current_edit_offset(file, directory)?;
    let stream = match file.child_ignoring_case(directory, POWERPOINT_DOCUMENT) {
        Some(entry) if file.kind(entry) == EntryKind::Document => entry,
        Some(_) | None => {
            if file.child_ignoring_case(directory, POWERPOINT_95).is_some() {
                return Err(unported(
                    "a PowerPoint 95 presentation, which POI refuses and Tika fails on",
                ));
            }
            return Err(poi_throws(
                "the presentation has no PowerPoint Document stream",
            ));
        }
    };
    let document = file.read(stream).map_err(|Unreadable(reason)| {
        unported(format!("POI cannot read the document stream: {reason}"))
    })?;
    if document.len() > MAX_DOCUMENT {
        return Err(poi_throws("the document stream is longer than POI reads"));
    }
    let records = build_records(&document, edit as i32)?;
    Ok((document, records))
}

/// A record of the user edit chain: a user edit or its persist pointers.
enum Chained {
    Edit(Record),
    Pointers(Record),
    Pending,
}

/// `HSLFSlideShowImpl.read`: the user edits and persist pointers from `edit` back, then every
/// record a persist pointer gives, by offset.
fn build_records(document: &[u8], edit: i32) -> Result<Vec<Top>, Unported> {
    let mut map: BTreeMap<i32, Chained> = BTreeMap::new();
    let mut persist_ids: HashMap<i32, i32> = HashMap::new();
    let mut offset = edit;
    let mut followed = 0usize;
    while offset != 0 {
        followed += 1;
        if followed > MAX_USER_EDITS {
            return Err(unported(
                "user edits POI follows in a loop, which never ends Tika's parse",
            ));
        }
        let built = build_at(document, i64::from(offset), Depth::TOP).map_err(Fault::unported)?;
        let Some(user) = built.filter(|record| record.kind == kind::USER_EDIT_ATOM) else {
            return Err(poi_throws("Did not have a user edit atom"));
        };
        let pointers_offset = user.int(12).map_err(|_| poi_throws("a short user edit"))?;
        let mut previous = user.int(8).map_err(|_| poi_throws("a short user edit"))?;
        map.insert(offset, Chained::Edit(user));
        let pointers = build_at(document, i64::from(pointers_offset), Depth::TOP)
            .map_err(Fault::unported)?
            .filter(|record| {
                record.kind == kind::PERSIST_PTR_FULL_BLOCK
                    || record.kind == kind::PERSIST_PTR_INCREMENTAL_BLOCK
            })
            .ok_or_else(|| poi_throws("Powerpoint document is missing a PersistPtrHolder"))?;
        let lookup = slide_locations(&pointers)?;
        map.insert(pointers_offset, Chained::Pointers(pointers));
        let mut offsets = HashSet::new();
        for (&id, &location) in &lookup {
            if !offsets.insert(location) {
                return Err(unported(
                    "persist pointers that give two ids one offset, of which POI keeps one in \
                     hash order",
                ));
            }
            map.insert(location, Chained::Pending);
            persist_ids.insert(location, id);
        }
        if previous > 0 && map.contains_key(&previous) {
            let first = *map.keys().next().expect("the map holds this edit");
            previous = first.wrapping_sub(36);
            let repaired = previous >= 0
                && document.len() >= previous as usize + 8
                && u16::from_le_bytes([
                    document[previous as usize],
                    document[previous as usize + 1],
                ]) == 0
                && u16::from_le_bytes([
                    document[previous as usize + 2],
                    document[previous as usize + 3],
                ]) == kind::USER_EDIT_ATOM
                && matches!(
                    u32::from_le_bytes([
                        document[previous as usize + 4],
                        document[previous as usize + 5],
                        document[previous as usize + 6],
                        document[previous as usize + 7],
                    ]),
                    0x1C | 0x20
                );
            if !repaired {
                return Err(poi_throws(
                    "Powerpoint document contains invalid user edit atom",
                ));
            }
        }
        offset = previous;
    }
    if let Some(reason) = encryption(document, &map)? {
        return Err(reason);
    }
    let mut records = Vec::with_capacity(map.len());
    for (offset, chained) in map {
        let record = match chained {
            Chained::Edit(record) | Chained::Pointers(record) => Some(record),
            Chained::Pending => {
                build_at(document, i64::from(offset), Depth::TOP).map_err(Fault::unported)?
            }
        };
        records.push(Top {
            offset,
            record,
            persist_id: persist_ids.get(&offset).copied(),
        });
    }
    Ok(records)
}

/// `PersistPtrHolder.getSlideLocationsLookup`: each persist id's offset, the last a holder gives.
pub(super) fn slide_locations(holder: &Record) -> Result<BTreeMap<i32, i32>, Unported> {
    let pointers = persist_pointers(holder.data())
        .map_err(|_| poi_throws("persist pointers run past their record"))?;
    Ok(pointers.into_iter().collect())
}

/// `new HSLFSlideShowEncrypted(docstream, records)`: why the presentation is left to Tika where
/// a user edit names an encryption session the persist pointers give.
fn encryption(document: &[u8], map: &BTreeMap<i32, Chained>) -> Result<Option<Unported>, Unported> {
    for chained in map.values().rev() {
        let Chained::Edit(user) = chained else {
            continue;
        };
        let session = if user.data().len() >= 32 {
            user.int(28).map_err(|_| poi_throws("a short user edit"))?
        } else {
            -1
        };
        if session == -1 {
            continue;
        }
        let pointers_offset = user.int(12).map_err(|_| poi_throws("a short user edit"))?;
        let Some(Chained::Pointers(pointers)) = map.get(&pointers_offset) else {
            return Err(poi_throws("Encountered an unexpected record-type"));
        };
        let Some(&location) = slide_locations(pointers)?.get(&session) else {
            return Ok(None);
        };
        let atom = build_at(document, i64::from(location), Depth::TOP);
        return Ok(Some(match atom {
            Err(Fault::Unported(reason)) => reason,
            _ => poi_throws("Did not have a DocumentEncryptionAtom"),
        }));
    }
    Ok(None)
}

/// What kind of sheet a sheet is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SheetKind {
    Slide,
    Notes,
    SlideMaster,
    TitleMaster,
}

/// A sheet (`HSLFSheet`): its record, its number and the lists of paragraphs of its text.
#[derive(Debug)]
pub(super) struct Sheet<'a> {
    pub(super) kind: SheetKind,
    pub(super) container: &'a Record,
    /// `_getSheetNumber`.
    pub(super) number: i32,
    pub(super) drawing: &'a Drawing,
    pub(super) paragraphs: Vec<Paragraph<'a>>,
    /// `getTextParagraphs`: lists of paragraphs, by their index in `paragraphs`.
    pub(super) lists: Vec<Vec<usize>>,
}

impl<'a> Sheet<'a> {
    fn new(kind: SheetKind, container: &'a Record, number: i32) -> Result<Sheet<'a>, Unported> {
        let drawing = container
            .last(kind::PP_DRAWING)
            .and_then(Record::drawing)
            .ok_or_else(|| poi_throws("Did not receive a valid drawing for a sheet"))?;
        Ok(Sheet {
            kind,
            container,
            number,
            drawing,
            paragraphs: Vec::new(),
            lists: Vec::new(),
        })
    }

    fn add_list(&mut self, paragraphs: Vec<Paragraph<'a>>) -> Vec<usize> {
        let start = self.paragraphs.len();
        self.paragraphs.extend(paragraphs);
        (start..self.paragraphs.len()).collect()
    }

    /// The constructor's `findTextParagraphs(getPPDrawing(), this)`: the lists of the drawing's
    /// text box wrappers this sheet does not hold yet.
    fn add_drawing_lists(&mut self) -> Result<(), Unported> {
        for &wrapper in &self.drawing.wrappers {
            let Some(list) = self.wrapper_list(wrapper)? else {
                continue;
            };
            if !self.lists.contains(&list) {
                self.lists.push(list);
            }
        }
        Ok(())
    }

    /// `findTextParagraphs(wrapper, sheet)` for one of the drawing's wrappers, the paragraphs it
    /// creates added to the sheet's.
    fn wrapper_list(&mut self, wrapper: Wrapper) -> Result<Option<Vec<usize>>, Unported> {
        let shape = self.drawing.wrapped(wrapper);
        let found = self.find_in_sheet(shape)?;
        Ok(match found {
            Found::List(list) => Some(list),
            Found::Nothing => None,
            Found::New(paragraphs) => Some(self.add_list(paragraphs)),
        })
    }

    /// `findTextParagraphs(wrapper, sheet)` of the text box of a shape container: a list of
    /// the sheet's, one combined of the sheet's slide lists, new paragraphs, or nothing.
    pub(super) fn find_in_sheet(
        &self,
        shape: &'a super::drawing::Node,
    ) -> Result<Found<'a>, Unported> {
        let records = match textbox_records(shape) {
            Some(Ok(records)) => records,
            Some(Err(fault)) => return Err(fault.unported()),
            None => return Ok(Found::Nothing),
        };
        if let Some(reference) = records
            .iter()
            .find(|record| record.kind == kind::OUTLINE_TEXT_REF_ATOM)
        {
            let index = reference
                .int(0)
                .map_err(|_| poi_throws("a short outline text reference"))?;
            let mut found: Option<Vec<usize>> = None;
            for list in &self.lists {
                let Some(&first) = list.first() else {
                    continue;
                };
                let list_index = self.paragraphs[first].index;
                if list_index > index {
                    break;
                }
                if list_index == index {
                    match &mut found {
                        None => found = Some(list.clone()),
                        Some(combined) => combined.extend(list),
                    }
                }
            }
            return Ok(match found {
                Some(list) if !list.is_empty() => Found::List(list),
                Some(_) | None => Found::Nothing,
            });
        }
        for list in &self.lists {
            if let Some(&first) = list.first() {
                if std::ptr::eq(self.paragraphs[first].parent.as_ptr(), records.as_ptr()) {
                    return Ok(Found::List(list.clone()));
                }
            }
        }
        let mut lists = find_paragraphs(records, 0..records.len(), false)?;
        match lists.len() {
            0 => Ok(Found::Nothing),
            1 => Ok(Found::New(lists.pop().expect("one list"))),
            _ => Err(poi_throws(
                "TextBox contains more than one list of paragraphs.",
            )),
        }
    }

    /// The slide atom of a slide or title master.
    fn slide_atom(&self) -> Option<&'a Record> {
        self.container.last(kind::SLIDE_ATOM)
    }

    /// `getMasterID` of a slide's or title master's slide atom.
    pub(super) fn master_id(&self) -> Option<i32> {
        self.slide_atom().and_then(|atom| atom.int(12).ok())
    }
}

/// What `findTextParagraphs(wrapper, sheet)` finds.
pub(super) enum Found<'a> {
    /// A list of the sheet's, or one combined of its lists.
    List(Vec<usize>),
    /// Paragraphs made from the text box's records.
    New(Vec<Paragraph<'a>>),
    Nothing,
}

/// A slide master (`HSLFSlideMaster`) and the styles of its text types.
#[derive(Debug)]
pub(super) struct SlideMaster<'a> {
    pub(super) sheet: Sheet<'a>,
    styles: Vec<Option<MasterStyles>>,
}

impl SlideMaster<'_> {
    /// `getPropCollection(txtype, level, "paragraph_flags", false)`'s collection's bullet flag,
    /// or `None` where it finds no collection with paragraph flags.
    pub(super) fn bullet(&self, text_type: i32, level: i32) -> Result<Option<bool>, Unported> {
        if let Some(found) = self.flags_of(text_type, level)? {
            return Ok(Some(found));
        }
        let fallback = match text_type {
            placeholder::TITLE | placeholder::CENTER_TITLE => placeholder::TITLE,
            placeholder::NOTES | placeholder::OTHER => return Ok(None),
            _ => placeholder::BODY,
        };
        self.flags_of(fallback, level)
    }

    /// `getPropHelper`.
    fn flags_of(&self, text_type: i32, level: i32) -> Result<Option<bool>, Unported> {
        if text_type >= MASTER_TEXT_TYPES as i32 {
            return Ok(None);
        }
        let styles = usize::try_from(text_type)
            .ok()
            .and_then(|slot| self.styles[slot].as_ref())
            .ok_or_else(|| poi_throws("a master has no styles for a text type"))?;
        let paragraphs = styles
            .paragraphs
            .as_ref()
            .ok_or_else(|| poi_throws("a master's styles were not read"))?;
        let highest = level.min(paragraphs.len() as i32 - 1);
        for at in (0..=highest).rev() {
            if let Some(flags) = paragraphs[at as usize].flags {
                return Ok(Some(flags.bullet()));
            }
        }
        Ok(None)
    }
}

/// A slide (`HSLFSlide`).
#[derive(Debug)]
pub(super) struct Slide<'a> {
    pub(super) sheet: Sheet<'a>,
    /// `getSlideNumber`.
    pub(super) number: i32,
    /// The index of its notes in the presentation's notes.
    pub(super) notes: Option<usize>,
}

/// A presentation (`HSLFSlideShow`).
#[derive(Debug)]
pub(super) struct Show<'a> {
    pub(super) records: &'a [Top],
    pub(super) document: &'a Record,
    pub(super) masters: Vec<SlideMaster<'a>>,
    pub(super) title_masters: Vec<Sheet<'a>>,
    pub(super) notes: Vec<Sheet<'a>>,
    pub(super) slides: Vec<Slide<'a>>,
}

impl<'a> Show<'a> {
    /// `new HSLFSlideShow(impl)`: the most recent records, then the masters, notes and slides.
    pub(super) fn new(records: &'a [Top]) -> Result<Show<'a>, Unported> {
        let core = most_recent(records)?;
        let document = core
            .values()
            .rev()
            .filter_map(|record| *record)
            .find(|record| record.kind == kind::DOCUMENT)
            .ok_or_else(|| {
                poi_throws(
                    "The PowerPoint file didn't contain a Document Record in its PersistPtr \
                     blocks",
                )
            })?;
        let mut show = Show {
            records,
            document,
            masters: Vec::new(),
            title_masters: Vec::new(),
            notes: Vec::new(),
            slides: Vec::new(),
        };
        let lists: Vec<&Record> = document
            .children()
            .iter()
            .skip(1)
            .filter(|child| child.kind == kind::SLIDE_LIST_WITH_TEXT)
            .collect();
        let list_of = |instance: i32| {
            lists
                .iter()
                .copied()
                .find(|list| list.instance() == instance)
        };
        if let Some(masters) = list_of(MASTERS) {
            show.find_masters(masters, &core)?;
        }
        let mut notes_of_slides = HashMap::new();
        if let Some(notes) = list_of(NOTES) {
            show.find_notes(notes, &core, &mut notes_of_slides)?;
        }
        if let Some(slides) = list_of(SLIDES) {
            show.find_slides(slides, &core, &notes_of_slides)?;
        }
        Ok(show)
    }

    /// The document's `ExObjList`, if it has one.
    pub(super) fn objects(&self) -> Option<&'a Record> {
        self.document.last_after_first(kind::EX_OBJ_LIST)
    }

    fn find_masters(
        &mut self,
        list: &'a Record,
        core: &BTreeMap<i32, Option<&'a Record>>,
    ) -> Result<(), Unported> {
        for set in atom_sets(list) {
            let persist = &list.children()[set.start];
            let record = core_record(core, persist)?;
            let number = persist
                .int(12)
                .map_err(|_| poi_throws("a short slide persist"))?;
            match record.map(|record| record.kind) {
                Some(kind::SLIDE) => {
                    let mut sheet =
                        Sheet::new(SheetKind::TitleMaster, record.expect("a slide"), number)?;
                    sheet.add_drawing_lists()?;
                    self.supply(&mut sheet)?;
                    self.title_masters.push(sheet);
                }
                Some(kind::MAIN_MASTER) => {
                    let container = record.expect("a master");
                    let mut sheet = Sheet::new(SheetKind::SlideMaster, container, number)?;
                    sheet.add_drawing_lists()?;
                    self.supply(&mut sheet)?;
                    let styles = self.master_styles(container)?;
                    let master = SlideMaster { sheet, styles };
                    master_styles_cover(&master)?;
                    self.masters.push(master);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// `HSLFSlideMaster.setSlideShow`'s styles: the document's for its text type, then the
    /// master's own for the types not set yet.
    fn master_styles(&self, master: &Record) -> Result<Vec<Option<MasterStyles>>, Unported> {
        let mut styles: Vec<Option<MasterStyles>> = vec![None; MASTER_TEXT_TYPES];
        let environment = self
            .document
            .children()
            .iter()
            .skip(1)
            .rev()
            .find(|child| child.kind == kind::ENVIRONMENT)
            .ok_or_else(|| {
                poi_throws("Did not find a TxMasterStyleAtom in the current slide show")
            })?;
        let document_styles = environment
            .children()
            .iter()
            .rev()
            .find(|child| child.kind == kind::TX_MASTER_STYLE_ATOM)
            .map(MasterStyles::read)
            .ok_or_else(|| {
                poi_throws("Did not find a TxMasterStyleAtom in the current slide show")
            })?;
        let document_slot = slot(document_styles.text_type)?;
        styles[document_slot] = Some(document_styles);
        for atom in master
            .children()
            .iter()
            .filter(|child| child.kind == kind::TX_MASTER_STYLE_ATOM)
        {
            let own = MasterStyles::read(atom);
            if own.text_type >= MASTER_TEXT_TYPES as i32 {
                continue;
            }
            let own_slot = slot(own.text_type)?;
            if styles[own_slot].is_none() {
                styles[own_slot] = Some(own);
            }
        }
        Ok(styles)
    }

    fn find_notes(
        &mut self,
        list: &'a Record,
        core: &BTreeMap<i32, Option<&'a Record>>,
        notes_of_slides: &mut HashMap<i32, usize>,
    ) -> Result<(), Unported> {
        for (index, set) in atom_sets(list).into_iter().enumerate() {
            let persist = &list.children()[set.start];
            let Some(record) = core_record(core, persist)? else {
                continue;
            };
            if record.kind != kind::NOTES {
                continue;
            }
            let slide_id = persist
                .int(12)
                .map_err(|_| poi_throws("a short slide persist"))?;
            notes_of_slides.insert(slide_id, index);
            let atom = record
                .last(kind::NOTES_ATOM)
                .ok_or_else(|| poi_throws("Could not read NotesAtom from the NotesRecord"))?;
            let number = atom.int(0).map_err(|_| poi_throws("a short notes atom"))?;
            let mut sheet = Sheet::new(SheetKind::Notes, record, number)?;
            sheet.add_drawing_lists()?;
            self.supply(&mut sheet)?;
            self.notes.push(sheet);
        }
        Ok(())
    }

    fn find_slides(
        &mut self,
        list: &'a Record,
        core: &BTreeMap<i32, Option<&'a Record>>,
        notes_of_slides: &HashMap<i32, usize>,
    ) -> Result<(), Unported> {
        for (index, set) in atom_sets(list).into_iter().enumerate() {
            let persist = &list.children()[set.start];
            let Some(record) =
                core_record(core, persist)?.filter(|record| record.kind == kind::SLIDE)
            else {
                continue;
            };
            let Some(atom) = record.last(kind::SLIDE_ATOM) else {
                continue;
            };
            let notes_id = atom.int(16).map_err(|_| poi_throws("a short slide atom"))?;
            let notes = if notes_id == 0 {
                None
            } else {
                notes_of_slides
                    .get(&notes_id)
                    .copied()
                    .filter(|&position| position < self.notes.len())
            };
            let number = persist
                .int(12)
                .map_err(|_| poi_throws("a short slide persist"))?;
            let mut sheet = Sheet::new(SheetKind::Slide, record, number)?;
            if set.len() > 1 {
                let lists = find_paragraphs(list.children(), set.start + 1..set.end, true)?;
                if lists.is_empty() {
                    return Err(poi_throws("No text records found for slide"));
                }
                for paragraphs in lists {
                    let ids = sheet.add_list(paragraphs);
                    sheet.lists.push(ids);
                }
            }
            sheet.add_drawing_lists()?;
            self.supply(&mut sheet)?;
            self.slides.push(Slide {
                sheet,
                number: index as i32 + 1,
                notes,
            });
        }
        Ok(())
    }

    /// `HSLFSheet.setSlideShow`: the hyperlinks of each list of the sheet applied.
    fn supply(&self, sheet: &mut Sheet<'a>) -> Result<(), Unported> {
        let objects = self.objects();
        for list in sheet.lists.clone() {
            apply_hyperlinks(&mut sheet.paragraphs, &list, objects)?;
        }
        Ok(())
    }

    /// `HSLFSlide.getMasterSheet`: the slide or title master a slide names.
    pub(super) fn master_of(&self, sheet: &Sheet) -> Option<MasterRef<'_, 'a>> {
        let id = sheet.master_id()?;
        if let Some(master) = self.masters.iter().find(|master| master.sheet.number == id) {
            return Some(MasterRef::Slide(master));
        }
        self.title_masters
            .iter()
            .find(|master| master.number == id)
            .map(MasterRef::Title)
    }

    /// Whether a paragraph of `sheet` is a bullet (`isBullet`): by its own flags, else by its
    /// master's styles.
    pub(super) fn is_bullet(&self, paragraph: &Paragraph, sheet: &Sheet) -> Result<bool, Unported> {
        if let Some(bullet) = paragraph.own_bullet() {
            return Ok(bullet);
        }
        let master = match sheet.kind {
            SheetKind::Notes | SheetKind::SlideMaster => None,
            SheetKind::Slide => self.master_of(sheet),
            SheetKind::TitleMaster => Some(MasterRef::Title(sheet)),
        };
        let master = match master {
            None => return Ok(false),
            Some(MasterRef::Slide(master)) => master,
            Some(MasterRef::Title(title)) => {
                let Some(id) = title.master_id() else {
                    return Ok(false);
                };
                let Some(master) = self.masters.iter().find(|master| master.sheet.number == id)
                else {
                    return Ok(false);
                };
                master
            }
        };
        Ok(master
            .bullet(paragraph.run_type, paragraph.indent_level())?
            .unwrap_or(false))
    }
}

/// The master a slide names.
pub(super) enum MasterRef<'s, 'a> {
    Slide(&'s SlideMaster<'a>),
    Title(&'s Sheet<'a>),
}

impl<'s, 'a> MasterRef<'s, 'a> {
    pub(super) fn sheet(&self) -> &'s Sheet<'a> {
        match self {
            MasterRef::Slide(master) => &master.sheet,
            MasterRef::Title(sheet) => sheet,
        }
    }
}

/// A master's styles' slot for a text type, failing where Java's array index does.
fn slot(text_type: i32) -> Result<usize, Unported> {
    usize::try_from(text_type)
        .ok()
        .filter(|&slot| slot < MASTER_TEXT_TYPES)
        .ok_or_else(|| poi_throws("a master's styles of a text type outside its array"))
}

/// `HSLFSlideMaster.setSlideShow`'s check that the styles cover each paragraph of the master.
fn master_styles_cover(master: &SlideMaster) -> Result<(), Unported> {
    for paragraph in &master.sheet.paragraphs {
        let styles = usize::try_from(paragraph.run_type)
            .ok()
            .filter(|&slot| slot < MASTER_TEXT_TYPES)
            .and_then(|slot| master.styles[slot].as_ref())
            .ok_or_else(|| poi_throws("Master styles not initialized"))?;
        let level = paragraph.indent_level();
        let covered = |list: &Option<Vec<_>>| {
            list.as_ref()
                .is_some_and(|list: &Vec<_>| list.len() as i64 > i64::from(level))
        };
        if !covered(&styles.characters) || !covered(&styles.paragraphs) {
            return Err(poi_throws("Master styles not initialized"));
        }
    }
    Ok(())
}

/// The slide atom sets of a slide list (`getSlideAtomsSets`): each slide persist atom and the
/// records up to the next, as ranges of its children.
pub(super) fn atom_sets(list: &Record) -> Vec<std::ops::Range<usize>> {
    let children = list.children();
    let mut sets = Vec::new();
    let mut i = 0;
    while i < children.len() {
        if children[i].kind == kind::SLIDE_PERSIST_ATOM {
            let end = children[i + 1..]
                .iter()
                .position(|child| child.kind == kind::SLIDE_PERSIST_ATOM)
                .map_or(children.len(), |at| i + 1 + at);
            sets.push(i..end);
            i = end;
        } else {
            i += 1;
        }
    }
    sets
}

/// `getCoreRecordForSAS`: the most recent record of a slide persist atom's reference.
fn core_record<'a>(
    core: &BTreeMap<i32, Option<&'a Record>>,
    persist: &Record,
) -> Result<Option<&'a Record>, Unported> {
    let reference = persist
        .int(0)
        .map_err(|_| poi_throws("a short slide persist"))?;
    Ok(core.get(&reference).copied().flatten())
}

/// `findMostRecentCoreRecords`: for each persist id, the record at the offset the last persist
/// pointer holder by offset gives it, where one of the records that know their offset is there.
fn most_recent(records: &[Top]) -> Result<BTreeMap<i32, Option<&Record>>, Unported> {
    let mut latest: HashMap<i32, i32> = HashMap::new();
    for top in records {
        let Some(record) = &top.record else {
            continue;
        };
        if record.kind == kind::PERSIST_PTR_FULL_BLOCK
            || record.kind == kind::PERSIST_PTR_INCREMENTAL_BLOCK
        {
            latest.extend(slide_locations(record)?);
        }
    }
    let mut by_offset: HashMap<i32, i32> = HashMap::new();
    for (&id, &offset) in &latest {
        if by_offset.insert(offset, id).is_some() {
            return Err(unported(
                "persist ids that share an offset, of which POI keeps one in hash order",
            ));
        }
    }
    let mut core: BTreeMap<i32, Option<&Record>> = latest.keys().map(|&id| (id, None)).collect();
    for top in records {
        let Some(record) = &top.record else {
            continue;
        };
        if !is_position_dependent(record.kind) {
            continue;
        }
        if let Some(id) = by_offset.get(&top.offset) {
            core.insert(*id, Some(record));
        }
    }
    Ok(core)
}

/// Whether records of a type know their offset (`PositionDependentRecord`).
fn is_position_dependent(record_kind: u16) -> bool {
    matches!(
        record_kind,
        kind::BINARY_TAG_DATA
            | kind::DOCUMENT_ENCRYPTION_ATOM
            | kind::ENVIRONMENT
            | kind::HAND_OUT
            | kind::PROG_TAGS
            | kind::PROG_BINARY_TAG
            | kind::EX_OLE_OBJ_STG
            | kind::DOCUMENT
            | kind::MAIN_MASTER
            | kind::SLIDE
            | kind::NOTES
            | kind::PERSIST_PTR_FULL_BLOCK
            | kind::PERSIST_PTR_INCREMENTAL_BLOCK
            | kind::USER_EDIT_ATOM
    )
}
