//! Tika's `RTFEmbObjHandler` and `RTFObjDataParser`: the pictures (`\pict`) and objects
//! (`\objdata`) of an RTF document, collected as the document is read, and each finished one
//! made into the document it embeds, with the metadata Tika gives it, for the embedded document
//! extractor to parse. Their bytes are spooled as they are read: in memory, and past what is
//! read whole, to a file of their own.

use std::io::{self, BufWriter, Write};

use super::charsets;
use super::input::Input;
use crate::errors::Error;
use crate::formats::embedded::{
    record_stream_exception, PARSER_OVERRIDE, RELATIONSHIP_ID, RESOURCE_NAME,
};
use crate::formats::metadata::office::{ORIGINAL_RESOURCE_NAME, RESOURCE_NAME_EXTENSION_INFERRED};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole10_native::{self, Ole10Native, Unread};
use crate::formats::ole2::document_type;
use crate::formats::Failure;
use crate::input::{self, Spool};
use crate::mime::{self, ole_directory_type, DetectionError, MediaType, Origin};
use crate::ole2::{is_ole, EntryId, EntryKind, Ole2, Unreadable};

/// `RTFParser`'s default limit on the bytes of one picture or object, 20 MB.
const MEMORY_LIMIT: u64 = 20 * 1024 * 1024;
const PACKAGE: &str = "package";
const PAINTBRUSH: &str = "pbrush";
const EMBEDDED_OBJECT: u32 = 2;
const EMBEDDED_PACKAGE: u16 = 3;
const CONTENT_TYPE: &str = "Content-Type";
const CONTENT_LENGTH: &str = "Content-Length";
const PICTURE_PROPERTY: &str = "rtf:pict:";
const DESCRIPTION: &str = "rtf:pict:wzDescription";
const THUMBNAIL: &str = "rtf:thumbnail";
const APP_VERSION: &str = "rtf:embedded-app-version";
const CLASS: &str = "rtf:embedded-class";
const TOPIC: &str = "rtf:embedded-topic";
const ITEM: &str = "rtf:embedded-item";
const RAW_BITMAP: &str = "image/x-rtf-raw-bitmap";
const OLE10_NATIVE: &str = "\u{1}Ole10Native";
const NULL_IN_NAME: &str = "java.lang.IllegalArgumentException: Null character present in \
     file/path name. There are no known legitimate use cases for such data, but several injection \
     attacks may use it";
const EMPTY_FILE: &str =
    "org.apache.poi.EmptyFileException: The supplied file was empty (zero bytes long)";

/// An exception Tika's handling of a picture or object throws, by the first line of its stack
/// trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Thrown {
    /// An `IOException`.
    Io(String),
    /// A `TikaException`.
    Tika(String),
    /// A `RuntimeException`.
    Runtime(String),
}

impl Thrown {
    pub(super) fn line(&self) -> &str {
        match self {
            Thrown::Io(line) | Thrown::Tika(line) | Thrown::Runtime(line) => line,
        }
    }

    fn memory_limit(requested: u64, limit: u64) -> Self {
        Thrown::Tika(format!(
            "org.apache.tika.exception.TikaMemoryLimitException: Tried to allocate {requested} \
             bytes, but {limit} is the maximum allowed. Please open an issue \
             https://issues.apache.org/jira/projects/TIKA if you believe this file is not corrupt."
        ))
    }

    fn truncated() -> Self {
        Thrown::Io("java.io.IOException: the object data ends early".to_owned())
    }
}

/// A finished picture or object, made into the document Tika hands its embedded document
/// extractor.
#[derive(Debug)]
pub(super) struct Handed {
    pub(super) data: input::Input<'static>,
    pub(super) metadata: TikaMetadata,
}

/// The bytes of a picture or object being collected.
type Collected = BufWriter<Spool>;

fn collected() -> Collected {
    BufWriter::new(Spool::new(None))
}

/// How a picture or object failed to spool.
fn spool_failed(error: io::Error) -> Failure {
    Failure::Extraction(Error::IoError(format!(
        "Could not spool a picture or object of the RTF document: {error}"
    )))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Nothing,
    Picture,
    Object,
}

/// The picture or object being collected, and what the document's earlier ones leave.
pub(super) struct Embedded {
    state: State,
    bytes: Collected,
    high: Option<u8>,
    metadata: TikaMetadata,
    in_object: bool,
    raw_bitmap: bool,
    /// The text of the picture property name or value being read, in UTF-16 units.
    text: Vec<u16>,
    name: String,
    value: String,
    thumbnails: u32,
    unnamed: u32,
}

impl Embedded {
    pub(super) fn new() -> Self {
        Self {
            state: State::Nothing,
            bytes: collected(),
            high: None,
            metadata: TikaMetadata::new(),
            in_object: false,
            raw_bitmap: false,
            text: Vec::new(),
            name: String::new(),
            value: String::new(),
            thumbnails: 0,
            unnamed: 0,
        }
    }

    pub(super) fn start_picture(&mut self) {
        self.state = State::Picture;
        self.metadata = TikaMetadata::new();
    }

    pub(super) fn start_object(&mut self) {
        self.state = State::Object;
        self.metadata = TikaMetadata::new();
    }

    pub(super) fn set_in_object(&mut self, in_object: bool) {
        self.in_object = in_object;
    }

    pub(super) fn set_raw_bitmap(&mut self) {
        self.raw_bitmap = true;
    }

    /// Starts the text of a picture property's name (`\sn`) or value (`\sv`).
    pub(super) fn start_property_text(&mut self) {
        self.text.clear();
    }

    pub(super) fn end_property_name(&mut self) {
        self.name = String::from_utf16_lossy(&self.text);
    }

    pub(super) fn end_property_value(&mut self) {
        self.value = String::from_utf16_lossy(&self.text);
    }

    /// Ends a picture property (`\sp`): its value is added under its name.
    pub(super) fn end_property(&mut self) {
        self.metadata
            .add_prefixed(PICTURE_PROPERTY, &self.name, self.value.clone());
    }

    pub(super) fn write_property_unit(&mut self, unit: u16) {
        self.text.push(unit);
    }

    /// Takes a byte of hex data; anything other than a hex digit, such as a line break, is
    /// skipped.
    pub(super) fn write_hex_char(&mut self, byte: u8) -> Result<(), Failure> {
        let Some(value) = hex_value(byte) else {
            return Ok(());
        };
        match self.high.take() {
            None => self.high = Some(16 * value),
            Some(high) => self
                .bytes
                .write_all(&[high + value])
                .map_err(spool_failed)?,
        }
        Ok(())
    }

    /// Takes `length` bytes of `\binN` data from `input`. Data over the memory limit, or cut
    /// off, which is then read to its end, is thrown on.
    pub(super) fn write_bytes(
        &mut self,
        input: &mut Input,
        length: u32,
    ) -> Result<Result<(), Thrown>, Failure> {
        if u64::from(length) > MEMORY_LIMIT {
            return Ok(Err(Thrown::memory_limit(u64::from(length), MEMORY_LIMIT)));
        }
        let data = match input.take(length as usize) {
            Ok(data) => data,
            Err(actual) => {
                return Ok(Err(Thrown::Io(format!(
                    "java.io.EOFException: Length to read: {length} actual: {actual}"
                ))))
            }
        };
        self.bytes.write_all(&data).map_err(spool_failed)?;
        Ok(Ok(()))
    }

    /// `handleCompletedObject`: the document a finished picture or object embeds, if Tika hands
    /// one on. An object whose data `RTFObjDataParser` fails to read with an `IOException` hands
    /// on nothing.
    pub(super) fn complete(&mut self) -> Result<Result<Option<Handed>, Thrown>, Failure> {
        let bytes = std::mem::replace(&mut self.bytes, collected())
            .into_inner()
            .map_err(|error| spool_failed(error.into_error()))?
            .finish()
            .map_err(spool_failed)?;
        Ok(self.finish(bytes))
    }

    fn finish(&mut self, bytes: input::Input<'static>) -> Result<Option<Handed>, Thrown> {
        let state = self.state;
        let mut metadata = std::mem::take(&mut self.metadata);
        let in_object = self.in_object;
        let raw_bitmap = self.raw_bitmap;
        self.reset();
        let data = match state {
            State::Nothing => return Ok(None),
            State::Object => match object(&bytes, &mut metadata, &mut self.unnamed) {
                Ok(Some(data)) => input::Input::Read(std::sync::Arc::new(data)),
                Ok(None) | Err(Thrown::Io(_)) => return Ok(None),
                Err(thrown) => return Err(thrown),
            },
            State::Picture => {
                if let Some(path) = metadata
                    .get(DESCRIPTION)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
                {
                    metadata.set(RELATIONSHIP_ID, path.clone());
                    metadata.set(RESOURCE_NAME, file_name(&path)?);
                    metadata.set(ORIGINAL_RESOURCE_NAME, path);
                }
                metadata.set(THUMBNAIL, in_object.to_string());
                if raw_bitmap {
                    metadata.set(PARSER_OVERRIDE, RAW_BITMAP);
                }
                bytes
            }
        };
        metadata.set(CONTENT_LENGTH, data.len().to_string());
        if metadata.get(RESOURCE_NAME).is_none() {
            let extension = extension(&data, &mut metadata)?;
            if in_object && state == State::Picture {
                metadata.set(
                    RESOURCE_NAME,
                    format!("thumbnail-{}{extension}", self.thumbnails),
                );
                self.thumbnails += 1;
                metadata.set(THUMBNAIL, "true");
            } else {
                metadata.set(
                    RESOURCE_NAME,
                    format!("embedded-{}{extension}", self.unnamed),
                );
                self.unnamed += 1;
            }
            metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
        }
        Ok(Some(Handed { data, metadata }))
    }

    /// Forgets the picture or object being collected, but not how many were named.
    pub(super) fn reset(&mut self) {
        self.state = State::Nothing;
        self.bytes = collected();
        self.metadata = TikaMetadata::new();
        self.high = None;
        self.name.clear();
        self.value.clear();
        self.text.clear();
        self.raw_bitmap = false;
    }
}

pub(super) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `EmbeddedDocumentUtil.getExtension`: the extension of the document's declared type, or of
/// the type detected in it, which becomes its declared type; `.bin` where the detection throws
/// an `IOException`.
fn extension(data: &[u8], metadata: &mut TikaMetadata) -> Result<&'static str, Thrown> {
    if let Some(declared) = metadata.get(CONTENT_TYPE).and_then(MediaType::parse) {
        return Ok(mime::registered_extension(&declared));
    }
    match mime::detect(data, Origin::Bytes) {
        Ok(detection) => {
            metadata.set(CONTENT_TYPE, detection.media_type.to_string());
            Ok(mime::registered_extension(&detection.media_type))
        }
        Err(DetectionError::Unreadable(_)) => Ok(".bin"),
        Err(error) => Err(Thrown::Runtime(error.to_string())),
    }
}

/// Commons IO's `FilenameUtils.getName`: what follows the last slash or backslash, failing on
/// a name with a null character.
fn file_name(path: &str) -> Result<String, Thrown> {
    if path.contains('\0') {
        return Err(Thrown::Runtime(NULL_IN_NAME.to_owned()));
    }
    Ok(path[path.rfind(['/', '\\']).map_or(0, |at| at + 1)..].to_owned())
}

/// `RTFObjDataParser.parse`: the document an object embeds, with what it learns added to
/// `metadata`; none for a link. An object other than a package or a Paintbrush picture that
/// is an OLE2 compound document is unwrapped as `handleEmbeddedPOIFS` unwraps it.
fn object(
    bytes: &[u8],
    metadata: &mut TikaMetadata,
    unnamed: &mut u32,
) -> Result<Option<Vec<u8>>, Thrown> {
    let mut reader = Reader { bytes, at: 0 };
    metadata.add(APP_VERSION, reader.u32()?.to_string());
    if reader.u32()? != EMBEDDED_OBJECT {
        return Ok(None);
    }
    let class = reader.windows_1252()?;
    let topic = reader.windows_1252()?;
    let item = reader.windows_1252()?;
    for (name, value) in [(CLASS, &class), (TOPIC, &topic), (ITEM, &item)] {
        if !value.is_empty() {
            metadata.add(name, value.clone());
        }
    }
    let data = reader.length_prefixed()?;
    let class = crate::java::to_lower_case(&class);
    if class == PACKAGE {
        return package(data, metadata);
    }
    if class == PAINTBRUSH {
        return Ok(Some(data.to_vec()));
    }
    if data.is_empty() {
        return Err(Thrown::Runtime(EMPTY_FILE.to_owned()));
    }
    if !is_ole(data) {
        return Ok(Some(data.to_vec()));
    }
    match compound(data, metadata, unnamed) {
        Ok(unwrapped) => Ok(unwrapped),
        Err(reason) => {
            record_stream_exception(metadata, &reason);
            Ok(Some(data.to_vec()))
        }
    }
}

/// `handleEmbeddedPOIFS`: the OOXML package, the file of an OLE 1.0 object or the `CONTENTS`
/// stream an object's compound document holds, or the compound document itself named by its
/// type; none for an OLE 1.0 record POI does not understand. Why POI fails to read it, which
/// Tika records before it hands on the compound document as it is.
fn compound(
    data: &[u8],
    metadata: &mut TikaMetadata,
    unnamed: &mut u32,
) -> Result<Option<Vec<u8>>, String> {
    let file = Ole2::open_stream(data).map_err(unreadable)?;
    let root = file.root();
    if let Some(package) = file.child(root, "Package") {
        let bytes = read_document(&file, package)?;
        if bytes.len() as u64 >= MEMORY_LIMIT {
            return Err(Thrown::memory_limit(MEMORY_LIMIT + 1, MEMORY_LIMIT)
                .line()
                .to_owned());
        }
        return Ok(Some(bytes));
    }
    let (kind, _) = document_type(&ole_directory_type(&file, root));
    if kind == ole10_native_type() {
        let entry = file
            .child_ignoring_case(root, OLE10_NATIVE)
            .ok_or_else(|| "java.io.FileNotFoundException: no Ole10Native entry".to_owned())?;
        let record = read_document(&file, entry)?;
        if record.len() > ole10_native::MAX_RECORD_LENGTH as usize {
            return Err(format!(
                "org.apache.poi.util.RecordFormatException: an OLE 1.0 record of {} bytes is \
                 longer than POI reads",
                record.len()
            ));
        }
        return match Ole10Native::parse(&record) {
            Ok(native) => Ok(Some(native.data.into_owned())),
            Err(Unread::Invalid) => Ok(None),
            Err(Unread::Failed(message)) => Err(message),
        };
    }
    if kind == comp_obj_type() {
        let entry = file
            .child(root, "CONTENTS")
            .or_else(|| file.child(root, "Contents"))
            .ok_or_else(|| "java.io.FileNotFoundException: no CONTENTS entry".to_owned())?;
        return read_document(&file, entry).map(Some);
    }
    let name = kind.to_string();
    metadata.set(
        RESOURCE_NAME,
        format!("embedded-{unnamed}{}", mime::registered_extension(&kind)),
    );
    *unnamed += 1;
    metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
    metadata.set(CONTENT_TYPE, name);
    Ok(Some(data.to_vec()))
}

fn ole10_native_type() -> MediaType {
    MediaType::parse("application/x-tika-msoffice-embedded; format=ole10_native")
        .expect("a media type")
}

fn comp_obj_type() -> MediaType {
    MediaType::parse("application/x-tika-msoffice-embedded; format=comp_obj").expect("a media type")
}

fn unreadable(Unreadable(reason): Unreadable) -> String {
    format!("java.io.IOException: {reason}")
}

/// A document entry read whole, failing as POI's cast and read do.
fn read_document(file: &Ole2<'_>, entry: EntryId) -> Result<Vec<u8>, String> {
    if file.kind(entry) != EntryKind::Document {
        return Err(format!(
            "java.lang.ClassCastException: the entry {:?} is a directory",
            file.name(entry)
        ));
    }
    file.read(entry).map_err(unreadable)
}

/// `RTFObjDataParser.handlePackage`: the file a package embeds, its names in `metadata`; none
/// for a link.
fn package(bytes: &[u8], metadata: &mut TikaMetadata) -> Result<Option<Vec<u8>>, Thrown> {
    let mut reader = Reader { bytes, at: 0 };
    reader.u16()?;
    let display_name = reader.ansi_string()?;
    reader.ansi_string()?;
    reader.u16()?;
    if reader.u16()? != EMBEDDED_PACKAGE {
        return Ok(None);
    }
    reader.u32()?;
    let ansi_path = reader.ansi_string()?;
    let length = reader.u32()?;
    let data = reader.take(length)?.to_vec();
    let unicode_path = reader.unicode_path();
    let (name, path) = if unicode_path.is_empty() {
        (display_name, ansi_path)
    } else {
        (unicode_path.clone(), unicode_path)
    };
    metadata.set(ORIGINAL_RESOURCE_NAME, name.clone());
    metadata.set(RESOURCE_NAME, file_name(&name)?);
    metadata.set(RELATIONSHIP_ID, path);
    Ok(Some(data))
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], Thrown> {
        let bytes = self
            .bytes
            .get(self.at..self.at + N)
            .ok_or_else(Thrown::truncated)?;
        self.at += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    fn u16(&mut self) -> Result<u16, Thrown> {
        self.fixed().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, Thrown> {
        self.fixed().map(u32::from_le_bytes)
    }

    /// `initByteArray` and `readFully`: a length over the limit fails before the bytes are read.
    fn take(&mut self, length: u32) -> Result<&'a [u8], Thrown> {
        if u64::from(length) > MEMORY_LIMIT {
            return Err(Thrown::memory_limit(u64::from(length), MEMORY_LIMIT));
        }
        let end = self.at + length as usize;
        let bytes = self.bytes.get(self.at..end).ok_or_else(Thrown::truncated)?;
        self.at = end;
        Ok(bytes)
    }

    fn length_prefixed(&mut self) -> Result<&'a [u8], Thrown> {
        let length = self.u32()?;
        self.take(length)
    }

    /// `readLengthPrefixedAnsiString`, trimmed: the bytes decoded as windows-1252.
    fn windows_1252(&mut self) -> Result<String, Thrown> {
        let bytes = self.length_prefixed()?;
        let text = charsets::windows_1252()
            .decode(bytes)
            .expect("the JDK decodes windows-1252");
        Ok(crate::java::trim(&text).to_owned())
    }

    /// `readAnsiString`: bytes up to a zero byte, which must come before the end, each a
    /// character.
    fn ansi_string(&mut self) -> Result<String, Thrown> {
        let rest = &self.bytes[self.at..];
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(Thrown::truncated)?;
        self.at += end + 1;
        Ok(crate::java::latin1(&rest[..end]))
    }

    /// The optional Unicode path after a package's file: empty where it is missing or cut off.
    fn unicode_path(&mut self) -> String {
        let Ok(length) = self.u32() else {
            return String::new();
        };
        let Some(bytes) = self.bytes[self.at..].get(..2 * length as usize) else {
            return String::new();
        };
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect();
        String::from_utf16_lossy(&units)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ole2::tests::compound as compound_document;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn object_bytes(class: &[u8], data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0501u32.to_le_bytes());
        bytes.extend_from_slice(&EMBEDDED_OBJECT.to_le_bytes());
        for part in [class, b"", b"", data] {
            bytes.extend_from_slice(&(part.len() as u32).to_le_bytes());
            bytes.extend_from_slice(part);
        }
        bytes
    }

    fn package(kind: u16, unicode_path: Option<&str>) -> Vec<u8> {
        let mut bytes = vec![2, 0];
        bytes.extend_from_slice(b"name.txt\0C:\\icon\0");
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&9u32.to_le_bytes());
        bytes.extend_from_slice(b"C:\\a.txt\0");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(b"hi");
        if let Some(path) = unicode_path {
            let units: Vec<u16> = path.encode_utf16().collect();
            bytes.extend_from_slice(&(units.len() as u32).to_le_bytes());
            for unit in units {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
        }
        bytes
    }

    fn read_object(bytes: &[u8]) -> (Result<Option<Vec<u8>>, Thrown>, TikaMetadata) {
        let mut metadata = TikaMetadata::new();
        let mut unnamed = 0;
        (object(bytes, &mut metadata, &mut unnamed), metadata)
    }

    fn finished(embedded: &mut Embedded, bytes: &[u8]) -> Handed {
        for digit in bytes
            .iter()
            .flat_map(|byte| format!("{byte:02x}").into_bytes())
        {
            embedded.write_hex_char(digit).unwrap();
        }
        embedded.complete().unwrap().unwrap().unwrap()
    }

    fn property(embedded: &mut Embedded, name: &str, value: &str) {
        embedded.start_property_text();
        name.encode_utf16()
            .for_each(|unit| embedded.write_property_unit(unit));
        embedded.end_property_name();
        embedded.start_property_text();
        value
            .encode_utf16()
            .for_each(|unit| embedded.write_property_unit(unit));
        embedded.end_property_value();
        embedded.end_property();
    }

    #[test]
    fn unwraps_packages_by_their_names() {
        let (data, metadata) = read_object(&object_bytes(b" Package\0", &package(3, None)));
        assert_eq!(data, Ok(Some(b"hi".to_vec())));
        assert_eq!(metadata.get(RESOURCE_NAME), Some("name.txt"));
        assert_eq!(metadata.get(RELATIONSHIP_ID), Some("C:\\a.txt"));
        assert_eq!(metadata.get(CLASS), Some("Package"));
        assert_eq!(metadata.get(APP_VERSION), Some("1281"));
        let unicode = package(3, Some("D:\\dir\\b.txt"));
        let (_, metadata) = read_object(&object_bytes(b"Package", &unicode));
        assert_eq!(metadata.get(RESOURCE_NAME), Some("b.txt"));
        assert_eq!(metadata.get(ORIGINAL_RESOURCE_NAME), Some("D:\\dir\\b.txt"));
        let cut = &unicode[..unicode.len() - 1];
        let (_, metadata) = read_object(&object_bytes(b"Package", cut));
        assert_eq!(metadata.get(RESOURCE_NAME), Some("name.txt"));
        let (thrown, _) = read_object(&object_bytes(b"Package", &package(3, Some("a\0b"))));
        assert_eq!(thrown, Err(Thrown::Runtime(NULL_IN_NAME.to_owned())));
    }

    #[test]
    fn keeps_links_and_hands_on_other_objects_whole() {
        let mut link = object_bytes(b"Word.Document.8", b"");
        link[4] = 1;
        assert_eq!(read_object(&link).0, Ok(None));
        let package_link = object_bytes(b"Package", &package(1, None));
        assert_eq!(read_object(&package_link).0, Ok(None));
        let paintbrush = object_bytes(b"PBrush", b"");
        assert_eq!(read_object(&paintbrush).0, Ok(Some(Vec::new())));
        let word = object_bytes(b"Word.Document.8", b"data");
        assert_eq!(read_object(&word).0, Ok(Some(b"data".to_vec())));
        let empty = object_bytes(b"Word.Document.8", b"");
        assert_eq!(
            read_object(&empty).0,
            Err(Thrown::Runtime(EMPTY_FILE.to_owned()))
        );
    }

    #[test]
    fn unwraps_compound_documents_as_handle_embedded_poifs_does() {
        let contents = compound_document(&["\u{1}CompObj", "CONTENTS", "\u{3}ObjInfo"], PNG);
        let picture = object_bytes(b"Paint.Picture", &contents);
        assert_eq!(read_object(&picture).0, Ok(Some(PNG.to_vec())));
        let package = compound_document(&["Package"], b"PK\x03\x04");
        let sheet = object_bytes(b"Excel.Sheet.12", &package);
        assert_eq!(read_object(&sheet).0, Ok(Some(b"PK\x03\x04".to_vec())));
        let word = compound_document(&["WordDocument"], b"x");
        let (data, metadata) = read_object(&object_bytes(b"Word.Document.8", &word));
        assert_eq!(data, Ok(Some(word.clone())));
        assert_eq!(metadata.get(RESOURCE_NAME), Some("embedded-0.doc"));
        assert_eq!(metadata.get(CONTENT_TYPE), Some("application/msword"));
        let mut broken = word;
        broken.truncate(600);
        let (data, metadata) = read_object(&object_bytes(b"Word.Document.8", &broken));
        assert_eq!(data, Ok(Some(broken)));
        assert!(metadata
            .get("tk:exception:embedded-stream-exception")
            .is_some());
    }

    #[test]
    fn fails_on_cut_objects_and_lengths_over_the_limit() {
        let mut cut = object_bytes(b"Word.Document.8", b"data");
        cut.truncate(cut.len() - 1);
        assert!(matches!(read_object(&cut).0, Err(Thrown::Io(_))));
        assert!(matches!(read_object(&[1, 0, 0]).0, Err(Thrown::Io(_))));
        let mut over = object_bytes(b"Word.Document.8", b"");
        let at = over.len() - 4;
        over[at..].copy_from_slice(&(MEMORY_LIMIT as u32 + 1).to_le_bytes());
        let Err(Thrown::Tika(line)) = read_object(&over).0 else {
            panic!("a length over the limit fails as a TikaException");
        };
        assert!(line.contains("Tried to allocate 20971521 bytes, but 20971520"));
        let mut embedded = Embedded::new();
        embedded.start_picture();
        let mut input = Input::new(b"abc", crate::input::Pages::IN_MEMORY);
        assert!(matches!(
            embedded.write_bytes(&mut input, MEMORY_LIMIT as u32 + 1),
            Ok(Err(Thrown::Tika(_)))
        ));
        assert_eq!(embedded.write_bytes(&mut input, 2).unwrap(), Ok(()));
        assert_eq!(
            embedded.write_bytes(&mut input, 4).unwrap(),
            Err(Thrown::Io(
                "java.io.EOFException: Length to read: 4 actual: 1".to_owned()
            ))
        );
        assert_eq!(&*embedded.complete().unwrap().unwrap().unwrap().data, b"ab");
    }

    #[test]
    fn names_pictures_as_tika_does() {
        let mut embedded = Embedded::new();
        assert!(embedded.complete().unwrap().unwrap().is_none());
        embedded.start_picture();
        let picture = finished(&mut embedded, PNG);
        assert_eq!(&*picture.data, PNG);
        assert_eq!(picture.metadata.get(RESOURCE_NAME), Some("embedded-0.png"));
        assert_eq!(picture.metadata.get(CONTENT_TYPE), Some("image/png"));
        assert_eq!(picture.metadata.get(THUMBNAIL), Some("false"));
        assert_eq!(picture.metadata.get(CONTENT_LENGTH), Some("16"));
        embedded.set_in_object(true);
        embedded.start_picture();
        embedded.set_raw_bitmap();
        let thumbnail = finished(&mut embedded, PNG);
        assert_eq!(
            thumbnail.metadata.get(RESOURCE_NAME),
            Some("thumbnail-0.png")
        );
        assert_eq!(thumbnail.metadata.get(THUMBNAIL), Some("true"));
        assert_eq!(thumbnail.metadata.get(PARSER_OVERRIDE), Some(RAW_BITMAP));
        embedded.set_in_object(false);
        embedded.start_picture();
        property(&mut embedded, "wzName", "x");
        property(&mut embedded, "wzDescription", "C:\\pics\\a.png");
        let described = finished(&mut embedded, PNG);
        assert_eq!(described.metadata.get(RESOURCE_NAME), Some("a.png"));
        assert_eq!(
            described.metadata.get(RELATIONSHIP_ID),
            Some("C:\\pics\\a.png")
        );
        assert_eq!(described.metadata.get("rtf:pict:wzName"), Some("x"));
        assert_eq!(described.metadata.get(CONTENT_TYPE), None);
        embedded.start_picture();
        let next = finished(&mut embedded, PNG);
        assert_eq!(next.metadata.get(RESOURCE_NAME), Some("embedded-1.png"));
    }
}
