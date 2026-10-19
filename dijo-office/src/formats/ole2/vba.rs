//! POI's `VBAMacroReader` as Tika's `OfficeParser.extractMacros` reads it: the VBA projects of
//! a compound document, each a `VBA` storage anywhere in it, their `dir` streams decompressed
//! as MS-OVBA compresses them (POI's `RLEDecompressingInputStream`) and read for the modules'
//! streams and where their source starts, and each module's source decompressed from there; the
//! modules handed on, each an embedded document of its source.
//!
//! What POI reads is read as it reads it, up to the exceptions it throws, which fail the reading
//! of the macros. Where Tika writes the macros otherwise than the native reading, the
//! reading names the difference: modules in the order the projects name them, where Tika writes
//! them in the order of a `java.util.HashMap`; a stream of a `VBA` storage no project names as a
//! module, which POI hands on undecompressed as if it were a module's source; a project in code
//! page 1256, which POI reads in code page 1255; text whose malformed bytes the native decoders
//! may replace otherwise than Java's; and macros POI fails to read, which Tika records in an
//! entry of no content (#186).

use std::collections::HashMap;

use crate::config::ParserConfigs;
use crate::formats::embedded::{ResourceType, Wrapping, EXCEPTION_WARNING, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{noted, unless_different, Decline, Difference, Failure, Prepared};
use crate::java::{equals_ignore_case, is_blank, split, JavaHashMap};
use crate::ole2::{EntryId, EntryKind, Ole2, Unreadable};
use crate::text::charset::Charset;

use super::codepage;

/// The size of a decompressed chunk, `RLEDecompressingInputStream`'s buffer.
const CHUNK: usize = 4096;
/// `IOUtils.SKIP_BUFFER_SIZE`.
const SKIP_BUFFER: usize = 2048;
/// `VBAMacroReader.MAX_STRING_LENGTH`.
const MAX_STRING_LENGTH: i32 = 20_000;
const CONTENT_TYPE: &str = "Content-Type";
/// The content type Tika gives a module's source.
const VBASIC: &str = "text/x-vbasic";
/// The code page POI reads in code page 1255 (`codepageToEncoding(1256, true)`).
const ARABIC: i32 = 1256;

/// A Java exception reading macros, of the kinds `VBAMacroReader` tells apart, with what it
/// says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Thrown {
    IllegalArgument(String),
    IllegalState(String),
    Io(String),
    /// Any other, such as an `ArrayIndexOutOfBoundsException` or a `NullPointerException`.
    Runtime(String),
}

impl Thrown {
    fn message(&self) -> &str {
        match self {
            Thrown::IllegalArgument(message)
            | Thrown::IllegalState(message)
            | Thrown::Io(message)
            | Thrown::Runtime(message) => message,
        }
    }
}

type Read<T> = Result<T, Thrown>;

fn out_of_bounds(index: i64) -> Thrown {
    Thrown::Runtime(format!(
        "java.lang.ArrayIndexOutOfBoundsException: Index {index} out of bounds for length {CHUNK}"
    ))
}

/// A Java `InputStream`, as the macro reader reads one.
trait JavaInput {
    /// `read()`: the next byte, or -1 at the end.
    fn read_byte(&mut self) -> Read<i32>;

    /// `read(b, 0, b.length)`: how many bytes were read into `out`, or -1 at the end.
    fn read_into(&mut self, out: &mut [u8]) -> Read<i32>;

    /// `readShort` of `RLEDecompressingInputStream`: two bytes little-endian, unsigned, or -1
    /// where either is past the end.
    fn read_short(&mut self) -> Read<i32> {
        let low = self.read_byte()?;
        if low == -1 {
            return Ok(-1);
        }
        let high = self.read_byte()?;
        if high == -1 {
            return Ok(-1);
        }
        Ok(low | (high << 8))
    }

    /// `readInt` of `RLEDecompressingInputStream`: four bytes little-endian, or -1 where any is
    /// past the end.
    fn read_int(&mut self) -> Read<i32> {
        let mut value = 0u32;
        for shift in [0, 8, 16, 24] {
            let byte = self.read_byte()?;
            if byte == -1 {
                return Ok(-1);
            }
            value |= (byte as u32) << shift;
        }
        Ok(value as i32)
    }

    /// `IOUtils.readFully(in, b)`: bytes read into `out` until it is full or the input ends,
    /// how many, or -1 where it ended before any.
    fn read_fully(&mut self, out: &mut [u8]) -> Read<i32> {
        let mut total = 0;
        loop {
            let got = self.read_into(&mut out[total..])?;
            if got < 0 {
                return Ok(if total == 0 { -1 } else { total as i32 });
            }
            total += got as usize;
            if total == out.len() {
                return Ok(total as i32);
            }
        }
    }

    /// `IOUtils.skipFully`: how many bytes were skipped, -1 where none were.
    fn skip_fully(&mut self, count: i64) -> Read<i64> {
        if count < 0 {
            return Err(Thrown::IllegalArgument(format!(
                "Skip count must be non-negative, actual: {count}"
            )));
        }
        if count == 0 {
            return Ok(0);
        }
        let mut buffer = [0; SKIP_BUFFER];
        let mut remain = count;
        while remain > 0 {
            let wanted = remain.min(SKIP_BUFFER as i64) as usize;
            let got = self.read_into(&mut buffer[..wanted])?;
            if got < 0 {
                break;
            }
            remain -= i64::from(got);
        }
        Ok(if remain == count { -1 } else { count - remain })
    }

    /// `VBAMacroReader.trySkip`: `count` bytes skipped, or an `IOException` where fewer were.
    fn try_skip(&mut self, count: i64) -> Read<()> {
        let skipped = self.skip_fully(count)?;
        if skipped != count {
            return Err(Thrown::Io(format!(
                "Tried skipping {count} bytes, but only {skipped} bytes were skipped"
            )));
        }
        Ok(())
    }

    /// `IOUtils.toByteArray`: the rest of the input.
    fn to_end(&mut self) -> Read<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0; CHUNK];
        loop {
            let got = self.read_into(&mut buffer)?;
            if got < 0 {
                return Ok(bytes);
            }
            bytes.extend_from_slice(&buffer[..got as usize]);
        }
    }
}

/// A stream over bytes, as `ByteArrayInputStream` and `DocumentInputStream` read them.
struct Bytes<'d> {
    data: &'d [u8],
    at: usize,
}

impl<'d> Bytes<'d> {
    fn new(data: &'d [u8]) -> Self {
        Self { data, at: 0 }
    }
}

impl JavaInput for Bytes<'_> {
    fn read_byte(&mut self) -> Read<i32> {
        let Some(&byte) = self.data.get(self.at) else {
            return Ok(-1);
        };
        self.at += 1;
        Ok(i32::from(byte))
    }

    fn read_into(&mut self, out: &mut [u8]) -> Read<i32> {
        let rest = &self.data[self.at..];
        if rest.is_empty() {
            return Ok(-1);
        }
        let count = rest.len().min(out.len());
        out[..count].copy_from_slice(&rest[..count]);
        self.at += count;
        Ok(count as i32)
    }
}

/// `RLEDecompressingInputStream`: MS-OVBA's decompression of `input`, a chunk at a time.
struct Rle<I> {
    input: I,
    buf: [u8; CHUNK],
    pos: usize,
    /// How many bytes the chunk read last holds; `None` past the end.
    len: Option<usize>,
}

impl<I: JavaInput> Rle<I> {
    /// The decompression of `input`, which starts with the signature byte 1, its first chunk
    /// read.
    fn new(mut input: I) -> Read<Self> {
        let header = input.read_byte()?;
        if header != 0x01 {
            return Err(Thrown::IllegalArgument(format!(
                "Header byte 0x01 expected, received 0x{:02X}",
                header & 0xFF
            )));
        }
        let mut rle = Self {
            input,
            buf: [0; CHUNK],
            pos: 0,
            len: None,
        };
        rle.len = rle.read_chunk()?;
        Ok(rle)
    }

    /// `readChunk`: the next chunk decompressed into the buffer, and how many bytes it holds;
    /// `None` where the input ends, at the chunk's header or within a literal or a copy token.
    fn read_chunk(&mut self) -> Read<Option<usize>> {
        self.pos = 0;
        let header = self.input.read_short()?;
        if header == -1 || header == 0 {
            return Ok(None);
        }
        let size = ((header & 0x0FFF) + 1) as usize;
        if header & 0x7000 != 0x3000 {
            return Err(Thrown::IllegalArgument(format!(
                "Chunksize header A should be 0x3000, received 0x{:04X}",
                header & 0xE000
            )));
        }
        if header & 0x8000 == 0 {
            if self.input.read_fully(&mut self.buf[..size])? < size as i32 {
                return Err(Thrown::IllegalState(format!(
                    "Not enough bytes read, expected {size}"
                )));
            }
            return Ok(Some(size));
        }
        let mut in_offset = 0;
        let mut out_offset = 0usize;
        while in_offset < size {
            let flags = self.input.read_byte()?;
            in_offset += 1;
            if flags == -1 {
                break;
            }
            for bit in 0..8 {
                if in_offset >= size {
                    break;
                }
                if flags & (1 << bit) == 0 {
                    let byte = self.input.read_byte()?;
                    if byte == -1 {
                        return Ok(None);
                    }
                    *self
                        .buf
                        .get_mut(out_offset)
                        .ok_or_else(|| out_of_bounds(out_offset as i64))? = byte as u8;
                    out_offset += 1;
                    in_offset += 1;
                } else {
                    let token = self.input.read_short()?;
                    if token == -1 {
                        return Ok(None);
                    }
                    in_offset += 2;
                    let length_bits = copy_length_bits(out_offset as i32 - 1);
                    let copy_offset = (token >> length_bits) + 1;
                    let copy_length = (token & ((1 << length_bits) - 1)) + 3;
                    let start = out_offset as i64 - i64::from(copy_offset);
                    for from in start..start + i64::from(copy_length) {
                        let byte = usize::try_from(from)
                            .ok()
                            .and_then(|from| self.buf.get(from).copied())
                            .ok_or_else(|| out_of_bounds(from))?;
                        *self
                            .buf
                            .get_mut(out_offset)
                            .ok_or_else(|| out_of_bounds(out_offset as i64))? = byte;
                        out_offset += 1;
                    }
                }
            }
        }
        Ok(Some(out_offset))
    }
}

/// `getCopyLenBits`: how many bits of a copy token give the length, after `offset` bytes of
/// the chunk less one.
fn copy_length_bits(offset: i32) -> i32 {
    (4..=11)
        .rev()
        .find(|&bit| offset & (1 << bit) != 0)
        .map_or(12, |bit| 15 - bit)
}

impl<I: JavaInput> JavaInput for Rle<I> {
    /// `read()`. After a chunk of no bytes, it gives the byte the buffer holds first, as POI's
    /// does.
    fn read_byte(&mut self) -> Read<i32> {
        let Some(len) = self.len else {
            return Ok(-1);
        };
        if self.pos >= len {
            self.len = self.read_chunk()?;
            if self.len.is_none() {
                return Ok(-1);
            }
        }
        let byte = self.buf[self.pos];
        self.pos += 1;
        Ok(i32::from(byte))
    }

    fn read_into(&mut self, out: &mut [u8]) -> Read<i32> {
        if self.len.is_none() {
            return Ok(-1);
        }
        let mut offset = 0;
        while offset < out.len() {
            let len = match self.len {
                Some(len) if self.pos < len => len,
                _ => match self.read_chunk()? {
                    Some(len) => {
                        self.len = Some(len);
                        continue;
                    }
                    None => {
                        self.len = None;
                        return Ok(if offset > 0 { offset as i32 } else { -1 });
                    }
                },
            };
            let count = (out.len() - offset).min(len - self.pos);
            out[offset..offset + count].copy_from_slice(&self.buf[self.pos..self.pos + count]);
            self.pos += count;
            offset += count;
        }
        Ok(out.len() as i32)
    }
}

/// `RLEDecompressingInputStream.decompress` of `compressed`, read to its end.
fn decompress(compressed: &[u8]) -> Read<Vec<u8>> {
    Rle::new(Bytes::new(compressed))?.to_end()
}

/// What `extractMacros` hands on of a compound document's macros: each module's name and
/// source, in the order the projects name them, or why POI fails to read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Macros {
    read: Result<Vec<Module>, String>,
    /// Where Tika hands them on otherwise, if it does.
    difference: Option<Difference>,
}

/// A module of a VBA project: the name of its stream and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Module {
    name: String,
    source: String,
}

impl Macros {
    /// The macros of `file`, where `configs` asks for them, as `OfficeParser.extractMacros`
    /// reads them from the whole compound document.
    pub(crate) fn of(file: &Ole2<'_>, configs: &ParserConfigs) -> Option<Macros> {
        configs.office.extracts_macros().then(|| Self::read(file))
    }

    /// The macros of `file`, as `OfficeParser.extractMacros` reads them from the whole compound
    /// document.
    pub(crate) fn read(file: &Ole2<'_>) -> Macros {
        match read_macros(file) {
            Ok((modules, differences)) => Macros {
                read: Ok(modules),
                difference: (!differences.is_empty()).then(|| {
                    Difference(format!(
                        "Tika hands on the macros otherwise: {}",
                        differences.join("; ")
                    ))
                }),
            },
            Err(thrown) => {
                let reason = thrown.message().to_owned();
                Macros {
                    difference: Some(Difference(format!(
                        "POI fails to read the macros, which Tika records in an entry of no \
                         content: {reason}"
                    ))),
                    read: Err(reason),
                }
            }
        }
    }

    /// Where Tika hands the macros on otherwise, if it does.
    pub(crate) fn difference(&self) -> Option<Difference> {
        self.difference.clone()
    }

    /// The reading a parser of a compound document `prepared`, with `macros`, if any, written
    /// after it, as `OfficeParser.parse` extracts them once its parser is done; and different
    /// from Tika's where either is. A storage of a compound document, which Tika hands its
    /// parser as an open container, has its macros handed on by the document it is stored in,
    /// and none of its own (TIKA-4116).
    pub(crate) fn followed_by<'a>(
        prepared: Result<Prepared<'a>, Decline<'a>>,
        macros: Option<Macros>,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let Some(macros) = macros else {
            return prepared;
        };
        let (reading, difference) = noted(prepared)?;
        let difference = difference.or_else(|| macros.difference.clone());
        unless_different(
            Box::new(move |xhtml| {
                reading(xhtml)?;
                if xhtml.is_storage() {
                    return Ok(());
                }
                macros.write(xhtml)
            }),
            difference,
        )
    }

    /// Hands on each module as `extractMacros` does: an embedded document of its source in
    /// UTF-8, a `MACRO` of `text/x-vbasic` named by its stream. Macros POI fails to read are
    /// recorded as a warning.
    pub(crate) fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        match self.read {
            Ok(modules) => {
                for Module { name, source } in modules {
                    let mut metadata = TikaMetadata::new();
                    ResourceType::Macro.set(&mut metadata);
                    metadata.set(CONTENT_TYPE, VBASIC);
                    if !is_blank(&name) {
                        metadata.set(RESOURCE_NAME, name);
                    }
                    xhtml.parse_embedded(source.as_bytes(), &metadata, Wrapping::PackageEntry)?;
                }
            }
            Err(reason) => xhtml.metadata_mut().add(
                EXCEPTION_WARNING,
                format!("the macros cannot be read: {reason}"),
            ),
        }
        Ok(())
    }
}

/// A module as POI's `ModuleMap` holds it while it reads the projects: where its source starts
/// in its stream, once a project names it, and its stream's bytes, decompressed once that is
/// known.
#[derive(Debug, Default)]
struct ModuleImpl {
    offset: Option<i32>,
    buf: Option<Vec<u8>>,
}

/// `VBAMacroReader.ModuleMap`: the modules by the names of their streams, in the order they were
/// first met, and the charset of the projects, the one the last names.
struct ModuleMap {
    modules: Vec<(String, ModuleImpl)>,
    /// Where each name's module is in `modules`.
    index: HashMap<String, usize>,
    charset: Charset,
    /// The code page the last project names.
    code_page: Option<i32>,
    /// What of the projects' text the native decoders may decode otherwise than Java's.
    inexact: bool,
}

impl ModuleMap {
    fn get_mut(&mut self, name: &str) -> Option<&mut ModuleImpl> {
        let &at = self.index.get(name)?;
        Some(&mut self.modules[at].1)
    }

    /// `put` of a module of a name not met before.
    fn insert(&mut self, name: String, module: ModuleImpl) {
        self.index.insert(name.clone(), self.modules.len());
        self.modules.push((name, module));
    }

    /// `new String(bytes, charset)`, noting where the native decoder may differ from Java's.
    fn decode(&mut self, bytes: &[u8]) -> Read<String> {
        let exact = self.charset.decode_exactly(bytes);
        self.inexact |= exact.is_none();
        match exact.or_else(|| self.charset.decode(bytes)) {
            Some(text) => Ok(text),
            None => Err(Thrown::Runtime(format!(
                "text in {}, which the native decoders do not decode",
                self.charset.name()
            ))),
        }
    }
}

/// `readMacros` of the whole compound document: the modules in the order the projects name
/// them, and where Tika hands them on otherwise; or the exception POI throws.
fn read_macros(file: &Ole2<'_>) -> Read<(Vec<Module>, Vec<String>)> {
    let mut modules = ModuleMap {
        modules: Vec::new(),
        index: HashMap::new(),
        charset: Charset::named("windows-1252"),
        code_page: None,
        inexact: false,
    };
    find_macros(file, file.root(), &mut modules)?;
    find_named(file, file.root(), "projectwm", &mut |_| Ok(()))?;
    let charset = modules.charset;
    find_named(file, file.root(), "project", &mut |bytes| {
        let properties = charset.decode(bytes).ok_or_else(|| {
            Thrown::Runtime(format!(
                "a PROJECT stream in {}, which the native decoders do not decode",
                charset.name()
            ))
        })?;
        check_project_properties(&properties)
    })?;
    let mut differences = Vec::new();
    let mut read = Vec::with_capacity(modules.modules.len());
    let mut names = JavaHashMap::default();
    let named: Vec<(String, ModuleImpl)> = std::mem::take(&mut modules.modules);
    for (name, module) in named {
        let Some(buf) = module.buf else {
            return Err(Thrown::Runtime(format!(
                "java.lang.NullPointerException: the module {name} has no stream"
            )));
        };
        names.insert(&name, ());
        if module.offset.is_none() {
            differences.push(format!(
                "the stream {name}, which no project names as a module, which POI hands on \
                 undecompressed and the port leaves out"
            ));
            continue;
        }
        let source = modules.decode(&buf)?;
        read.push(Module { name, source });
    }
    if modules.code_page == Some(ARABIC) {
        differences.push("a project in code page 1256, which POI reads in code page 1255".into());
    }
    if modules.inexact {
        differences.push(format!(
            "text in {} with malformed bytes, which the native decoders may replace otherwise \
             than Java's",
            modules.charset.name()
        ));
    }
    if !in_java_order(&names, &read) {
        differences.push(
            "modules in the order of a java.util.HashMap, which the port hands on in the order \
             the projects name them"
                .into(),
        );
    }
    Ok((read, differences))
}

/// Whether Tika hands on the modules `read` in their order: as it iterates the `HashMap` that
/// `readMacros` fills from the one `readMacroModules` fills from the `ModuleMap` of `names`.
fn in_java_order(names: &JavaHashMap<()>, read: &[Module]) -> bool {
    let put_one_by_one = |map: &JavaHashMap<()>| {
        let mut copy = JavaHashMap::default();
        for (name, ()) in map.iter() {
            copy.insert(name, ());
        }
        copy
    };
    put_one_by_one(&put_one_by_one(names))
        .iter()
        .map(|(name, _)| name)
        .filter(|name| read.iter().any(|module| module.name == *name))
        .eq(read.iter().map(|module| module.name.as_str()))
}

/// `findMacros`: each `VBA` storage below `directory`, the directory itself first.
fn find_macros(file: &Ole2<'_>, directory: EntryId, modules: &mut ModuleMap) -> Read<()> {
    if equals_ignore_case("VBA", &file.name(directory)) {
        return read_project(file, directory, modules);
    }
    for child in file.children(directory) {
        if file.kind(child) != EntryKind::Document {
            find_macros(file, child, modules)?;
        }
    }
    Ok(())
}

/// `findModuleNameMap` and `findProjectProperties`: each document named `name` in any case
/// below `directory`, the first of each directory, handed to `read`; an entry of that name that
/// is a directory fails the cast to a document.
fn find_named(
    file: &Ole2<'_>,
    directory: EntryId,
    name: &str,
    read: &mut dyn FnMut(&[u8]) -> Read<()>,
) -> Read<()> {
    for child in file.children(directory) {
        if equals_ignore_case(name, &file.name(child)) {
            return read(&document(file, child)?);
        }
        if file.kind(child) != EntryKind::Document {
            find_named(file, child, name, read)?;
        }
    }
    Ok(())
}

/// The bytes of the document `entry`, as `(DocumentNode) entry` and a `DocumentInputStream`
/// read them.
fn document(file: &Ole2<'_>, entry: EntryId) -> Read<Vec<u8>> {
    if file.kind(entry) != EntryKind::Document {
        return Err(Thrown::Runtime(format!(
            "java.lang.ClassCastException: the entry {} is no document",
            file.name(entry)
        )));
    }
    file.read(entry).map_err(|Unreadable(reason)| {
        Thrown::Io(format!(
            "the stream {} cannot be read: {reason}",
            file.name(entry)
        ))
    })
}

/// `readProjectProperties` as far as it throws: a `Document` line whose module lacks the `/&H`
/// before its cookie, which `substring` throws on.
fn check_project_properties(properties: &str) -> Read<()> {
    for line in split_lines(properties) {
        if line.starts_with('[') {
            continue;
        }
        let tokens = split(line, '=');
        let [key, value, ..] = tokens.as_slice() else {
            continue;
        };
        let value = match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            Some(unquoted) if value.chars().count() > 1 => unquoted,
            _ => value,
        };
        if *key == "Document" && !value.contains("/&H") {
            return Err(Thrown::Runtime(format!(
                "java.lang.StringIndexOutOfBoundsException: the document module {value} has no \
                 cookie"
            )));
        }
    }
    Ok(())
}

/// `String.split("\r\n|\n\r")`: the lines, the empty ones at the end left out.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut at = 0;
    let bytes = text.as_bytes();
    while at + 1 < bytes.len() {
        if matches!(&bytes[at..at + 2], b"\r\n" | b"\n\r") {
            lines.push(&text[start..at]);
            at += 2;
            start = at;
        } else {
            at += 1;
        }
    }
    lines.push(&text[start..]);
    while lines.len() > 1 && lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

/// `VBAMacroReader.readMacros(macroDir, modules)`: the project's `dir` stream, then each stream
/// of the storage but `dir`, `__SRP` and `_VBA_PROJECT`.
fn read_project(file: &Ole2<'_>, directory: EntryId, modules: &mut ModuleMap) -> Read<()> {
    let found = file
        .children(directory)
        .find(|&child| equals_ignore_case("dir", &file.name(child)));
    if let Some(found) = found {
        let dir = file
            .child_ignoring_case(directory, &file.name(found))
            .expect("the dir stream is in the storage");
        read_dir_stream(&document(file, dir)?, modules)?;
    }
    for child in file.children(directory) {
        if file.kind(child) != EntryKind::Document {
            continue;
        }
        let name = file.name(child);
        if equals_ignore_case("dir", &name)
            || starts_with_ignoring_case(&name, "__SRP")
            || starts_with_ignoring_case(&name, "_VBA_PROJECT")
        {
            continue;
        }
        read_module_stream(file, child, name, modules)?;
    }
    Ok(())
}

/// `StringUtil.startsWithIgnoreCase`.
fn starts_with_ignoring_case(text: &str, prefix: &str) -> bool {
    let start: String = text.chars().take(prefix.chars().count()).collect();
    equals_ignore_case(&start, prefix)
}

/// A string `readStringPair` reads, and, where the reserved id after it is not the one
/// expected, that id.
struct StringPair {
    ascii: String,
    pushback: Option<i32>,
}

/// `processDirStream`: the records of a project's `dir` stream, decompressed: the code page,
/// each module's stream name, and where its source starts in its stream.
fn read_dir_stream(compressed: &[u8], modules: &mut ModuleMap) -> Read<()> {
    let mut input = Rle::new(Bytes::new(compressed))?;
    let mut stream_name: Option<String> = None;
    let mut record = 0;
    let result = (|| -> Read<()> {
        loop {
            record = input.read_short()?;
            match record {
                -1 | 0x10 => return Ok(()),
                0x09 => input.try_skip(10)?,
                0x03 => {
                    input.read_int()?;
                    let code_page = input.read_short()?;
                    modules.charset = codepage::charset(code_page).map_err(|_| {
                        Thrown::Runtime(format!(
                            "java.nio.charset.UnsupportedCharsetException: code page {code_page}"
                        ))
                    })?;
                    modules.code_page = Some(code_page);
                }
                0x1A => {
                    stream_name = Some(read_string_pair(&mut input, modules, 0x32, true)?.ascii);
                }
                0x05 => {
                    read_string_pair(&mut input, modules, 0x40, true)?;
                }
                0x06 => {
                    read_string_pair(&mut input, modules, 0x3D, true)?;
                }
                0x0C => {
                    read_string_pair(&mut input, modules, 0x3C, true)?;
                }
                0x16 => match read_string_pair(&mut input, modules, 0x3E, false)?.pushback {
                    None | Some(-1) => {}
                    Some(0x0D) => {
                        let length = input.read_int()?;
                        input.try_skip(i64::from(length))?;
                    }
                    Some(other) => {
                        return Err(Thrown::IllegalArgument(format!(
                            "Unexpected reserved character. Expected 3e or d not: {other:x}"
                        )))
                    }
                },
                0x1C => {
                    let length = input.read_int()?;
                    read_string(&mut input, length, modules)?;
                    let reserved = input.read_short()?;
                    if reserved != 0x48 {
                        return Err(Thrown::Io(format!(
                            "Expected x003C after stream name before Unicode stream name, but \
                             found: {reserved:x}"
                        )));
                    }
                    let length = input.read_int()?;
                    read_unicode_string(&mut input, length)?;
                }
                0x31 => {
                    input.read_int()?;
                    let offset = input.read_int()?;
                    module_offset(stream_name.as_deref(), offset, modules)?;
                }
                0x0F => {
                    input.read_int()?;
                    input.read_short()?;
                }
                0x2F => {
                    let length = input.read_int()?;
                    input.try_skip(i64::from(length))?;
                    let mut next = input.read_short()?;
                    if next == 0x16 {
                        read_string_pair(&mut input, modules, 0x3E, true)?;
                        next = input.read_short()?;
                    }
                    if next != 0x30 {
                        return Err(Thrown::Io(
                            "Expected 0x30 as Reserved3 in a ReferenceControl record".into(),
                        ));
                    }
                    let length = input.read_int()?;
                    input.try_skip(i64::from(length))?;
                }
                0x2B => {
                    input.read_int()?;
                }
                0x07 => input.try_skip(8)?,
                0x21 | 0x22 | 0x28 => input.try_skip(4)?,
                _ => {
                    let length = input.read_int()?;
                    input.try_skip(i64::from(length))?;
                }
            }
        }
    })();
    result.map_err(|thrown| match thrown {
        Thrown::Io(message) => Thrown::Io(format!(
            "Error occurred while reading macros at section id {record} (0x{:04X}): {message}",
            record as u16
        )),
        other => other,
    })
}

/// `readStringPair`: a string in the projects' charset, the reserved id after it, which must be
/// `reserved` where `strict`, and the Unicode string after that.
fn read_string_pair(
    input: &mut impl JavaInput,
    modules: &mut ModuleMap,
    reserved: i32,
    strict: bool,
) -> Read<StringPair> {
    let length = input.read_int()?;
    let ascii = read_string(input, length, modules)?;
    let found = input.read_short()?;
    if found != reserved {
        if strict {
            return Err(Thrown::Io(format!(
                "Expected {reserved:x}after name before Unicode name, but found: {found:x}"
            )));
        }
        return Ok(StringPair {
            ascii,
            pushback: Some(found),
        });
    }
    let length = input.read_int()?;
    read_unicode_string(input, length)?;
    Ok(StringPair {
        ascii,
        pushback: None,
    })
}

/// `IOUtils.safelyAllocate(length, MAX_STRING_LENGTH)`: room for a string of `length` bytes.
fn string_buffer(length: i32) -> Read<Vec<u8>> {
    if !(0..=MAX_STRING_LENGTH).contains(&length) {
        return Err(Thrown::Runtime(format!(
            "org.apache.poi.util.RecordFormatException: a string of {length} bytes, past \
             {MAX_STRING_LENGTH}"
        )));
    }
    Ok(vec![0; length as usize])
}

/// `readString`: `length` bytes in the projects' charset.
fn read_string(input: &mut impl JavaInput, length: i32, modules: &mut ModuleMap) -> Read<String> {
    let mut buffer = string_buffer(length)?;
    let read = input.read_fully(&mut buffer)?;
    if read != length {
        return Err(Thrown::Io(format!(
            "Tried to read: {length}, but could only read: {read}"
        )));
    }
    modules.decode(&buffer)
}

/// `readUnicodeString`: `length` bytes of UTF-16LE, which the reader leaves unused.
fn read_unicode_string(input: &mut impl JavaInput, length: i32) -> Read<()> {
    let mut buffer = string_buffer(length)?;
    if input.read_fully(&mut buffer)? != length {
        return Err(Thrown::Io("java.io.EOFException".into()));
    }
    Ok(())
}

/// `readModuleMetadataFromDirStream`: where the source of the module of the stream `name`
/// starts; a module whose stream was read before is decompressed from there.
fn module_offset(name: Option<&str>, offset: i32, modules: &mut ModuleMap) -> Read<()> {
    let Some(name) = name else {
        return Err(Thrown::Runtime(
            "java.lang.NullPointerException: a module offset before any stream name".into(),
        ));
    };
    let Some(module) = modules.get_mut(name) else {
        modules.insert(
            name.to_owned(),
            ModuleImpl {
                offset: Some(offset),
                buf: None,
            },
        );
        return Ok(());
    };
    let Some(buf) = &module.buf else {
        return Err(Thrown::Runtime(format!(
            "java.lang.NullPointerException: the module {name} is named twice before its stream"
        )));
    };
    let start = usize::try_from(offset)
        .ok()
        .filter(|&start| start <= buf.len())
        .ok_or_else(|| {
            Thrown::IllegalArgument(format!(
                "the module {name} starts at {offset}, outside its {} bytes",
                buf.len()
            ))
        })?;
    let decompressed = decompress(&buf[start..])?;
    module.buf = Some(decompressed);
    Ok(())
}

/// `readModuleFromDocumentStream`: the stream of a module, kept as it is where no project has
/// named it yet, and otherwise decompressed from where its source starts, or, where that
/// fails, from the first place that decompresses into what starts like a module's source.
fn read_module_stream(
    file: &Ole2<'_>,
    entry: EntryId,
    name: String,
    modules: &mut ModuleMap,
) -> Read<()> {
    let Some(module) = modules.get_mut(&name) else {
        let buf = document(file, entry)?;
        modules.insert(
            name,
            ModuleImpl {
                offset: None,
                buf: Some(buf),
            },
        );
        return Ok(());
    };
    if module.buf.is_some() {
        return Ok(());
    }
    let offset = module
        .offset
        .expect("a module without its stream's bytes was named by a project");
    let compressed = document(file, entry)?;
    let mut input = Bytes::new(&compressed);
    let at_offset = input
        .try_skip(i64::from(offset))
        .and_then(|()| Rle::new(input))
        .and_then(|mut source| source.to_end());
    module.buf = match at_offset {
        Ok(source) => Some(source),
        Err(Thrown::IllegalArgument(_) | Thrown::IllegalState(_)) => {
            decompress_by_brute_force(&compressed)?
        }
        Err(thrown) => return Err(thrown),
    };
    Ok(())
}

/// `findCompressedStreamWBruteForce`: the decompression from the first signature byte whose
/// chunk header is sound, that holds `Attribute` in its first 20 bytes; failing that, the last
/// decompression tried, which may have failed.
fn decompress_by_brute_force(compressed: &[u8]) -> Read<Option<Vec<u8>>> {
    let mut decompressed = None;
    for at in 0..compressed.len() {
        if compressed[at] != 0x01 || at + 1 >= compressed.len() {
            continue;
        }
        let header = compressed.get(at + 1..at + 3).ok_or_else(|| {
            Thrown::Runtime(format!(
                "java.lang.ArrayIndexOutOfBoundsException: Index {} out of bounds for length {}",
                at + 2,
                compressed.len()
            ))
        })?;
        let header = u16::from_le_bytes([header[0], header[1]]);
        if header == 0 || header & 0x7000 != 0x3000 {
            continue;
        }
        decompressed = match decompress(&compressed[at..]) {
            Ok(source) => Some(source),
            Err(Thrown::IllegalArgument(_) | Thrown::IllegalState(_) | Thrown::Io(_)) => None,
            Err(thrown) => return Err(thrown),
        };
        if let Some(source) = &decompressed {
            if source.len() > 9 {
                let start = &source[..source.len().min(20)];
                let text = Charset::named("windows-1252")
                    .decode(start)
                    .expect("the JDK decodes windows-1252");
                if text.contains("Attribute") {
                    return Ok(decompressed);
                }
            }
        }
    }
    Ok(decompressed)
}

#[cfg(test)]
mod tests;
