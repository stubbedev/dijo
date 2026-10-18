//! MATLAB 5 MAT-files, as Tika's `MatParser` reads them through jmatio's `MatFileReader`: the
//! header's description, platform, creation date and byte order as metadata, then a paragraph
//! for each variable with its size and class, the fields of a structure listed after it.
//!
//! The file is read element by element as `MatFileReader` reads it, compressed elements inflated,
//! so that it fails where jmatio fails on data that ends early or does not fill its element. A
//! compressed element is inflated twice as a stream, where jmatio inflates it into memory: once
//! for its length, and again as it is read, the data of its arrays passed over; only the
//! variables' descriptions are kept, as jmatio reads them all before `MatParser` writes. It
//! reads what jmatio fails on (#156): `uint16` arrays, function handles, and Java objects, which
//! it lists by their class where they are a field of a structure, where `MatParser` throws on
//! them. A structure held in a field is named by its field, where jmatio names it "@".

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{Cursor, Read};

use flate2::bufread::ZlibDecoder;

use crate::compress::{Decoding, Undecoded};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::java;

const MEDIA_TYPE: &str = "application/x-matlab-data";

/// `MatFileReader.readHeader`: what the description of a file it reads starts with.
const MAGIC: &[u8] = b"MATLAB 5.0 MAT-file";
/// The bytes of the header's description.
const DESCRIPTION_BYTES: usize = 116;
/// The bytes `InflaterInputStream` reads its input in.
const INFLATER_CHUNK: usize = 512;
/// The bytes of a compressed element inflated at a time.
const WINDOW: usize = 64 << 10;
/// The bytes of a character array decoded at a time.
const PIECE: usize = 64 << 10;

const MI_INT8: i32 = 1;
const MI_UINT8: i32 = 2;
const MI_INT16: i32 = 3;
const MI_UINT16: i32 = 4;
const MI_INT32: i32 = 5;
const MI_UINT32: i32 = 6;
const MI_SINGLE: i32 = 7;
const MI_DOUBLE: i32 = 9;
const MI_INT64: i32 = 12;
const MI_UINT64: i32 = 13;
const MI_MATRIX: i32 = 14;
const MI_COMPRESSED: i32 = 15;
const MI_UTF8: i32 = 16;
const MI_UTF16: i32 = 17;
const MI_UTF32: i32 = 18;

const MX_CELL: i32 = 1;
const MX_STRUCT: i32 = 2;
const MX_OBJECT: i32 = 3;
const MX_CHAR: i32 = 4;
const MX_SPARSE: i32 = 5;
const MX_DOUBLE: i32 = 6;
const MX_SINGLE: i32 = 7;
const MX_INT8: i32 = 8;
const MX_UINT8: i32 = 9;
const MX_INT16: i32 = 10;
const MX_UINT16: i32 = 11;
const MX_INT32: i32 = 12;
const MX_UINT32: i32 = 13;
const MX_INT64: i32 = 14;
const MX_UINT64: i32 = 15;
const MX_FUNCTION: i32 = 16;
const MX_OPAQUE: i32 = 17;

const FLAG_COMPLEX: i32 = 0x0800;
const FLAG_LOGICAL: i32 = 0x0200;

/// Tika's `MatParser`.
pub struct MatParser;

impl Parser for MatParser {
    fn name(&self) -> &'static str {
        "extractous::MatParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.mat.MatParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let fails = |thrown: Thrown| Decline::Fails(thrown.into_error(self.tika_name()));
        let file = MatFile::read(input).map_err(|unread| match unread {
            Unread::Thrown(thrown) => fails(thrown),
            Unread::TooLarge(reason) => too_large(reason),
        })?;
        let header = Header::of(&file.description, file.endian).map_err(fails)?;
        Ok(Box::new(move |xhtml| {
            header.write(xhtml.metadata_mut());
            write(&file.variables, xhtml)
        }))
    }
}

/// A file holding an element that inflates into more than
/// [`decode_limit`](crate::compress::decode_limit) of its compressed bytes, as `reason` says,
/// which jmatio inflates into memory whole: Tika's path takes it where there is one.
fn too_large(reason: String) -> Decline<'static> {
    if cfg!(feature = "tika") {
        Decline::Deferred(format!("{reason}, which Tika's path inflates whole"))
    } else {
        Decline::Fails(Error::LimitExceeded(format!(
            "{reason}, the most the native parser inflates"
        )))
    }
}

/// The metadata `MatParser` takes from the header.
struct Header {
    created: Option<String>,
    platform: Option<String>,
    file_type: String,
    endian: String,
}

impl Header {
    /// What `MatParser` reads from the header's `description` and `endian` indicator, or what it
    /// throws on a description of fewer than three parts.
    fn of(description: &[u8], endian: [u8; 2]) -> Result<Self, Thrown> {
        let text = java::latin1(description);
        let parts = java::split(&text, ',');
        let Some(third) = parts.get(2) else {
            return Err(out_of_bounds(2, parts.len()));
        };
        let created = third
            .contains("Created")
            .then(|| after_last(third, "Created on:"))
            .transpose()?;
        let platform = parts[1]
            .contains("Platform")
            .then(|| after_last(parts[1], "Platform:"))
            .transpose()?;
        Ok(Self {
            created,
            platform,
            file_type: parts[0].to_owned(),
            endian: java::utf8(&endian),
        })
    }

    fn write(&self, metadata: &mut TikaMetadata) {
        metadata.set("Content-Type", MEDIA_TYPE);
        if let Some(created) = &self.created {
            metadata.set("dcterms:created", created.as_str());
        }
        if let Some(platform) = &self.platform {
            metadata.set("platform", platform.as_str());
        }
        metadata.set("fileType", self.file_type.as_str());
        metadata.set("endian", self.endian.as_str());
    }
}

/// `part.substring(part.lastIndexOf(label) + label.length()).trim()`, where the label may be
/// missing, `lastIndexOf` then giving -1, so that the text starts one character before the
/// label's length.
fn after_last(part: &str, label: &str) -> Result<String, Thrown> {
    let units: Vec<u16> = part.encode_utf16().collect();
    let found = part
        .rfind(label)
        .map_or(-1, |at| to_i64(java::utf16_length(&part[..at])));
    let start = found + to_i64(java::utf16_length(label));
    let rest = usize::try_from(start)
        .ok()
        .and_then(|start| units.get(start..))
        .ok_or_else(|| {
            runtime(
                "java.lang.StringIndexOutOfBoundsException",
                format!("begin {start}, end {}, length {}", units.len(), units.len()),
            )
        })?;
    Ok(java::trim(&String::from_utf16_lossy(rest)).to_owned())
}

/// The paragraphs `MatParser` writes: a line break, then each variable named with its size and
/// class, each field of a structure listed after it, a structure among them with its own fields.
fn write(variables: &[Array], xhtml: &mut Xhtml) -> Result<(), Failure> {
    xhtml.newline()?;
    for variable in variables {
        xhtml.element("p", &format!("{}:{}", variable.name, variable.describe()))?;
        let Some(fields) = &variable.fields else {
            continue;
        };
        xhtml.start_element("ul")?;
        xhtml.newline()?;
        for (name, field) in fields.all() {
            xhtml.start_element("li")?;
            xhtml.characters(&field.describe())?;
            if let Field::Array(
                array @ Array {
                    fields: Some(nested),
                    ..
                },
            ) = field
            {
                xhtml.start_element("ul")?;
                xhtml.element("li", &nested.content(name, array))?;
                xhtml.end_element("ul")?;
            }
            xhtml.end_element("li")?;
        }
        xhtml.end_element("ul")?;
    }
    Ok(())
}

/// Why a file is not read: what jmatio throws, or an element that inflates into too much.
enum Unread {
    Thrown(Thrown),
    TooLarge(String),
}

impl From<Thrown> for Unread {
    fn from(thrown: Thrown) -> Self {
        Self::Thrown(thrown)
    }
}

/// `MatlabIOException`, the `IOException` jmatio throws on what it cannot read.
fn matlab(message: impl Into<String>) -> Thrown {
    Thrown::Io(Exception::new("com.jmatio.io.MatlabIOException", message))
}

fn runtime(class: &'static str, message: impl Into<String>) -> Thrown {
    Thrown::Runtime(Exception::new(class, message))
}

fn underflow() -> Thrown {
    Thrown::Runtime(Exception::bare("java.nio.BufferUnderflowException"))
}

fn out_of_bounds(index: impl std::fmt::Display, length: usize) -> Thrown {
    runtime(
        "java.lang.ArrayIndexOutOfBoundsException",
        format!("Index {index} out of bounds for length {length}"),
    )
}

fn negative_size(size: i32) -> Thrown {
    runtime("java.lang.NegativeArraySizeException", size.to_string())
}

fn illegal_argument(message: String) -> Thrown {
    runtime("java.lang.IllegalArgumentException", message)
}

fn to_i64(value: usize) -> i64 {
    i64::try_from(value).expect("a length fits")
}

/// What `MatFileReader` reads of a file: its header's description and byte order indicator,
/// and its variables, in the order they come, each name once.
struct MatFile {
    description: Vec<u8>,
    endian: [u8; 2],
    variables: Vec<Array>,
}

impl MatFile {
    /// `MatFileReader.read`: the header, then the elements up to the end of the file.
    fn read(input: &[u8]) -> Result<Self, Unread> {
        let mut buffer = Buffer {
            data: Data::Slice(input),
            length: input.len(),
            at: 0,
            little_endian: false,
        };
        let description = zero_ended(&buffer.bytes(DESCRIPTION_BYTES)?).to_vec();
        if !description.starts_with(MAGIC) {
            return Err(matlab("This is not a valid MATLAB 5.0 MAT-file.").into());
        }
        buffer.skip(8)?;
        buffer.bytes(2)?;
        let endian: [u8; 2] = buffer.array()?;
        buffer.little_endian = endian == *b"IM";
        let mut variables = Variables::default();
        while buffer.remaining() > 0 {
            read_data(&mut buffer, &mut variables)?;
        }
        Ok(Self {
            description,
            endian,
            variables: variables.arrays,
        })
    }
}

/// The variables read so far, in their order, and their names.
#[derive(Default)]
struct Variables {
    arrays: Vec<Array>,
    names: HashSet<String>,
}

/// `MatFileReader.readData`: one element, a matrix kept as a variable unless one of its name was
/// read before, a compressed element inflated and the first element in it read.
fn read_data(buffer: &mut Buffer, variables: &mut Variables) -> Result<(), Unread> {
    let tag = Tag::read(buffer)?;
    match tag.kind {
        MI_COMPRESSED => {
            if to_i64(buffer.remaining()) < i64::from(tag.size) {
                return Err(matlab("Compressed buffer length miscalculated!").into());
            }
            let size = usize::try_from(tag.size)
                .ok()
                .filter(|&size| size > 0)
                .ok_or_else(|| {
                    matlab(
                        "Could not decompress data: java.io.EOFException: Unexpected end of \
                         ZLIB input stream",
                    )
                })?;
            let compressed = buffer.peek(size)?;
            let (length, taken) =
                inflated_length(&compressed).map_err(|undecoded| match undecoded {
                    Undecoded::TooLarge => {
                        Unread::TooLarge(format!("the compressed element at byte {}", buffer.at))
                    }
                    other => Unread::Thrown(matlab(format!("Could not decompress data: {other}"))),
                })?;
            buffer.at += (taken.div_ceil(INFLATER_CHUNK) * INFLATER_CHUNK).min(compressed.len());
            let mut inflated = Buffer {
                data: Data::Inflated(Inflated::new(compressed)),
                length,
                at: 0,
                little_endian: buffer.little_endian,
            };
            read_data(&mut inflated, variables)
        }
        MI_MATRIX => {
            let start = to_i64(buffer.at);
            match read_matrix(buffer, tag.size)? {
                Field::Array(array) if !variables.names.contains(&array.name) => {
                    variables.names.insert(array.name.clone());
                    variables.arrays.push(array);
                }
                _ => buffer.seek(start + i64::from(tag.size))?,
            }
            let unread = i64::from(tag.size) - (to_i64(buffer.at) - start);
            if unread != 0 {
                return Err(matlab(format!(
                    "Matrix was not red fully! {unread} remaining in the buffer."
                ))
                .into());
            }
            Ok(())
        }
        _ => Err(matlab(format!("Incorrect data tag: {}", tag.describe())).into()),
    }
}

/// `MatFileReader.readMatrix`: the array of the matrix element of `size` bytes the buffer is at,
/// or a Java object, past which it goes to the element's end.
fn read_matrix(buffer: &mut Buffer, size: i32) -> Result<Field, Thrown> {
    let end = to_i64(buffer.at) + i64::from(size);
    let flags = Tag::read(buffer)?.ints(buffer)?;
    if flags.len() == 1 {
        return Err(out_of_bounds(1, 1));
    }
    let attributes = flags.first().copied().unwrap_or(0);
    let class = attributes & 0xff;
    if class == MX_OPAQUE {
        Tag::read(buffer)?.string(buffer)?;
        Tag::read(buffer)?.string(buffer)?;
        let class_name = Tag::read(buffer)?.string(buffer)?;
        buffer.seek(end)?;
        return Ok(Field::Object(class_name));
    }
    let dims = Tag::read(buffer)?.ints(buffer)?;
    let name = Tag::read(buffer)?.string(buffer)?;
    if dims.len() < 2 {
        return Err(out_of_bounds(dims.len(), dims.len()));
    }
    let mut array = Array {
        name,
        dims,
        class,
        attributes,
        fields: None,
    };
    match class {
        MX_STRUCT => {
            capacity(array.dims[0].wrapping_mul(array.dims[1]))?;
            let mut fields = Fields::read_names(buffer)?;
            for index in 0..array.size().max(0) {
                fields.read_element(buffer, usize::try_from(index).expect("not negative"))?;
            }
            array.fields = Some(fields);
        }
        MX_CELL => {
            let size = array.size();
            capacity(size)?;
            for _ in 0..size.max(0) {
                let tag = Tag::read(buffer)?;
                if tag.size > 0 {
                    read_matrix(buffer, tag.size)?;
                }
            }
        }
        MX_CHAR => {
            let size = usize::try_from(array.size()).map_err(|_| negative_size(array.size()))?;
            if Tag::read(buffer)?.string_length(buffer)? > size {
                return Err(out_of_bounds(size, size));
            }
        }
        MX_SPARSE => read_sparse(buffer, &array)?,
        MX_FUNCTION => buffer.seek(end)?,
        MX_OBJECT => {
            Tag::read(buffer)?.string(buffer)?;
            capacity(array.dims[0].wrapping_mul(array.dims[1]))?;
            Fields::read_names(buffer)?.read_element(buffer, 0)?;
            array = Array {
                name: array.name,
                dims: vec![1, 1],
                class: MX_OBJECT,
                attributes: 0,
                fields: None,
            };
        }
        _ => {
            let Some(storage) = JavaType::of_class(class) else {
                return Err(matlab(format!(
                    "Incorrect matlab array class: {}",
                    class_name(class)
                )));
            };
            let bytes = array.size().wrapping_mul(storage.bytes());
            let bytes = usize::try_from(bytes)
                .map_err(|_| illegal_argument(format!("capacity < 0: ({bytes} < 0)")))?;
            let parts = if array.is_complex() { 2 } else { 1 };
            for _ in 0..parts {
                Tag::read(buffer)?.fill(buffer, storage, bytes)?;
            }
        }
    }
    Ok(Field::Array(array))
}

/// `new ArrayList<>(capacity)`, which throws on a negative capacity.
fn capacity(capacity: i32) -> Result<(), Thrown> {
    if capacity < 0 {
        return Err(illegal_argument(format!("Illegal Capacity: {capacity}")));
    }
    Ok(())
}

/// The row indices, column starts and values of a sparse array, each value placed by them as
/// jmatio places it, failing where they do not reach: the real values, then the imaginary ones.
fn read_sparse(buffer: &mut Buffer, array: &Array) -> Result<(), Thrown> {
    let rows = Tag::read(buffer)?.pass_ints(buffer)?;
    let columns = Tag::read(buffer)?.ints(buffer)?;
    let place = |values: usize| -> Result<(), Thrown> {
        let mut count = 0usize;
        for column in 0..usize::try_from(array.n()).unwrap_or(0) {
            let end = *columns
                .get(column + 1)
                .ok_or_else(|| out_of_bounds(column + 1, columns.len()))?;
            while to_i64(count) < i64::from(end) {
                if count >= values {
                    return Err(out_of_bounds(count, values));
                }
                if count >= rows {
                    return Err(out_of_bounds(count, rows));
                }
                count += 1;
            }
        }
        Ok(())
    };
    place(Tag::read(buffer)?.doubles(buffer)?)?;
    if array.is_complex() {
        place(Tag::read(buffer)?.doubles(buffer)?)?;
    }
    Ok(())
}

/// An array as jmatio holds it, as far as `MatParser` describes it: its name, dimensions,
/// class and flags, and a structure's fields.
struct Array {
    name: String,
    dims: Vec<i32>,
    class: i32,
    attributes: i32,
    fields: Option<Fields>,
}

impl Array {
    /// `MLEmptyArray`, which jmatio puts where a field of a structure is empty.
    fn empty() -> Self {
        Self {
            name: String::new(),
            dims: vec![0, 0],
            class: MX_DOUBLE,
            attributes: 0,
            fields: None,
        }
    }

    /// `getM`.
    fn m(&self) -> i32 {
        self.dims[0]
    }

    /// `getN`: the product of the dimensions after the first, in `int`.
    fn n(&self) -> i32 {
        if self.dims.len() > 2 {
            self.dims[1..]
                .iter()
                .fold(1i32, |n, &dim| n.wrapping_mul(dim))
        } else {
            self.dims[1]
        }
    }

    /// `getSize`.
    fn size(&self) -> i32 {
        self.m().wrapping_mul(self.n())
    }

    fn is_complex(&self) -> bool {
        self.attributes & FLAG_COMPLEX == FLAG_COMPLEX
    }

    /// `MLArray.toString`: its dimensions, class and flags in brackets.
    fn describe(&self) -> String {
        let dims = if self.dims.len() > 3 {
            format!("{}D", self.dims.len())
        } else {
            let mut dims = format!("{}x{}", self.dims[0], self.dims[1]);
            if let Some(third) = self.dims.get(2) {
                dims.push_str(&format!("x{third}"));
            }
            dims
        };
        let logical = if self.attributes & FLAG_LOGICAL == FLAG_LOGICAL {
            " (logical)"
        } else {
            ""
        };
        let flags = match (self.class == MX_SPARSE, self.is_complex()) {
            (true, true) => " (sparse complex)",
            (true, false) => " (sparse)",
            (false, true) => " (complex)",
            (false, false) => "",
        };
        format!("[{dims}  {}{logical} array{flags}]", class_name(self.class))
    }
}

/// `MLArray.typeToString`.
fn class_name(class: i32) -> &'static str {
    match class {
        MX_CELL => "cell",
        MX_STRUCT => "struct",
        MX_CHAR => "char",
        MX_SPARSE => "sparse",
        MX_DOUBLE => "double",
        MX_SINGLE => "single",
        MX_INT8 => "int8",
        MX_UINT8 => "uint8",
        MX_INT16 => "int16",
        MX_UINT16 => "uint16",
        MX_INT32 => "int32",
        MX_UINT32 => "uint32",
        MX_INT64 => "int64",
        MX_UINT64 => "uint64",
        MX_FUNCTION => "function_handle",
        MX_OPAQUE => "opaque",
        MX_OBJECT => "object",
        _ => "unknown",
    }
}

/// The value of a matrix element: an array, or a Java object, by its class name.
enum Field {
    Array(Array),
    Object(String),
}

impl Field {
    /// `String.valueOf` of an array, and a Java object as one of class `opaque` with its class
    /// name.
    fn describe(&self) -> String {
        match self {
            Self::Array(array) => array.describe(),
            Self::Object(class) => {
                format!("[1x1  {} array ({class})]", class_name(MX_OPAQUE))
            }
        }
    }
}

/// `MLStructure`'s fields: their names in the order first set, and each element's fields, in
/// the order set, a name set again keeping its place.
struct Fields {
    names: Vec<String>,
    keys: Vec<String>,
    elements: Vec<Vec<(String, Field)>>,
    current: usize,
}

impl Fields {
    /// The names of the fields: the longest name's length, then the names, each that long, ended
    /// by a zero byte.
    fn read_names(buffer: &mut Buffer) -> Result<Self, Thrown> {
        Tag::read(buffer)?;
        let length = buffer.int()?;
        let tag = Tag::read(buffer)?;
        if length == 0 {
            return Err(runtime("java.lang.ArithmeticException", "/ by zero"));
        }
        let count = tag.size.wrapping_div(length);
        let count = usize::try_from(count).map_err(|_| negative_size(count))?;
        let mut names = Vec::with_capacity(count.min(buffer.remaining()));
        for _ in 0..count {
            let length = usize::try_from(length).map_err(|_| negative_size(length))?;
            names.push(java::latin1(zero_ended(&buffer.bytes(length)?)));
        }
        buffer.skip(tag.padding)?;
        Ok(Self {
            names,
            keys: Vec::new(),
            elements: Vec::new(),
            current: 0,
        })
    }

    /// The fields of the element at `index`, each an array, an empty array where its element is
    /// empty, or a Java object's null.
    fn read_element(&mut self, buffer: &mut Buffer, index: usize) -> Result<(), Thrown> {
        for at in 0..self.names.len() {
            let tag = Tag::read(buffer)?;
            let value = if tag.size > 0 {
                read_matrix(buffer, tag.size)?
            } else {
                Field::Array(Array::empty())
            };
            let name = self.names[at].clone();
            self.set(name, value, index);
        }
        Ok(())
    }

    /// `setField(name, value, index)`.
    fn set(&mut self, name: String, value: Field, index: usize) {
        if !self.keys.contains(&name) {
            self.keys.push(name.clone());
        }
        self.current = index;
        if self.elements.len() <= index {
            self.elements.push(Vec::new());
        }
        let element = &mut self.elements[index];
        match element.iter_mut().find(|(set, _)| *set == name) {
            Some((_, field)) => *field = value,
            None => element.push((name, value)),
        }
    }

    /// `getAllFields`: every element's fields, element by element, with their names.
    fn all(&self) -> impl Iterator<Item = (&str, &Field)> {
        self.elements
            .iter()
            .flatten()
            .map(|(name, field)| (name.as_str(), field))
    }

    /// `MLStructure.contentToString` of the structure `array` these are the fields of, held in
    /// the field `name`: that name, then each field described, where it is one element, or its
    /// size and the names of its fields.
    fn content(&self, name: &str, array: &Array) -> String {
        let mut content = format!("{name} = \n");
        if array.size() == 1 {
            for key in &self.keys {
                let field = self.elements.get(self.current).and_then(|element| {
                    element
                        .iter()
                        .find(|(name, _)| name == key)
                        .map(|(_, field)| field)
                });
                let described = field.map_or_else(|| "null".to_owned(), Field::describe);
                content.push_str(&format!("\t{key} : {described}\n"));
            }
        } else {
            content.push_str(&format!(
                "\n{}x{} struct array with fields: \n",
                array.m(),
                array.n()
            ));
            for key in &self.keys {
                content.push_str(&format!("\t{key}\n"));
            }
        }
        content
    }
}

/// The bytes `MatFileReader` reads from, as a `ByteBuffer` in the file's byte order: `length`
/// bytes of data, read at `at`.
struct Buffer<'a> {
    data: Data<'a>,
    length: usize,
    at: usize,
    little_endian: bool,
}

/// What a buffer reads: the file's bytes in place, or what a compressed element inflates into.
enum Data<'a> {
    Slice(&'a [u8]),
    Inflated(Inflated<'a>),
}

/// The data a compressed element inflates into, read as a stream from its compressed bytes: the
/// last [`WINDOW`] bytes inflated, from `start`, inflated on to read further and again from the
/// start to read behind them.
struct Inflated<'a> {
    decoder: ZlibDecoder<Cursor<Cow<'a, [u8]>>>,
    window: Vec<u8>,
    start: usize,
}

impl<'a> Inflated<'a> {
    fn new(compressed: Cow<'a, [u8]>) -> Self {
        Self {
            decoder: ZlibDecoder::new(Cursor::new(compressed)),
            window: Vec::with_capacity(WINDOW),
            start: 0,
        }
    }

    /// Copies the inflated bytes at `at` into `out`, all of which the data holds.
    fn copy(&mut self, at: usize, out: &mut [u8]) -> Result<(), Thrown> {
        let mut done = 0;
        while done < out.len() {
            let position = at + done;
            if position < self.start {
                let mut compressed = std::mem::replace(
                    &mut self.decoder,
                    ZlibDecoder::new(Cursor::new(Cow::Borrowed(&[]))),
                )
                .into_inner();
                compressed.set_position(0);
                self.decoder = ZlibDecoder::new(compressed);
                self.window.clear();
                self.start = 0;
            }
            let end = self.start + self.window.len();
            if position >= end {
                self.start = end;
                self.window.resize(WINDOW, 0);
                let read = self
                    .decoder
                    .read(&mut self.window)
                    .map_err(|error| matlab(format!("Could not decompress data again: {error}")))?;
                self.window.truncate(read);
                if read == 0 {
                    return Err(underflow());
                }
                continue;
            }
            let offset = position - self.start;
            let count = (out.len() - done).min(self.window.len() - offset);
            out[done..done + count].copy_from_slice(&self.window[offset..offset + count]);
            done += count;
        }
        Ok(())
    }
}

/// How many bytes the zlib stream `compressed` starts with inflates into, up to
/// [`decode_limit`](crate::compress::decode_limit) of them, past which it fails, and how many
/// bytes of `compressed` the stream takes: jmatio's inflation of a compressed element, keeping
/// nothing of it.
fn inflated_length(compressed: &[u8]) -> Result<(usize, usize), Undecoded> {
    let mut decoder = ZlibDecoder::new(compressed);
    let mut decoding = Decoding::discarded(compressed.len());
    decoding.read_from(&mut decoder)?;
    let length = decoding.finish()?.whole()?;
    let length = usize::try_from(length).map_err(|_| Undecoded::TooLarge)?;
    let taken = usize::try_from(decoder.total_in()).expect("a stream takes at most its input");
    Ok((length, taken))
}

impl<'a> Buffer<'a> {
    fn remaining(&self) -> usize {
        self.length - self.at
    }

    /// The `count` bytes at the position, which stays where it is.
    fn peek(&mut self, count: usize) -> Result<Cow<'a, [u8]>, Thrown> {
        if count > self.remaining() {
            return Err(underflow());
        }
        match &mut self.data {
            Data::Slice(data) => Ok(Cow::Borrowed(&data[self.at..self.at + count])),
            Data::Inflated(inflated) => {
                let mut bytes = vec![0; count];
                inflated.copy(self.at, &mut bytes)?;
                Ok(Cow::Owned(bytes))
            }
        }
    }

    fn bytes(&mut self, count: usize) -> Result<Cow<'a, [u8]>, Thrown> {
        let read = self.peek(count)?;
        self.at += count;
        Ok(read)
    }

    /// Moves past `count` bytes, as reading them does, without reading them.
    fn pass(&mut self, count: usize) -> Result<(), Thrown> {
        if count > self.remaining() {
            return Err(underflow());
        }
        self.at += count;
        Ok(())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Thrown> {
        if N > self.remaining() {
            return Err(underflow());
        }
        let mut bytes = [0; N];
        match &mut self.data {
            Data::Slice(data) => bytes.copy_from_slice(&data[self.at..self.at + N]),
            Data::Inflated(inflated) => inflated.copy(self.at, &mut bytes)?,
        }
        self.at += N;
        Ok(bytes)
    }

    fn byte(&mut self) -> Result<u8, Thrown> {
        Ok(self.array::<1>()?[0])
    }

    fn short(&mut self) -> Result<i16, Thrown> {
        let bytes = self.array()?;
        Ok(if self.little_endian {
            i16::from_le_bytes(bytes)
        } else {
            i16::from_be_bytes(bytes)
        })
    }

    fn int(&mut self) -> Result<i32, Thrown> {
        let bytes = self.array()?;
        Ok(if self.little_endian {
            i32::from_le_bytes(bytes)
        } else {
            i32::from_be_bytes(bytes)
        })
    }

    fn long(&mut self) -> Result<i64, Thrown> {
        let bytes = self.array()?;
        Ok(if self.little_endian {
            i64::from_le_bytes(bytes)
        } else {
            i64::from_be_bytes(bytes)
        })
    }

    fn double(&mut self) -> Result<f64, Thrown> {
        Ok(f64::from_bits(self.long()?.cast_unsigned()))
    }

    /// `position(position)`, which throws beyond either end.
    fn seek(&mut self, position: i64) -> Result<(), Thrown> {
        self.at = usize::try_from(position)
            .ok()
            .filter(|&at| at <= self.length)
            .ok_or_else(|| {
                illegal_argument(format!(
                    "newPosition out of bounds: ({position}, limit {})",
                    self.length
                ))
            })?;
        Ok(())
    }

    /// `position(position() + count)`.
    fn skip(&mut self, count: i32) -> Result<(), Thrown> {
        self.seek(to_i64(self.at) + i64::from(count))
    }
}

/// `zeroEndByteArrayToString`'s bytes: those before the first zero byte.
fn zero_ended(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    &bytes[..end]
}

/// `MatDataTypes.sizeOf`: the bytes of a value of the data type, 1 for those it does not list.
fn size_of(kind: i32) -> i32 {
    match kind {
        MI_INT16 | MI_UINT16 => 2,
        MI_INT32 | MI_UINT32 => 4,
        MI_INT64 | MI_UINT64 | MI_DOUBLE => 8,
        _ => 1,
    }
}

/// The Java type a `MatFileInputStream` reads values as, and a numeric array holds them as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JavaType {
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
}

impl JavaType {
    /// The type the numeric array of `class` holds its values as, if jmatio reads that class.
    fn of_class(class: i32) -> Option<Self> {
        match class {
            MX_DOUBLE => Some(Self::Double),
            MX_SINGLE => Some(Self::Float),
            MX_INT8 | MX_UINT8 => Some(Self::Byte),
            MX_INT16 | MX_UINT16 => Some(Self::Short),
            MX_INT32 | MX_UINT32 => Some(Self::Int),
            MX_INT64 | MX_UINT64 => Some(Self::Long),
            _ => None,
        }
    }

    /// `getBytesAllocated`.
    fn bytes(self) -> i32 {
        match self {
            Self::Byte => 1,
            Self::Short => 2,
            Self::Int | Self::Float => 4,
            Self::Long | Self::Double => 8,
        }
    }

    /// The bytes reading a value of the data type `kind` as this takes, or what it throws on a
    /// type it does not read.
    fn width(self, kind: i32) -> Result<usize, Thrown> {
        match kind {
            MI_INT8 | MI_UINT8 => Ok(1),
            MI_INT16 | MI_UINT16 => Ok(2),
            MI_INT32 | MI_UINT32 => Ok(4),
            MI_DOUBLE => Ok(8),
            MI_INT64 | MI_UINT64 if matches!(self, Self::Short | Self::Int | Self::Long) => Ok(8),
            MI_SINGLE if self == Self::Float => Ok(4),
            MI_UTF8 if self == Self::Byte => Ok(1),
            _ => Err(illegal_argument(format!("Unknown data type: {kind}"))),
        }
    }
}

/// `MatTag.getPadding`: the bytes after an element's data up to its end, whose divisions by the
/// value size of its type throw on a small element of 8-byte values.
fn padding(size: i32, small: bool, value: i32) -> Result<i32, Thrown> {
    let boundary = if small { 4 } else { 8 };
    let values = boundary / value;
    if values == 0 {
        return Err(runtime("java.lang.ArithmeticException", "/ by zero"));
    }
    let b = ((size / value) % values) * value;
    Ok(if b != 0 { boundary - b } else { 0 })
}

/// `ISMatTag`: an element's data type and size, packed into four bytes for a small element,
/// and the padding after its data.
struct Tag {
    kind: i32,
    size: i32,
    padding: i32,
}

impl Tag {
    fn read(buffer: &mut Buffer) -> Result<Self, Thrown> {
        let first = buffer.int()?;
        let (kind, size, small) = if first >> 16 == 0 {
            (first, buffer.int()?, false)
        } else {
            (first & 0xffff, first >> 16, true)
        };
        Ok(Self {
            kind,
            size,
            padding: padding(size, small, size_of(kind))?,
        })
    }

    /// `MatTag.toString`.
    fn describe(&self) -> String {
        let name = match self.kind {
            MI_INT8 => "int8",
            MI_UINT8 => "uint8",
            MI_INT16 => "int16",
            MI_UINT16 => "uint16",
            MI_INT32 => "int32",
            MI_UINT32 => "uint32",
            MI_SINGLE => "single",
            MI_DOUBLE => "double",
            MI_INT64 => "int64",
            MI_UINT64 => "uint64",
            MI_MATRIX => "matrix",
            MI_COMPRESSED => "compressed",
            MI_UTF8 => "uft8",
            MI_UTF16 => "utf16",
            MI_UTF32 => "utf32",
            _ => "unknown",
        };
        format!("[tag: {name} size: {}]", self.size)
    }

    /// `size / sizeOf()` values, an array of which `new T[elements]` throws on where it is
    /// negative.
    fn elements(&self) -> Result<usize, Thrown> {
        let elements = self.size / size_of(self.kind);
        usize::try_from(elements).map_err(|_| negative_size(elements))
    }

    /// `readToIntArray`.
    fn ints(&self, buffer: &mut Buffer) -> Result<Vec<i32>, Thrown> {
        let elements = self.elements()?;
        let mut values = Vec::with_capacity(elements.min(buffer.remaining()));
        for _ in 0..elements {
            values.push(match self.kind {
                MI_UINT8 => i32::from(buffer.byte()?),
                MI_INT8 => i32::from(buffer.byte()?.cast_signed()),
                MI_UINT16 => i32::from(buffer.short()?.cast_unsigned()),
                MI_INT16 => i32::from(buffer.short()?),
                MI_UINT32 | MI_INT32 => buffer.int()?,
                MI_UINT64 | MI_INT64 => buffer.long()? as i32,
                MI_DOUBLE => buffer.double()? as i32,
                kind => return Err(illegal_argument(format!("Unknown data type: {kind}"))),
            });
        }
        buffer.skip(self.padding)?;
        Ok(values)
    }

    /// `readToIntArray`, of which only how many values it reads matters: they are passed over.
    fn pass_ints(&self, buffer: &mut Buffer) -> Result<usize, Thrown> {
        let elements = self.elements()?;
        if elements > 0 {
            let width = match self.kind {
                MI_UINT8 | MI_INT8 => 1,
                MI_UINT16 | MI_INT16 => 2,
                MI_UINT32 | MI_INT32 => 4,
                MI_UINT64 | MI_INT64 | MI_DOUBLE => 8,
                kind => return Err(illegal_argument(format!("Unknown data type: {kind}"))),
            };
            buffer.pass(elements * width)?;
        }
        buffer.skip(self.padding)?;
        Ok(elements)
    }

    /// `readToDoubleArray`, of which only how many values it reads matters.
    fn doubles(&self, buffer: &mut Buffer) -> Result<usize, Thrown> {
        let elements = self.elements()?;
        if elements > 0 {
            buffer.pass(elements * JavaType::Double.width(self.kind)?)?;
        }
        buffer.skip(self.padding)?;
        Ok(elements)
    }

    /// `readToString`: the bytes `readToByteArray` reads, a byte of each value, as UTF-8.
    fn string(&self, buffer: &mut Buffer) -> Result<String, Thrown> {
        let elements = self.elements()?;
        let mut bytes = Vec::with_capacity(elements.min(buffer.remaining()));
        for _ in 0..elements {
            bytes.push(self.byte(buffer)?);
        }
        buffer.skip(self.padding)?;
        Ok(java::utf8(&bytes))
    }

    /// The length in UTF-16 units of [`Tag::string`], decoded [`PIECE`] bytes at a time, each
    /// piece ending before a byte that starts a character or cannot continue one, so that each
    /// decodes as it does within the whole.
    fn string_length(&self, buffer: &mut Buffer) -> Result<usize, Thrown> {
        let elements = self.elements()?;
        let mut length = 0;
        let mut piece = Vec::with_capacity(elements.min(buffer.remaining()).min(PIECE));
        let mut continuing = 0;
        for _ in 0..elements {
            let byte = self.byte(buffer)?;
            continuing = if byte & 0xC0 == 0x80 {
                continuing + 1
            } else {
                0
            };
            if piece.len() >= PIECE && (continuing == 0 || continuing > 3) {
                length += java::utf16_length(&java::utf8(&piece));
                piece.clear();
            }
            piece.push(byte);
        }
        buffer.skip(self.padding)?;
        Ok(length + java::utf16_length(&java::utf8(&piece)))
    }

    /// The byte `readToByteArray` reads of the next value.
    fn byte(&self, buffer: &mut Buffer) -> Result<u8, Thrown> {
        Ok(match self.kind {
            MI_UINT8 | MI_INT8 | MI_UTF8 => buffer.byte()?,
            MI_UINT16 | MI_INT16 => buffer.short()? as u8,
            MI_UINT32 | MI_INT32 => buffer.int()? as u8,
            MI_DOUBLE => buffer.double()? as i32 as u8,
            kind => return Err(illegal_argument(format!("Unknown data type: {kind}"))),
        })
    }

    /// `readToByteBuffer` into a numeric array of `capacity` bytes holding `storage` values, then
    /// the padding: the data copied as it is where its values are as wide as the array's and in
    /// the order of Java's buffers, big-endian, the tag's size then deciding where it ends, and
    /// otherwise read value by value, as many as the array holds.
    fn fill(&self, buffer: &mut Buffer, storage: JavaType, capacity: usize) -> Result<(), Thrown> {
        if size_of(self.kind) == storage.bytes() && !buffer.little_endian {
            if capacity > 0 && buffer.remaining() == 0 {
                return Err(runtime(
                    "java.lang.IllegalStateException",
                    "jmatio copies nothing from an exhausted buffer, without end",
                ));
            }
            let start = to_i64(buffer.at);
            buffer.pass(capacity)?;
            let elements = self.size / size_of(self.kind);
            buffer.seek(start + i64::from(elements.wrapping_mul(storage.bytes())))?;
        } else {
            let values = capacity / usize::try_from(storage.bytes()).expect("positive");
            if values > 0 {
                buffer.pass(values * storage.width(self.kind)?)?;
            }
        }
        buffer.skip(self.padding)
    }
}

/// For the parity tests: a native result with each structure held in a field named "@", as
/// jmatio names it, where the parser names it by its field (#156): each line that ends a name
/// with " = ", the name after the tag or tab before it.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub fn as_jmatio_names(
    text: &mut String,
    _metadata: &mut crate::formats::parity::ComparedMetadata,
    _tika: &crate::formats::parity::ComparedMetadata,
) {
    *text = text
        .split_inclusive('\n')
        .map(|line| match line.strip_suffix(" = \n") {
            Some(named) => {
                let start = named.rfind(['>', '\t']).map_or(0, |at| at + 1);
                format!("{}@ = \n", &named[..start])
            }
            None => line.to_owned(),
        })
        .collect();
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::compress::zlib::tests::zlib;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_fails_as_tika, assert_read_where_tika_fails, assert_same_as_tika_rewriting,
        conformance_corpus, documents, Scratch,
    };

    const DESCRIPTION: &str =
        "MATLAB 5.0 MAT-file, Platform: GLNXA64, Created on: Sat Oct 18 08:00:00 2026";

    /// An element of `kind` holding `data`, padded to eight bytes, as MATLAB writes it.
    fn element(kind: i32, data: &[u8]) -> Vec<u8> {
        let mut bytes = kind.to_le_bytes().to_vec();
        bytes.extend_from_slice(&i32::try_from(data.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(data);
        bytes.resize(bytes.len().next_multiple_of(8), 0);
        bytes
    }

    fn ints(values: &[i32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }

    /// A matrix of `class` with the further `flags`, `dims` and `name`, then `body`.
    fn matrix(class: i32, flags: i32, dims: &[i32], name: &str, body: &[u8]) -> Vec<u8> {
        let mut content = element(MI_UINT32, &ints(&[class | flags, 0]));
        content.extend(element(MI_INT32, &ints(dims)));
        content.extend(element(MI_INT8, name.as_bytes()));
        content.extend_from_slice(body);
        element(MI_MATRIX, &content)
    }

    fn doubles(name: &str, dims: &[i32]) -> Vec<u8> {
        let count = usize::try_from(dims.iter().product::<i32>()).unwrap();
        let values: Vec<u8> = (0..count).flat_map(|n| (n as f64).to_le_bytes()).collect();
        matrix(MX_DOUBLE, 0, dims, name, &element(MI_DOUBLE, &values))
    }

    /// A structure of one element whose fields are named `names` and hold `fields`.
    fn structure(name: &str, dims: &[i32], names: &[&str], fields: &[Vec<u8>]) -> Vec<u8> {
        let length = names.iter().map(|name| name.len()).max().unwrap_or(0) + 1;
        let mut body = ((4 << 16) | MI_INT32).to_le_bytes().to_vec();
        body.extend_from_slice(&i32::try_from(length).unwrap().to_le_bytes());
        let mut packed = Vec::new();
        for name in names {
            let mut padded = name.as_bytes().to_vec();
            padded.resize(length, 0);
            packed.extend(padded);
        }
        body.extend(element(MI_INT8, &packed));
        for field in fields {
            body.extend_from_slice(field);
        }
        matrix(MX_STRUCT, 0, dims, name, &body)
    }

    fn file(description: &str, elements: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = description.as_bytes().to_vec();
        bytes.resize(DESCRIPTION_BYTES, b' ');
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&[0x00, 0x01]);
        bytes.extend_from_slice(b"IM");
        for element in elements {
            bytes.extend_from_slice(element);
        }
        bytes
    }

    /// A Java object of `class` as MATLAB writes it: a matrix of class `opaque` holding its
    /// name, type system and class name, then the matrix of its serialized bytes.
    fn object(class: &str) -> Vec<u8> {
        let mut content = element(MI_UINT32, &ints(&[MX_OPAQUE, 0]));
        content.extend(element(MI_INT8, b""));
        content.extend(element(MI_INT8, b"java"));
        content.extend(element(MI_INT8, class.as_bytes()));
        content.extend(matrix(
            MX_UINT8,
            0,
            &[1, 3],
            "",
            &element(MI_UINT8, &[0xac, 0xed, 0]),
        ));
        element(MI_MATRIX, &content)
    }

    /// The variables jmatio fails on (#156): a `uint16` array, a function handle, and a
    /// structure holding a Java object then an array.
    fn unread_by_jmatio() -> Vec<Vec<u8>> {
        vec![
            matrix(
                MX_UINT16,
                0,
                &[1, 2],
                "u",
                &element(MI_UINT16, &[1, 0, 2, 0]),
            ),
            matrix(MX_FUNCTION, 0, &[1, 1], "f", &doubles("", &[1, 1])),
            structure(
                "s",
                &[1, 1],
                &["o", "x"],
                &[object("java.lang.Double"), doubles("", &[2, 2])],
            ),
        ]
    }

    fn compressed(element: &[u8]) -> Vec<u8> {
        let data = zlib(element);
        let mut bytes = MI_COMPRESSED.to_le_bytes().to_vec();
        bytes.extend_from_slice(&i32::try_from(data.len()).unwrap().to_le_bytes());
        bytes.extend(data);
        bytes
    }

    /// A workbook of the classes `MatParser` describes: numbers, text, a logical and a complex
    /// array, a sparse one, a cell, a structure holding another and an empty field, one
    /// compressed variable and a name used twice.
    fn variables() -> Vec<Vec<u8>> {
        let text: Vec<u8> = "hav".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let sparse = [
            element(MI_INT32, &ints(&[0, 1])),
            element(MI_INT32, &ints(&[0, 1, 2])),
            element(
                MI_DOUBLE,
                &[1.5f64.to_le_bytes(), 2.5f64.to_le_bytes()].concat(),
            ),
        ]
        .concat();
        let inner = structure(
            "",
            &[1, 1],
            &["x", "y"],
            &[doubles("", &[1, 1]), element(MI_MATRIX, &[])],
        );
        vec![
            doubles("tal", &[2, 3]),
            matrix(MX_CHAR, 0, &[1, 3], "navn", &element(MI_UINT16, &text)),
            matrix(
                MX_UINT8,
                FLAG_LOGICAL,
                &[1, 2],
                "flag",
                &element(MI_UINT8, &[1, 0]),
            ),
            matrix(
                MX_DOUBLE,
                FLAG_COMPLEX,
                &[1, 1],
                "kompleks",
                &[
                    element(MI_DOUBLE, &1.0f64.to_le_bytes()),
                    element(MI_DOUBLE, &2.0f64.to_le_bytes()),
                ]
                .concat(),
            ),
            matrix(MX_SPARSE, 0, &[2, 2], "spredt", &sparse),
            matrix(
                MX_CELL,
                0,
                &[1, 2],
                "celle",
                &[doubles("", &[1, 1]), element(MI_MATRIX, &[])].concat(),
            ),
            structure(
                "s",
                &[1, 1],
                &["a", "indre", "tom"],
                &[doubles("", &[3, 4, 2]), inner, element(MI_MATRIX, &[])],
            ),
            compressed(&doubles("pakket", &[1, 4, 2, 2])),
            doubles("tal", &[9, 9]),
        ]
    }

    /// The text and metadata the native path extracts from the MAT-file `bytes`.
    fn extract(bytes: &[u8]) -> (String, crate::Metadata) {
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(bytes)
            .unwrap()
    }

    #[test]
    fn describes_each_variable_as_jmatio_does() {
        let (xml, metadata) = extract(&file(DESCRIPTION, &variables()));
        for described in [
            "<p>tal:[2x3  double array]</p>",
            "<p>navn:[1x3  char array]</p>",
            "<p>flag:[1x2  uint8 (logical) array]</p>",
            "<p>kompleks:[1x1  double array (complex)]</p>",
            "<p>spredt:[2x2  sparse array (sparse)]</p>",
            "<p>celle:[1x2  cell array]</p>",
            "<p>s:[1x1  struct array]</p>",
            "<li>[3x4x2  double array]</li>",
            "<li>[1x1  struct array]<ul>",
            "<li>indre = \n\tx : [1x1  double array]\n\ty : [0x0  double array]\n</li>",
            "<li>[0x0  double array]</li>",
            "<p>pakket:[4D  double array]</p>",
        ] {
            assert!(xml.contains(described), "{described:?} in {xml}");
        }
        assert_eq!(xml.matches("<p>tal:").count(), 1, "{xml}");
        assert_eq!(metadata["dcterms:created"], ["Sat Oct 18 08:00:00 2026"]);
        assert_eq!(metadata["platform"], ["GLNXA64"]);
        assert_eq!(metadata["fileType"], ["MATLAB 5.0 MAT-file"]);
        assert_eq!(metadata["endian"], ["IM"]);
    }

    #[test]
    fn reads_what_jmatio_fails_on() {
        let (xml, _) = extract(&file(DESCRIPTION, &unread_by_jmatio()));
        for described in [
            "<p>u:[1x2  uint16 array]</p>",
            "<p>f:[1x1  function_handle array]</p>",
            "<p>s:[1x1  struct array]</p>",
            "<li>[1x1  opaque array (java.lang.Double)]</li>",
            "<li>[2x2  double array]</li>",
        ] {
            assert!(xml.contains(described), "{described:?} in {xml}");
        }
    }

    #[test]
    fn fails_where_jmatio_or_mat_parser_throws() {
        let fails = |bytes: &[u8]| {
            crate::Extractor::new()
                .extract_bytes_to_string(bytes)
                .unwrap_err()
                .to_string()
        };
        let unknown = matrix(18, 0, &[1, 1], "u", &[]);
        assert!(fails(&file(DESCRIPTION, &[unknown])).contains("Incorrect matlab array class"));
        let two_parts = "MATLAB 5.0 MAT-file, written by Octave";
        assert!(fails(&file(two_parts, &[doubles("x", &[1, 1])]))
            .contains("ArrayIndexOutOfBoundsException"));
        let mut cut = file(DESCRIPTION, &[doubles("x", &[2, 2])]);
        cut.truncate(cut.len() - 8);
        assert!(fails(&cut).contains("BufferUnderflowException"));
    }

    /// A file too large to read whole: an array of doubles past what is read whole, then a
    /// compressed array that inflates past it, a character array behind it in the same
    /// element, then a last variable. The data is passed over in place and as it inflates,
    /// under a budget far below the file.
    #[test]
    fn describes_variables_of_files_too_large_to_read_whole() {
        use extractous_core::large::{LargeFile, BUDGET};

        use crate::input::MAX_READ_WHOLE;

        let values = i32::try_from(MAX_READ_WHOLE / 8 + (1 << 20)).unwrap();
        let name = element(MI_INT8, b"stor");
        let head = [
            element(MI_UINT32, &ints(&[MX_DOUBLE, 0])),
            element(MI_INT32, &ints(&[1, values])),
            name,
        ]
        .concat();
        let data_length = i32::try_from(values as usize * 8).unwrap();
        let mut large = MI_MATRIX.to_le_bytes().to_vec();
        large.extend_from_slice(&(head.len() as i32 + 8 + data_length).to_le_bytes());
        large.extend(head);
        large.extend_from_slice(&MI_DOUBLE.to_le_bytes());
        large.extend_from_slice(&data_length.to_le_bytes());
        let inflated = matrix(
            MX_DOUBLE,
            0,
            &[1, values],
            "pakket",
            &element(MI_DOUBLE, &vec![0; data_length as usize]),
        );
        let compressed = element(MI_COMPRESSED, &zlib(&inflated));
        drop(inflated);
        let last = doubles("sidst", &[1, 1]);
        let file = LargeFile::written("large.mat", |out| {
            out.write_all(&file(DESCRIPTION, &[]))?;
            out.write_all(&large)?;
            let zeros = vec![0u8; 1 << 20];
            let mut left = data_length as usize;
            while left > 0 {
                let count = left.min(zeros.len());
                out.write_all(&zeros[..count])?;
                left -= count;
            }
            out.write_all(&compressed)?;
            out.write_all(&last)
        });
        assert!(std::fs::metadata(file.path()).unwrap().len() > MAX_READ_WHOLE);
        let (text, _) = file.extract_with(&crate::Extractor::new(), BUDGET, MatParser.name());
        for described in [
            format!("stor:[1x{values}  double array]"),
            format!("pakket:[1x{values}  double array]"),
            "sidst:[1x1  double array]".to_owned(),
        ] {
            assert!(text.contains(&described), "{described:?} in {text}");
        }
    }

    /// The length of a character array's text, decoded a piece at a time, is that of all of it
    /// decoded at once, wherever its multi-byte characters and malformed bytes fall.
    #[test]
    fn measures_character_arrays_a_piece_at_a_time() {
        let mut bytes = "ø".repeat(PIECE).into_bytes();
        bytes.extend_from_slice(&[0xE2, 0x82, 0x80, 0x80, 0x80, 0x80, b'a', 0xF0]);
        bytes.extend("€".repeat(PIECE).into_bytes());
        let tag = Tag {
            kind: MI_UINT8,
            size: i32::try_from(bytes.len()).unwrap(),
            padding: 0,
        };
        let mut buffer = Buffer {
            data: Data::Slice(&bytes),
            length: bytes.len(),
            at: 0,
            little_endian: true,
        };
        assert_eq!(
            tag.string_length(&mut buffer).unwrap(),
            java::utf16_length(&java::utf8(&bytes))
        );
    }

    /// Bytes of an inflated element are read again from its start where they lie behind those
    /// the stream last gave.
    #[test]
    fn reads_an_inflated_element_behind_its_window_again() {
        let data: Vec<u8> = (0..3 * WINDOW).map(|at| (at % 251) as u8).collect();
        let mut inflated = Inflated::new(Cow::Owned(zlib(&data)));
        let mut read = [0; 4];
        for at in [2 * WINDOW + 7, 5, WINDOW - 2, 0] {
            inflated.copy(at, &mut read).unwrap();
            assert_eq!(read, data[at..at + 4], "at {at}");
        }
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_synthetic_mat_files_as_tikas_path_does() {
        let scratch = Scratch::new("mat");
        let read = [scratch.write("variables.mat", &file(DESCRIPTION, &variables()))];
        let paths: Vec<&Path> = read.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika_rewriting(&paths, MatParser.tika_name(), as_jmatio_names);
        let unread = unread_by_jmatio()
            .into_iter()
            .enumerate()
            .map(|(at, variable)| {
                scratch.write(&format!("unread-{at}.mat"), &file(DESCRIPTION, &[variable]))
            })
            .collect::<Vec<_>>();
        let paths: Vec<&Path> = unread.iter().map(PathBuf::as_path).collect();
        assert_read_where_tika_fails(&paths, "#156");
        let failing = [scratch.write(
            "octave.mat",
            &file(
                "MATLAB 5.0 MAT-file, written by Octave",
                &[doubles("x", &[1, 1])],
            ),
        )];
        let paths: Vec<&Path> = failing.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&paths);
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_mat_files_as_tikas_path_does() {
        let (parsed, deferred) = documents(&MatParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika_rewriting(&paths, MatParser.tika_name(), as_jmatio_names);
    }
}
