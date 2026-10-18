//! The objects of a document as PDFBox's COS layer reads them: each read where the
//! cross-reference table puts it the first time it is asked for, as `COSParser` parses an
//! object on demand, a stream's data left in the document until it is decoded; an indirect
//! reference resolves to its object, `null` and a missing object to nothing, a stream is a
//! dictionary too, and numbers, names and text strings convert as PDFBox converts them.

use std::borrow::Cow;
use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};

use lopdf::encryption::decrypt_object;
use lopdf::{Dictionary, Document, EncryptionState, Object, ObjectId, Stream};

use super::content::Lexer;
use super::xref::{self, java_trim, Placement, Read, Resolution, Source, Thrown};
use super::Unported;
use crate::formats::thrown::Exception;
use crate::formats::Difference;
use crate::input::Paging;
use crate::text::charset::Charset;

/// How many references in a row are followed before a chain counts as broken.
const MAX_REFERENCE_CHAIN: usize = 32;
/// What reading an object counts as having read of the document: at least a page of it.
pub(super) const OBJECT_READ: usize = 4096;
/// `PDFObjectStreamParser`'s check of a metadata stream an encrypted document leaves in clear.
const XPACKET: &[u8] = b"<?xpacket ";

/// The identity of an object, as PDFBox's identity-based sets tell objects apart: an indirect
/// object by its number, a direct one by where it is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum Identity {
    Indirect(ObjectId),
    Direct(usize),
    /// An object PDFBox creates afresh each time it is asked for, such as the font of a
    /// graphics state parameter dictionary: the count of those created before it.
    Unshared(usize),
}

impl Identity {
    pub(super) fn of_direct(object: &Object) -> Self {
        Self::Direct(std::ptr::from_ref(object) as usize)
    }
}

/// An object the cross-reference table lists: where it is read from, and once asked for, what
/// reading it gave, nothing where PDFBox fails to read it.
struct Slot {
    placement: Placement,
    object: OnceCell<Option<Object>>,
}

/// Where the data of a stream read from the document lies, and how it is decrypted.
#[derive(Debug, Clone, Copy)]
struct StreamData {
    id: ObjectId,
    length: usize,
    encrypted: bool,
}

/// A document's objects, read as they are asked for.
pub(super) struct Pdf<'d> {
    data: &'d [u8],
    paging: Paging<'d>,
    objects: HashMap<ObjectId, Slot>,
    trailer: Dictionary,
    /// What decrypts the document's strings and streams, opened with the empty password.
    crypt: Option<EncryptionState>,
    /// The encryption dictionary, which is not encrypted.
    encrypt_id: Option<ObjectId>,
    /// The data of the streams read, by where it starts in the document.
    streams: RefCell<HashMap<usize, StreamData>>,
    /// The objects being read, which a reference back to one of them reads as nothing, as
    /// PDFBox's `COSObject` marks itself dereferenced before it parses.
    loading: RefCell<HashSet<ObjectId>>,
    /// The first object the extraction needed that the port could not read, and why.
    unread: RefCell<Option<(ObjectId, Unported)>>,
    /// The objects the extraction asked for that the table does not list.
    unlisted_needed: RefCell<BTreeSet<ObjectId>>,
    /// The first detail in which the extraction read the document otherwise than Tika's path.
    difference: RefCell<Option<Difference>>,
    /// Whether the document's bytes spell `Filespec`, as a file specification outside an
    /// object stream must.
    pub(super) spells_filespec: bool,
    /// Whether PDFBox rebuilt the trailer from a search of the objects, after which it drops
    /// the kids of the page tree it cannot read.
    pub(super) trailer_rebuilt: bool,
}

impl<'d> Pdf<'d> {
    /// The document `data`, whose cross-reference PDFBox reads as `resolution`, its pages
    /// released as it is read. The objects PDFBox read while it read the cross-reference stay
    /// as it read them, but dictionaries, which may be streams, are read as other objects are.
    pub(super) fn new(
        data: &'d [u8],
        paging: Paging<'d>,
        resolution: &Resolution,
    ) -> Result<Self, Unported> {
        let mut objects: HashMap<ObjectId, Slot> = resolution
            .placements()?
            .into_iter()
            .map(|(id, placement)| {
                let slot = Slot {
                    placement,
                    object: OnceCell::new(),
                };
                (id, slot)
            })
            .collect();
        for (id, placement, object) in resolution.pooled()? {
            if matches!(object, Object::Dictionary(_)) {
                continue;
            }
            let slot = objects.entry(id).or_insert_with(|| Slot {
                placement,
                object: OnceCell::new(),
            });
            slot.object
                .set(Some(object.clone()))
                .expect("each object is pooled once");
        }
        let spells_filespec = paging.find(data, b"Filespec").is_some();
        let pdf = Self {
            data,
            paging,
            objects,
            trailer: resolution.trailer().clone(),
            crypt: None,
            encrypt_id: None,
            streams: RefCell::new(HashMap::new()),
            loading: RefCell::new(HashSet::new()),
            unread: RefCell::new(None),
            unlisted_needed: RefCell::new(BTreeSet::new()),
            difference: RefCell::new(None),
            spells_filespec,
            trailer_rebuilt: resolution.rebuilt(),
        };
        pdf.release_pages();
        Ok(pdf)
    }

    /// Releases the pages of the document read so far, after a search of all of it.
    pub(super) fn release_pages(&self) {
        self.paging.release();
    }

    /// How the document's pages are released as it is read.
    pub(super) fn paging(&self) -> &Paging<'d> {
        &self.paging
    }

    /// The trailer, as PDFBox merges it.
    pub(super) fn trailer(&self) -> &Dictionary {
        &self.trailer
    }

    /// The document's encryption dictionary, as `lopdf` reads it to open the document: in a
    /// document of its own, whose trailer is this one's with the document ID `document_id`, as
    /// PDFBox reads it, and whose dictionary has `entries` in place of its own.
    pub(super) fn encryption_document(
        &self,
        document_id: &[u8],
        entries: &Dictionary,
    ) -> Option<Document> {
        let encrypt = self.trailer.get(b"Encrypt").ok()?;
        let dict = self.resolve(encrypt).and_then(as_dict)?;
        let id = match encrypt {
            Object::Reference(id) => *id,
            _ => (u32::MAX, 0),
        };
        let mut doc = Document::new();
        doc.trailer = self.trailer.clone();
        doc.trailer.set("Encrypt", Object::Reference(id));
        let document_id = Object::string_literal(document_id.to_vec());
        doc.trailer
            .set("ID", Object::Array(vec![document_id.clone(), document_id]));
        let mut dict = self.inlined(dict, 0);
        for (key, value) in entries.iter() {
            dict.set(key.clone(), value.clone());
        }
        doc.objects.insert(id, Object::Dictionary(dict));
        Some(doc)
    }

    /// A copy of `dict` with the objects its references lead to in their place, as far as
    /// `lopdf`'s reading of an encryption dictionary goes, which follows none.
    fn inlined(&self, dict: &Dictionary, depth: usize) -> Dictionary {
        let mut copy = Dictionary::new();
        for (key, value) in dict.iter() {
            let value = match self.resolve(value) {
                Some(Object::Dictionary(inner)) if depth < 2 => {
                    Object::Dictionary(self.inlined(inner, depth + 1))
                }
                Some(object) => object.clone(),
                None => Object::Null,
            };
            copy.set(key.clone(), value);
        }
        copy
    }

    /// Decrypts the objects read from now on with `state`, the encryption dictionary `id`
    /// excepted.
    pub(super) fn decrypt_with(&mut self, state: EncryptionState, id: Option<ObjectId>) {
        self.crypt = Some(state);
        self.encrypt_id = id;
    }

    /// The first object the extraction needed that the port could not read, and why: what
    /// PDFBox makes of it is not known.
    pub(super) fn unread_object_needed(&self) -> Option<(ObjectId, Unported)> {
        self.unread.borrow().clone()
    }

    /// Notes that the extraction reads the document otherwise than Tika's path, in
    /// `difference`, the first one noted kept.
    pub(super) fn differ(&self, difference: Difference) {
        self.difference.borrow_mut().get_or_insert(difference);
    }

    /// The first detail in which the extraction read the document otherwise than Tika's path.
    pub(super) fn difference(&self) -> Option<Difference> {
        self.difference.borrow().clone()
    }

    /// The objects the extraction asked for that the table does not list, which PDFBox
    /// searches the document for.
    pub(super) fn unlisted_objects_needed(&self) -> Vec<ObjectId> {
        self.unlisted_needed.borrow().iter().copied().collect()
    }

    /// The objects the table lists, by key.
    pub(super) fn listed(&self) -> Vec<ObjectId> {
        let mut ids: Vec<ObjectId> = self.objects.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Whether the table puts the object `id` in an object stream.
    pub(super) fn is_compressed(&self, id: ObjectId) -> bool {
        self.objects
            .get(&id)
            .is_some_and(|slot| matches!(slot.placement, Placement::Stream(_)))
    }

    /// The indirect object `id`, read the first time it is asked for; nothing where it is no
    /// object the table lists, which is noted, or where PDFBox fails to read it.
    pub(super) fn indirect(&self, id: ObjectId) -> Option<&Object> {
        let Some(slot) = self.objects.get(&id) else {
            self.unlisted_needed.borrow_mut().insert(id);
            return None;
        };
        if let Some(object) = slot.object.get() {
            return object.as_ref();
        }
        if !self.loading.borrow_mut().insert(id) {
            return None;
        }
        let read = self.read(id, slot.placement);
        self.loading.borrow_mut().remove(&id);
        let object = match read {
            Ok(object) => object,
            Err(Thrown::Io) => None,
            Err(Thrown::Unported(unported)) => {
                self.unread.borrow_mut().get_or_insert((id, unported));
                None
            }
        };
        slot.object.get_or_init(|| object).as_ref()
    }

    /// `COSParser.parseObjectDynamically` of the object `id`, the table placing it as
    /// `placement`.
    fn read(&self, id: ObjectId, placement: Placement) -> Read<Option<Object>> {
        match placement {
            Placement::File(offset) => self.read_file_object(id, offset),
            Placement::Stream(number) => self.read_compressed_object(id, number),
        }
    }

    /// `COSParser.parseFileObject`: the object `id` at `offset`, a stream's data left where it
    /// stands, decrypted as `SecurityHandler` decrypts it.
    fn read_file_object(&self, id: ObjectId, offset: usize) -> Read<Option<Object>> {
        self.paging.read(OBJECT_READ);
        let mut source = Source::new(self.data);
        source.seek(offset as i64)?;
        let number = source.read_object_number()?;
        let generation = source.read_generation_number()?;
        source.read_expected(b"obj")?;
        if (number, generation) != (i64::from(id.0), i64::from(id.1)) {
            return Err(Thrown::Io);
        }
        source.skip_spaces();
        let parsed = source.lexer.file_object()?.ok_or(Thrown::Io)?;
        let end = source.read_string();
        if end != b"stream" {
            let mut parsed = parsed;
            if let Some(object) = parsed.as_mut() {
                self.decrypt(id, object)?;
            }
            return Ok(parsed);
        }
        source.rewind(end.len());
        let Some(Object::Dictionary(mut dict)) = parsed else {
            return Err(Thrown::Io);
        };
        source.start_stream();
        let length = self.length(&dict)?;
        let found = source.stream_data(length)?;
        if found.measured && length != Some(found.data.len() as i64) {
            dict.set("Length", Object::Integer(found.data.len() as i64));
        }
        let encrypted = self.decrypts_stream(id, &dict, found.data);
        if encrypted {
            let mut strings = Object::Dictionary(dict);
            self.decrypt(id, &mut strings)?;
            let Object::Dictionary(decrypted) = strings else {
                unreachable!("decrypting a dictionary keeps it one")
            };
            dict = decrypted;
        }
        source.skip_spaces();
        let line = source.read_line()?;
        if !line.starts_with(b"endobj")
            && line.starts_with(b"endstream")
            && java_trim(&line[b"endstream".len()..]).is_empty()
        {
            source.read_line()?;
        }
        self.streams.borrow_mut().insert(
            found.start,
            StreamData {
                id,
                length: found.data.len(),
                encrypted,
            },
        );
        Ok(Some(Object::Stream(Stream {
            dict,
            content: Vec::new(),
            allows_compression: false,
            start_position: Some(found.start),
        })))
    }

    /// `COSParser.getLength`: a stream's `/Length`, a reference to a number followed, a
    /// reference to nothing failing the stream, one to `null` reading as none.
    fn length(&self, dict: &Dictionary) -> Read<Option<i64>> {
        match dict.get(b"Length") {
            Err(_) => Ok(None),
            Ok(Object::Integer(length)) => Ok(Some(*length)),
            Ok(Object::Real(length)) => Ok(Some(*length as i64)),
            Ok(Object::Reference(id)) => match self.indirect(*id) {
                None => Err(Thrown::Io),
                Some(Object::Null) => Ok(None),
                Some(Object::Integer(length)) => Ok(Some(*length)),
                Some(Object::Real(length)) => Ok(Some(*length as i64)),
                Some(_) => Err(Thrown::Io),
            },
            Ok(_) => Err(Thrown::Io),
        }
    }

    /// Whether `SecurityHandler.decryptStream` decrypts the stream `id` of `dict`, whose data
    /// is `data`: not under the identity filter, not the cross-reference stream, and not a
    /// metadata stream the document leaves in clear.
    fn decrypts_stream(&self, id: ObjectId, dict: &Dictionary, data: &[u8]) -> bool {
        let Some(state) = &self.crypt else {
            return false;
        };
        if Some(id) == self.encrypt_id || state.default_stream_filter() == b"Identity" {
            return false;
        }
        match dict.get(b"Type") {
            Ok(Object::Name(name)) if name == b"XRef" => false,
            Ok(Object::Name(name)) if name == b"Metadata" => {
                state.encrypt_metadata() && !data.starts_with(XPACKET)
            }
            _ => true,
        }
    }

    /// `SecurityHandler.decrypt`: the strings of `object`, read as the object `id`.
    fn decrypt(&self, id: ObjectId, object: &mut Object) -> Read<()> {
        let Some(state) = &self.crypt else {
            return Ok(());
        };
        if Some(id) == self.encrypt_id {
            return Ok(());
        }
        decrypt_object(state, id, object).map_err(|error| {
            Thrown::Unported(Unported(format!(
                "object {} {}, which does not decrypt: {error}",
                id.0, id.1
            )))
        })
    }

    /// `COSParser.parseObjectStreamObject`: the object `id` of the object stream `number`, read
    /// with the others it holds, which are kept for when they are asked for, each where the
    /// table puts it in that stream.
    fn read_compressed_object(&self, id: ObjectId, number: u32) -> Read<Option<Object>> {
        let Some(Object::Stream(stream)) = self.indirect((number, 0)) else {
            return Ok(None);
        };
        let decoded = self.decode(stream)?;
        let count = self.int(&stream.dict, b"N", -1);
        let first = self.int(&stream.dict, b"First", -1);
        let objects = match object_stream_objects(&decoded, count, first) {
            Ok(objects) => objects,
            Err(Thrown::Io) => return Ok(None),
            Err(unported) => return Err(unported),
        };
        let mut wanted = None;
        for (found, object) in objects {
            if found == id {
                wanted.get_or_insert(object);
                continue;
            }
            let Some(slot) = self.objects.get(&found) else {
                continue;
            };
            let kept = slot.placement == Placement::Stream(number)
                && slot.object.get().is_none()
                && !self.loading.borrow().contains(&found);
            if kept {
                let _ = slot.object.set(object);
            }
        }
        Ok(wanted.flatten())
    }

    /// The object `object` stands for: itself, or what its references lead to; nothing for
    /// `null`, a reference to a missing object or a chain of references too long to follow.
    pub(super) fn resolve<'a>(&'a self, object: &'a Object) -> Option<&'a Object> {
        self.resolve_identified(object).map(|(_, object)| object)
    }

    /// [`Pdf::resolve`], with the identity of the object it resolves to.
    pub(super) fn resolve_identified<'a>(
        &'a self,
        object: &'a Object,
    ) -> Option<(Identity, &'a Object)> {
        let mut identity = Identity::of_direct(object);
        let mut current = object;
        for _ in 0..MAX_REFERENCE_CHAIN {
            match current {
                Object::Reference(id) => {
                    identity = Identity::Indirect(*id);
                    current = self.indirect(*id)?;
                }
                Object::Null => return None,
                _ => return Some((identity, current)),
            }
        }
        None
    }

    /// `COSDictionary.getDictionaryObject`: the resolved value of `key`.
    pub(super) fn get<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
        dict.get(key).ok().and_then(|object| self.resolve(object))
    }

    /// The resolved value of `key` and its identity.
    pub(super) fn get_identified<'a>(
        &'a self,
        dict: &'a Dictionary,
        key: &[u8],
    ) -> Option<(Identity, &'a Object)> {
        dict.get(key)
            .ok()
            .and_then(|object| self.resolve_identified(object))
    }

    /// `COSDictionary.getCOSDictionary`: the value of `key` when it is a dictionary or a stream.
    pub(super) fn dict<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Dictionary> {
        self.get(dict, key).and_then(as_dict)
    }

    /// `COSDictionary.getCOSStream`.
    pub(super) fn stream<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Stream> {
        match self.get(dict, key) {
            Some(Object::Stream(stream)) => Some(stream),
            _ => None,
        }
    }

    /// `COSDictionary.getCOSArray`.
    pub(super) fn array<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a [Object]> {
        match self.get(dict, key) {
            Some(Object::Array(array)) => Some(array),
            _ => None,
        }
    }

    /// `COSDictionary.getCOSName`: the bytes of a name value.
    pub(super) fn name<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
        match self.get(dict, key) {
            Some(Object::Name(name)) => Some(name),
            _ => None,
        }
    }

    /// `COSDictionary.getInt(key, default)`.
    pub(super) fn int(&self, dict: &Dictionary, key: &[u8], default: i32) -> i32 {
        self.get(dict, key).and_then(int_value).unwrap_or(default)
    }

    /// `COSDictionary.getFloat(key, default)`.
    pub(super) fn float(&self, dict: &Dictionary, key: &[u8], default: f32) -> f32 {
        self.get(dict, key).and_then(float_value).unwrap_or(default)
    }

    /// `COSDictionary.getString`: a text string value as PDFBox decodes it.
    pub(super) fn text(&self, dict: &Dictionary, key: &[u8]) -> Option<String> {
        match self.get(dict, key) {
            Some(Object::String(bytes, _)) => Some(text_string(bytes)),
            _ => None,
        }
    }

    /// `COSDictionary.getNameAsString`: a name, or a text string.
    pub(super) fn name_as_string(&self, dict: &Dictionary, key: &[u8]) -> Option<String> {
        match self.get(dict, key) {
            Some(Object::Name(name)) => Some(name_string(name)),
            Some(Object::String(bytes, _)) => Some(text_string(bytes)),
            _ => None,
        }
    }

    /// The raw data of `stream`, decrypted: borrowed from the document where it is not
    /// encrypted.
    pub(super) fn raw<'s>(&'s self, stream: &'s Stream) -> Result<Cow<'s, [u8]>, Unported> {
        let found = stream
            .start_position
            .and_then(|start| Some((start, *self.streams.borrow().get(&start)?)));
        let Some((start, found)) = found else {
            return Ok(Cow::Borrowed(&stream.content));
        };
        self.paging.read(found.length);
        let data = &self.data[start..start + found.length];
        if !found.encrypted {
            return Ok(Cow::Borrowed(data));
        }
        let state = self
            .crypt
            .as_ref()
            .expect("an encrypted stream is read from a document that decrypts");
        let decrypted = decrypt_data(state, found.id, &stream.dict, data).map_err(|error| {
            Unported(format!(
                "the data of stream {} {}, which does not decrypt: {error}",
                found.id.0, found.id.1
            ))
        })?;
        Ok(Cow::Owned(decrypted))
    }

    /// `SecurityHandler.decryptStream` of the data `data` of the stream `id` of `dict`, as
    /// `bfSearchForObjStreams` decrypts an object stream it finds: data that does not decrypt
    /// is a corrupt stream.
    pub(super) fn decrypted(&self, id: ObjectId, dict: &Dictionary, data: &[u8]) -> Read<Vec<u8>> {
        match &self.crypt {
            Some(state) if self.decrypts_stream(id, dict, data) => {
                decrypt_data(state, id, dict, data).map_err(|_| Thrown::Io)
            }
            _ => Ok(data.to_vec()),
        }
    }

    /// Reads each object of `placements` from where it says from now on, as `getObjectOffset`
    /// finds the objects `bfSearchForObjStreams` adds; an object already read keeps what it read.
    pub(super) fn place(&mut self, placements: HashMap<ObjectId, Placement>) {
        for (id, placement) in placements {
            match self.objects.get_mut(&id) {
                Some(slot) => slot.placement = placement,
                None => {
                    self.objects.insert(
                        id,
                        Slot {
                            placement,
                            object: OnceCell::new(),
                        },
                    );
                }
            }
        }
    }

    /// The decoded data of `stream`, its filters applied in order as PDFBox applies them, each
    /// with its own decode parameters.
    pub(super) fn decode(&self, stream: &Stream) -> Result<Vec<u8>, Unported> {
        self.decode_until(stream, &[])
    }

    /// `COSStream.createInputStream(stopFilters)`: the data of `stream` with its filters
    /// applied up to the first that `stop` names.
    pub(super) fn decode_until(
        &self,
        stream: &Stream,
        stop: &[&[u8]],
    ) -> Result<Vec<u8>, Unported> {
        let filters = self.get(&stream.dict, b"Filter");
        let parameters = self.get(&stream.dict, b"DecodeParms");
        self.decode_data(self.raw(stream)?, &stream.dict, filters, parameters, stop)
    }

    /// `content` of the stream of `dictionary` with the filters `filters` names applied in
    /// order up to the first that `stop` names, each with its own parameters of `parameters`.
    pub(super) fn decode_data(
        &self,
        content: Cow<'_, [u8]>,
        dictionary: &Dictionary,
        filters: Option<&Object>,
        parameters: Option<&Object>,
        stop: &[&[u8]],
    ) -> Result<Vec<u8>, Unported> {
        let filters = self.filter_names(filters);
        let mut data = content.into_owned();
        for (index, filter) in filters.iter().enumerate() {
            if stop.contains(filter) {
                break;
            }
            let params = match parameters {
                Some(Object::Array(each)) => each
                    .get(index)
                    .and_then(|params| self.resolve(params))
                    .and_then(as_dict),
                Some(other) => (index == 0).then(|| as_dict(other)).flatten(),
                None => None,
            };
            if *filter == b"Crypt" {
                if let Some(name) = self.unsupported_crypt_filter(dictionary) {
                    return Err(Unported(format!(
                        "a stream through the crypt filter {}, which PDFBox does not support",
                        name_string(name)
                    )));
                }
                continue;
            }
            data = decode_filter(filter, params, data)?;
        }
        Ok(data)
    }

    /// `COSStream.getFilters` of the `Filter` entry `value`: its name, or the names in its
    /// array.
    pub(super) fn filter_names<'a>(&'a self, value: Option<&'a Object>) -> Vec<&'a [u8]> {
        match value {
            Some(Object::Name(name)) => vec![name.as_slice()],
            Some(Object::Array(items)) => items
                .iter()
                .filter_map(|item| match self.resolve(item) {
                    Some(Object::Name(name)) => Some(name.as_slice()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The crypt filter `CryptFilter.decode` fails on for the stream of `dictionary`, if any:
    /// it passes data through by the identity crypt filter, the only one it has, and takes the
    /// crypt filter's name from the stream's own dictionary, where PDFBox hands it that
    /// dictionary rather than its decode parameters.
    pub(super) fn unsupported_crypt_filter<'a>(
        &'a self,
        dictionary: &'a Dictionary,
    ) -> Option<&'a [u8]> {
        self.name(dictionary, b"Name")
            .filter(|name| *name != b"Identity")
    }

    /// Searches the document for the first of the objects `ids` the table does not list that
    /// PDFBox finds there, releasing its pages after.
    pub(super) fn first_found(&self, ids: &[ObjectId]) -> Result<Option<ObjectId>, Unported> {
        let found = xref::first_found(self.data, ids);
        if !ids.is_empty() {
            self.release_pages();
        }
        found
    }
}

/// The data `data` of the stream `id` of `dict`, decrypted with `state`.
fn decrypt_data(
    state: &EncryptionState,
    id: ObjectId,
    dict: &Dictionary,
    data: &[u8],
) -> Result<Vec<u8>, lopdf::Error> {
    let mut encrypted = Object::Stream(Stream {
        dict: dict.clone(),
        content: data.to_vec(),
        allows_compression: false,
        start_position: None,
    });
    decrypt_object(state, id, &mut encrypted)?;
    let Object::Stream(decrypted) = encrypted else {
        unreachable!("decrypting a stream keeps it one")
    };
    Ok(decrypted.content)
}

/// `PDFObjectStreamParser.parseAllObjects`: the objects of an object stream of `count` objects,
/// the first at `first`, whose data is `decoded`, each with its key. Where PDFBox fails on the
/// stream, it reads none of them.
fn object_stream_objects(
    decoded: &[u8],
    count: i32,
    first: i32,
) -> Read<Vec<(ObjectId, Option<Object>)>> {
    if count < 0 || first < 0 {
        return Err(Thrown::Io);
    }
    let mut source = Source::new(decoded);
    let first_object = i64::from(first) - 1;
    let mut offsets: std::collections::BTreeMap<i32, i64> = std::collections::BTreeMap::new();
    for _ in 0..count {
        if source.position() >= first_object {
            break;
        }
        let number = source.read_object_number()?;
        let offset = source.read_long()?;
        offsets.insert(offset as i32, number);
    }
    let distinct: HashSet<i64> = offsets.values().copied().collect();
    if distinct.len() < offsets.len() {
        return Err(Thrown::Unported(Unported(
            "an object stream holding one object number twice, which PDFBox tells apart by the \
             index the cross-reference stream gives"
                .to_owned(),
        )));
    }
    let mut position = source.position();
    if first > 0 && position < i64::from(first) {
        position = i64::from(first);
    }
    let mut lexer = Lexer::file(decoded, position as usize);
    let mut objects = Vec::with_capacity(offsets.len());
    for (offset, number) in offsets {
        let number = u32::try_from(number).map_err(|_| {
            Thrown::Unported(Unported(format!(
                "object number {number} in an object stream, which lopdf cannot hold"
            )))
        })?;
        let at = i64::from(first) + i64::from(offset);
        if at > 0 && (lexer.position() as i64) < at {
            lexer.seek(at as usize);
        }
        let object = lexer.file_object()?.ok_or(Thrown::Io)?;
        objects.push(((number, 0), object));
    }
    Ok(objects)
}

/// One filter applied to `data`, by `lopdf`'s decoders.
fn decode_filter(
    filter: &[u8],
    params: Option<&Dictionary>,
    data: Vec<u8>,
) -> Result<Vec<u8>, Unported> {
    let name = match filter {
        b"FlateDecode" | b"Fl" => "FlateDecode",
        b"LZWDecode" | b"LZW" => "LZWDecode",
        b"ASCII85Decode" | b"A85" => "ASCII85Decode",
        b"ASCIIHexDecode" | b"AHx" => "ASCIIHexDecode",
        b"RunLengthDecode" | b"RL" => "RunLengthDecode",
        other => {
            return Err(Unported(format!(
                "a stream is encoded with the filter {} the port does not decode",
                name_string(other)
            )))
        }
    };
    let mut dict = Dictionary::new();
    dict.set("Filter", Object::Name(name.as_bytes().to_vec()));
    if let Some(params) = params {
        dict.set("DecodeParms", Object::Dictionary(params.clone()));
    }
    Stream::new(dict, data)
        .decompressed_content()
        .map_err(|e| Unported(format!("a stream's {name} data does not decode: {e}")))
}

/// The filters `FilterFactory` has, by their names and abbreviations.
const FILTERS: [&[u8]; 17] = [
    b"FlateDecode",
    b"Fl",
    b"DCTDecode",
    b"DCT",
    b"CCITTFaxDecode",
    b"CCF",
    b"LZWDecode",
    b"LZW",
    b"ASCIIHexDecode",
    b"AHx",
    b"ASCII85Decode",
    b"A85",
    b"RunLengthDecode",
    b"RL",
    b"Crypt",
    b"JPXDecode",
    b"JBIG2Decode",
];

/// The `IOException` `FilterFactory.getFilter` throws on the first of `filters` it has no filter
/// of, which `COSStream.getFilterList` meets before it decodes anything, if any.
pub(super) fn invalid_filter(filters: &[&[u8]]) -> Option<Exception> {
    let invalid = filters.iter().find(|filter| !FILTERS.contains(filter))?;
    Some(Exception::new(
        "java.io.IOException",
        format!("Invalid filter: COSName{{{}}}", name_string(invalid)),
    ))
}

/// The class of PDFBox's COS layer that reads `object`, a reference left unresolved read as
/// the `COSObject` that stands for it.
fn cos_class(object: &Object) -> &'static str {
    match object {
        Object::Null => "COSNull",
        Object::Boolean(_) => "COSBoolean",
        Object::Integer(_) => "COSInteger",
        Object::Real(_) => "COSFloat",
        Object::Name(_) => "COSName",
        Object::String(..) => "COSString",
        Object::Array(_) => "COSArray",
        Object::Dictionary(_) => "COSDictionary",
        Object::Stream(_) => "COSStream",
        Object::Reference(_) => "COSObject",
    }
}

/// The `ClassCastException` the JVM throws where PDFBox casts `object` to the COS class `to`.
pub(super) fn class_cast(object: &Object, to: &str) -> Exception {
    let from = format!("org.apache.pdfbox.cos.{}", cos_class(object));
    let to = format!("org.apache.pdfbox.cos.{to}");
    Exception::new(
        "java.lang.ClassCastException",
        format!(
            "class {from} cannot be cast to class {to} ({from} and {to} are in unnamed module of \
             loader 'app')"
        ),
    )
}

/// The dictionary of a dictionary or a stream, which PDFBox's `COSStream` extends.
pub(super) fn as_dict(object: &Object) -> Option<&Dictionary> {
    match object {
        Object::Dictionary(dict) => Some(dict),
        Object::Stream(stream) => Some(&stream.dict),
        _ => None,
    }
}

/// `COSNumber.intValue`: an integer cast as Java casts a `long` to `int`, a real truncated.
pub(super) fn int_value(object: &Object) -> Option<i32> {
    match object {
        Object::Integer(value) => Some(*value as i32),
        Object::Real(value) => Some(*value as i32),
        _ => None,
    }
}

/// `COSNumber.floatValue`.
pub(super) fn float_value(object: &Object) -> Option<f32> {
    match object {
        Object::Integer(value) => Some(*value as f32),
        Object::Real(value) => Some(*value),
        _ => None,
    }
}

/// `COSString.getString`: UTF-16 after a byte order mark, otherwise PDFDocEncoding.
pub(super) fn text_string(bytes: &[u8]) -> String {
    match bytes {
        [0xFE, 0xFF, rest @ ..] => decode_java("UTF-16BE", rest),
        [0xFF, 0xFE, rest @ ..] => decode_java("UTF-16LE", rest),
        _ => bytes.iter().map(|&byte| pdf_doc_char(byte)).collect(),
    }
}

/// `new String(bytes, charset)` for a charset the JDK always has.
pub(super) fn decode_java(charset: &str, bytes: &[u8]) -> String {
    Charset::named(charset)
        .decode(bytes)
        .unwrap_or_else(|| panic!("the port decodes {charset}"))
}

/// `COSName.getName`: the bytes of a name as UTF-8 when they are valid UTF-8, else as
/// Windows-1252.
pub(super) fn name_string(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => decode_java("windows-1252", bytes),
    }
}

/// The character PDFDocEncoding gives a byte, as PDFBox's table holds it: Latin-1 with the
/// deviations of ISO 32000-1, and U+0000 for the bytes it leaves undefined.
pub(super) fn pdf_doc_char(byte: u8) -> char {
    match byte {
        0x18 => '\u{02D8}',
        0x19 => '\u{02C7}',
        0x1A => '\u{02C6}',
        0x1B => '\u{02D9}',
        0x1C => '\u{02DD}',
        0x1D => '\u{02DB}',
        0x1E => '\u{02DA}',
        0x1F => '\u{02DC}',
        0x7F => '\u{FFFD}',
        0x80 => '\u{2022}',
        0x81 => '\u{2020}',
        0x82 => '\u{2021}',
        0x83 => '\u{2026}',
        0x84 => '\u{2014}',
        0x85 => '\u{2013}',
        0x86 => '\u{0192}',
        0x87 => '\u{2044}',
        0x88 => '\u{2039}',
        0x89 => '\u{203A}',
        0x8A => '\u{2212}',
        0x8B => '\u{2030}',
        0x8C => '\u{201E}',
        0x8D => '\u{201C}',
        0x8E => '\u{201D}',
        0x8F => '\u{2018}',
        0x90 => '\u{2019}',
        0x91 => '\u{201A}',
        0x92 => '\u{2122}',
        0x93 => '\u{FB01}',
        0x94 => '\u{FB02}',
        0x95 => '\u{0141}',
        0x96 => '\u{0152}',
        0x97 => '\u{0160}',
        0x98 => '\u{0178}',
        0x99 => '\u{017D}',
        0x9A => '\u{0131}',
        0x9B => '\u{0142}',
        0x9C => '\u{0153}',
        0x9D => '\u{0161}',
        0x9E => '\u{017E}',
        0x9F => '\u{FFFD}',
        0xA0 => '\u{20AC}',
        0xAD => '\u{0}',
        _ => char::from(byte),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_text_strings_as_pdfbox_does() {
        assert_eq!(text_string(b"\xFE\xFF\x00A\x00\xE6"), "Aæ");
        assert_eq!(text_string(b"\xFF\xFEA\x00"), "A");
        assert_eq!(text_string(b"a\x80b\xAD"), "a\u{2022}b\u{0}");
        assert_eq!(text_string(b"\xFE\xFF\x00"), "\u{FFFD}");
    }

    #[test]
    fn names_are_utf8_or_windows_1252() {
        assert_eq!(name_string(b"F1"), "F1");
        assert_eq!(name_string("æ".as_bytes()), "æ");
        assert_eq!(name_string(b"\xE6"), "æ");
    }

    #[test]
    fn numbers_convert_as_java_casts_them() {
        assert_eq!(int_value(&Object::Real(2.9)), Some(2));
        assert_eq!(int_value(&Object::Real(-2.9)), Some(-2));
        assert_eq!(int_value(&Object::Integer(1 << 32)), Some(0));
        assert_eq!(float_value(&Object::Integer(3)), Some(3.0));
        assert_eq!(int_value(&Object::Null), None);
    }
}
