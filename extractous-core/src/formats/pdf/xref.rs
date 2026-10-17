//! Where PDFBox's `COSParser` finds a document's objects: the cross-reference chain from the
//! last `startxref`, merged as `XrefTrailerResolver` merges it, with the repairs its lenient mode
//! makes, which `BruteForceParser` searches the bytes for: a `startxref` or `/Prev` that misses
//! its section, a table whose offsets miss their objects, and the object streams of a document
//! whose table it replaces. `lopdf` reads the objects where PDFBox finds them: from the
//! document as it is, when it finds the same, else from the document with PDFBox's table
//! appended as a cross-reference stream.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use lopdf::xref::XrefEntry;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use super::content::{is_digit, is_end_of_name, is_whitespace, Lexer};
use super::Unported;

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

/// Why reading the cross-reference stopped: an `IOException` PDFBox catches, or something the
/// port does not do.
#[derive(Debug)]
enum Thrown {
    Io,
    Unported(Unported),
}

impl From<Unported> for Thrown {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

type Read<T> = Result<T, Thrown>;

/// What PDFBox reads of a document's cross-reference: the objects it finds where, and the
/// trailer.
#[derive(Debug)]
pub(super) struct Resolution {
    table: HashMap<Key, Location>,
    trailer: Dictionary,
}

/// Reads the cross-reference of `data` as PDFBox's lenient `COSParser.retrieveTrailer` does,
/// or says why the port leaves the document to Tika.
pub(super) fn resolve(data: &[u8]) -> Result<Resolution, Unported> {
    let mut parser = Parser::new(data);
    let trailer = match parser.retrieve() {
        Ok(trailer) => trailer,
        Err(Thrown::Io) => None,
        Err(Thrown::Unported(unported)) => return Err(unported),
    };
    let Some(trailer) = trailer.filter(|trailer| trailer.has(b"Root")) else {
        return rebuild(data).map_err(|thrown| match thrown {
            Thrown::Io => Unported(
                "a document whose objects PDFBox fails to search for, as it rebuilds its trailer"
                    .to_owned(),
            ),
            Thrown::Unported(unported) => unported,
        });
    };
    let mut table = parser.resolved.table;
    if let Some(mut objects) = parser.brute.objects.take() {
        let decrypts = trailer.has(b"Encrypt");
        brute_force_object_streams(data, decrypts, &mut objects, &mut table)?;
    }
    Ok(Resolution { table, trailer })
}

/// `BruteForceParser.rebuildTrailer`, for a document without a trailer PDFBox can use: the
/// objects a search finds, and a trailer of the catalog and the information dictionary found
/// in a `trailer` of the document, or among the objects. The object streams are searched with
/// no security handler, so an encrypted one reads as corrupt.
fn rebuild(data: &[u8]) -> Read<Resolution> {
    let mut objects = search_objects(data)?;
    let mut table = objects.clone();
    let mut trailer = Dictionary::new();
    let mut streams_searched = false;
    if !search_trailer(data, &objects, &mut trailer)?
        && !search_trailer_items(data, &objects, &mut trailer)
    {
        brute_force_object_streams(data, false, &mut objects, &mut table)?;
        streams_searched = true;
        search_trailer_items(data, &objects, &mut trailer);
    }
    if !streams_searched {
        brute_force_object_streams(data, false, &mut objects, &mut table)?;
    }
    Ok(Resolution { table, trailer })
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
    match objects.get(&key)? {
        Location::File(offset) => {
            let mut source = Source::new(data);
            let found = file_object_dictionary(&mut source, *offset).ok()?;
            (found.0 == key).then_some(found.1)
        }
        Location::Stream(stream) => {
            let stream_key = Key {
                number: *stream,
                generation: 0,
            };
            let Location::File(offset) = *objects.get(&stream_key)? else {
                return None;
            };
            let mut source = Source::new(data);
            let (found, dict) = file_object_dictionary(&mut source, offset).ok()?;
            if found != stream_key {
                return None;
            }
            let raw = source.stream_data(&dict).ok()?;
            let decoded = decode(&dict, raw).ok()?;
            compressed_dictionary(&dict, &decoded, key.number)
        }
    }
}

/// The key and dictionary of the object at `offset`, as `parseFileObject` starts reading it.
fn file_object_dictionary(source: &mut Source, offset: i64) -> Read<(Key, Dictionary)> {
    source.seek(offset)?;
    let number = source.read_object_number()?;
    let generation = source.read_generation_number()?;
    source.read_expected(b"obj")?;
    source.skip_spaces();
    let dict = source.dictionary()?;
    Ok((Key::new(number, generation)?, dict))
}

/// The dictionary of the object `number` in an object stream, which `parseObjectStreamObject`
/// reads at its offset after `/First`.
fn compressed_dictionary(stream: &Dictionary, decoded: &[u8], number: i64) -> Option<Dictionary> {
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
    Lexer::file(decoded, position).file_dictionary().ok()?
}

impl Resolution {
    /// The document as `lopdf` should read it to find the objects where PDFBox finds them:
    /// `data` itself when `doc`, read from it, already does, else `data` with PDFBox's table
    /// appended.
    pub(super) fn needs_rewrite(&self, doc: &Document, data: &[u8]) -> Result<bool, Unported> {
        let header = header_offset(data);
        let ours = self.lopdf_entries(header)?;
        let theirs = lopdf_table(doc);
        let differs = ours
            .iter()
            .any(|(number, entry)| theirs.get(number) != Some(entry));
        let read_more = theirs
            .keys()
            .filter(|number| !ours.contains_key(number))
            .any(|&number| {
                doc.objects
                    .range((number, 0)..=(number, u16::MAX))
                    .next()
                    .is_some()
            });
        Ok(differs
            || read_more
            || self.trailer.get(b"ID").ok() != doc.trailer.get(b"ID").ok()
            || self.trailer.get(b"Encrypt").ok() != lopdf_encrypt(doc).as_ref())
    }

    /// The encryption dictionary the trailer names, as PDFBox reads it from `data`, and the
    /// first string of the trailer's `ID`, empty where it has none; `None` for a document the
    /// trailer names no encryption of.
    pub(super) fn encryption(
        &self,
        data: &[u8],
    ) -> Result<Option<(Dictionary, Vec<u8>)>, Unported> {
        let dictionary = match self.trailer.get(b"Encrypt") {
            Err(_) => return Ok(None),
            Ok(Object::Dictionary(dictionary)) => dictionary.clone(),
            Ok(Object::Reference(id)) => dereference(data, &self.table, Key::from_id(*id))
                .ok_or_else(|| {
                    Unported(format!(
                        "an encryption dictionary, object {} {}, the port cannot read",
                        id.0, id.1
                    ))
                })?,
            Ok(_) => {
                return Err(Unported(
                    "an encryption entry that is no dictionary".to_owned(),
                ))
            }
        };
        let id = match self.trailer.get(b"ID") {
            Ok(Object::Array(items)) => match items.first() {
                None => Vec::new(),
                Some(Object::String(bytes, _)) => bytes.clone(),
                Some(_) => {
                    return Err(Unported(
                        "a document ID that is no string, which PDFBox fails on".to_owned(),
                    ))
                }
            },
            _ => Vec::new(),
        };
        Ok(Some((dictionary, id)))
    }

    /// The objects the table lists, as `lopdf` names them.
    pub(super) fn listed(&self) -> BTreeSet<ObjectId> {
        self.table
            .iter()
            .filter_map(|(key, location)| match location {
                Location::File(_) => key.id(),
                Location::Stream(_) => key.id().map(|(number, _)| (number, 0)),
            })
            .collect()
    }

    /// `data` with PDFBox's table and trailer appended as a cross-reference stream, which the
    /// last `startxref` points to.
    pub(super) fn rewritten(&self, data: &[u8]) -> Result<Vec<u8>, Unported> {
        let header = header_offset(data);
        let entries = self.lopdf_entries(header)?;
        let size = entries.keys().next_back().map_or(1, |&number| number + 1);
        let mut rows = Vec::with_capacity(size as usize * 11);
        for number in 0..size {
            let (kind, field, generation): (u8, u64, u16) = match entries.get(&number) {
                None => (0, 0, 0),
                Some(Entry::Normal { offset, generation }) => (1, u64::from(*offset), *generation),
                Some(Entry::Compressed { container }) => (2, u64::from(*container), 0),
            };
            rows.push(kind);
            rows.extend_from_slice(&field.to_be_bytes());
            rows.extend_from_slice(&generation.to_be_bytes());
        }
        let mut out = Vec::with_capacity(data.len() + rows.len() + 256);
        out.extend_from_slice(data);
        out.push(b'\n');
        let start = out.len() - header;
        out.extend_from_slice(
            format!(
                "{size} 0 obj\n<< /Type /XRef /Size {size} /W [1 8 2] /Length {}",
                rows.len()
            )
            .as_bytes(),
        );
        for key in [&b"Root"[..], b"Info", b"Encrypt", b"ID"] {
            if let Ok(value) = self.trailer.get(key) {
                out.push(b' ');
                write_name(key, &mut out);
                out.push(b' ');
                write_object(value, &mut out)?;
            }
        }
        out.extend_from_slice(b" >>\nstream\n");
        out.extend_from_slice(&rows);
        out.extend_from_slice(
            format!("\nendstream\nendobj\nstartxref\n{start}\n%%EOF\n").as_bytes(),
        );
        Ok(out)
    }

    /// Gives `doc` PDFBox's trailer, without the encryption dictionary `lopdf` has decrypted
    /// it with.
    pub(super) fn set_trailer(&self, doc: &mut Document) {
        let decrypted = doc.trailer.get(b"Encrypt").is_err();
        doc.trailer = self.trailer.clone();
        if decrypted {
            doc.trailer.remove(b"Encrypt");
        }
    }

    /// The table as `lopdf` holds one, its offsets from the header `lopdf` reads from.
    fn lopdf_entries(&self, header: usize) -> Result<BTreeMap<u32, Entry>, Unported> {
        let mut entries = BTreeMap::new();
        for (key, location) in &self.table {
            let number = u32::try_from(key.number).map_err(|_| too_large(key.number))?;
            let entry = match *location {
                Location::File(offset) => {
                    let offset = usize::try_from(offset)
                        .ok()
                        .and_then(|offset| offset.checked_sub(header))
                        .and_then(|offset| u32::try_from(offset).ok())
                        .ok_or_else(|| {
                            Unported(format!(
                                "object {number} at offset {offset}, which lopdf cannot read \
                                 from there"
                            ))
                        })?;
                    Entry::Normal {
                        offset,
                        generation: key.generation,
                    }
                }
                Location::Stream(container) => Entry::Compressed {
                    container: u32::try_from(container).map_err(|_| too_large(container))?,
                },
            };
            if entries.insert(number, entry).is_some() {
                return Err(Unported(format!(
                    "two generations of object {number} in the cross-reference table, which \
                     lopdf cannot hold"
                )));
            }
        }
        Ok(entries)
    }
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

fn too_large(number: i64) -> Unported {
    Unported(format!("object number {number}, which lopdf cannot hold"))
}

/// An entry of the table as `lopdf` holds it, without the index in an object stream, which
/// `lopdf` does not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Normal { offset: u32, generation: u16 },
    Compressed { container: u32 },
}

fn lopdf_table(doc: &Document) -> BTreeMap<u32, Entry> {
    doc.reference_table
        .entries
        .iter()
        .filter_map(|(&number, entry)| match *entry {
            XrefEntry::Normal { offset, generation } => {
                Some((number, Entry::Normal { offset, generation }))
            }
            XrefEntry::Compressed { container, .. } => {
                Some((number, Entry::Compressed { container }))
            }
            XrefEntry::Free | XrefEntry::UnusableFree => None,
        })
        .collect()
}

/// The encryption dictionary `lopdf` read the document with.
fn lopdf_encrypt(doc: &Document) -> Option<Object> {
    match doc.trailer.get(b"Encrypt") {
        Ok(encrypt) => Some(encrypt.clone()),
        Err(_) => doc
            .encryption_state
            .as_ref()
            .and_then(lopdf::EncryptionState::encrypt_object_id)
            .map(Object::Reference),
    }
}

/// Where `lopdf` takes the document to start: at its first `%PDF-`.
fn header_offset(data: &[u8]) -> usize {
    data.windows(5)
        .position(|window| window == b"%PDF-")
        .unwrap_or(0)
}

/// `BaseParser`'s reading of the document's bytes.
struct Source<'a> {
    data: &'a [u8],
    lexer: Lexer<'a>,
}

impl<'a> Source<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            lexer: Lexer::file(data, 0),
        }
    }

    fn position(&self) -> i64 {
        self.lexer.position() as i64
    }

    /// `RandomAccessRead.seek`, which refuses a negative position.
    fn seek(&mut self, position: i64) -> Read<()> {
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

    fn rewind(&mut self, count: usize) {
        self.lexer.rewind(count);
    }

    fn is_eof(&self) -> bool {
        self.lexer.position() >= self.data.len()
    }

    fn skip_spaces(&mut self) {
        self.lexer.skip_spaces();
    }

    /// `BaseParser.readString`.
    fn read_string(&mut self) -> Vec<u8> {
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
    fn read_line(&mut self) -> Read<Vec<u8>> {
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
    fn read_long(&mut self) -> Read<i64> {
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

    fn read_object_number(&mut self) -> Read<i64> {
        let number = self.read_long()?;
        if !(0..OBJECT_NUMBER_THRESHOLD).contains(&number) {
            return Err(Thrown::Io);
        }
        Ok(number)
    }

    fn read_generation_number(&mut self) -> Read<i64> {
        let generation = self.read_int()?;
        if !(0..=GENERATION_NUMBER_THRESHOLD).contains(&generation) {
            return Err(Thrown::Io);
        }
        Ok(generation)
    }

    /// `BaseParser.readExpectedString` with the spaces around it.
    fn read_expected(&mut self, string: &[u8]) -> Read<()> {
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

    /// `COSParser.parseCOSStream` of a stream whose dictionary has been read: its raw data,
    /// after which the source stands after `endstream`.
    fn stream_data(&mut self, dict: &Dictionary) -> Read<&'a [u8]> {
        self.read_string();
        self.skip_white_spaces();
        let length = match dict.get(b"Length") {
            Err(_) => None,
            Ok(Object::Integer(length)) => Some(*length),
            Ok(Object::Real(length)) => Some(*length as i64),
            Ok(Object::Reference(_)) => {
                return Err(Thrown::Unported(Unported(
                    "a stream PDFBox reads while it finds the objects, whose length is an \
                     object of its own"
                        .to_owned(),
                )))
            }
            Ok(_) => return Err(Thrown::Io),
        };
        let start = self.lexer.position();
        let data = match length.filter(|&length| self.valid_stream_length(length)) {
            Some(length) => {
                let end = start + length as usize;
                self.lexer.seek(end);
                &self.data[start..end]
            }
            None => self.read_until_end_stream(),
        };
        let end_stream = self.read_string();
        if end_stream == b"endobj" {
            self.rewind(b"endobj".len());
        } else if end_stream.len() > 9 && end_stream.starts_with(b"endstream") {
            self.rewind(end_stream.len() - 9);
        } else if end_stream != b"endstream" {
            return Err(Thrown::Io);
        }
        Ok(data)
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

    /// `COSParser.readUntilEndStream`: the data up to the first `endstream` or `endobj`,
    /// without the line break that ends it. PDFBox's `EndstreamFilterStream` keeps the line
    /// break when the stream's first 10 bytes look like text (#115).
    fn read_until_end_stream(&mut self) -> &'a [u8] {
        let start = self.lexer.position();
        let rest = &self.data[start..];
        let end = (0..rest.len())
            .find(|&at| rest[at..].starts_with(b"endstream") || rest[at..].starts_with(b"endobj"))
            .unwrap_or(rest.len());
        self.lexer.seek(start + end);
        let content = &rest[..end];
        content
            .strip_suffix(b"\r\n")
            .or_else(|| content.strip_suffix(b"\n"))
            .or_else(|| content.strip_suffix(b"\r"))
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

/// `Long.parseLong` of digits, which fails on none and on too many.
fn parse_long(digits: &[u8]) -> Option<i64> {
    if digits.is_empty() {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// `COSDictionary.getLong(key, -1)` of a trailer, which the port reads only when direct.
fn trailer_long(dict: &Dictionary, key: &[u8]) -> Read<i64> {
    match dict.get(key) {
        Err(_) => Ok(-1),
        Ok(Object::Integer(value)) => Ok(*value),
        Ok(Object::Real(value)) => Ok(*value as i64),
        Ok(Object::Reference(_)) => Err(Thrown::Unported(Unported(format!(
            "a /{} that is an object of its own, which the port does not resolve while it finds \
             the objects",
            String::from_utf8_lossy(key)
        )))),
        Ok(_) => Ok(-1),
    }
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
    fn resolve(&self, start: i64) -> Read<Resolved> {
        let mut chain = Vec::new();
        match self.sections.get(&start) {
            None => chain.extend(self.sections.keys().copied()),
            Some(mut section) => {
                chain.push(start);
                while let Some(trailer) = &section.trailer {
                    let prev = trailer_long(trailer, b"Prev")?;
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

/// `COSParser` as far as the cross-reference goes.
struct Parser<'a> {
    source: Source<'a>,
    resolver: Resolver,
    resolved: Resolved,
    brute: BruteForce<'a>,
}

impl<'a> Parser<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            source: Source::new(data),
            resolver: Resolver::default(),
            resolved: Resolved::default(),
            brute: BruteForce {
                data,
                objects: None,
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
                prev = trailer_long(trailer, b"Prev")?;
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
        self.resolved = self.resolver.resolve(start)?;
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
        let mut offset =
            match trailer.get(b"XRefStm") {
                Ok(Object::Integer(value)) => i64::from(*value as i32),
                Ok(Object::Real(value)) => i64::from(*value as i32),
                Ok(Object::Reference(_)) => return Err(Thrown::Unported(Unported(
                    "an /XRefStm that is an object of its own, which the port does not resolve \
                     while it finds the objects"
                        .to_owned(),
                ))),
                Ok(_) | Err(_) => -1,
            };
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
        let fixed = self.brute.search_xref(offset);
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
        let data = self.source.stream_data(&dict)?;
        if standalone {
            self.resolver.next_section(offset);
            self.resolver.set_trailer(dict.clone());
        }
        let decoded = decode(&dict, data)?;
        for (key, location) in xref_stream_entries(&dict, &decoded)? {
            self.resolver.set(key, location);
        }
        trailer_long(&dict, b"Prev")
    }

    /// `checkXrefOffsets`: the table replaced by the objects a search finds, where an offset
    /// misses its object.
    fn check_xref_offsets(&mut self) -> Read<()> {
        if self.valid_xref_offsets()? {
            return Ok(());
        }
        let objects = self.brute.objects()?;
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
fn java_trim(bytes: &[u8]) -> &[u8] {
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
        .map(|value| match value {
            Object::Integer(value) => i64::from(*value as i32),
            Object::Real(value) => i64::from(*value as i32),
            _ => 0,
        })
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
            let size = match dict.get(b"Size") {
                Ok(Object::Integer(size)) => i64::from(*size as i32),
                Ok(Object::Real(size)) => i64::from(*size as i32),
                _ => 0,
            };
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
/// table where the stream stands after the object's other copy.
fn brute_force_object_streams(
    data: &[u8],
    decrypts: bool,
    objects: &mut HashMap<Key, Location>,
    table: &mut HashMap<Key, Location>,
) -> Result<(), Unported> {
    let streams = search_object_stream_offsets(data)?;
    let mut offsets: Vec<(i64, Key)> = streams
        .into_iter()
        .filter(|(offset, key)| objects.get(key) == Some(&Location::File(*offset)))
        .collect();
    offsets.sort_unstable();
    if !offsets.is_empty() && decrypts {
        return Err(Unported(
            "the object streams of an encrypted document whose table PDFBox replaces".to_owned(),
        ));
    }
    for (offset, stream_key) in offsets {
        let numbers = match object_stream_numbers(data, offset) {
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

/// Why the port leaves a document to Tika whose loading PDFBox fails on while it searches for
/// its object streams.
fn loading_fails() -> Unported {
    Unported(
        "an object stream PDFBox fails to load the document on while it searches for it".to_owned(),
    )
}

/// `PDFObjectStreamParser.readObjectNumbers` of the object stream at `offset`, whose header
/// PDFBox reads outside its handling of a corrupt stream.
fn object_stream_numbers(data: &[u8], offset: i64) -> Read<Vec<i64>> {
    let mut source = Source::new(data);
    source
        .seek(offset)
        .and_then(|()| source.read_object_number())
        .and_then(|_| source.read_generation_number())
        .and_then(|_| source.read_expected(b"obj"))
        .map_err(|_| Thrown::Unported(loading_fails()))?;
    let dict = source.dictionary()?;
    let raw = source.stream_data(&dict)?;
    let decoded = decode(&dict, raw)?;
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

/// `COSDictionary.getInt(key)`: a direct number's value, or -1.
fn int_entry(dict: &Dictionary, key: &[u8]) -> i64 {
    match dict.get(key) {
        Ok(Object::Integer(value)) => i64::from(*value as i32),
        Ok(Object::Real(value)) => i64::from(*value as i32),
        _ => -1,
    }
}

/// A name as PDF writes it, each byte that is not a regular character as `#xx`.
fn write_name(name: &[u8], out: &mut Vec<u8>) {
    out.push(b'/');
    for &byte in name {
        let regular = byte.is_ascii_graphic()
            && !matches!(
                byte,
                b'#' | b'/' | b'%' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}'
            );
        if regular {
            out.push(byte);
        } else {
            out.extend_from_slice(format!("#{byte:02X}").as_bytes());
        }
    }
}

/// An object as PDF writes it, its strings in hexadecimal so that each byte survives.
fn write_object(object: &Object, out: &mut Vec<u8>) -> Result<(), Unported> {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Object::Real(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Object::Name(name) => write_name(name, out),
        Object::String(bytes, _) => {
            out.push(b'<');
            for byte in bytes {
                out.extend_from_slice(format!("{byte:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Object::Array(items) => {
            out.push(b'[');
            for item in items {
                write_object(item, out)?;
                out.push(b' ');
            }
            out.push(b']');
        }
        Object::Dictionary(dict) => {
            out.extend_from_slice(b"<<");
            for (key, value) in dict.iter() {
                write_name(key, out);
                out.push(b' ');
                write_object(value, out)?;
                out.push(b' ');
            }
            out.extend_from_slice(b">>");
        }
        Object::Reference((number, generation)) => {
            out.extend_from_slice(format!("{number} {generation} R").as_bytes());
        }
        Object::Stream(_) => {
            return Err(Unported(
                "a stream in the trailer, which a cross-reference stream cannot hold".to_owned(),
            ))
        }
    }
    Ok(())
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
