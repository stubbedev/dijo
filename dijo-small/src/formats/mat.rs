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
use std::collections::{HashMap, HashSet};
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
/// The most bytes of values read from a compressed element at a time.
const VALUES: usize = 4 << 10;

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
        "dijo::MatParser"
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
/// which jmatio inflates into memory whole: the extraction fails past the limit.
fn too_large(reason: String) -> Decline<'static> {
    Decline::Fails(Error::LimitExceeded(format!(
        "{reason}, the most the native parser inflates"
    )))
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
            for _ in 0..array.size().max(0) {
                fields.read_element(buffer)?;
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
            Fields::read_names(buffer)?.read_element(buffer)?;
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
    let n = usize::try_from(array.n()).unwrap_or(0);
    place(&columns, n, rows, Tag::read(buffer)?.doubles(buffer)?)?;
    if array.is_complex() {
        place(&columns, n, rows, Tag::read(buffer)?.doubles(buffer)?)?;
    }
    Ok(())
}

/// Places `values` values by the starts of `n` columns among `rows` row indices, a column at a
/// time, failing at the first place either does not reach: the values before the rows.
fn place(columns: &[i32], n: usize, rows: usize, values: usize) -> Result<(), Thrown> {
    let reached = values.min(rows);
    let mut count = 0usize;
    for column in 0..n {
        let end = *columns
            .get(column + 1)
            .ok_or_else(|| out_of_bounds(column + 1, columns.len()))?;
        if to_i64(count) < i64::from(end) {
            if to_i64(reached) < i64::from(end) {
                let length = if reached >= values { values } else { rows };
                return Err(out_of_bounds(reached, length));
            }
            count = usize::try_from(end).expect("past a count");
        }
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

/// `MLStructure`'s fields: their names, the place of each name's field in an element, the
/// position of each name in the order first set, and each element's fields, in the order set, a
/// name set again keeping its place.
struct Fields {
    names: Vec<String>,
    places: Vec<usize>,
    keys: Vec<usize>,
    elements: Vec<Vec<Field>>,
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
        let mut keys = Vec::new();
        let mut first = HashMap::new();
        let places = names
            .iter()
            .enumerate()
            .map(|(at, name)| {
                *first.entry(name.as_str()).or_insert_with(|| {
                    keys.push(at);
                    keys.len() - 1
                })
            })
            .collect();
        Ok(Self {
            names,
            places,
            keys,
            elements: Vec::new(),
        })
    }

    /// The fields of the next element, each an array, an empty array where its element is
    /// empty, or a Java object's null, set as `setField(name, value, index)` sets them.
    fn read_element(&mut self, buffer: &mut Buffer) -> Result<(), Thrown> {
        let mut element: Vec<Field> = Vec::with_capacity(self.keys.len());
        for &place in &self.places {
            let tag = Tag::read(buffer)?;
            let value = if tag.size > 0 {
                read_matrix(buffer, tag.size)?
            } else {
                Field::Array(Array::empty())
            };
            if place < element.len() {
                element[place] = value;
            } else {
                element.push(value);
            }
        }
        self.elements.push(element);
        Ok(())
    }

    /// The names in the order first set: none before an element is set.
    fn keys(&self) -> impl Iterator<Item = &str> {
        let keys: &[usize] = if self.elements.is_empty() {
            &[]
        } else {
            self.keys.as_slice()
        };
        keys.iter().map(move |&at| self.names[at].as_str())
    }

    /// `getAllFields`: every element's fields, element by element, with their names.
    fn all(&self) -> impl Iterator<Item = (&str, &Field)> {
        self.elements
            .iter()
            .flat_map(move |element| self.keys().zip(element))
    }

    /// `MLStructure.contentToString` of the structure `array` these are the fields of, held in
    /// the field `name`: that name, then each field described, where it is one element, or its
    /// size and the names of its fields.
    fn content(&self, name: &str, array: &Array) -> String {
        let mut content = format!("{name} = \n");
        if array.size() == 1 {
            if let Some(element) = self.elements.last() {
                for (key, field) in self.keys().zip(element) {
                    content.push_str(&format!("\t{key} : {}\n", field.describe()));
                }
            }
        } else {
            content.push_str(&format!(
                "\n{}x{} struct array with fields: \n",
                array.m(),
                array.n()
            ));
            for key in self.keys() {
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

    /// Reads `count` values of `width` bytes, a piece at a time, handing the bytes of each to
    /// `value`, then throws, as reading them one at a time does, where the buffer does not hold
    /// them all.
    fn values(
        &mut self,
        count: usize,
        width: usize,
        mut value: impl FnMut(&[u8]),
    ) -> Result<(), Thrown> {
        let whole = count.min(self.remaining() / width);
        let length = whole * width;
        match &mut self.data {
            Data::Slice(data) => data[self.at..self.at + length]
                .chunks_exact(width)
                .for_each(&mut value),
            Data::Inflated(inflated) => {
                let mut piece = [0; VALUES];
                let most = VALUES / width * width;
                let mut done = 0;
                while done < length {
                    let size = (length - done).min(most);
                    inflated.copy(self.at + done, &mut piece[..size])?;
                    piece[..size].chunks_exact(width).for_each(&mut value);
                    done += size;
                }
            }
        }
        self.at += length;
        if whole < count {
            return Err(underflow());
        }
        Ok(())
    }

    fn int(&mut self) -> Result<i32, Thrown> {
        let bytes = self.array()?;
        Ok(if self.little_endian {
            i32::from_le_bytes(bytes)
        } else {
            i32::from_be_bytes(bytes)
        })
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
            _ => Err(unknown_type(kind)),
        }
    }
}

/// What a read of values throws on a data type it does not read.
fn unknown_type(kind: i32) -> Thrown {
    illegal_argument(format!("Unknown data type: {kind}"))
}

/// How `readToIntArray` and `readToByteArray` read a value of a data type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Numeric {
    Int8,
    UInt8,
    Int16,
    UInt16,
    Int32,
    Int64,
    Double,
}

impl Numeric {
    /// How `readToIntArray` reads a value of the data type `kind`, or what it throws on a type
    /// it does not read.
    fn of_ints(kind: i32) -> Result<Self, Thrown> {
        Ok(match kind {
            MI_UINT8 => Self::UInt8,
            MI_INT8 => Self::Int8,
            MI_UINT16 => Self::UInt16,
            MI_INT16 => Self::Int16,
            MI_UINT32 | MI_INT32 => Self::Int32,
            MI_UINT64 | MI_INT64 => Self::Int64,
            MI_DOUBLE => Self::Double,
            kind => return Err(unknown_type(kind)),
        })
    }

    /// How `readToByteArray` reads a value of the data type `kind`, its byte the low byte of
    /// [`Numeric::int`], or what it throws on a type it does not read.
    fn of_bytes(kind: i32) -> Result<Self, Thrown> {
        Ok(match kind {
            MI_UINT8 | MI_INT8 | MI_UTF8 => Self::UInt8,
            MI_UINT16 | MI_INT16 => Self::Int16,
            MI_UINT32 | MI_INT32 => Self::Int32,
            MI_DOUBLE => Self::Double,
            kind => return Err(unknown_type(kind)),
        })
    }

    /// The bytes of a value.
    fn width(self) -> usize {
        match self {
            Self::Int8 | Self::UInt8 => 1,
            Self::Int16 | Self::UInt16 => 2,
            Self::Int32 => 4,
            Self::Int64 | Self::Double => 8,
        }
    }

    /// The int `readToIntArray` reads of the [`Numeric::width`] bytes of a value.
    fn int(self, bytes: &[u8], little_endian: bool) -> i32 {
        match self {
            Self::UInt8 => i32::from(bytes[0]),
            Self::Int8 => i32::from(bytes[0].cast_signed()),
            Self::UInt16 => i32::from(u16::from_le_bytes(ordered(bytes, little_endian))),
            Self::Int16 => i32::from(i16::from_le_bytes(ordered(bytes, little_endian))),
            Self::Int32 => i32::from_le_bytes(ordered(bytes, little_endian)),
            Self::Int64 => i64::from_le_bytes(ordered(bytes, little_endian)) as i32,
            Self::Double => {
                f64::from_bits(u64::from_le_bytes(ordered(bytes, little_endian))) as i32
            }
        }
    }
}

/// The `N` bytes of a value in little-endian order.
fn ordered<const N: usize>(bytes: &[u8], little_endian: bool) -> [u8; N] {
    let mut ordered: [u8; N] = bytes.try_into().expect("the bytes of a value");
    if !little_endian {
        ordered.reverse();
    }
    ordered
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
        if elements > 0 {
            let numeric = Numeric::of_ints(self.kind)?;
            let little_endian = buffer.little_endian;
            buffer.values(elements, numeric.width(), |bytes| {
                values.push(numeric.int(bytes, little_endian));
            })?;
        }
        buffer.skip(self.padding)?;
        Ok(values)
    }

    /// `readToIntArray`, of which only how many values it reads matters: they are passed over.
    fn pass_ints(&self, buffer: &mut Buffer) -> Result<usize, Thrown> {
        let elements = self.elements()?;
        if elements > 0 {
            buffer.pass(elements * Numeric::of_ints(self.kind)?.width())?;
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
        let mut bytes = Vec::with_capacity(self.elements()?.min(buffer.remaining()));
        self.bytes(buffer, |byte| bytes.push(byte))?;
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
        self.bytes(buffer, |byte| {
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
        })?;
        buffer.skip(self.padding)?;
        Ok(length + java::utf16_length(&java::utf8(&piece)))
    }

    /// The bytes `readToByteArray` reads, a byte of each value, handed to `byte` in turn.
    fn bytes(&self, buffer: &mut Buffer, mut byte: impl FnMut(u8)) -> Result<(), Thrown> {
        let elements = self.elements()?;
        if elements == 0 {
            return Ok(());
        }
        let numeric = Numeric::of_bytes(self.kind)?;
        let little_endian = buffer.little_endian;
        buffer.values(elements, numeric.width(), |bytes| {
            byte(numeric.int(bytes, little_endian) as u8);
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

#[cfg(test)]
mod tests {

    use super::*;
    use crate::compress::zlib::tests::zlib;

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
        let mut body = field_names(names);
        for field in fields {
            body.extend_from_slice(field);
        }
        matrix(MX_STRUCT, 0, dims, name, &body)
    }

    /// The names of a structure's fields as MATLAB writes them: the longest name's length, then
    /// the names, each padded to it.
    fn field_names(names: &[&str]) -> Vec<u8> {
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
        body
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
    /// compressed array that inflates past it, then a last variable. The data is passed over
    /// in place and as it inflates, under a budget far below the file.
    #[test]
    fn describes_variables_of_files_too_large_to_read_whole() {
        use dijo_core::large::{LargeFile, BUDGET};

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
        let packed = compressed(&inflated);
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
            out.write_all(&packed)?;
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

    /// What `readToIntArray`, or `readToByteArray` where `bytes`, reads value by value of
    /// `elements` values of `kind` in `data`, as ints, and how far it reads.
    fn one_at_a_time(
        kind: i32,
        bytes: bool,
        elements: usize,
        data: &[u8],
        little_endian: bool,
    ) -> (Result<Vec<i32>, Thrown>, usize) {
        let mut values = Vec::new();
        let mut at = 0;
        for _ in 0..elements {
            let width = match (kind, bytes) {
                (MI_UINT8 | MI_INT8, _) | (MI_UTF8, true) => 1,
                (MI_UINT16 | MI_INT16, _) => 2,
                (MI_UINT32 | MI_INT32, _) => 4,
                (MI_UINT64 | MI_INT64, false) | (MI_DOUBLE, _) => 8,
                _ => return (Err(unknown_type(kind)), at),
            };
            let Some(value) = data.get(at..at + width) else {
                return (Err(underflow()), at);
            };
            at += width;
            let short = || {
                let value = value.try_into().unwrap();
                if little_endian {
                    i16::from_le_bytes(value)
                } else {
                    i16::from_be_bytes(value)
                }
            };
            let int = || {
                let value = value.try_into().unwrap();
                if little_endian {
                    i32::from_le_bytes(value)
                } else {
                    i32::from_be_bytes(value)
                }
            };
            let long = || {
                let value = value.try_into().unwrap();
                if little_endian {
                    i64::from_le_bytes(value)
                } else {
                    i64::from_be_bytes(value)
                }
            };
            let double = || f64::from_bits(long().cast_unsigned());
            values.push(if bytes {
                i32::from(match kind {
                    MI_UINT8 | MI_INT8 | MI_UTF8 => value[0],
                    MI_UINT16 | MI_INT16 => short() as u8,
                    MI_UINT32 | MI_INT32 => int() as u8,
                    _ => double() as i32 as u8,
                })
            } else {
                match kind {
                    MI_UINT8 => i32::from(value[0]),
                    MI_INT8 => i32::from(value[0].cast_signed()),
                    MI_UINT16 => i32::from(short().cast_unsigned()),
                    MI_INT16 => i32::from(short()),
                    MI_UINT32 | MI_INT32 => int(),
                    MI_UINT64 | MI_INT64 => long() as i32,
                    _ => double() as i32,
                }
            });
        }
        (Ok(values), at)
    }

    /// The ints and bytes read a piece at a time are those read a value at a time, as are how
    /// far the reads go and what they throw, from data in place and inflated alike.
    #[test]
    fn reads_values_a_piece_at_a_time_as_one_at_a_time() {
        const START: usize = 3;
        let kinds = [
            MI_INT8, MI_UINT8, MI_INT16, MI_UINT16, MI_INT32, MI_UINT32, MI_INT64, MI_UINT64,
            MI_DOUBLE, MI_UTF8, MI_SINGLE, 99,
        ];
        for length in [0, 1, 7, 8, 9, 2 * VALUES + 5] {
            let data: Vec<u8> = (0..START + length)
                .map(|at| (at * 37 % 256) as u8)
                .collect();
            let compressed = zlib(&data);
            for inflated in [false, true] {
                for little_endian in [true, false] {
                    for kind in kinds {
                        for elements in [0, 1, 2, 5, 1100, 2100] {
                            for bytes in [false, true] {
                                let mut buffer = Buffer {
                                    data: if inflated {
                                        Data::Inflated(Inflated::new(Cow::Borrowed(
                                            compressed.as_slice(),
                                        )))
                                    } else {
                                        Data::Slice(&data)
                                    },
                                    length: data.len(),
                                    at: START,
                                    little_endian,
                                };
                                let tag = Tag {
                                    kind,
                                    size: i32::try_from(elements).unwrap() * size_of(kind),
                                    padding: 0,
                                };
                                let read = if bytes {
                                    let mut read = Vec::new();
                                    tag.bytes(&mut buffer, |byte| read.push(i32::from(byte)))
                                        .map(|()| read)
                                } else {
                                    tag.ints(&mut buffer)
                                };
                                let expected = one_at_a_time(
                                    kind,
                                    bytes,
                                    elements,
                                    &data[START..],
                                    little_endian,
                                );
                                let case = (length, inflated, little_endian, kind, elements, bytes);
                                assert_eq!((read, buffer.at - START), expected, "{case:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// What placing the values of a sparse array one at a time gives.
    fn place_one_at_a_time(
        columns: &[i32],
        n: usize,
        rows: usize,
        values: usize,
    ) -> Result<(), Thrown> {
        let mut count = 0usize;
        for column in 0..n {
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
    }

    /// A sparse array's values placed a column at a time fail or not as one at a time, at the
    /// same place, whichever of the values, rows and column starts runs short.
    #[test]
    fn places_sparse_values_a_column_at_a_time_as_one_at_a_time() {
        let starts: [&[i32]; 7] = [
            &[],
            &[0],
            &[0, 2, 5],
            &[0, 0, 0, 3],
            &[0, 4, 2, 6],
            &[0, -3, 1, -1, 7],
            &[5, 1, 1, 9, 9, 12],
        ];
        for columns in starts {
            for n in 0..=columns.len() + 1 {
                for rows in 0..14 {
                    for values in 0..14 {
                        assert_eq!(
                            place(columns, n, rows, values),
                            place_one_at_a_time(columns, n, rows, values),
                            "{columns:?} {n} {rows} {values}"
                        );
                    }
                }
            }
        }
    }

    /// Each element's fields by name, described.
    type Set = Vec<Vec<(String, String)>>;

    /// The names first set and each element's named fields, as `setField` sets the fields
    /// described `elements`, each element's in the order of `names`, one at a time.
    fn set_one_at_a_time(names: &[&str], elements: &[Vec<String>]) -> (Vec<String>, Set, usize) {
        let mut keys: Vec<String> = Vec::new();
        let mut set: Set = Vec::new();
        let mut current = 0;
        for (index, values) in elements.iter().enumerate() {
            for (name, value) in names.iter().zip(values) {
                if !keys.iter().any(|key| key == name) {
                    keys.push((*name).to_owned());
                }
                current = index;
                if set.len() <= index {
                    set.push(Vec::new());
                }
                let element = &mut set[index];
                match element.iter_mut().find(|(key, _)| key == name) {
                    Some((_, field)) => field.clone_from(value),
                    None => element.push(((*name).to_owned(), value.clone())),
                }
            }
        }
        (keys, set, current)
    }

    /// A structure's fields set by position are those set one at a time by name, where names
    /// repeat too, as are its content and the names it lists, whatever its size.
    #[test]
    fn sets_fields_by_position_as_by_name() {
        let cases: [&[&str]; 6] = [
            &[],
            &["a"],
            &["a", "b"],
            &["a", "b", "a"],
            &["x", "x", "x"],
            &["b", "a", "b", "c", "a"],
        ];
        for names in cases {
            for count in 0..4 {
                let mut bytes = field_names(names);
                let mut described = Vec::new();
                for index in 0..count {
                    let mut values = Vec::new();
                    for at in 0..names.len() {
                        let columns = i32::try_from(index * names.len() + at + 1).unwrap();
                        bytes.extend(doubles("", &[1, columns]));
                        values.push(format!("[1x{columns}  double array]"));
                    }
                    described.push(values);
                }
                let mut buffer = Buffer {
                    data: Data::Slice(&bytes),
                    length: bytes.len(),
                    at: 0,
                    little_endian: true,
                };
                let mut fields = Fields::read_names(&mut buffer).unwrap();
                for _ in 0..count {
                    fields.read_element(&mut buffer).unwrap();
                }
                let (keys, set, current) = set_one_at_a_time(names, &described);
                let all: Vec<(String, String)> = fields
                    .all()
                    .map(|(name, field)| (name.to_owned(), field.describe()))
                    .collect();
                let expected: Vec<(String, String)> = set.iter().flatten().cloned().collect();
                assert_eq!(all, expected, "{names:?} {count}");
                let one = format!(
                    "s = \n{}",
                    keys.iter()
                        .map(|key| {
                            let field = set
                                .get(current)
                                .and_then(|element| element.iter().find(|(name, _)| name == key));
                            let described = field.map_or("null", |(_, field)| field.as_str());
                            format!("\t{key} : {described}\n")
                        })
                        .collect::<String>()
                );
                let many = format!(
                    "s = \n\n2x3 struct array with fields: \n{}",
                    keys.iter()
                        .map(|key| format!("\t{key}\n"))
                        .collect::<String>()
                );
                for (dims, expected) in [(vec![1, 1], one), (vec![2, 3], many)] {
                    let array = Array {
                        name: "s".to_owned(),
                        dims,
                        class: MX_STRUCT,
                        attributes: 0,
                        fields: None,
                    };
                    assert_eq!(fields.content("s", &array), expected, "{names:?} {count}");
                }
            }
        }
    }
}
