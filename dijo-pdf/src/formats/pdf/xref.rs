//! Where PDFBox's `COSParser` finds a document's objects: the cross-reference chain from the
//! last `startxref`, merged as `XrefTrailerResolver` merges it, with the repairs its lenient mode
//! makes, which `BruteForceParser` searches the bytes for: a `startxref` or `/Prev` that misses
//! its section, a table whose offsets miss their objects, and the object streams of a document
//! whose table it replaces.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use lopdf::{Dictionary, Object, ObjectId, Stream};

use super::content::{is_digit, is_end_of_name, is_whitespace, Lexer};
use super::cos::{int_value, OBJECT_READ};
use super::security::DocumentId;
use super::Unported;
use crate::formats::Difference;
use crate::input::Paging;

/// `COSParser.DEFAULT_TRAIL_BYTECOUNT`: the bytes at the end searched for `startxref`.
const TRAIL_BYTES: usize = 2048;
/// `COSParser.MINIMUM_SEARCH_OFFSET`: no object starts before it.
const MINIMUM_SEARCH_OFFSET: usize = 6;
/// `BaseParser.OBJECT_NUMBER_THRESHOLD`.
const OBJECT_NUMBER_THRESHOLD: i64 = 10_000_000_000;
/// `BaseParser.GENERATION_NUMBER_THRESHOLD`.
const GENERATION_NUMBER_THRESHOLD: i64 = 65_535;
/// `BaseParser.MAX_LENGTH_LONG`, the digits of `Long.MAX_VALUE`.
const MAX_LENGTH_LONG: usize = 19;
/// `PDFXrefStreamParser`'s limit on the bytes of an entry.
const MAX_ENTRY_BYTES: i64 = 20;
/// How far back from `/XRef` or `/ObjStm` `BruteForceParser` looks for the object's start.
const BACKWARD_STEPS: i64 = 40;

/// `COSObjectKey`: an object's number and generation, the generation as its 16 bits hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Key {
    number: i64,
    generation: u16,
}

impl Key {
    /// `new COSObjectKey(num, gen)`, which refuses negative values.
    fn new(number: i64, generation: i64) -> Result<Self, Thrown> {
        if number < 0 || generation < 0 {
            return Err(Thrown::Io);
        }
        Ok(Self {
            number,
            generation: (generation & 0xFFFF) as u16,
        })
    }

    fn from_id((number, generation): ObjectId) -> Self {
        Self {
            number: i64::from(number),
            generation,
        }
    }

    /// The key as `lopdf` holds one, if its number fits.
    fn id(self) -> Option<ObjectId> {
        Some((u32::try_from(self.number).ok()?, self.generation))
    }
}

/// Where PDFBox's `parseObjectDynamically` reads an object the table lists: from the file at
/// a positive offset, else from the object stream whose number the value negates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Placement {
    File(usize),
    Stream(u32),
}

/// Where the table puts an object: an offset in the file, or the number of the object stream
/// holding it. PDFBox keeps a `Long`, the stream's number negated, so that an object in object
/// stream 0 reads as at offset 0 (#115).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    File(i64),
    Stream(i64),
}

impl Location {
    fn value(self) -> i64 {
        match self {
            Self::File(offset) => offset,
            Self::Stream(number) => -number,
        }
    }
}

/// Why reading stopped: an `IOException` PDFBox catches, or something the port does not do.
#[derive(Debug)]
pub(super) enum Thrown {
    Io,
    Unported(Unported),
}

impl From<Unported> for Thrown {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

pub(super) type Read<T> = Result<T, Thrown>;

/// What PDFBox reads of a document's cross-reference: the objects it finds where, and the
/// trailer.
#[derive(Debug)]
pub(super) struct Resolution {
    table: HashMap<Key, Location>,
    trailer: Dictionary,
    /// For an encrypted document whose objects PDFBox searched for, those it found: it searches
    /// their object streams once it can decrypt them ([`Resolution::object_stream_placements`]).
    encrypted_search: Option<HashMap<Key, Location>>,
    /// Whether PDFBox rebuilt the trailer from a search of the document's objects
    /// (`trailerWasRebuild`).
    rebuilt: bool,
    /// The objects PDFBox read while it read the cross-reference, where it found them, which
    /// its pool keeps for the rest of the document.
    pooled: HashMap<Key, (Location, Object)>,
}

/// What decrypts the data of an object stream the search finds, the stream's key and
/// dictionary given, as `SecurityHandler.decryptStream` does.
pub(super) type Decrypt<'d> = dyn Fn(ObjectId, &Dictionary, &[u8]) -> Read<Vec<u8>> + 'd;

/// What reading a document's cross-reference gave: what PDFBox reads of it, and where the port
/// reads it otherwise, if anywhere.
pub(super) struct CrossReference {
    pub(super) resolution: Resolution,
    pub(super) difference: Option<Difference>,
}

/// Reads the cross-reference of `data` as PDFBox's lenient `COSParser.retrieveTrailer` does,
/// or says why the objects cannot be found. Where the port does not read the cross-reference as
/// PDFBox does, it rebuilds the trailer from a search of the objects, as PDFBox does where its
/// reading fails, and notes the difference; object streams a search finds that the port does
/// not read are left out, the difference noted.
pub(super) fn resolve(data: &[u8], paging: &Paging) -> Result<CrossReference, String> {
    let mut parser = Parser::new(data, paging);
    let (trailer, difference) = match parser.retrieve() {
        Ok(trailer) => (trailer, None),
        Err(Thrown::Io) => (None, None),
        Err(Thrown::Unported(Unported(reason))) => (
            None,
            Some(Difference(format!(
                "the port rebuilds the trailer from a search of the objects, where PDFBox reads \
                 the cross-reference: {reason}"
            ))),
        ),
    };
    let pooled = parser.lookup.pooled();
    let Some(trailer) = trailer.filter(|trailer| trailer.has(b"Root")) else {
        let resolution = rebuild(data, pooled).map_err(|thrown| match thrown {
            Thrown::Io => "PDFBox's search for the objects fails".to_owned(),
            Thrown::Unported(Unported(reason)) => reason,
        })?;
        return Ok(CrossReference {
            resolution,
            difference,
        });
    };
    let mut table = parser.resolved.table;
    let mut encrypted_search = None;
    let mut difference = None;
    if let Some(mut objects) = parser.lookup.brute.objects.take() {
        match trailer.has(b"Encrypt") {
            true => encrypted_search = Some(objects),
            false => {
                let placed = placed(&objects, &table);
                let searched =
                    brute_force_object_streams(data, None, &placed, &mut objects, &mut table);
                if let Err(Unported(reason)) = searched {
                    difference = Some(Difference(format!(
                        "the port leaves out object streams PDFBox's search finds: {reason}"
                    )));
                }
            }
        }
    }
    Ok(CrossReference {
        resolution: Resolution {
            table,
            trailer,
            encrypted_search,
            rebuilt: false,
            pooled,
        },
        difference,
    })
}

/// Where PDFBox finds an object while it searches for object streams: where the document's
/// table puts it, else where the search for objects found it.
fn placed(
    objects: &HashMap<Key, Location>,
    table: &HashMap<Key, Location>,
) -> HashMap<Key, Location> {
    let mut placed = objects.clone();
    placed.extend(table.iter().map(|(key, location)| (*key, *location)));
    placed
}

/// `BruteForceParser.rebuildTrailer`, for a document without a trailer PDFBox can use: the
/// objects a search finds, and a trailer of the catalog and the information dictionary found
/// in a `trailer` of the document, or among the objects. The object streams are searched with
/// no security handler, so an encrypted one reads as corrupt. The objects PDFBox read while it
/// read the cross-reference stay `pooled`.
fn rebuild(data: &[u8], pooled: HashMap<Key, (Location, Object)>) -> Read<Resolution> {
    let mut objects = search_objects(data)?;
    let mut table = objects.clone();
    let mut trailer = Dictionary::new();
    let mut streams_searched = false;
    if !search_trailer(data, &objects, &mut trailer)?
        && !search_trailer_items(data, &objects, &mut trailer)
    {
        let placed = objects.clone();
        brute_force_object_streams(data, None, &placed, &mut objects, &mut table)?;
        streams_searched = true;
        search_trailer_items(data, &objects, &mut trailer);
    }
    if !streams_searched {
        let placed = objects.clone();
        brute_force_object_streams(data, None, &placed, &mut objects, &mut table)?;
    }
    Ok(Resolution {
        table,
        trailer,
        encrypted_search: None,
        rebuilt: true,
        pooled,
    })
}

/// `bfSearchForTrailer`: the first `trailer` of the document whose `/Root` leads to a catalog,
/// its `/Info`, if it leads to an information dictionary, `/Encrypt` and `/ID` with it. PDFBox
/// takes only a trailer of both, so that a document without information loses its trailer
/// (#115).
fn search_trailer(
    data: &[u8],
    objects: &HashMap<Key, Location>,
    trailer: &mut Dictionary,
) -> Read<bool> {
    let mut source = Source::new(data);
    source.lexer.seek(MINIMUM_SEARCH_OFFSET);
    while source.find_string(b"trailer").is_some() {
        source.skip_spaces();
        let found = match source.dictionary() {
            Ok(found) => found,
            Err(Thrown::Io) => continue,
            Err(unported) => return Err(unported),
        };
        let leads_to = |key: &[u8], test: fn(&Dictionary) -> bool| match found.get(key) {
            Ok(Object::Reference(id)) => {
                dereference(data, objects, Key::from_id(*id)).is_some_and(|dict| test(&dict))
            }
            _ => false,
        };
        if leads_to(b"Root", is_catalog) {
            if let Ok(value) = found.get(b"Root") {
                trailer.set("Root", value.clone());
            }
            if leads_to(b"Info", is_info) {
                if let Ok(value) = found.get(b"Info") {
                    trailer.set("Info", value.clone());
                }
            }
            if leads_to(b"Encrypt", |_| true) {
                if let Ok(value) = found.get(b"Encrypt") {
                    trailer.set("Encrypt", value.clone());
                }
            }
            if let Ok(id @ Object::Array(_)) = found.get(b"ID") {
                trailer.set("ID", id.clone());
            }
            return Ok(true);
        }
    }
    Ok(false)
}

/// `searchForTrailerItems`: the catalog and the information dictionary among the objects the
/// search found, the later of two by `compareCOSObjects`; whether a catalog was found.
fn search_trailer_items(
    data: &[u8],
    objects: &HashMap<Key, Location>,
    trailer: &mut Dictionary,
) -> bool {
    let mut keys: Vec<(&Key, &Location)> = objects.iter().collect();
    keys.sort_unstable_by_key(|(key, _)| **key);
    let mut root: Option<Key> = None;
    let mut info: Option<Key> = None;
    for (key, location) in keys {
        let Some(dict) = dereference(data, objects, *key) else {
            continue;
        };
        if is_catalog(&dict) {
            root = Some(later(objects, *key, location.value(), root));
        } else if is_info(&dict) {
            info = Some(later(objects, *key, location.value(), info));
        }
    }
    for (name, key) in [("Root", root), ("Info", info)] {
        if let Some(id) = key.and_then(Key::id) {
            trailer.set(name, Object::Reference(id));
        }
    }
    root.is_some()
}

/// `compareCOSObjects`: of an object found and the one found before, the later generation of
/// one number, else the one at the greater offset.
fn later(objects: &HashMap<Key, Location>, new: Key, new_offset: i64, current: Option<Key>) -> Key {
    let Some(current) = current else {
        return new;
    };
    if current.number == new.number {
        return if current.generation < new.generation {
            new
        } else {
            current
        };
    }
    match objects.get(&current) {
        Some(location) if new_offset > location.value() => new,
        _ => current,
    }
}

/// `isCatalog`.
fn is_catalog(dict: &Dictionary) -> bool {
    matches!(dict.get(b"Type"), Ok(Object::Name(name)) if name == b"Catalog") || dict.has(b"FDF")
}

/// `isInfo`.
fn is_info(dict: &Dictionary) -> bool {
    if dict.has(b"Parent") || dict.has(b"A") || dict.has(b"Dest") {
        return false;
    }
    [
        &b"ModDate"[..],
        b"Title",
        b"Author",
        b"Subject",
        b"Keywords",
        b"Creator",
        b"Producer",
        b"CreationDate",
    ]
    .iter()
    .any(|key| dict.has(key))
}

/// The dictionary of the object `key` where the search found it, as `COSObject.getObject`
/// reads it, or `None` where it is no dictionary or does not read.
fn dereference(data: &[u8], objects: &HashMap<Key, Location>, key: Key) -> Option<Dictionary> {
    match referenced(data, objects, key, &mut Vec::new())? {
        Object::Dictionary(dict) => Some(dict),
        _ => None,
    }
}

/// `COSObject.getObject` of the object `key` while PDFBox finds the document's objects, which
/// `objects` place: what it reads where they put it, `None` where they do not or it fails to
/// read; a stream's dictionary stands for the stream. An object asked for while it is being
/// read, among `reading`, reads as `None`, as a `COSObject` marked dereferenced does.
fn referenced(
    data: &[u8],
    objects: &HashMap<Key, Location>,
    key: Key,
    reading: &mut Vec<Key>,
) -> Option<Object> {
    if reading.contains(&key) {
        return None;
    }
    reading.push(key);
    let object = read_placed(data, objects, key, reading);
    reading.pop();
    object
}

/// The object `key` where `objects` put it, as [`referenced`] reads it.
fn read_placed(
    data: &[u8],
    objects: &HashMap<Key, Location>,
    key: Key,
    reading: &mut Vec<Key>,
) -> Option<Object> {
    match *objects.get(&key)? {
        Location::File(offset) => {
            let mut source = Source::new(data);
            let (found, object) = file_object(&mut source, offset).ok()?;
            (found == key).then_some(object)
        }
        Location::Stream(stream) => {
            let stream_key = Key {
                number: stream,
                generation: 0,
            };
            let Location::File(offset) = *objects.get(&stream_key)? else {
                return None;
            };
            let mut source = Source::new(data);
            let (found, Object::Dictionary(dict)) = file_object(&mut source, offset).ok()? else {
                return None;
            };
            if found != stream_key {
                return None;
            }
            source.start_stream();
            let mut length = |key| Ok(referenced(data, objects, key, reading));
            let raw = source
                .stream_data(stream_length(&dict, &mut length).ok()?)
                .ok()?;
            let decoded = decode(&dict, raw.data).ok()?;
            compressed_object(&dict, &decoded, key.number)
        }
    }
}

/// The key and object at `offset`, as `parseFileObject` starts reading it: a stream's
/// dictionary, the source standing after it.
fn file_object(source: &mut Source, offset: i64) -> Read<(Key, Object)> {
    source.seek(offset)?;
    let number = source.read_object_number()?;
    let generation = source.read_generation_number()?;
    source.read_expected(b"obj")?;
    source.skip_spaces();
    let object = source.lexer.file_object()?.flatten().ok_or(Thrown::Io)?;
    Ok((Key::new(number, generation)?, object))
}

/// The object `number` in an object stream, which `parseObjectStreamObject` reads at its
/// offset after `/First`.
fn compressed_object(stream: &Dictionary, decoded: &[u8], number: i64) -> Option<Object> {
    let count = int_entry(stream, b"N");
    let first = int_entry(stream, b"First");
    if count < 0 || first < 0 {
        return None;
    }
    let mut source = Source::new(decoded);
    let mut offset = None;
    for _ in 0..count {
        if source.position() >= first - 1 {
            break;
        }
        let found = source.read_object_number().ok()?;
        let at = source.read_long().ok()?;
        if found == number {
            offset = Some(at);
        }
    }
    let position = usize::try_from(first + offset?).ok()?;
    Lexer::file(decoded, position).file_object().ok()??
}

impl Resolution {
    /// The encryption dictionary the trailer names, as PDFBox reads it from `data`, each
    /// reference among its entries and those of the dictionaries in it resolved once, as
    /// `getDictionaryObject` resolves it, and the first entry of the trailer's `ID`; `None` for
    /// a document whose trailer names no dictionary for its encryption, which PDFBox takes for
    /// unencrypted (`getCOSDictionary`).
    pub(super) fn encryption(&self, data: &[u8]) -> Option<(Dictionary, DocumentId)> {
        let dictionary = match self.trailer.get(b"Encrypt").ok()? {
            Object::Dictionary(dictionary) => dictionary.clone(),
            Object::Reference(id) => dereference(data, &self.table, Key::from_id(*id))?,
            _ => return None,
        };
        let resolved = |object: &Object| self.resolved_once(data, object);
        let id = match self.trailer.get(b"ID").ok().and_then(resolved) {
            Some(Object::Array(items)) => match items.first() {
                None => DocumentId::Bytes(Vec::new()),
                Some(first) => match resolved(first) {
                    Some(Object::String(bytes, _)) => DocumentId::Bytes(bytes),
                    Some(other) => DocumentId::NotAString(other),
                    None => DocumentId::Null,
                },
            },
            _ => DocumentId::Bytes(Vec::new()),
        };
        Some((self.resolved_entries(data, &dictionary, 0), id))
    }

    /// `COSObject.getObject` of `object` where it is a reference, as PDFBox resolves it once:
    /// `None` for `null` or an object it does not find; a reference read where the reference
    /// leads stays a reference.
    fn resolved_once(&self, data: &[u8], object: &Object) -> Option<Object> {
        let object = match object {
            Object::Reference(id) => {
                referenced(data, &self.table, Key::from_id(*id), &mut Vec::new())?
            }
            object => object.clone(),
        };
        (!matches!(object, Object::Null)).then_some(object)
    }

    /// A copy of `dictionary` with each entry resolved once, and the entries of the
    /// dictionaries among them, `depth` levels down, likewise.
    fn resolved_entries(&self, data: &[u8], dictionary: &Dictionary, depth: usize) -> Dictionary {
        const MAX_DEPTH: usize = 2;
        let mut resolved = Dictionary::new();
        for (key, value) in dictionary.iter() {
            match self.resolved_once(data, value) {
                Some(Object::Dictionary(inner)) if depth < MAX_DEPTH => resolved.set(
                    key.clone(),
                    Object::Dictionary(self.resolved_entries(data, &inner, depth + 1)),
                ),
                Some(object) => resolved.set(key.clone(), object),
                None => {}
            }
        }
        resolved
    }

    /// Where each object the table lists is read from, by its key as `lopdf` names it.
    pub(super) fn placements(&self) -> HashMap<ObjectId, Placement> {
        placements(&self.table)
    }

    /// The number of an object the table lists or PDFBox's pool holds whose number `lopdf`
    /// cannot hold, which the port leaves out, if any.
    pub(super) fn unheld_object(&self) -> Option<i64> {
        self.table
            .keys()
            .chain(self.pooled.keys())
            .find(|key| key.id().is_none())
            .map(|key| key.number)
    }

    /// `bfSearchForObjStreams` of an encrypted document whose objects PDFBox searched for, which
    /// it runs once it has prepared the document's decryption: where the objects of the object
    /// streams the search finds are read from, each stream's data decrypted with `decrypt`.
    pub(super) fn object_stream_placements(
        &self,
        data: &[u8],
        decrypt: &Decrypt,
    ) -> Result<HashMap<ObjectId, Placement>, Unported> {
        let Some(objects) = &self.encrypted_search else {
            return Ok(HashMap::new());
        };
        let placed = placed(objects, &self.table);
        let mut objects = objects.clone();
        let mut table = HashMap::new();
        brute_force_object_streams(data, Some(decrypt), &placed, &mut objects, &mut table)?;
        if let Some(key) = table.keys().find(|key| key.id().is_none()) {
            return Err(Unported(format!(
                "object number {}, which lopdf cannot hold",
                key.number
            )));
        }
        Ok(placements(&table))
    }

    /// The objects PDFBox read while it read the cross-reference, which its pool keeps: where
    /// each was found, and what it read there.
    pub(super) fn pooled(&self) -> Vec<(ObjectId, Placement, &Object)> {
        let locations: HashMap<Key, Location> = self
            .pooled
            .iter()
            .map(|(key, (location, _))| (*key, *location))
            .collect();
        placements(&locations)
            .into_iter()
            .map(|(id, placement)| (id, placement, &self.pooled[&Key::from_id(id)].1))
            .collect()
    }

    /// The trailer, as PDFBox merges it.
    pub(super) fn trailer(&self) -> &Dictionary {
        &self.trailer
    }

    /// Whether PDFBox rebuilt the trailer from a search of the document's objects.
    pub(super) fn rebuilt(&self) -> bool {
        self.rebuilt
    }
}

/// Where each object of `table` is read from, by its key as `lopdf` names it: from the file at
/// a positive offset, else from the object stream whose number the value negates. An object
/// whose number `lopdf` cannot hold is left out ([`Resolution::unheld_object`]).
fn placements(table: &HashMap<Key, Location>) -> HashMap<ObjectId, Placement> {
    let mut placements = HashMap::with_capacity(table.len());
    for (key, location) in table {
        let Some(id) = key.id() else {
            continue;
        };
        let value = location.value();
        let placement = match usize::try_from(value) {
            Ok(offset) if offset > 0 => Placement::File(offset),
            _ => match u32::try_from(-value) {
                Ok(number) => Placement::Stream(number),
                Err(_) => continue,
            },
        };
        placements.insert(id, placement);
    }
    placements
}

/// The first of the objects `ids`, which the table does not list, that PDFBox finds by
/// searching the bytes for it, as `COSParser.getObjectOffset` does for an object it is asked
/// for.
pub(super) fn first_found(data: &[u8], ids: &[ObjectId]) -> Result<Option<ObjectId>, Unported> {
    if ids.is_empty() {
        return Ok(None);
    }
    let objects = match search_objects(data) {
        Ok(objects) => objects,
        Err(Thrown::Io) => {
            return Err(Unported(
                "an object the table does not list, whose search PDFBox fails on".to_owned(),
            ))
        }
        Err(Thrown::Unported(unported)) => return Err(unported),
    };
    Ok(ids
        .iter()
        .copied()
        .find(|&id| objects.contains_key(&Key::from_id(id))))
}

/// `BaseParser`'s reading of the document's bytes.
pub(super) struct Source<'a> {
    data: &'a [u8],
    pub(super) lexer: Lexer<'a>,
}

/// The data of a stream as `COSParser.parseCOSStream` finds it.
pub(super) struct StreamData<'a> {
    /// Where it starts in the document.
    pub(super) start: usize,
    pub(super) data: &'a [u8],
    /// Whether its length was measured up to `endstream`, the `/Length` given not fitting it.
    pub(super) measured: bool,
}

impl<'a> Source<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            lexer: Lexer::file(data, 0),
        }
    }

    pub(super) fn position(&self) -> i64 {
        self.lexer.position() as i64
    }

    /// `RandomAccessRead.seek`, which refuses a negative position.
    pub(super) fn seek(&mut self, position: i64) -> Read<()> {
        let position = usize::try_from(position).map_err(|_| Thrown::Io)?;
        self.lexer.seek(position);
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.lexer.peek()
    }

    fn read(&mut self) -> Option<u8> {
        self.lexer.read()
    }

    pub(super) fn rewind(&mut self, count: usize) {
        self.lexer.rewind(count);
    }

    fn is_eof(&self) -> bool {
        self.lexer.position() >= self.data.len()
    }

    pub(super) fn skip_spaces(&mut self) {
        self.lexer.skip_spaces();
    }

    /// `BaseParser.readString`.
    pub(super) fn read_string(&mut self) -> Vec<u8> {
        self.lexer.read_string()
    }

    /// `isString`: whether `string` follows, without reading it.
    fn is_string(&self, string: &[u8]) -> bool {
        self.data
            .get(self.lexer.position()..)
            .is_some_and(|rest| rest.starts_with(string))
    }

    /// `BaseParser.skipWhiteSpaces`: the spaces and the line break after `stream`.
    fn skip_white_spaces(&mut self) {
        let mut c = self.read();
        while c == Some(b' ') {
            c = self.read();
        }
        if !self.skip_linebreak(c) {
            self.rewind(1);
        }
    }

    fn skip_linebreak(&mut self, c: Option<u8>) -> bool {
        match c {
            Some(b'\r') => {
                if self.read() != Some(b'\n') {
                    self.rewind(1);
                }
                true
            }
            Some(b'\n') => true,
            _ => false,
        }
    }

    /// `BaseParser.readLine`, which fails at the end of the data.
    pub(super) fn read_line(&mut self) -> Read<Vec<u8>> {
        if self.is_eof() {
            return Err(Thrown::Io);
        }
        let mut line = Vec::new();
        let mut c = self.read();
        while let Some(byte) = c {
            if byte == b'\n' || byte == b'\r' {
                break;
            }
            line.push(byte);
            c = self.read();
        }
        if c == Some(b'\r') && self.peek() == Some(b'\n') {
            self.read();
        }
        Ok(line)
    }

    /// `BaseParser.readStringNumber`: the digits up to the first other byte.
    fn read_digits(&mut self) -> Read<Vec<u8>> {
        let mut digits = Vec::new();
        let mut c = self.read();
        while let Some(digit @ b'0'..=b'9') = c {
            digits.push(digit);
            if digits.len() > MAX_LENGTH_LONG {
                return Err(Thrown::Io);
            }
            c = self.read();
        }
        if c.is_some() {
            self.rewind(1);
        }
        Ok(digits)
    }

    /// `BaseParser.readLong`.
    pub(super) fn read_long(&mut self) -> Read<i64> {
        self.skip_spaces();
        let digits = self.read_digits()?;
        parse_long(&digits).ok_or_else(|| {
            self.rewind(digits.len());
            Thrown::Io
        })
    }

    /// `BaseParser.readInt`.
    fn read_int(&mut self) -> Read<i64> {
        self.skip_spaces();
        let digits = self.read_digits()?;
        match parse_long(&digits).filter(|&value| i32::try_from(value).is_ok()) {
            Some(value) => Ok(value),
            None => {
                self.rewind(digits.len());
                Err(Thrown::Io)
            }
        }
    }

    pub(super) fn read_object_number(&mut self) -> Read<i64> {
        let number = self.read_long()?;
        if !(0..OBJECT_NUMBER_THRESHOLD).contains(&number) {
            return Err(Thrown::Io);
        }
        Ok(number)
    }

    pub(super) fn read_generation_number(&mut self) -> Read<i64> {
        let generation = self.read_int()?;
        if !(0..=GENERATION_NUMBER_THRESHOLD).contains(&generation) {
            return Err(Thrown::Io);
        }
        Ok(generation)
    }

    /// `BaseParser.readExpectedString` with the spaces around it.
    pub(super) fn read_expected(&mut self, string: &[u8]) -> Read<()> {
        self.skip_spaces();
        for &expected in string {
            if self.read() != Some(expected) {
                return Err(Thrown::Io);
            }
        }
        self.skip_spaces();
        Ok(())
    }

    /// `BaseParser.parseCOSDictionary`.
    fn dictionary(&mut self) -> Read<Dictionary> {
        self.lexer.file_dictionary()?.ok_or(Thrown::Io)
    }

    /// `COSParser.parseCOSStream` of a stream whose dictionary has been read and whose
    /// `/Length` is `length`, if it has one: its data, after which the source stands after
    /// `endstream`.
    pub(super) fn stream_data(&mut self, length: Option<i64>) -> Read<StreamData<'a>> {
        let start = self.lexer.position();
        let (data, measured) = match length.filter(|&length| self.valid_stream_length(length)) {
            Some(length) => {
                let end = start + length as usize;
                self.lexer.seek(end);
                (&self.data[start..end], false)
            }
            None => (self.read_until_end_stream(), true),
        };
        let end_stream = self.read_string();
        if end_stream == b"endobj" {
            self.rewind(b"endobj".len());
        } else if end_stream.len() > 9 && end_stream.starts_with(b"endstream") {
            self.rewind(end_stream.len() - 9);
        } else if end_stream != b"endstream" {
            return Err(Thrown::Io);
        }
        Ok(StreamData {
            start,
            data,
            measured,
        })
    }

    /// The `stream` keyword and the line break after it, which the data follows.
    pub(super) fn start_stream(&mut self) {
        self.read_string();
        self.skip_white_spaces();
    }

    /// `COSParser.validateStreamLength`.
    fn valid_stream_length(&mut self, length: i64) -> bool {
        let origin = self.lexer.position();
        if length <= 0 || origin as i64 + length > self.data.len() as i64 {
            return false;
        }
        self.lexer.seek(origin + length as usize);
        self.skip_spaces();
        let found = self.is_string(b"endstream");
        self.lexer.seek(origin);
        found
    }

    /// `COSParser.readUntilEndStream`: the data up to the first `endstream` or `endobj`, as
    /// PDFBox's `EndstreamFilterStream` keeps it: without a line feed, or a carriage return and
    /// a line feed, that ends it, unless the stream is longer than 10 bytes and its first 10 look
    /// like text, which keeps the line break (#115).
    fn read_until_end_stream(&mut self) -> &'a [u8] {
        let start = self.lexer.position();
        let rest = &self.data[start..];
        let end = [&b"endstream"[..], b"endobj"]
            .iter()
            .filter_map(|keyword| memchr::memmem::find(rest, keyword))
            .min()
            .unwrap_or(rest.len());
        self.lexer.seek(start + end);
        let content = &rest[..end];
        let text = content.len() > 10
            && content[..10]
                .iter()
                .all(|&b| matches!(b, 0x09 | 0x0A | 0x0D | 0x20..=0x7F));
        if text {
            return content;
        }
        content
            .strip_suffix(b"\r\n")
            .or_else(|| content.strip_suffix(b"\n"))
            .unwrap_or(content)
    }

    /// `BruteForceParser.findString`: from here, where `string` next starts; the source stands
    /// after it, or at the end. PDFBox's search starts over after a partial match at the byte
    /// that broke it, so it misses a match that overlaps the partial one, as `%%EOF` in
    /// `%%%EOF`, and it returns a partial match that runs into the end (#115).
    fn find_string(&mut self, string: &[u8]) -> Option<i64> {
        let start = self.lexer.position().min(self.data.len());
        let found = self.data[start..]
            .windows(string.len())
            .position(|window| window == string);
        match found {
            Some(at) => {
                self.lexer.seek(start + at + string.len());
                Some((start + at) as i64)
            }
            None => {
                self.lexer.seek(self.data.len());
                None
            }
        }
    }
}

/// What reads an indirect object while PDFBox finds the document's objects: what
/// `COSObject.getObject` gives for its key, `None` for nothing.
type Referenced<'r> = dyn FnMut(Key) -> Read<Option<Object>> + 'r;

/// `COSParser.getLength` of a stream PDFBox reads while it finds the objects: its `/Length`,
/// a reference to a number followed with `referenced`, a reference to nothing failing the
/// stream and one to `null` reading as none.
fn stream_length(dict: &Dictionary, referenced: &mut Referenced) -> Read<Option<i64>> {
    match dict.get(b"Length") {
        Err(_) => Ok(None),
        Ok(Object::Integer(length)) => Ok(Some(*length)),
        Ok(Object::Real(length)) => Ok(Some(*length as i64)),
        Ok(Object::Reference(id)) => match referenced(Key::from_id(*id))? {
            Some(Object::Integer(length)) => Ok(Some(length)),
            Some(Object::Real(length)) => Ok(Some(length as i64)),
            Some(Object::Null) => Ok(None),
            _ => Err(Thrown::Io),
        },
        Ok(_) => Err(Thrown::Io),
    }
}

/// [`stream_length`] of a stream PDFBox reads with the objects `objects` places.
fn placed_length(
    data: &[u8],
    objects: &HashMap<Key, Location>,
    dict: &Dictionary,
) -> Read<Option<i64>> {
    stream_length(dict, &mut |key| {
        Ok(referenced(data, objects, key, &mut Vec::new()))
    })
}

/// `Long.parseLong` of digits, which fails on none and on too many.
fn parse_long(digits: &[u8]) -> Option<i64> {
    if digits.is_empty() {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// The number `COSDictionary.getDictionaryObject` gives for `key` of a trailer, a reference
/// followed with `referenced`: `None` for anything else.
fn trailer_number(
    dict: &Dictionary,
    key: &[u8],
    referenced: &mut Referenced,
) -> Read<Option<Object>> {
    let value = match dict.get(key) {
        Err(_) => return Ok(None),
        Ok(Object::Reference(id)) => referenced(Key::from_id(*id))?,
        Ok(value) => Some(value.clone()),
    };
    Ok(value.filter(|value| matches!(value, Object::Integer(_) | Object::Real(_))))
}

/// `COSDictionary.getLong(key, -1)` of a trailer, a reference followed with `referenced`.
fn trailer_long(dict: &Dictionary, key: &[u8], referenced: &mut Referenced) -> Read<i64> {
    Ok(match trailer_number(dict, key, referenced)? {
        Some(Object::Integer(value)) => value,
        Some(Object::Real(value)) => value as i64,
        _ => -1,
    })
}

/// `COSDictionary.getInt(key, -1)` of a trailer, a reference followed with `referenced`: -1
/// also for a number no `int` holds.
fn trailer_int(dict: &Dictionary, key: &[u8], referenced: &mut Referenced) -> Read<i64> {
    Ok(trailer_number(dict, key, referenced)?
        .as_ref()
        .and_then(int_value)
        .map_or(-1, i64::from))
}

/// `XrefTrailerResolver.XrefTrailerObj`.
#[derive(Debug)]
struct Section {
    trailer: Option<Dictionary>,
    table: HashMap<Key, Location>,
}

/// `XrefTrailerResolver`: the sections by where they start, and what they resolve to.
#[derive(Debug, Default)]
struct Resolver {
    sections: BTreeMap<i64, Section>,
    current: Option<i64>,
}

/// `XrefTrailerResolver`'s resolved trailer and table.
#[derive(Debug, Default)]
struct Resolved {
    trailer: Dictionary,
    table: HashMap<Key, Location>,
}

impl Resolver {
    fn next_section(&mut self, start: i64) {
        self.sections.insert(
            start,
            Section {
                trailer: None,
                table: HashMap::new(),
            },
        );
        self.current = Some(start);
    }

    fn current_section(&mut self) -> Option<&mut Section> {
        let current = self.current?;
        self.sections.get_mut(&current)
    }

    /// `setXRef`, which keeps the first entry of a section for a key.
    fn set(&mut self, key: Key, location: Location) {
        if let Some(section) = self.current_section() {
            section.table.entry(key).or_insert(location);
        }
    }

    fn set_trailer(&mut self, trailer: Dictionary) {
        if let Some(section) = self.current_section() {
            section.trailer = Some(trailer);
        }
    }

    fn current_trailer(&mut self) -> Option<&mut Dictionary> {
        self.current_section()?.trailer.as_mut()
    }

    /// `setStartxref`: the sections from the one at `start` along their `/Prev` chain, or all
    /// of them in the order they stand when none starts there, merged oldest first.
    fn resolve(&self, start: i64, referenced: &mut Referenced) -> Read<Resolved> {
        let mut chain = Vec::new();
        match self.sections.get(&start) {
            None => chain.extend(self.sections.keys().copied()),
            Some(mut section) => {
                chain.push(start);
                while let Some(trailer) = &section.trailer {
                    let prev = trailer_long(trailer, b"Prev", referenced)?;
                    if prev == -1 {
                        break;
                    }
                    let Some(previous) = self.sections.get(&prev) else {
                        break;
                    };
                    section = previous;
                    chain.push(prev);
                    if chain.len() >= self.sections.len() {
                        break;
                    }
                }
                chain.reverse();
            }
        }
        let mut resolved = Resolved::default();
        for start in chain {
            let section = &self.sections[&start];
            if let Some(trailer) = &section.trailer {
                for (key, value) in trailer.iter() {
                    resolved.trailer.set(key.clone(), value.clone());
                }
            }
            resolved.table.extend(
                section
                    .table
                    .iter()
                    .map(|(key, location)| (*key, *location)),
            );
        }
        Ok(resolved)
    }
}

/// The indirect objects PDFBox reads while it reads the cross-reference, before the document
/// holds a table: each where a search of the document finds it, and kept once read, as the
/// document's pool keeps it.
struct Lookup<'a> {
    brute: BruteForce<'a>,
    /// The objects read, by key, `None` for those not found or that fail to read.
    pool: HashMap<Key, Option<Object>>,
}

impl Lookup<'_> {
    /// `COSObject.getObject` of the object `key`.
    fn referenced(&mut self, key: Key) -> Read<Option<Object>> {
        if let Some(object) = self.pool.get(&key) {
            return Ok(object.clone());
        }
        let data = self.brute.data;
        let object = referenced(data, self.brute.objects()?, key, &mut Vec::new());
        self.pool.insert(key, object.clone());
        Ok(object)
    }

    /// The objects read and found, each with where the search found it.
    fn pooled(&self) -> HashMap<Key, (Location, Object)> {
        let Some(objects) = &self.brute.objects else {
            return HashMap::new();
        };
        self.pool
            .iter()
            .filter_map(|(key, object)| Some((*key, (*objects.get(key)?, object.clone()?))))
            .collect()
    }
}

/// `COSParser` as far as the cross-reference goes.
struct Parser<'a> {
    source: Source<'a>,
    /// Counts the objects each check of the table reads.
    paging: &'a Paging<'a>,
    resolver: Resolver,
    resolved: Resolved,
    lookup: Lookup<'a>,
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8], paging: &'a Paging<'a>) -> Self {
        Self {
            source: Source::new(data),
            paging,
            resolver: Resolver::default(),
            resolved: Resolved::default(),
            lookup: Lookup {
                brute: BruteForce {
                    data,
                    objects: None,
                },
                pool: HashMap::new(),
            },
        }
    }

    /// `retrieveTrailer` up to where PDFBox rebuilds the trailer: `None` where it does.
    fn retrieve(&mut self) -> Read<Option<Dictionary>> {
        match self.startxref_offset() {
            Some(offset) => self.parse_xref(offset).map(Some),
            None => Ok(None),
        }
    }

    /// `getStartxrefOffset`: where the last `startxref` before the last `%%EOF` of the
    /// document's end stands.
    fn startxref_offset(&self) -> Option<i64> {
        let data = self.source.data;
        let skip = data.len() - data.len().min(TRAIL_BYTES);
        let tail = &data[skip..];
        let eof = last_index_of(b"%%EOF", tail, tail.len()).unwrap_or(tail.len());
        last_index_of(b"startxref", tail, eof).map(|at| (skip + at) as i64)
    }

    /// `parseXref`.
    fn parse_xref(&mut self, offset: i64) -> Read<Dictionary> {
        self.source.seek(offset)?;
        let mut start = self.parse_startxref()?.max(0);
        let fixed = self.check_xref_offset(start)?;
        if fixed > -1 {
            start = fixed;
        }
        let mut prev = start;
        let mut seen: HashSet<i64> = HashSet::new();
        while prev > 0 {
            seen.insert(prev);
            self.source.seek(prev)?;
            self.source.skip_spaces();
            seen.insert(self.source.position());
            if self.source.peek() == Some(b'x') {
                if !self.parse_xref_table(prev)? || !self.parse_trailer()? {
                    return Err(Thrown::Io);
                }
                self.hybrid_stream(prev)?;
                let trailer = self.resolver.current_trailer().ok_or(Thrown::Io)?;
                prev = trailer_long(trailer, b"Prev", &mut |key| self.lookup.referenced(key))?;
            } else {
                prev = self.parse_xref_stream(prev, true)?;
            }
            if prev > 0 {
                let fixed = self.check_xref_offset(prev)?;
                if fixed > -1 && fixed != prev {
                    prev = fixed;
                    if let Some(trailer) = self.resolver.current_trailer() {
                        trailer.set("Prev", Object::Integer(prev));
                    }
                }
            }
            if seen.contains(&prev) {
                return Err(Thrown::Io);
            }
        }
        self.resolved = self
            .resolver
            .resolve(start, &mut |key| self.lookup.referenced(key))?;
        let trailer = self.resolved.trailer.clone();
        self.check_xref_offsets()?;
        Ok(trailer)
    }

    /// The `/XRefStm` of a table's trailer, read into the table's section as a hybrid
    /// document's is.
    fn hybrid_stream(&mut self, prev: i64) -> Read<()> {
        let Some(trailer) = self.resolver.current_trailer() else {
            return Ok(());
        };
        if !trailer.has(b"XRefStm") {
            return Ok(());
        }
        let mut offset = trailer_int(trailer, b"XRefStm", &mut |key| self.lookup.referenced(key))?;
        let fixed = self.check_xref_offset(offset)?;
        if fixed > -1 && fixed != offset {
            offset = fixed;
            if let Some(trailer) = self.resolver.current_trailer() {
                trailer.set("XRefStm", Object::Integer(offset));
            }
        }
        if offset > 0 {
            self.source.seek(offset)?;
            self.source.skip_spaces();
            match self.parse_xref_stream(prev, false) {
                Ok(_) | Err(Thrown::Io) => {}
                Err(unported) => return Err(unported),
            }
        }
        Ok(())
    }

    /// `parseStartXref`.
    fn parse_startxref(&mut self) -> Read<i64> {
        if !self.source.is_string(b"startxref") {
            return Ok(-1);
        }
        self.source.read_string();
        self.source.skip_spaces();
        self.source.read_long()
    }

    /// `checkXRefOffset`: `offset`, where a table or stream starts there, else the nearest
    /// one PDFBox finds, 0 where it finds none, or -1 for offset 0.
    fn check_xref_offset(&mut self, offset: i64) -> Read<i64> {
        self.source.seek(offset)?;
        self.source.skip_spaces();
        if self.source.is_string(b"xref") {
            return Ok(offset);
        }
        if offset > 0 {
            if self.check_xref_stream_offset(offset)? {
                return Ok(offset);
            }
            return self.calculate_xref_fixed_offset(offset);
        }
        Ok(-1)
    }

    /// `checkXRefStreamOffset`.
    fn check_xref_stream_offset(&mut self, offset: i64) -> Read<bool> {
        if offset == 0 {
            return Ok(true);
        }
        self.source.seek(offset - 1)?;
        let next = self.source.read();
        if is_whitespace(next) {
            self.source.skip_spaces();
            if is_digit(self.source.peek()) {
                match self.xref_stream_dictionary() {
                    Ok(dict) => {
                        self.source.seek(offset)?;
                        let is_xref = match dict.get(b"Type") {
                            Ok(Object::Name(name)) => name == b"XRef",
                            Ok(Object::String(text, _)) => text == b"XRef",
                            _ => false,
                        };
                        if is_xref {
                            return Ok(true);
                        }
                    }
                    Err(Thrown::Io) => self.source.seek(offset)?,
                    Err(unported) => return Err(unported),
                }
            }
        }
        Ok(false)
    }

    fn xref_stream_dictionary(&mut self) -> Read<Dictionary> {
        self.source.read_object_number()?;
        self.source.read_generation_number()?;
        self.source.read_expected(b"obj")?;
        self.source.dictionary()
    }

    /// `calculateXRefFixedOffset`.
    fn calculate_xref_fixed_offset(&mut self, offset: i64) -> Read<i64> {
        if offset < 0 {
            return Ok(0);
        }
        let fixed = self.lookup.brute.search_xref(offset);
        Ok(if fixed > -1 { fixed } else { 0 })
    }

    /// `parseXrefTable`.
    fn parse_xref_table(&mut self, start: i64) -> Read<bool> {
        if self.source.peek() != Some(b'x') {
            return Ok(false);
        }
        let xref = self.source.read_string();
        if java_trim(&xref) != b"xref" {
            return Ok(false);
        }
        let next = self.source.read_string();
        self.source.rewind(next.len());
        self.resolver.next_section(start);
        if next.starts_with(b"trailer") {
            return Ok(false);
        }
        loop {
            let line = self.source.read_line()?;
            let fields = split_on_space(&line);
            if fields.len() != 2 {
                return Ok(false);
            }
            let Some(mut number) = parse_signed_long(fields[0]) else {
                return Ok(false);
            };
            let Some(count) =
                parse_signed_long(fields[1]).filter(|&count| i32::try_from(count).is_ok())
            else {
                return Ok(false);
            };
            self.source.skip_spaces();
            for _ in 0..count.max(0) {
                if self.source.is_eof() || is_end_of_name(self.source.peek()) {
                    break;
                }
                if self.source.peek() == Some(b't') {
                    break;
                }
                let line = self.source.read_line()?;
                let fields = split_on_space(&line);
                if fields.len() < 3 {
                    break;
                }
                if fields[fields.len() - 1] == b"n" {
                    let offset = parse_signed_long(fields[0]).ok_or(Thrown::Io)?;
                    if offset > 0 {
                        let generation = parse_signed_long(fields[1])
                            .filter(|&generation| i32::try_from(generation).is_ok())
                            .ok_or(Thrown::Io)?;
                        self.resolver
                            .set(Key::new(number, generation)?, Location::File(offset));
                    }
                } else if fields[2] != b"f" {
                    return Err(Thrown::Io);
                }
                number += 1;
                self.source.skip_spaces();
            }
            self.source.skip_spaces();
            if !is_digit(self.source.peek()) {
                break;
            }
        }
        Ok(true)
    }

    /// `parseTrailer`.
    fn parse_trailer(&mut self) -> Read<bool> {
        while self.source.peek() != Some(b't') && is_digit(self.source.peek()) {
            self.source.read_line()?;
        }
        if self.source.peek() != Some(b't') {
            return Ok(false);
        }
        let line_start = self.source.position();
        let line = self.source.read_line()?;
        if java_trim(&line) != b"trailer" {
            if line.starts_with(b"trailer") {
                self.source.seek(line_start + 7)?;
            } else {
                return Ok(false);
            }
        }
        self.source.skip_spaces();
        let trailer = self.source.dictionary()?;
        self.resolver.set_trailer(trailer);
        self.source.skip_spaces();
        Ok(true)
    }

    /// `parseXrefObjStream`: the stream's entries, in a section of their own when the stream
    /// stands alone, and its `/Prev`.
    fn parse_xref_stream(&mut self, offset: i64, standalone: bool) -> Read<i64> {
        let dict = self.xref_stream_dictionary()?;
        self.source.start_stream();
        let length = stream_length(&dict, &mut |key| self.lookup.referenced(key))?;
        let data = self.source.stream_data(length)?.data;
        if standalone {
            self.resolver.next_section(offset);
            self.resolver.set_trailer(dict.clone());
        }
        let decoded = decode(&dict, data)?;
        for (key, location) in xref_stream_entries(&dict, &decoded)? {
            self.resolver.set(key, location);
        }
        trailer_long(&dict, b"Prev", &mut |key| self.lookup.referenced(key))
    }

    /// `checkXrefOffsets`: the table replaced by the objects a search finds, where an offset
    /// misses its object.
    fn check_xref_offsets(&mut self) -> Read<()> {
        if self.valid_xref_offsets()? {
            return Ok(());
        }
        let objects = self.lookup.brute.objects()?;
        if !objects.is_empty() {
            self.resolved.table = objects.clone();
        }
        Ok(())
    }

    /// `validateXrefOffsets`: whether each offset holds its object, the table corrected for
    /// generations that differ.
    fn valid_xref_offsets(&mut self) -> Read<bool> {
        let mut corrected: Vec<(Key, Key)> = Vec::new();
        let mut valid: HashSet<Key> = HashSet::new();
        let mut entries: Vec<(Key, Location)> = self
            .resolved
            .table
            .iter()
            .map(|(key, location)| (*key, *location))
            .collect();
        entries.sort_unstable_by_key(|(key, _)| *key);
        for (key, location) in entries {
            let Location::File(offset) = location else {
                continue;
            };
            self.paging.read(OBJECT_READ);
            match self.find_object_key(key, offset)? {
                None => return Ok(false),
                Some(found) if found != key => corrected.push((key, found)),
                Some(_) => {
                    valid.insert(key);
                }
            }
        }
        let table = &mut self.resolved.table;
        let pointers: Vec<(Key, Location)> = corrected
            .iter()
            .filter(|(_, found)| !valid.contains(found))
            .map(|(key, found)| (*found, table[key]))
            .collect();
        for (key, _) in &corrected {
            table.remove(key);
        }
        table.extend(pointers);
        Ok(true)
    }

    /// `findObjectKey`: the key of the object at `offset`, if one stands there.
    fn find_object_key(&mut self, mut key: Key, offset: i64) -> Read<Option<Key>> {
        if offset < MINIMUM_SEARCH_OFFSET as i64 {
            return Ok(None);
        }
        match self.object_key_at(&mut key, offset) {
            Ok(found) => Ok(found),
            Err(Thrown::Io) => Ok(None),
            Err(unported) => Err(unported),
        }
    }

    fn object_key_at(&mut self, key: &mut Key, offset: i64) -> Read<Option<Key>> {
        let source = &mut self.source;
        source.seek(offset)?;
        source.skip_white_spaces();
        if source.position() == offset {
            source.seek(offset - 1)?;
            if !is_digit(source.peek()) {
                source.read();
            } else {
                let mut current = source.position();
                current -= 1;
                source.seek(current)?;
                while is_digit(source.peek()) {
                    current -= 1;
                    source.seek(current)?;
                }
                let number = source.read_object_number()?;
                let generation = source.read_generation_number()?;
                let found = Key::new(number, generation)?;
                if let Some(Location::File(existing)) = self.resolved.table.get(&found) {
                    if *existing > 0 && (offset - existing).abs() < 10 {
                        return Ok(None);
                    }
                }
                source.seek(offset)?;
            }
        }
        let number = source.read_object_number()?;
        if number != key.number {
            *key = Key {
                number,
                generation: key.generation,
            };
        }
        let generation = source.read_generation_number()?;
        source.read_expected(b"obj")?;
        let generation = (generation & 0xFFFF) as u16;
        Ok(if generation == key.generation {
            Some(*key)
        } else if generation > key.generation {
            Some(Key {
                number: key.number,
                generation,
            })
        } else {
            None
        })
    }
}

/// `COSParser.lastIndexOf`: where `pattern` last starts before `end`. PDFBox starts over after
/// a partial match at the byte before the one that broke it, so it misses a match that
/// overlaps the partial one (#115).
fn last_index_of(pattern: &[u8], buffer: &[u8], end: usize) -> Option<usize> {
    buffer[..end.min(buffer.len())]
        .windows(pattern.len())
        .rposition(|window| window == pattern)
}

/// Java's `String.trim`.
pub(super) fn java_trim(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|&b| b > b' ').unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| b > b' ')
        .map_or(start, |at| at + 1);
    &bytes[start..end]
}

/// `StringUtil.splitOnSpace`: split at each of Java's `\s`, trailing empty fields dropped.
fn split_on_space(line: &[u8]) -> Vec<&[u8]> {
    if !line.iter().any(|&b| is_java_space(b)) {
        return vec![line];
    }
    let mut fields: Vec<&[u8]> = line.split(|&b| is_java_space(b)).collect();
    while fields.last().is_some_and(|field| field.is_empty()) {
        fields.pop();
    }
    fields
}

fn is_java_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// `Long.parseLong`, with its sign.
fn parse_signed_long(text: &[u8]) -> Option<i64> {
    let digits = text
        .strip_prefix(b"-")
        .or_else(|| text.strip_prefix(b"+"))
        .unwrap_or(text);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(text).ok()?.parse().ok()
}

/// The decoded data of a stream PDFBox reads while it finds the objects.
fn decode(dict: &Dictionary, data: &[u8]) -> Read<Vec<u8>> {
    for key in [&b"Filter"[..], b"DecodeParms"] {
        if matches!(dict.get(key), Ok(Object::Reference(_))) {
            return Err(Thrown::Unported(Unported(
                "a stream PDFBox reads while it finds the objects, whose filters are objects of \
                 their own"
                    .to_owned(),
            )));
        }
    }
    if dict.get(b"Filter").is_err() {
        return Ok(data.to_vec());
    }
    Stream::new(dict.clone(), data.to_vec())
        .decompressed_content()
        .map_err(|_| Thrown::Io)
}

/// `PDFXrefStreamParser.parse`: the entries of a cross-reference stream.
fn xref_stream_entries(dict: &Dictionary, data: &[u8]) -> Read<Vec<(Key, Location)>> {
    let Ok(Object::Array(w_array)) = dict.get(b"W") else {
        return Err(Thrown::Io);
    };
    if w_array.len() != 3 {
        return Err(Thrown::Io);
    }
    let w: Vec<i64> = w_array
        .iter()
        .map(|value| int_value(value).map_or(0, i64::from))
        .collect();
    if w.iter().any(|&width| width < 0) || w.iter().sum::<i64>() > MAX_ENTRY_BYTES {
        return Err(Thrown::Io);
    }
    let index: Vec<i64> = match dict.get(b"Index") {
        Ok(Object::Array(index)) => index
            .iter()
            .map(|value| match value {
                Object::Integer(value) => Ok(*value),
                _ => Err(Thrown::Io),
            })
            .collect::<Read<_>>()?,
        _ => {
            let size = dict
                .get(b"Size")
                .ok()
                .and_then(int_value)
                .map_or(0, i64::from);
            vec![0, size]
        }
    };
    if index.is_empty() || index.len() % 2 == 1 {
        return Err(Thrown::Io);
    }
    let mut numbers = ObjectNumbers::new(&index);
    let widths = [w[0] as usize, w[1] as usize, w[2] as usize];
    let mut line = vec![0u8; widths.iter().sum()];
    let mut position = 0;
    let mut entries = Vec::new();
    // PDFBox reads a cut-off last row over the bytes of the row before (#115).
    let width = line.len();
    while position + width <= data.len() && numbers.has_next() {
        line.copy_from_slice(&data[position..position + width]);
        position += width;
        let number = numbers.next();
        let kind = if widths[0] == 0 {
            1
        } else {
            value(&line[..widths[0]]) as i32
        };
        if kind == 0 {
            continue;
        }
        let field = value(&line[widths[0]..widths[0] + widths[1]]);
        let third = value(&line[widths[0] + widths[1]..]) as i32;
        let fails = || {
            Thrown::Unported(Unported(
                "a cross-reference stream entry PDFBox fails on".to_owned(),
            ))
        };
        let field = i64::try_from(field).map_err(|_| fails())?;
        // PDFBox keeps an object at offset 0, which no object can start at, so that its
        // check of the offsets fails and it searches the whole file for objects, where a
        // table leaves such an entry out (#115).
        if kind == 1 && field == 0 {
            continue;
        }
        let generation = if kind == 1 { i64::from(third) } else { 0 };
        let key = Key::new(number, generation).map_err(|_| fails())?;
        entries.push((
            key,
            match kind {
                1 => Location::File(field),
                _ => Location::Stream(field),
            },
        ));
    }
    Ok(entries)
}

/// `PDFXrefStreamParser.parseValue`: big-endian bytes.
fn value(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |value, &byte| value.wrapping_shl(8) | u64::from(byte))
}

/// `PDFXrefStreamParser.ObjectNumbers`: the object numbers of an `/Index`, range by range.
/// PDFBox's yields the start of an empty range all the same (#115).
struct ObjectNumbers {
    /// Each range's first number and the number after its last.
    ranges: Vec<(i64, i64)>,
    range: usize,
    current: i64,
}

impl ObjectNumbers {
    fn new(index: &[i64]) -> Self {
        let ranges: Vec<(i64, i64)> = index
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[start, size]| (*start, start.saturating_add(*size)))
            .collect();
        let mut numbers = Self {
            current: ranges.first().map_or(0, |range| range.0),
            ranges,
            range: 0,
        };
        numbers.skip_ended();
        numbers
    }

    /// Moves past the ranges whose numbers have all been yielded.
    fn skip_ended(&mut self) {
        while self
            .ranges
            .get(self.range)
            .is_some_and(|&(_, end)| self.current >= end)
        {
            self.range += 1;
            if let Some(&(start, _)) = self.ranges.get(self.range) {
                self.current = start;
            }
        }
    }

    fn has_next(&self) -> bool {
        self.range < self.ranges.len()
    }

    fn next(&mut self) -> i64 {
        let number = self.current;
        self.current += 1;
        self.skip_ended();
        number
    }
}

/// `BruteForceParser`'s searches of the document's bytes.
struct BruteForce<'a> {
    data: &'a [u8],
    /// `getBFCOSObjectOffsets`, once searched for.
    objects: Option<HashMap<Key, Location>>,
}

impl BruteForce<'_> {
    /// `getBFCOSObjectOffsets`.
    fn objects(&mut self) -> Read<&HashMap<Key, Location>> {
        if self.objects.is_none() {
            self.objects = Some(search_objects(self.data)?);
        }
        Ok(self.objects.as_ref().unwrap_or_else(|| unreachable!()))
    }

    /// `bfSearchForXRef`: of the tables and streams the search finds, the one nearest
    /// `offset`, a table on a tie, or -1.
    fn search_xref(&self, offset: i64) -> i64 {
        let tables = search_xref_tables(self.data);
        let streams = search_xref_streams(self.data);
        let table = nearest(&tables, offset);
        let stream = nearest(&streams, offset);
        match (table, stream) {
            (Some(table), Some(stream)) => {
                if (offset - table).abs() > (offset - stream).abs() {
                    stream
                } else {
                    table
                }
            }
            (Some(table), None) => table,
            (None, Some(stream)) => stream,
            (None, None) => -1,
        }
    }
}

/// `searchNearestValue`: the first of the values nearest `offset`.
fn nearest(values: &[i64], offset: i64) -> Option<i64> {
    let mut best: Option<i64> = None;
    for &value in values {
        if best.is_none_or(|best| (offset - best).abs() > (offset - value).abs()) {
            best = Some(value);
        }
    }
    best
}

/// `bfSearchForXRefTables`: where each `xref` after whitespace starts.
fn search_xref_tables(data: &[u8]) -> Vec<i64> {
    let mut source = Source::new(data);
    source.lexer.seek(MINIMUM_SEARCH_OFFSET);
    let mut found = Vec::new();
    while let Some(at) = source.find_string(b"xref") {
        source.lexer.seek((at - 1).max(0) as usize);
        if is_whitespace(source.peek()) {
            found.push(at);
        }
        source.lexer.seek(at as usize + 4);
    }
    found
}

/// `bfSearchForXRefStreams`: where the object holding each `/XRef` starts, as found
/// backwards from it.
fn search_xref_streams(data: &[u8]) -> Vec<i64> {
    let mut source = Source::new(data);
    source.lexer.seek(MINIMUM_SEARCH_OFFSET);
    let mut found = Vec::new();
    while let Some(at) = source.find_string(b"/XRef") {
        if let Some(start) = object_start_before(&mut source, at) {
            found.push(start);
        }
        source.lexer.seek(at as usize + 5);
    }
    found
}

/// The backward search of `bfSearchForXRefStreams` and `bfSearchForObjStreamOffsets`: where
/// the object whose ` obj` stands up to 400 bytes before `marker` starts, with its number and
/// generation, read with the source left after them.
fn object_start_before(source: &mut Source, marker: i64) -> Option<i64> {
    let mut start = None;
    'steps: for step in 1..BACKWARD_STEPS {
        let first = marker - step * 10;
        if first <= 0 {
            continue;
        }
        source.lexer.seek(first as usize);
        for current in first..first + 10 {
            if source.is_string(b" obj") {
                let mut temp = current - 1;
                source.lexer.seek(temp as usize);
                if is_digit(source.peek()) {
                    temp -= 1;
                    source.lexer.seek(temp as usize);
                    if source.peek() == Some(b' ') {
                        let mut length = 0;
                        temp -= 1;
                        source.lexer.seek(temp.max(0) as usize);
                        while temp > MINIMUM_SEARCH_OFFSET as i64 && is_digit(source.peek()) {
                            temp -= 1;
                            source.lexer.seek(temp as usize);
                            length += 1;
                        }
                        if length > 0 {
                            source.read();
                            start = Some(source.position());
                        }
                    }
                }
                break 'steps;
            }
            source.read();
        }
    }
    start
}

/// `bfSearchForObjects`: where each object starts, the last of a key kept, found by its
/// ` obj` after a digit after whitespace, up to the last `%%EOF` that ends the document.
fn search_objects(data: &[u8]) -> Read<HashMap<Key, Location>> {
    let last_eof = last_eof_marker(data)?;
    let mut source = Source::new(data);
    let mut objects = HashMap::new();
    let mut current = MINIMUM_SEARCH_OFFSET as i64;
    let mut last: Option<(i64, i64, i64)> = None;
    let mut end_of_object = false;
    loop {
        source.seek(current)?;
        let next = source.read();
        current += 1;
        if is_whitespace(next) && source.is_string(b"obj") {
            let mut temp = current - 2;
            source.seek(temp)?;
            // PDFBox reads a single digit of generation, so that it never finds an object of
            // generation 10 or more (#115).
            let generation_end = temp + 1;
            while temp > MINIMUM_SEARCH_OFFSET as i64 && is_digit(source.peek()) {
                temp -= 1;
                source.seek(temp)?;
            }
            let generation = parse_long(&data[(temp + 1) as usize..generation_end as usize])
                .filter(|generation| (0..=GENERATION_NUMBER_THRESHOLD).contains(generation));
            if let Some(generation) = generation {
                if is_whitespace(source.peek()) {
                    while temp > MINIMUM_SEARCH_OFFSET as i64 && is_whitespace(source.peek()) {
                        temp -= 1;
                        source.seek(temp)?;
                    }
                    let mut found = false;
                    while temp > MINIMUM_SEARCH_OFFSET as i64 && is_digit(source.peek()) {
                        temp -= 1;
                        source.seek(temp)?;
                        found = true;
                    }
                    if found {
                        source.read();
                        let number = source.read_object_number()?;
                        if let Some((number, generation, offset)) = last {
                            objects.insert(Key::new(number, generation)?, Location::File(offset));
                        }
                        last = Some((number, generation, temp + 1));
                        current += 2;
                        end_of_object = false;
                    }
                }
            }
        } else if next == Some(b'e') && source.is_string(b"ndo") {
            current += 3;
            source.seek(current)?;
            if source.is_eof() {
                end_of_object = true;
            } else if source.is_string(b"bj") {
                current += 2;
                end_of_object = true;
            }
        }
        if !(current < last_eof && !source.is_eof()) {
            break;
        }
    }
    if let Some((number, generation, offset)) = last {
        if last_eof < i64::MAX || end_of_object {
            objects.insert(Key::new(number, generation)?, Location::File(offset));
        }
    }
    Ok(objects)
}

/// `bfSearchForLastEOFMarker`: the last `%%EOF` not followed by more of the document, or
/// `Long.MAX_VALUE`.
fn last_eof_marker(data: &[u8]) -> Read<i64> {
    let mut source = Source::new(data);
    source.lexer.seek(MINIMUM_SEARCH_OFFSET);
    let mut last = None;
    while let Some(marker) = source.find_string(b"%%EOF") {
        source.skip_spaces();
        if !source.is_string(b"xref") {
            let more = source
                .read_object_number()
                .and_then(|_| source.read_generation_number());
            match more {
                Ok(_) => {}
                Err(Thrown::Io) => last = Some(marker),
                Err(unported) => return Err(unported),
            }
        }
    }
    Ok(last.unwrap_or(i64::MAX))
}

/// `bfSearchForObjStreams`: the objects of the object streams the search finds, added to the
/// table where the stream stands after the object's other copy, each stream's data decrypted
/// with `decrypt`, if the search has a security handler, and a length that is an object of its
/// own read where `placed` puts it.
fn brute_force_object_streams(
    data: &[u8],
    decrypt: Option<&Decrypt>,
    placed: &HashMap<Key, Location>,
    objects: &mut HashMap<Key, Location>,
    table: &mut HashMap<Key, Location>,
) -> Result<(), Unported> {
    let streams = search_object_stream_offsets(data)?;
    let mut offsets: Vec<(i64, Key)> = streams
        .into_iter()
        .filter(|(offset, key)| objects.get(key) == Some(&Location::File(*offset)))
        .collect();
    offsets.sort_unstable();
    for (offset, stream_key) in offsets {
        let numbers = match object_stream_numbers(data, offset, decrypt, placed) {
            Ok(numbers) => numbers,
            Err(Thrown::Io) => continue,
            Err(Thrown::Unported(unported)) => return Err(unported),
        };
        for number in numbers {
            let key = Key::new(number, 0)
                .map_err(|_| Unported(format!("object number {number} in an object stream")))?;
            let existing = match objects.get(&key) {
                Some(Location::Stream(stream)) => objects
                    .get(&Key {
                        number: *stream,
                        generation: 0,
                    })
                    .map(|location| location.value()),
                Some(location) => Some(location.value()),
                None => None,
            };
            if existing.is_none_or(|existing| offset > existing) {
                objects.insert(key, Location::Stream(stream_key.number));
                table.insert(key, Location::Stream(stream_key.number));
            }
        }
    }
    Ok(())
}

/// `bfSearchForObjStreamOffsets`: where the object holding each `/ObjStm` starts, with its key.
fn search_object_stream_offsets(data: &[u8]) -> Result<Vec<(i64, Key)>, Unported> {
    let mut source = Source::new(data);
    source.lexer.seek(MINIMUM_SEARCH_OFFSET);
    let mut found: BTreeMap<i64, Key> = BTreeMap::new();
    while let Some(at) = source.find_string(b"/ObjStm") {
        if let Some(start) = object_start_before(&mut source, at) {
            let key = source
                .read_object_number()
                .and_then(|number| Key::new(number, source.read_generation_number()?))
                .map_err(|_| loading_fails())?;
            found.insert(start, key);
        }
        source.lexer.seek(at as usize + 7);
    }
    Ok(found.into_iter().collect())
}

/// Why the port does not read a document whose loading PDFBox fails on while it searches for
/// its object streams.
fn loading_fails() -> Unported {
    Unported(
        "an object stream PDFBox fails to load the document on while it searches for it".to_owned(),
    )
}

/// `PDFObjectStreamParser.readObjectNumbers` of the object stream at `offset`, whose header
/// PDFBox reads outside its handling of a corrupt stream, its data decrypted with `decrypt`,
/// if given, and a length that is an object of its own read where `placed` puts it.
fn object_stream_numbers(
    data: &[u8],
    offset: i64,
    decrypt: Option<&Decrypt>,
    placed: &HashMap<Key, Location>,
) -> Read<Vec<i64>> {
    let mut source = Source::new(data);
    let (number, generation) = source
        .seek(offset)
        .and_then(|()| source.read_object_number())
        .and_then(|number| Ok((number, source.read_generation_number()?)))
        .and_then(|key| source.read_expected(b"obj").map(|()| key))
        .map_err(|_| Thrown::Unported(loading_fails()))?;
    let dict = source.dictionary()?;
    source.start_stream();
    let raw = source.stream_data(placed_length(data, placed, &dict)?)?;
    let decrypted = match decrypt {
        None => Cow::Borrowed(raw.data),
        Some(decrypt) => {
            let id = Key::new(number, generation)?.id().ok_or(Thrown::Io)?;
            Cow::Owned(decrypt(id, &dict, raw.data)?)
        }
    };
    let decoded = decode(&dict, &decrypted)?;

    let count = int_entry(&dict, b"N");
    let first = int_entry(&dict, b"First");
    if count < 0 || first < 0 {
        return Err(Thrown::Io);
    }
    let mut stream = Source::new(&decoded);
    let first_object = first - 1;
    let mut numbers = Vec::new();
    for _ in 0..count {
        if stream.position() >= first_object {
            break;
        }
        let number = stream.read_object_number()?;
        stream.read_long()?;
        numbers.push(number);
    }
    Ok(numbers)
}

/// `COSDictionary.getInt(key)`: a direct number's value, or -1, also for one no `int` holds.
fn int_entry(dict: &Dictionary, key: &[u8]) -> i64 {
    dict.get(key).ok().and_then(int_value).map_or(-1, i64::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_backwards() {
        assert_eq!(last_index_of(b"%%EOF", b"x%%EOF\n%%EOF", 12), Some(7));
        assert_eq!(last_index_of(b"%%EOF", b"x%%EOF\n%%EOF", 7), Some(1));
        assert_eq!(last_index_of(b"startxref", b"startxref", 8), None);
        assert_eq!(last_index_of(b"abab", b"xabababy", 8), Some(3));
        assert_eq!(last_index_of(b"%%EOF", b"%%EOFF", 6), Some(0));
    }

    #[test]
    fn finds_strings_overlapping_partial_matches() {
        let mut source = Source::new(b"ab%%%EOF and %%EOF");
        assert_eq!(source.find_string(b"%%EOF"), Some(3));
        assert_eq!(source.position(), 8);
        assert_eq!(source.find_string(b"%%EOF"), Some(13));
        let mut source = Source::new(b"xref then %%EO");
        assert_eq!(source.find_string(b"%%EOF"), None);
        assert!(source.is_eof());
    }

    #[test]
    fn searches_objects_of_any_generation() {
        let data = b"%PDF-1.4\n1 0 obj\n<< >>\nendobj\n2 12 obj\n<< >>\nendobj\n%%EOF\n";
        let objects = search_objects(data).unwrap();
        let mut keys: Vec<(i64, u16)> = objects
            .keys()
            .map(|key| (key.number, key.generation))
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, [(1, 0), (2, 12)]);
    }

    #[test]
    fn reads_whole_cross_reference_stream_rows() {
        let mut dict = Dictionary::new();
        dict.set("W", vec![1.into(), 2.into(), 1.into()]);
        dict.set("Index", vec![1.into(), 4.into()]);
        let data = [
            [1, 0, 20, 0], // object 1 at offset 20
            [1, 0, 0, 0],  // object 2 at offset 0, which no object starts at
            [2, 0, 0, 3],  // object 3 in object stream 0
        ]
        .concat();
        let entries = xref_stream_entries(&dict, &[&data[..], &[1, 0]].concat()).unwrap();
        assert_eq!(
            entries,
            [
                (Key::new(1, 0).unwrap(), Location::File(20)),
                (Key::new(3, 0).unwrap(), Location::Stream(0)),
            ]
        );
    }

    #[test]
    fn splits_lines_as_string_util_does() {
        assert_eq!(
            split_on_space(b"0000000016 00000 n"),
            [&b"0000000016"[..], b"00000", b"n"]
        );
        assert_eq!(split_on_space(b" 1  2 "), [&b""[..], b"1", b"", b"2"]);
        assert_eq!(split_on_space(b""), [&b""[..]]);
        assert!(split_on_space(b"  ").is_empty());
    }

    #[test]
    fn counts_index_ranges_without_the_empty_ones() {
        for (index, expected) in [
            (&[0, 2, 5, 0][..], &[0, 1][..]),
            (&[3, 0, 5, 2, 9, 0, 10, 1], &[5, 6, 10]),
            (&[4, 0], &[]),
        ] {
            let mut numbers = ObjectNumbers::new(index);
            let mut read = Vec::new();
            while numbers.has_next() {
                read.push(numbers.next());
            }
            assert_eq!(read, expected);
        }
    }
}
