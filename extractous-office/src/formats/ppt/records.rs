//! The records of a PowerPoint document stream as POI's `Record.createRecordForType` builds them:
//! containers with their children, atoms with the bytes their constructors keep, slide drawings
//! and the drawing group as Escher, each constructor's own checks, and the exceptions they throw,
//! which POI passes over for a record that claims more bytes than its source holds.

use crate::formats::crypt::{EncryptionInfo, Mode};
use crate::formats::escher::{self, Escher, Factory};
use crate::formats::poi::{i16_at, i32_at, safely_clone, u16_at, u8_at, Read, Thrown, Unread};
use crate::formats::Unported;

use super::drawing::Drawing;

/// Record types, as POI's `RecordTypes` numbers them.
pub(super) mod kind {
    pub(crate) const DOCUMENT: u16 = 1000;
    pub(crate) const DOCUMENT_ATOM: u16 = 1001;
    pub(crate) const SLIDE: u16 = 1006;
    pub(crate) const SLIDE_ATOM: u16 = 1007;
    pub(crate) const NOTES: u16 = 1008;
    pub(crate) const NOTES_ATOM: u16 = 1009;
    pub(crate) const ENVIRONMENT: u16 = 1010;
    pub(crate) const SLIDE_PERSIST_ATOM: u16 = 1011;
    pub(crate) const MAIN_MASTER: u16 = 1016;
    pub(crate) const SS_SLIDE_INFO_ATOM: u16 = 1017;
    pub(crate) const VBA_INFO: u16 = 1023;
    pub(crate) const VBA_INFO_ATOM: u16 = 1024;
    pub(crate) const EX_OBJ_LIST: u16 = 1033;
    pub(crate) const EX_OBJ_LIST_ATOM: u16 = 1034;
    pub(crate) const PP_DRAWING_GROUP: u16 = 1035;
    pub(crate) const PP_DRAWING: u16 = 1036;
    pub(crate) const ROUND_TRIP_HF_PLACEHOLDER: u16 = 1056;
    pub(crate) const LIST: u16 = 2000;
    pub(crate) const FONT_COLLECTION: u16 = 2005;
    pub(crate) const SOUND_COLLECTION: u16 = 2020;
    pub(crate) const SOUND: u16 = 2022;
    pub(crate) const SOUND_DATA: u16 = 2023;
    pub(crate) const COLOR_SCHEME_ATOM: u16 = 2032;
    pub(crate) const EX_OBJ_REF_ATOM: u16 = 3009;
    pub(crate) const OE_PLACEHOLDER_ATOM: u16 = 3011;
    pub(crate) const OUTLINE_TEXT_REF_ATOM: u16 = 3998;
    pub(crate) const TEXT_HEADER_ATOM: u16 = 3999;
    pub(crate) const TEXT_CHARS_ATOM: u16 = 4000;
    pub(crate) const STYLE_TEXT_PROP_ATOM: u16 = 4001;
    pub(crate) const MASTER_TEXT_PROP_ATOM: u16 = 4002;
    pub(crate) const TX_MASTER_STYLE_ATOM: u16 = 4003;
    pub(crate) const TEXT_RULER_ATOM: u16 = 4006;
    pub(crate) const TEXT_BYTES_ATOM: u16 = 4008;
    pub(crate) const TEXT_SPEC_INFO_ATOM: u16 = 4010;
    pub(crate) const STYLE_TEXT_PROP9_ATOM: u16 = 4012;
    pub(crate) const FONT_ENTITY_ATOM: u16 = 4023;
    pub(crate) const FONT_EMBEDDED_DATA: u16 = 4024;
    pub(crate) const C_STRING: u16 = 4026;
    pub(crate) const EX_OLE_OBJ_ATOM: u16 = 4035;
    pub(crate) const HAND_OUT: u16 = 4041;
    pub(crate) const EX_EMBED: u16 = 4044;
    pub(crate) const EX_EMBED_ATOM: u16 = 4045;
    pub(crate) const EX_HYPERLINK_ATOM: u16 = 4051;
    pub(crate) const EX_HYPERLINK: u16 = 4055;
    pub(crate) const HEADERS_FOOTERS: u16 = 4057;
    pub(crate) const HEADERS_FOOTERS_ATOM: u16 = 4058;
    pub(crate) const TX_INTERACTIVE_INFO_ATOM: u16 = 4063;
    pub(crate) const EX_CONTROL: u16 = 4078;
    pub(crate) const SLIDE_LIST_WITH_TEXT: u16 = 4080;
    pub(crate) const ANIMATION_INFO_ATOM: u16 = 4081;
    pub(crate) const INTERACTIVE_INFO: u16 = 4082;
    pub(crate) const INTERACTIVE_INFO_ATOM: u16 = 4083;
    pub(crate) const USER_EDIT_ATOM: u16 = 4085;
    pub(crate) const DATE_TIME_MC_ATOM: u16 = 4087;
    pub(crate) const EX_CONTROL_ATOM: u16 = 4091;
    pub(crate) const EX_MEDIA_ATOM: u16 = 4100;
    pub(crate) const EX_VIDEO_CONTAINER: u16 = 4101;
    pub(crate) const EX_AVI_MOVIE: u16 = 4102;
    pub(crate) const EX_MCI_MOVIE: u16 = 4103;
    pub(crate) const EX_OLE_OBJ_STG: u16 = 4113;
    pub(crate) const ANIMATION_INFO: u16 = 4116;
    pub(crate) const PROG_TAGS: u16 = 5000;
    pub(crate) const PROG_BINARY_TAG: u16 = 5002;
    pub(crate) const BINARY_TAG_DATA: u16 = 5003;
    pub(crate) const PERSIST_PTR_FULL_BLOCK: u16 = 6001;
    pub(crate) const PERSIST_PTR_INCREMENTAL_BLOCK: u16 = 6002;
    pub(crate) const COMMENT2000: u16 = 12000;
    pub(crate) const COMMENT2000_ATOM: u16 = 12001;
    pub(crate) const DOCUMENT_ENCRYPTION_ATOM: u16 = 12052;
}

/// `RecordAtom.getMaxRecordLength`.
const MAX_ATOM: i64 = 1_000_000;
/// The largest record `UnknownRecordPlaceholder` copies.
const MAX_UNKNOWN: i64 = 20_000_000;
/// The largest `ExOleObjStg` and `SoundData`.
const MAX_STORAGE: i64 = 100_000_000;
/// The largest `ExOleObjAtom` and drawing group.
const MAX_OLE_ATOM: i64 = 10_485_760;
/// The largest `MasterTextPropAtom` and `StyleTextProp9Atom`.
const MAX_STYLE_ATOM: i64 = 100_000;
/// The largest `FontEmbeddedData`.
const MAX_FONT: i64 = 5_000_000;
/// The longest a `SlidePersistAtom`'s reserved fields may be.
const MAX_PERSIST_RESERVED: i64 = 32;

pub(super) type Built<T> = Result<T, Unread>;

/// How deep a record lies among the records, the Escher records and the client data holding
/// it, which this port follows only so far, where Java's stack would end first.
#[derive(Debug, Clone, Copy)]
pub(super) struct Depth(usize);

impl Depth {
    pub(super) const TOP: Depth = Depth(0);
    const MAX: usize = 64;

    /// The depth of what a record at this depth holds.
    pub(super) fn deeper(self) -> Built<Depth> {
        if self.0 >= Self::MAX {
            return Err(
                Unported("records nested deeper than this port follows them".to_owned()).into(),
            );
        }
        Ok(Depth(self.0 + 1))
    }
}

/// A record of the document stream.
#[derive(Debug)]
pub(super) struct Record {
    pub(super) kind: u16,
    /// The first two bytes of its header: its version and instance.
    pub(super) options: u16,
    pub(super) body: Body,
}

/// What a record holds.
#[derive(Debug)]
pub(super) enum Body {
    /// An atom's bytes after its header, as far as its constructor reads them.
    Atom(Vec<u8>),
    Container(Vec<Record>),
    /// A slide's drawing (`PPDrawing`).
    Drawing(Box<Drawing>),
    /// The document's drawing group (`PPDrawingGroup`): its `DggContainer`.
    DrawingGroup(Box<Escher<'static>>),
    /// A `DocumentEncryptionAtom`: the encryption it reads, as CryptoAPI where its version
    /// allows.
    Encryption(Box<EncryptionInfo>),
}

impl Record {
    /// `getInstance` as POI's records read it from a signed short: the options shifted.
    pub(super) fn instance(&self) -> i32 {
        i32::from(self.options as i16 >> 4)
    }

    /// The children of a container, none for an atom.
    pub(super) fn children(&self) -> &[Record] {
        match &self.body {
            Body::Container(children) => children,
            _ => &[],
        }
    }

    /// The bytes of an atom.
    pub(super) fn data(&self) -> &[u8] {
        match &self.body {
            Body::Atom(data) => data,
            _ => &[],
        }
    }

    /// `findFirstOfType`.
    pub(super) fn first(&self, kind: u16) -> Option<&Record> {
        self.children().iter().find(|child| child.kind == kind)
    }

    /// The last child of the type, which the containers that keep one of their children of a
    /// type, assigning each they meet, keep.
    pub(super) fn last(&self, kind: u16) -> Option<&Record> {
        self.children()
            .iter()
            .rev()
            .find(|child| child.kind == kind)
    }

    /// The last child of the type after the first, as `Document` keeps its environment, drawing
    /// group and objects.
    pub(super) fn last_after_first(&self, kind: u16) -> Option<&Record> {
        self.children()
            .iter()
            .skip(1)
            .rev()
            .find(|child| child.kind == kind)
    }

    /// `LittleEndian.getInt` of the atom's bytes at `at`.
    pub(super) fn int(&self, at: usize) -> Read<i32> {
        i32_at(self.data(), at)
    }

    pub(super) fn short(&self, at: usize) -> Read<i16> {
        i16_at(self.data(), at)
    }

    pub(super) fn byte(&self, at: usize) -> Read<u8> {
        u8_at(self.data(), at)
    }

    /// `CString.getText`, and the text of every atom POI decodes as UTF-16LE: the whole units
    /// of its bytes.
    pub(super) fn utf16(&self) -> Vec<u16> {
        java_utf16(self.data())
    }

    /// `TextBytesAtom.getText`: its bytes as ISO-8859-1.
    pub(super) fn latin1(&self) -> Vec<u16> {
        self.data().iter().map(|&byte| u16::from(byte)).collect()
    }

    /// The drawing of a `PPDrawing`.
    pub(super) fn drawing(&self) -> Option<&Drawing> {
        match &self.body {
            Body::Drawing(drawing) => Some(drawing),
            _ => None,
        }
    }

    /// The `DggContainer` of a `PPDrawingGroup`.
    pub(super) fn drawing_group(&self) -> Option<&Escher<'static>> {
        match &self.body {
            Body::DrawingGroup(group) => Some(group),
            _ => None,
        }
    }

    /// The encryption of a `DocumentEncryptionAtom`.
    pub(super) fn encryption(&self) -> Option<&EncryptionInfo> {
        match &self.body {
            Body::Encryption(info) => Some(info),
            _ => None,
        }
    }
}

/// UTF-16LE bytes decoded as Java's `new String(bytes, UTF_16LE)` does, kept as Java's units: a
/// trailing odd byte dropped and each surrogate without its pair replaced by U+FFFD.
pub(super) fn java_utf16(bytes: &[u8]) -> Vec<u16> {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let mut decoded = Vec::with_capacity(units.len());
    for unit in char::decode_utf16(units) {
        match unit {
            Ok(character) => {
                let mut buffer = [0; 2];
                decoded.extend_from_slice(character.encode_utf16(&mut buffer));
            }
            Err(_) => decoded.push(0xFFFD),
        }
    }
    decoded
}

/// `LittleEndian.getUInt` read as POI's records then cast it to `int`, at a Java offset.
fn uint_at(data: &[u8], at: i64) -> Read<i64> {
    let at = usize::try_from(at).map_err(|_| {
        Thrown::new(format!(
            "Index {at} out of bounds for length {}",
            data.len()
        ))
    })?;
    Ok(i64::from(i32_at(data, at)? as u32))
}

fn ushort_at(data: &[u8], at: i64) -> Read<u16> {
    let at = usize::try_from(at).map_err(|_| {
        Thrown::new(format!(
            "Index {at} out of bounds for length {}",
            data.len()
        ))
    })?;
    u16_at(data, at)
}

/// `Record.buildRecordAtOffset`: the record at `offset`, `None` where it claims more bytes than
/// `source` holds and its constructor throws.
pub(super) fn build_at(source: &[u8], offset: i64, depth: Depth) -> Built<Option<Record>> {
    let kind = ushort_at(source, offset + 2)?;
    let length = record_length(uint_at(source, offset + 4)?)?;
    create(kind, source, offset, length, depth)
}

/// `8 + rleni` of a record's claimed length, its negative int read as nothing.
fn record_length(claimed: i64) -> Built<i64> {
    let length = (claimed as i32).max(0);
    if length > i32::MAX - 8 {
        return Err(Unported(
            "a record claims a length whose int arithmetic POI overflows".to_owned(),
        )
        .into());
    }
    Ok(8 + i64::from(length))
}

/// `Record.findChildRecords`: the records from `start` up to `length` bytes on.
pub(super) fn children_at(
    source: &[u8],
    start: i64,
    length: i64,
    depth: Depth,
) -> Built<Vec<Record>> {
    let mut children = Vec::new();
    let mut position = start;
    while position <= start + length - 8 {
        let kind = ushort_at(source, position + 2)?;
        let claimed = record_length(uint_at(source, position + 4)?)?;
        if position == 0 && kind == 0 && claimed - 8 == 0xFFFF {
            return Err(Thrown::new(
                "Corrupt document - starts with record of type 0000 and length 0xFFFF",
            )
            .into());
        }
        if let Some(child) = create(kind, source, position, claimed, depth)? {
            children.push(child);
        }
        position += claimed;
    }
    Ok(children)
}

/// `Record.createRecordForType`: the record its type's constructor builds, `None` where the
/// constructor throws on a record that claims more bytes than `source` holds.
fn create(
    kind: u16,
    source: &[u8],
    start: i64,
    length: i64,
    depth: Depth,
) -> Built<Option<Record>> {
    match construct(kind, source, start, length, depth) {
        Ok(body) => {
            let options = u16_at(source, start as usize).unwrap_or_default();
            Ok(Some(Record {
                kind,
                options,
                body,
            }))
        }
        Err(Unread::Thrown(_)) if start + length > source.len() as i64 => Ok(None),
        Err(fault) => Err(fault),
    }
}

/// `IOUtils.safelyClone(source, offset, length, max)` at Java offsets.
fn clone(source: &[u8], offset: i64, length: i64, max: i64) -> Read<Vec<u8>> {
    safely_clone(source, offset, length, max)
}

/// The bytes after the header up to `fixed` bytes from the start, which a constructor reads
/// whatever the record claims, failing where `source` ends first.
fn fixed(source: &[u8], start: i64, fixed: i64, length: i64) -> Read<Vec<u8>> {
    if start + fixed > source.len() as i64 {
        return Err(Thrown::new(format!(
            "Index {} out of bounds for length {}",
            start + fixed - 1,
            source.len()
        )));
    }
    let end = (start + fixed.max(length)).min(source.len() as i64);
    Ok(source[(start + 8) as usize..end as usize].to_vec())
}

/// An atom whose constructor copies what follows its header, up to `max` bytes.
fn atom(source: &[u8], start: i64, length: i64, max: i64) -> Read<Body> {
    Ok(Body::Atom(clone(source, start + 8, length - 8, max)?))
}

/// A container whose `findInterestingChildren` reads its first `needed` children, failing
/// where it has fewer.
fn container(source: &[u8], start: i64, length: i64, needed: usize, depth: Depth) -> Built<Body> {
    let children = children_at(source, start + 8, length - 8, depth.deeper()?)?;
    if children.len() < needed {
        return Err(Thrown::new(format!(
            "Index {} out of bounds for length {}",
            children.len(),
            children.len()
        ))
        .into());
    }
    Ok(Body::Container(children))
}

/// The constructor of a record type, as `RecordTypes` names it, run on the record.
fn construct(kind: u16, source: &[u8], start: i64, length: i64, depth: Depth) -> Built<Body> {
    use kind::*;
    Ok(match kind {
        SLIDE | NOTES | MAIN_MASTER | VBA_INFO | LIST | SOUND_COLLECTION | HAND_OUT
        | HEADERS_FOOTERS | SLIDE_LIST_WITH_TEXT | INTERACTIVE_INFO | PROG_TAGS
        | PROG_BINARY_TAG | BINARY_TAG_DATA | COMMENT2000 => {
            container(source, start, length, 0, depth)?
        }
        EX_HYPERLINK | EX_MCI_MOVIE | EX_AVI_MOVIE | ANIMATION_INFO => {
            container(source, start, length, 1, depth)?
        }
        EX_EMBED | EX_CONTROL | EX_VIDEO_CONTAINER | SOUND => {
            container(source, start, length, 2, depth)?
        }
        DOCUMENT => {
            let body = container(source, start, length, 1, depth)?;
            if body_children(&body)[0].kind != DOCUMENT_ATOM {
                return Err(
                    Thrown::new("The first child of a Document must be a DocumentAtom").into(),
                );
            }
            body
        }
        ENVIRONMENT => {
            let body = container(source, start, length, 0, depth)?;
            if !body_children(&body)
                .iter()
                .any(|child| child.kind == FONT_COLLECTION)
            {
                return Err(
                    Thrown::new("Environment didn't contain a FontCollection record!").into(),
                );
            }
            body
        }
        EX_OBJ_LIST => {
            let body = container(source, start, length, 1, depth)?;
            if body_children(&body)[0].kind != EX_OBJ_LIST_ATOM {
                return Err(Thrown::new("First child record wasn't a ExObjListAtom").into());
            }
            body
        }
        FONT_COLLECTION => {
            let body = container(source, start, length, 0, depth)?;
            for child in body_children(&body) {
                match child.kind {
                    FONT_ENTITY_ATOM if child.data().len() < 68 => {
                        return Err(Thrown::new("a font entity is too short for its fields").into())
                    }
                    FONT_EMBEDDED_DATA => {
                        return Err(Unported(
                            "an embedded font, whose header POI reads, which this port does not"
                                .to_owned(),
                        )
                        .into())
                    }
                    _ => {}
                }
            }
            body
        }
        PP_DRAWING => Body::Drawing(Box::new(Drawing::read(source, start, depth.deeper()?)?)),
        PP_DRAWING_GROUP => {
            let contents = clone(source, start, length, MAX_OLE_ATOM)?;
            let (record, _) = escher::made_at(&contents, 0, Factory::Slides)?;
            let Some(group) = record.children.into_iter().next() else {
                return Err(Thrown::new("Index 0 out of bounds for length 0").into());
            };
            if group.content != escher::Content::Container {
                return Err(Thrown::new("the drawing group's first record is no container").into());
            }
            Body::DrawingGroup(Box::new(group.into_owned()))
        }
        DOCUMENT_ATOM => {
            let needed = length.max(48);
            if needed - 48 > MAX_ATOM {
                return Err(Thrown::new(format!(
                    "Tried to allocate an array of length {}",
                    needed - 48
                ))
                .into());
            }
            Body::Atom(fixed(source, start, needed, length)?)
        }
        SLIDE_ATOM => {
            let length = length.max(30);
            let data = fixed(source, start, 30, length)?;
            clone(source, start + 30, length - 30, MAX_ATOM)?;
            Body::Atom(data)
        }
        NOTES_ATOM => {
            let length = length.max(8);
            let data = fixed(source, start, 14, length)?;
            clone(source, start + 14, length - 14, MAX_ATOM)?;
            Body::Atom(data)
        }
        SLIDE_PERSIST_ATOM => {
            let length = length.max(8);
            let data = fixed(source, start, 24, length)?;
            clone(source, start + 24, length - 24, MAX_PERSIST_RESERVED)?;
            Body::Atom(data)
        }
        SS_SLIDE_INFO_ATOM => Body::Atom(fixed(source, start, 24, 24)?),
        TEXT_HEADER_ATOM => {
            if length < 12 && (source.len() as i64) - start < 12 {
                return Err(Thrown::new("Not enough data to form a TextHeaderAtom").into());
            }
            Body::Atom(fixed(source, start, 12, length.max(12))?)
        }
        STYLE_TEXT_PROP_ATOM => {
            let mut length = length;
            if length < 18 {
                length = 18;
                if (source.len() as i64) - start < 18 {
                    return Err(Thrown::new("Not enough data to form a StyleTextPropAtom").into());
                }
            }
            atom(source, start, length, MAX_ATOM)?
        }
        COLOR_SCHEME_ATOM => Body::Atom(fixed(source, start, 40, length)?),
        DATE_TIME_MC_ATOM | OE_PLACEHOLDER_ATOM => Body::Atom(fixed(source, start, 16, length)?),
        EX_OBJ_REF_ATOM | OUTLINE_TEXT_REF_ATOM | EX_CONTROL_ATOM => {
            Body::Atom(fixed(source, start, 12, length)?)
        }
        VBA_INFO_ATOM => Body::Atom(fixed(source, start, 20, length)?),
        ROUND_TRIP_HF_PLACEHOLDER => Body::Atom(fixed(source, start, 9, length)?),
        USER_EDIT_ATOM => {
            let length = length.max(34);
            let read = if 36 < length { 40 } else { 36 };
            let data = fixed(source, start, read, read)?;
            if read != length {
                return Err(Thrown::new(format!(
                    "Having invalid data in UserEditAtom: len: {length}"
                ))
                .into());
            }
            Body::Atom(data)
        }
        PERSIST_PTR_FULL_BLOCK | PERSIST_PTR_INCREMENTAL_BLOCK => {
            let data = clone(source, start + 8, length.max(8) - 8, MAX_ATOM)?;
            persist_pointers(&data)?;
            Body::Atom(data)
        }
        TEXT_RULER_ATOM => {
            check_ruler(source, start, length)?;
            Body::Atom(Vec::new())
        }
        STYLE_TEXT_PROP9_ATOM => {
            let data = clone(source, start + 8, length - 8, MAX_STYLE_ATOM)?;
            check_numbering(&data)?;
            Body::Atom(data)
        }
        MASTER_TEXT_PROP_ATOM => atom(source, start, length, MAX_STYLE_ATOM)?,
        EX_OLE_OBJ_ATOM => {
            let body = atom(source, start, length, MAX_OLE_ATOM)?;
            at_least(&body, 24, "ExOleObjAtom")?;
            body
        }
        EX_OLE_OBJ_STG | SOUND_DATA => atom(source, start, length, MAX_STORAGE)?,
        FONT_EMBEDDED_DATA => {
            let body = atom(source, start, length, MAX_FONT)?;
            at_least(&body, 4, "FontEmbeddedData")?;
            body
        }
        EX_EMBED_ATOM => {
            let body = atom(source, start, length, MAX_ATOM)?;
            at_least(&body, 8, "ExEmbedAtom")?;
            body
        }
        EX_HYPERLINK_ATOM | EX_OBJ_LIST_ATOM => {
            let body = atom(source, start, length, MAX_ATOM)?;
            at_least(&body, 4, "ExHyperlinkAtom")?;
            body
        }
        INTERACTIVE_INFO_ATOM => {
            let body = atom(source, start, length, MAX_ATOM)?;
            at_least(&body, 16, "InteractiveInfoAtom")?;
            body
        }
        C_STRING | TEXT_CHARS_ATOM | TEXT_BYTES_ATOM => {
            atom(source, start, length.max(8), MAX_ATOM)?
        }
        ANIMATION_INFO_ATOM
        | COMMENT2000_ATOM
        | TX_MASTER_STYLE_ATOM
        | TEXT_SPEC_INFO_ATOM
        | FONT_ENTITY_ATOM
        | HEADERS_FOOTERS_ATOM
        | TX_INTERACTIVE_INFO_ATOM
        | EX_MEDIA_ATOM => atom(source, start, length, MAX_ATOM)?,
        DOCUMENT_ENCRYPTION_ATOM => {
            let body = source
                .get(start as usize + 8..source.len().min((start + length) as usize))
                .unwrap_or_default();
            let info = EncryptionInfo::read(body, Mode::CryptoApi)
                .map_err(|unopened| unopened.unported())?;
            Body::Encryption(Box::new(info))
        }
        _ => {
            let contents = clone(source, start, length.max(0), MAX_UNKNOWN)?;
            u16_at(&contents, 2)?;
            Body::Atom(Vec::new())
        }
    })
}

fn body_children(body: &Body) -> &[Record] {
    match body {
        Body::Container(children) => children,
        _ => &[],
    }
}

/// An atom's check that it holds at least `length` bytes.
fn at_least(body: &Body, length: usize, name: &str) -> Read<()> {
    let Body::Atom(data) = body else {
        return Ok(());
    };
    if data.len() < length {
        return Err(Thrown::new(format!(
            "The length of the data for a {name} must be at least {length} bytes, but was only {}",
            data.len()
        )));
    }
    Ok(())
}

/// `PersistPtrHolder`'s reading of its data: each persist id and the offset of its record, in
/// the order the data gives them.
pub(super) fn persist_pointers(data: &[u8]) -> Read<Vec<(i32, i32)>> {
    let mut pointers = Vec::new();
    let mut position = 0usize;
    while position < data.len() {
        let info = i32_at(data, position)?;
        let first = info & 0x000F_FFFF;
        let count = (info & !0x000F_FFFF) >> 20;
        position += 4;
        for i in 0..count {
            let offset = i32_at(data, position)?;
            pointers.push((first.wrapping_add(i), offset));
            position += 4;
        }
    }
    Ok(pointers)
}

/// `TextRulerAtom`'s reading, which throws where its fields run past the record.
fn check_ruler(source: &[u8], start: i64, length: i64) -> Read<()> {
    let end = (start + length.min(MAX_ATOM)).min(source.len() as i64);
    let mut position = (start + 8).min(end.max(start));
    let mut take = |count: i64| -> Read<i64> {
        if count > end - position {
            return Err(Thrown::new("Buffer overrun in a TextRulerAtom"));
        }
        let at = position;
        position += count;
        Ok(at)
    };
    let mask_at = take(4)?;
    let mask = i32_at(source, mask_at as usize)?;
    for bit in [0x0002, 0x0001] {
        if mask & bit != 0 {
            take(2)?;
        }
    }
    if mask & 0x0004 != 0 {
        let count_at = take(2)?;
        let count = i64::from(u16_at(source, count_at as usize)?);
        take(count * 4)?;
    }
    for level in 0..5 {
        for bit in [0x0008 << level, 0x0100 << level] {
            if mask & bit != 0 {
                take(2)?;
            }
        }
    }
    Ok(())
}

/// `StyleTextProp9Atom`'s reading of its numbering exceptions, which throws where one runs past
/// the data.
fn check_numbering(data: &[u8]) -> Read<()> {
    let length = data.len();
    let mut i = 0usize;
    while i < length {
        let mask3 = u8_at(data, i + 2)?;
        let mask4 = u8_at(data, i + 3)?;
        let mut index = i + 4;
        let mut record = 4;
        if mask3 & 0x80 != 0 {
            i16_at(data, index)?;
            index += 2;
            record = 6;
        }
        if mask4 & 2 != 0 {
            i16_at(data, index)?;
            index += 2;
            record += 2;
        }
        if mask4 & 1 != 0 {
            i16_at(data, index)?;
            i16_at(data, index + 2)?;
            record += 4;
        }
        i += record;
        if i + 4 >= length {
            break;
        }
        i32_at(data, i)?;
        i += 4;
        if i + 4 >= length {
            break;
        }
        let si = i32_at(data, i)?;
        i += 4;
        if si & 0x40 != 0 {
            i += 2;
        }
        if i + 4 >= length {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A record of a type with the options and body given.
    pub(crate) fn record(options: u16, kind: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = options.to_le_bytes().to_vec();
        bytes.extend(kind.to_le_bytes());
        bytes.extend((body.len() as u32).to_le_bytes());
        bytes.extend(body);
        bytes
    }

    #[test]
    fn builds_containers_and_atoms_as_poi_does() {
        let header = record(0, kind::TEXT_HEADER_ATOM, &1i32.to_le_bytes());
        let chars = record(0, kind::TEXT_CHARS_ATOM, &[b'h', 0, b'i', 0]);
        let list = record(0x0F, kind::SLIDE_LIST_WITH_TEXT, &[header, chars].concat());
        let built = build_at(&list, 0, Depth::TOP).unwrap().unwrap();
        assert_eq!(built.kind, kind::SLIDE_LIST_WITH_TEXT);
        assert_eq!(built.children().len(), 2);
        assert_eq!(built.children()[0].int(0), Ok(1));
        assert_eq!(
            built.children()[1].utf16(),
            "hi".encode_utf16().collect::<Vec<_>>()
        );
    }

    #[test]
    fn passes_over_a_record_past_the_end_whose_constructor_throws() {
        let short = record(0, kind::TEXT_HEADER_ATOM, &[1, 0, 0, 0]);
        let mut outer = record(0x0F, kind::LIST, &short);
        outer.truncate(outer.len() - 2);
        let built = build_at(&outer, 0, Depth::TOP).unwrap().unwrap();
        assert!(built.children().is_empty());
    }

    #[test]
    fn fails_where_a_constructor_throws_within_the_data() {
        let atom = record(0, kind::INTERACTIVE_INFO_ATOM, &[0; 4]);
        let outer = record(0x0F, kind::INTERACTIVE_INFO, &[atom, vec![0; 16]].concat());
        assert!(matches!(
            build_at(&outer, 0, Depth::TOP),
            Err(Unread::Thrown(_))
        ));
    }

    #[test]
    fn leaves_records_nested_too_deep_to_tika() {
        let mut nested = Vec::new();
        for _ in 0..100 {
            nested = record(0x0F, kind::LIST, &nested);
        }
        assert!(matches!(
            build_at(&nested, 0, Depth::TOP),
            Err(Unread::Unported(_))
        ));
    }

    #[test]
    fn reads_persist_pointers() {
        let mut data = ((2u32 << 20) | 5).to_le_bytes().to_vec();
        data.extend(100i32.to_le_bytes());
        data.extend(200i32.to_le_bytes());
        assert_eq!(persist_pointers(&data), Ok(vec![(5, 100), (6, 200)]));
        assert!(persist_pointers(&data[..8]).is_err());
    }

    #[test]
    fn decodes_utf16_as_java_does() {
        assert_eq!(
            java_utf16(&[0x00, 0xD8, b'a', 0, 7]),
            vec![0xFFFD, u16::from(b'a')]
        );
    }
}
