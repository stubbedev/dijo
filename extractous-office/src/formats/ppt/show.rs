//! A presentation as POI's `HSLFSlideShowImpl` and `HSLFSlideShow` read it: the document stream's
//! records reached from the current user's edit through every user edit and its persist
//! pointers, the most recent record of each persist id, and the masters, notes and slides of the
//! document's slide lists with their text.
//!
//! The records are kept as an index of where each lies in the document stream, and built from
//! it where they are read: the document and the masters for the whole presentation, a slide or
//! notes while it is written, the records borrowing their bytes from the stream.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use crate::config::ParserConfigs;
use crate::formats::poi::{Thrown, Unread};
use crate::formats::{Decline, Difference, Unported};
use crate::ole2::{EntryId, EntryKind, Ole2, Unreadable};

use super::drawing::{textbox_records, Drawing, Wrapper};
use super::encryption::Encryption;
use super::records::{build_at, kind, persist_pointers, Depth, Record};
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
/// in a loop unread.
const MAX_USER_EDITS: usize = 100_000;
/// The slide lists' instances: of slides, masters and notes.
const SLIDES: i32 = 0;
const MASTERS: i32 = 1;
const NOTES: i32 = 2;
/// The text types a master's styles are kept for.
const MASTER_TEXT_TYPES: usize = 9;

fn unported(reason: impl Into<String>) -> Unread {
    Unported(reason.into()).into()
}

fn poi_throws(reason: impl Into<String>) -> Unread {
    Thrown::new(reason.into()).into()
}

/// A record of the document stream at the top level, as the index keeps it.
#[derive(Debug)]
pub(super) struct Top {
    pub(super) offset: i32,
    /// The record's type, `None` where it claims more bytes than the stream holds.
    pub(super) kind: Option<u16>,
    /// The persist id a persist pointer gives its offset.
    pub(super) persist_id: Option<i32>,
    /// The offset of each persist id, of a persist pointer holder.
    pointers: Option<BTreeMap<i32, i32>>,
}

/// The directory POI reads the presentation from: the dual storage's where there is one.
pub(super) fn presentation_directory(file: &Ole2<'_>, root: EntryId) -> Result<EntryId, Unread> {
    match file.child_ignoring_case(root, DUAL_STORAGE) {
        None => Ok(root),
        Some(entry) if file.kind(entry) != EntryKind::Document => Ok(entry),
        Some(_) => Err(poi_throws("the dual storage is no directory")),
    }
}

/// `CurrentUserAtom`'s offset of the current edit, as `readCurrentUserStream` reads it.
fn current_edit_offset(file: &Ole2<'_>, directory: EntryId) -> Result<i64, Unread> {
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

/// The document stream of a presentation, in place where it lies in one run of the compound
/// document, and the offset of its current edit.
pub(super) fn document_stream<'a>(
    file: &Ole2<'a>,
    directory: EntryId,
) -> Result<(Cow<'a, [u8]>, i32), Unread> {
    let edit = current_edit_offset(file, directory)?;
    let stream = match file.child_ignoring_case(directory, POWERPOINT_DOCUMENT) {
        Some(entry) if file.kind(entry) == EntryKind::Document => entry,
        Some(_) | None => {
            if file.child_ignoring_case(directory, POWERPOINT_95).is_some() {
                return Err(poi_throws(
                    "Based on the Current User stream, you seem to have supplied a PowerPoint95 \
                     file, which isn't supported",
                ));
            }
            return Err(poi_throws(
                "the presentation has no PowerPoint Document stream",
            ));
        }
    };
    let document = file.content(stream).map_err(|Unreadable(reason)| {
        poi_throws(format!("cannot read the document stream: {reason}"))
    })?;
    if document.len() > MAX_DOCUMENT {
        return Err(poi_throws("the document stream is longer than POI reads"));
    }
    Ok((document.bytes(), edit as i32))
}

/// The records of a presentation's document stream as an index, the most recent record of each
/// persist id, and its encryption where it is encrypted.
pub(super) struct Records {
    pub(super) tops: Vec<Top>,
    /// `findMostRecentCoreRecords`: each persist id's most recent record, by its index in
    /// `tops`.
    core: BTreeMap<i32, Option<usize>>,
    pub(super) encryption: Option<Encryption>,
    /// Where Tika reads the records otherwise, if it does: persist pointers that give two
    /// persist ids one offset, of which POI keeps the one a `java.util.HashMap` iterates last,
    /// where the native reading keeps the greatest id (#186).
    pub(super) difference: Option<Difference>,
}

/// Why Tika reads persist pointers that give two persist ids one offset otherwise.
const SHARED_OFFSET: &str = "persist pointers that give two persist ids one offset, of which \
                             POI keeps the one a java.util.HashMap iterates last, and the port \
                             the greatest";

/// `HSLFSlideShowImpl`'s records: the document stream's records found and built once, the
/// stream decrypted in place with the password where the presentation is encrypted, then the
/// most recent record of each persist id.
pub(super) fn read_records(
    document: &mut Cow<'_, [u8]>,
    edit: i32,
    configs: &ParserConfigs,
) -> Result<Records, Decline<'static>> {
    let (mut map, persist_ids, shared) = chain(document, edit)?;
    let encryption = encryption(document, &mut map, configs)?;
    let mut tops = Vec::with_capacity(map.len());
    let mut decrypted = Vec::with_capacity(map.len());
    for (offset, chained) in map {
        let persist_id = persist_ids.get(&offset).copied();
        let pending = matches!(chained, Chained::Pending);
        let (kind, pointers) = match chained {
            Chained::Edit { .. } => (Some(kind::USER_EDIT_ATOM), None),
            Chained::Pointers { kind, pointers } => (Some(kind), Some(pointers)),
            Chained::Encryption => (Some(kind::DOCUMENT_ENCRYPTION_ATOM), None),
            Chained::Pending => {
                if let Some(encryption) = &encryption {
                    let persist_id =
                        persist_id.expect("a persist pointer gives each pending record's offset");
                    encryption.decrypt_record(document.to_mut(), persist_id, offset)?;
                }
                let record = build_at(document, i64::from(offset), Depth::TOP)?;
                let pointers = match &record {
                    Some(record) if is_pointer_holder(record.kind) => {
                        Some(slide_locations(record)?)
                    }
                    _ => None,
                };
                (record.map(|record| record.kind), pointers)
            }
        };
        decrypted.push(encryption.is_some() && pending);
        tops.push(Top {
            offset,
            kind,
            persist_id,
            pointers,
        });
    }
    check_decrypted_apart(document, &tops, &decrypted)?;
    let (core, shared_most_recent) = most_recent(&tops);
    Ok(Records {
        tops,
        core,
        encryption,
        difference: (shared || shared_most_recent).then(|| Difference(SHARED_OFFSET.to_owned())),
    })
}

/// Fails where a record decrypted in place overlaps another record of the index: POI built
/// each record as it decrypted it, where they are built again from the stream decrypted whole.
fn check_decrypted_apart(document: &[u8], tops: &[Top], decrypted: &[bool]) -> Result<(), Unread> {
    let mut any_end = 0usize;
    let mut decrypted_end = 0usize;
    for (top, &was_decrypted) in tops.iter().zip(decrypted) {
        let start = top.offset as usize;
        let claimed = document.get(start + 4..start + 8).map_or(0, |bytes| {
            u32::from_le_bytes(bytes.try_into().expect("four bytes"))
        });
        let end = start.saturating_add(8).saturating_add(claimed as usize);
        if start < any_end && (was_decrypted || start < decrypted_end) {
            return Err(unported(
                "an encrypted record that overlaps another, which POI decrypts as it builds them",
            ));
        }
        any_end = any_end.max(end);
        if was_decrypted {
            decrypted_end = decrypted_end.max(end);
        }
    }
    Ok(())
}

/// Whether records of a type are persist pointer holders.
fn is_pointer_holder(record_kind: u16) -> bool {
    record_kind == kind::PERSIST_PTR_FULL_BLOCK
        || record_kind == kind::PERSIST_PTR_INCREMENTAL_BLOCK
}

/// A record of the map `HSLFSlideShowImpl.read` builds: a user edit, its persist pointers, the
/// encryption atom `HSLFSlideShowEncrypted` builds, or a record still to be built.
enum Chained {
    Edit {
        /// Where its persist pointers are.
        pointers: i32,
        /// The persist id of its encryption session, -1 for none.
        session: i32,
    },
    Pointers {
        kind: u16,
        pointers: BTreeMap<i32, i32>,
    },
    Encryption,
    Pending,
}

/// The records `initRecordOffsets` maps by offset: the user edits from `edit` back with their
/// persist pointers, and a slot for each record the pointers give; each slot's persist id, the
/// greatest of the ids a holder gives it; and whether a holder gives one slot several ids.
type Chain = (BTreeMap<i32, Chained>, HashMap<i32, i32>, bool);

/// `initRecordOffsets`.
fn chain(document: &[u8], edit: i32) -> Result<Chain, Unread> {
    let mut map: BTreeMap<i32, Chained> = BTreeMap::new();
    let mut persist_ids: HashMap<i32, i32> = HashMap::new();
    let mut shared = false;
    let mut offset = edit;
    let mut followed = 0usize;
    while offset != 0 {
        followed += 1;
        if followed > MAX_USER_EDITS {
            return Err(poi_throws(
                "user edits POI follows in a loop, which never ends Tika's parse",
            ));
        }
        let built = build_at(document, i64::from(offset), Depth::TOP)?;
        let Some(user) = built.filter(|record| record.kind == kind::USER_EDIT_ATOM) else {
            return Err(poi_throws("Did not have a user edit atom"));
        };
        let pointers_offset = user.int(12).map_err(|_| poi_throws("a short user edit"))?;
        let mut previous = user.int(8).map_err(|_| poi_throws("a short user edit"))?;
        let session = if user.data().len() >= 32 {
            user.int(28).map_err(|_| poi_throws("a short user edit"))?
        } else {
            -1
        };
        map.insert(
            offset,
            Chained::Edit {
                pointers: pointers_offset,
                session,
            },
        );
        let pointers = build_at(document, i64::from(pointers_offset), Depth::TOP)?
            .filter(|record| is_pointer_holder(record.kind))
            .ok_or_else(|| poi_throws("Powerpoint document is missing a PersistPtrHolder"))?;
        let lookup = slide_locations(&pointers)?;
        let mut offsets = HashSet::new();
        for &location in lookup.values() {
            shared |= !offsets.insert(location);
        }
        map.insert(
            pointers_offset,
            Chained::Pointers {
                kind: pointers.kind,
                pointers: lookup.clone(),
            },
        );
        for (&id, &location) in &lookup {
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
    Ok((map, persist_ids, shared))
}

/// `PersistPtrHolder.getSlideLocationsLookup`: each persist id's offset, the last a holder gives.
pub(super) fn slide_locations(holder: &Record) -> Result<BTreeMap<i32, i32>, Unread> {
    let pointers = persist_pointers(holder.data())
        .map_err(|_| poi_throws("persist pointers run past their record"))?;
    Ok(pointers.into_iter().collect())
}

/// `new HSLFSlideShowEncrypted(docstream, records)`: the encryption of the most recent user
/// edit that names an encryption session the persist pointers give, its atom added to the
/// records, opened with the password.
fn encryption(
    document: &[u8],
    map: &mut BTreeMap<i32, Chained>,
    configs: &ParserConfigs,
) -> Result<Option<Encryption>, Decline<'static>> {
    let mut found = None;
    for chained in map.values().rev() {
        let Chained::Edit { pointers, session } = chained else {
            continue;
        };
        if *session == -1 {
            continue;
        }
        let Some(Chained::Pointers { pointers, .. }) = map.get(pointers) else {
            return Err(poi_throws("Encountered an unexpected record-type").into());
        };
        found = pointers.get(session).copied();
        break;
    }
    let Some(location) = found else {
        return Ok(None);
    };
    if !matches!(map.get(&location), None | Some(Chained::Pending)) {
        return Err(poi_throws("Did not have a DocumentEncryptionAtom").into());
    }
    let atom = build_at(document, i64::from(location), Depth::TOP)?
        .filter(|record| record.kind == kind::DOCUMENT_ENCRYPTION_ATOM)
        .ok_or_else(|| poi_throws("Did not have a DocumentEncryptionAtom"))?;
    let info = atom
        .encryption()
        .expect("a DocumentEncryptionAtom holds its encryption");
    let encryption = Encryption::open(location, info, configs)?;
    map.insert(location, Chained::Encryption);
    Ok(Some(encryption))
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
    pub(super) container: &'a Record<'a>,
    /// `_getSheetNumber`.
    pub(super) number: i32,
    pub(super) drawing: &'a Drawing<'a>,
    pub(super) paragraphs: Vec<Paragraph<'a>>,
    /// `getTextParagraphs`: lists of paragraphs, by their index in `paragraphs`.
    pub(super) lists: Vec<Vec<usize>>,
}

impl<'a> Sheet<'a> {
    fn new(kind: SheetKind, container: &'a Record<'a>, number: i32) -> Result<Sheet<'a>, Unread> {
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
    fn add_drawing_lists(&mut self) -> Result<(), Unread> {
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
    fn wrapper_list(&mut self, wrapper: Wrapper) -> Result<Option<Vec<usize>>, Unread> {
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
        shape: &'a super::drawing::Node<'a>,
    ) -> Result<Found<'a>, Unread> {
        let records = match textbox_records(shape) {
            Some(Ok(records)) => records,
            Some(Err(unread)) => return Err(unread),
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
    fn slide_atom(&self) -> Option<&'a Record<'a>> {
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
    pub(super) fn bullet(&self, text_type: i32, level: i32) -> Result<Option<bool>, Unread> {
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
    fn flags_of(&self, text_type: i32, level: i32) -> Result<Option<bool>, Unread> {
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

/// A notes sheet of the notes list, by where its record is.
#[derive(Debug)]
pub(super) struct NotesEntry {
    offset: i32,
    /// `_getSheetNumber`: its notes atom's slide id.
    number: i32,
}

/// A slide (`HSLFSlide`) of the slide list, by where its record and its text are.
#[derive(Debug)]
pub(super) struct SlideEntry {
    offset: i32,
    /// Its slide persist atom and the records after it, among the slide list's children.
    set: Range<usize>,
    /// `_getSheetNumber`: its slide persist atom's slide id.
    sheet_number: i32,
    /// `getSlideNumber`.
    pub(super) number: i32,
    /// The index of its notes in the presentation's notes.
    pub(super) notes: Option<usize>,
}

/// The records a presentation keeps built while it is written: its document record, and the
/// records of the slide atom sets of its masters list.
pub(super) struct Kept<'d> {
    document: Record<'d>,
    document_offset: i32,
    masters: Vec<Option<Record<'d>>>,
}

impl<'d> Kept<'d> {
    /// `new HSLFSlideShow(impl)`'s most recent document record, and the most recent record of
    /// each slide atom set of its masters list, built from `source`.
    pub(super) fn read(source: &'d [u8], records: &Records) -> Result<Kept<'d>, Unread> {
        let top = records
            .core
            .values()
            .rev()
            .filter_map(|&top| top.map(|index| &records.tops[index]))
            .find(|top| top.kind == Some(kind::DOCUMENT))
            .ok_or_else(|| {
                poi_throws(
                    "The PowerPoint file didn't contain a Document Record in its PersistPtr \
                     blocks",
                )
            })?;
        let document = build(source, top.offset)?;
        let mut masters = Vec::new();
        if let Some(list) = slide_list(&document, MASTERS) {
            for set in atom_sets(list) {
                masters.push(match records.core_top(&list.children()[set.start])? {
                    Some(top) => Some(build(source, top.offset)?),
                    None => None,
                });
            }
        }
        Ok(Kept {
            document_offset: top.offset,
            document,
            masters,
        })
    }
}

/// The record at `offset` of `source`, which the index found built there.
pub(super) fn build(source: &[u8], offset: i32) -> Result<Record<'_>, Unread> {
    build_at(source, i64::from(offset), Depth::TOP)?.ok_or_else(|| {
        poi_throws("a record the index found built does not build again from the same bytes")
    })
}

/// The document's slide list of an instance (`getSlideListWithText`), if it has one.
fn slide_list<'r, 'd>(document: &'r Record<'d>, instance: i32) -> Option<&'r Record<'d>> {
    document
        .children()
        .iter()
        .skip(1)
        .filter(|child| child.kind == kind::SLIDE_LIST_WITH_TEXT)
        .find(|list| list.instance() == instance)
}

impl Records {
    /// `getCoreRecordForSAS`: the most recent record of a slide persist atom's reference, if
    /// it has one.
    fn core_top(&self, persist: &Record) -> Result<Option<&Top>, Unread> {
        let reference = persist
            .int(0)
            .map_err(|_| poi_throws("a short slide persist"))?;
        Ok(self
            .core
            .get(&reference)
            .copied()
            .flatten()
            .map(|index| &self.tops[index]))
    }
}

/// A presentation (`HSLFSlideShow`): its document, masters and title masters, and its notes and
/// slides by where their records are.
#[derive(Debug)]
pub(super) struct Show<'a> {
    source: &'a [u8],
    pub(super) document: &'a Record<'a>,
    /// Where the document record is in the document stream.
    pub(super) document_offset: i32,
    slide_list: Option<&'a Record<'a>>,
    pub(super) masters: Vec<SlideMaster<'a>>,
    pub(super) title_masters: Vec<Sheet<'a>>,
    pub(super) notes: Vec<NotesEntry>,
    pub(super) slides: Vec<SlideEntry>,
}

impl<'a> Show<'a> {
    /// `new HSLFSlideShow(impl)`: the masters, then the notes and slides, each sheet read once
    /// as POI reads it, failing where POI fails.
    pub(super) fn new(
        source: &'a [u8],
        records: &Records,
        kept: &'a Kept<'a>,
    ) -> Result<Show<'a>, Unread> {
        let document = &kept.document;
        let mut show = Show {
            source,
            document,
            document_offset: kept.document_offset,
            slide_list: slide_list(document, SLIDES),
            masters: Vec::new(),
            title_masters: Vec::new(),
            notes: Vec::new(),
            slides: Vec::new(),
        };
        if let Some(masters) = slide_list(document, MASTERS) {
            show.find_masters(masters, kept)?;
        }
        let mut notes_of_slides = HashMap::new();
        if let Some(notes) = slide_list(document, NOTES) {
            show.find_notes(notes, records, &mut notes_of_slides)?;
        }
        if let Some(slides) = show.slide_list {
            show.find_slides(slides, records, &notes_of_slides)?;
        }
        Ok(show)
    }

    /// The document's `ExObjList`, if it has one.
    pub(super) fn objects(&self) -> Option<&'a Record<'a>> {
        self.document.last_after_first(kind::EX_OBJ_LIST)
    }

    /// The record at `offset` of the document stream.
    pub(super) fn build(&self, offset: i32) -> Result<Record<'a>, Unread> {
        build(self.source, offset)
    }

    fn find_masters(&mut self, list: &'a Record<'a>, kept: &'a Kept<'a>) -> Result<(), Unread> {
        for (set, record) in atom_sets(list).into_iter().zip(&kept.masters) {
            let persist = &list.children()[set.start];
            let number = persist
                .int(12)
                .map_err(|_| poi_throws("a short slide persist"))?;
            match record.as_ref().map(|record| (record.kind, record)) {
                Some((kind::SLIDE, record)) => {
                    let mut sheet = Sheet::new(SheetKind::TitleMaster, record, number)?;
                    sheet.add_drawing_lists()?;
                    self.supply(&mut sheet)?;
                    self.title_masters.push(sheet);
                }
                Some((kind::MAIN_MASTER, container)) => {
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
    fn master_styles(&self, master: &Record) -> Result<Vec<Option<MasterStyles>>, Unread> {
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
        list: &'a Record<'a>,
        records: &Records,
        notes_of_slides: &mut HashMap<i32, usize>,
    ) -> Result<(), Unread> {
        for (index, set) in atom_sets(list).into_iter().enumerate() {
            let persist = &list.children()[set.start];
            let Some(top) = records.core_top(persist)? else {
                continue;
            };
            if top.kind != Some(kind::NOTES) {
                continue;
            }
            let offset = top.offset;
            let slide_id = persist
                .int(12)
                .map_err(|_| poi_throws("a short slide persist"))?;
            notes_of_slides.insert(slide_id, index);
            let record = self.build(offset)?;
            let atom = record
                .last(kind::NOTES_ATOM)
                .ok_or_else(|| poi_throws("Could not read NotesAtom from the NotesRecord"))?;
            let number = atom.int(0).map_err(|_| poi_throws("a short notes atom"))?;
            let entry = NotesEntry { offset, number };
            self.notes_sheet(&entry, &record)?;
            self.notes.push(entry);
        }
        Ok(())
    }

    fn find_slides(
        &mut self,
        list: &'a Record<'a>,
        records: &Records,
        notes_of_slides: &HashMap<i32, usize>,
    ) -> Result<(), Unread> {
        for (index, set) in atom_sets(list).into_iter().enumerate() {
            let persist = &list.children()[set.start];
            let Some(offset) = records
                .core_top(persist)?
                .filter(|top| top.kind == Some(kind::SLIDE))
                .map(|top| top.offset)
            else {
                continue;
            };
            let record = self.build(offset)?;
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
            let sheet_number = persist
                .int(12)
                .map_err(|_| poi_throws("a short slide persist"))?;
            let entry = SlideEntry {
                offset,
                set,
                sheet_number,
                number: index as i32 + 1,
                notes,
            };
            self.slide_sheet(&entry, &record)?;
            self.slides.push(entry);
        }
        Ok(())
    }

    /// The record of a slide, built from the document stream.
    pub(super) fn slide_record(&self, slide: &SlideEntry) -> Result<Record<'a>, Unread> {
        self.build(slide.offset)
    }

    /// The record of notes, built from the document stream.
    pub(super) fn notes_record(&self, notes: &NotesEntry) -> Result<Record<'a>, Unread> {
        self.build(notes.offset)
    }

    /// The sheet of a slide, of its `record`: its slide list's text, then its drawing's.
    pub(super) fn slide_sheet<'r>(
        &'r self,
        slide: &SlideEntry,
        record: &'r Record<'a>,
    ) -> Result<Sheet<'r>, Unread> {
        let mut sheet = Sheet::new(SheetKind::Slide, record, slide.sheet_number)?;
        if slide.set.len() > 1 {
            let list = self
                .slide_list
                .expect("a slide of the slide list the document has");
            let lists = find_paragraphs(list.children(), slide.set.start + 1..slide.set.end, true)?;
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
        Ok(sheet)
    }

    /// The sheet of notes, of its `record`.
    pub(super) fn notes_sheet<'r>(
        &'r self,
        notes: &NotesEntry,
        record: &'r Record<'a>,
    ) -> Result<Sheet<'r>, Unread> {
        let mut sheet = Sheet::new(SheetKind::Notes, record, notes.number)?;
        sheet.add_drawing_lists()?;
        self.supply(&mut sheet)?;
        Ok(sheet)
    }

    /// `HSLFSheet.setSlideShow`: the hyperlinks of each list of the sheet applied.
    fn supply(&self, sheet: &mut Sheet<'_>) -> Result<(), Unread> {
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
    pub(super) fn is_bullet(&self, paragraph: &Paragraph, sheet: &Sheet) -> Result<bool, Unread> {
        if let Some(bullet) = paragraph.own_bullet() {
            return Ok(bullet);
        }
        let bullet = |master: &SlideMaster| {
            Ok(master
                .bullet(paragraph.run_type, paragraph.indent_level())?
                .unwrap_or(false))
        };
        let title: &Sheet<'_> = match sheet.kind {
            SheetKind::Notes | SheetKind::SlideMaster => return Ok(false),
            SheetKind::TitleMaster => sheet,
            SheetKind::Slide => match self.master_of(sheet) {
                None => return Ok(false),
                Some(MasterRef::Slide(master)) => return bullet(master),
                Some(MasterRef::Title(title)) => title,
            },
        };
        let master = title
            .master_id()
            .and_then(|id| self.masters.iter().find(|master| master.sheet.number == id));
        match master {
            Some(master) => bullet(master),
            None => Ok(false),
        }
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
fn slot(text_type: i32) -> Result<usize, Unread> {
    usize::try_from(text_type)
        .ok()
        .filter(|&slot| slot < MASTER_TEXT_TYPES)
        .ok_or_else(|| poi_throws("a master's styles of a text type outside its array"))
}

/// `HSLFSlideMaster.setSlideShow`'s check that the styles cover each paragraph of the master.
fn master_styles_cover(master: &SlideMaster) -> Result<(), Unread> {
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
pub(super) fn atom_sets(list: &Record) -> Vec<Range<usize>> {
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

/// `findMostRecentCoreRecords`: for each persist id, the record at the offset the last persist
/// pointer holder by offset gives it, where one of the records that know their offset is there,
/// by its index among the tops; an offset several ids share the greatest's; and whether any
/// share one.
fn most_recent(tops: &[Top]) -> (BTreeMap<i32, Option<usize>>, bool) {
    let mut latest: BTreeMap<i32, i32> = BTreeMap::new();
    for top in tops {
        if let Some(pointers) = &top.pointers {
            latest.extend(pointers);
        }
    }
    let mut by_offset: HashMap<i32, i32> = HashMap::new();
    let mut shared = false;
    for (&id, &offset) in &latest {
        shared |= by_offset.insert(offset, id).is_some();
    }
    let mut core: BTreeMap<i32, Option<usize>> = latest.keys().map(|&id| (id, None)).collect();
    for (index, top) in tops.iter().enumerate() {
        let Some(record_kind) = top.kind else {
            continue;
        };
        if !is_position_dependent(record_kind) {
            continue;
        }
        if let Some(id) = by_offset.get(&top.offset) {
            core.insert(*id, Some(index));
        }
    }
    (core, shared)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Persist ids that share an offset give its record to the greatest of them, where POI gives
    /// it to the one a `java.util.HashMap` iterates last, and the reading says so (#186).
    #[test]
    fn gives_a_shared_offset_to_the_greatest_persist_id() {
        let top = |offset, kind, pointers: Option<&[(i32, i32)]>| Top {
            offset,
            kind: Some(kind),
            persist_id: None,
            pointers: pointers.map(|pointers| pointers.iter().copied().collect()),
        };
        let tops = [
            top(0, kind::SLIDE, None),
            top(40, kind::SLIDE, None),
            top(
                80,
                kind::PERSIST_PTR_INCREMENTAL_BLOCK,
                Some(&[(17, 0), (3, 40), (2, 40)]),
            ),
        ];
        let (core, shared) = most_recent(&tops);
        assert!(shared);
        assert_eq!(
            core.into_iter().collect::<Vec<_>>(),
            [(2, None), (3, Some(1)), (17, Some(0))]
        );
        let (_, shared) = most_recent(&tops[..2]);
        assert!(!shared);
    }
}
