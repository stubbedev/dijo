//! Files too large to read whole, which the tests of the crates of the formats write and extract
//! under a memory budget far below their size, so that a parser that declares its memory bounded
//! is held to it: to what it holds where the extraction checks its budget, to the most the
//! extraction's thread holds at any moment, and to the most its budget's large allocations and
//! reservations hold on any thread.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufWriter, Write as _};
use std::path::PathBuf;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::config::ParserConfigs;
use crate::formats::handlers::Write;
use crate::formats::xhtml::Stop;
use crate::formats::{parse_through, route_file, Chain, Output};
use crate::input::MAX_READ_WHOLE;
use crate::{memory, Extractor, Metadata};

/// The memory an extraction of a large file may take by default: a fraction of the file.
pub const BUDGET: u64 = 16 << 20;
/// The budget of an extraction that decodes a stream of unknown length, which holds as much of
/// what it decodes in memory as a file read whole before it spools the rest to a file.
pub const SPOOLING_BUDGET: u64 = MAX_READ_WHOLE + BUDGET;
/// The base64 alphabet, which [`write_base64`] encodes with.
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// What [`LargeFile::padded`] pads a header with, a body at a time.
const PADDING: &[u8] = &[0; 1 << 20];
/// The text after the large part of a document, which only a document read to its end writes.
pub const END: &str = "the end of the document";
/// Twice what a spool holds in memory, so that a part this large spools to a file.
pub const LARGE_PART: u64 = 2 * MAX_READ_WHOLE;
/// The characters [`noise`] draws from.
const NOISE_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 .";

thread_local! {
    /// The bytes this thread holds of what it allocated.
    static HELD: Cell<i64> = const { Cell::new(0) };
    /// The most [`HELD`] has been since [`peak_of`] last started.
    static PEAK: Cell<i64> = const { Cell::new(0) };
}

/// An allocator that counts the bytes each thread holds, and the most it held, around the
/// allocator `A` that allocates: the tests' measure of an extraction's peak, which a budget,
/// checked only where the extraction checks it, does not see. A test crate installs it inside
/// [`memory::CountingAllocator`] as its global allocator.
pub struct Peak<A>(pub A);

/// Adds `change` bytes, negative for a free, to what this thread holds.
fn hold(change: i64) {
    let _ = HELD.try_with(|held| {
        let now = held.get() + change;
        held.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}

fn signed(size: usize) -> i64 {
    i64::try_from(size).expect("an allocation's size fits in i64")
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for Peak<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let memory = unsafe { self.0.alloc(layout) };
        if !memory.is_null() {
            hold(signed(layout.size()));
        }
        memory
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let memory = unsafe { self.0.alloc_zeroed(layout) };
        if !memory.is_null() {
            hold(signed(layout.size()));
        }
        memory
    }

    unsafe fn dealloc(&self, memory: *mut u8, layout: Layout) {
        unsafe { self.0.dealloc(memory, layout) };
        hold(-signed(layout.size()));
    }

    unsafe fn realloc(&self, memory: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { self.0.realloc(memory, layout, new_size) };
        if !moved.is_null() {
            hold(signed(new_size) - signed(layout.size()));
        }
        moved
    }
}

/// What `work` returns, and the most this thread held while it ran beyond what it held before.
pub fn peak_of<R>(work: impl FnOnce() -> R) -> (R, u64) {
    let before = HELD.with(Cell::get);
    PEAK.with(|peak| peak.set(before));
    let result = work();
    let peak = PEAK.with(Cell::get) - before;
    (
        result,
        u64::try_from(peak).expect("the peak is at least what was held before"),
    )
}

/// Binary data of a length, as large as a test needs, which no parser writes out and which
/// compresses well: lines of 64 bytes that each hold their number among bytes that repeat.
pub struct Binary {
    at: u64,
    length: u64,
}

impl Binary {
    pub fn new(length: u64) -> Self {
        Self { at: 0, length }
    }
}

impl std::io::Read for Binary {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while written < buffer.len() && self.at < self.length {
            let mut line = [0xABu8; 64];
            line[0] = 0;
            line[1..9].copy_from_slice(&(self.at / 64).to_le_bytes());
            let offset = (self.at % 64) as usize;
            let left = usize::try_from(self.length - self.at).unwrap_or(usize::MAX);
            let length = (64 - offset).min(buffer.len() - written).min(left);
            buffer[written..written + length].copy_from_slice(&line[offset..offset + length]);
            written += length;
            self.at += length as u64;
        }
        Ok(written)
    }
}

/// Writes to `out` what `data` reads, in base64, in lines of 76 characters, each ended by
/// `line_end`.
pub fn write_base64(
    out: &mut dyn std::io::Write,
    mut data: impl std::io::Read,
    line_end: &[u8],
) -> std::io::Result<()> {
    let mut line = [0u8; 57];
    let mut encoded = Vec::with_capacity(76 + line_end.len());
    loop {
        let mut read = 0;
        while read < line.len() {
            let count = data.read(&mut line[read..])?;
            if count == 0 {
                break;
            }
            read += count;
        }
        if read == 0 {
            return Ok(());
        }
        encoded.clear();
        for group in line[..read].chunks(3) {
            let bytes = [
                group[0],
                *group.get(1).unwrap_or(&0),
                *group.get(2).unwrap_or(&0),
            ];
            let bits = u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
            for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
                encoded.push(if index <= group.len() {
                    BASE64_ALPHABET[(bits >> shift & 63) as usize]
                } else {
                    b'='
                });
            }
        }
        encoded.extend_from_slice(line_end);
        out.write_all(&encoded)?;
    }
}

/// Writes to `out` at least `length` bytes of `element` elements, each on a line of its own,
/// holding characters that deflate barely compresses, drawn from a fixed sequence, so that a
/// package holding them deflated is itself too large to read whole.
pub fn noise(out: &mut dyn io::Write, element: &str, length: u64) -> io::Result<()> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut line = [0u8; 1024];
    let mut written = 0;
    while written < length {
        for character in &mut line {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *character = NOISE_ALPHABET[(state >> 58) as usize];
        }
        write!(out, "<{element}>")?;
        out.write_all(&line)?;
        writeln!(out, "</{element}>")?;
        written += (line.len() + 2 * element.len() + 6) as u64;
    }
    Ok(())
}

/// What writes the data of an entry of an archive.
pub type EntryData<'a> = Box<dyn FnOnce(&mut dyn io::Write) -> io::Result<()> + 'a>;

/// The data of an entry that holds `bytes`.
pub fn bytes(bytes: &[u8]) -> EntryData<'_> {
    Box::new(move |out| out.write_all(bytes))
}

/// Writes to `out` a ZIP archive of `entries`, each with its name and what writes its data, the
/// first stored, as a package's `mimetype` is, the others deflated.
pub fn package(out: &mut dyn io::Write, entries: Vec<(&str, EntryData<'_>)>) -> io::Result<()> {
    zip_archive(
        out,
        entries
            .into_iter()
            .enumerate()
            .map(|(index, (name, data))| {
                let method = if index == 0 {
                    CompressionMethod::Stored
                } else {
                    CompressionMethod::Deflated
                };
                (name, method, data)
            })
            .collect(),
    )
}

/// Writes to `out` a ZIP archive of `entries`, each with its name, how it is compressed and
/// what writes its data, as a stream, deflating at the fastest level.
pub fn zip_archive(
    out: &mut dyn io::Write,
    entries: Vec<(&str, CompressionMethod, EntryData<'_>)>,
) -> io::Result<()> {
    let mut zip = ZipWriter::new_stream(out);
    for (name, method, data) in entries {
        let level = (method == CompressionMethod::Deflated).then_some(1);
        let options = SimpleFileOptions::default()
            .compression_method(method)
            .compression_level(level);
        zip.start_file(name, options).map_err(io::Error::other)?;
        data(&mut zip)?;
    }
    zip.finish().map_err(io::Error::other)?.flush()
}

/// How many bodies of `length` bytes make a file too large to read whole.
pub fn bodies_past_limit(length: usize) -> u64 {
    MAX_READ_WHOLE / length as u64 + 1
}

/// A file larger than [`MAX_READ_WHOLE`] in a directory of its own, removed when it is dropped.
pub struct LargeFile {
    dir: PathBuf,
    path: PathBuf,
}

impl LargeFile {
    /// `head`, then the `count` bodies that `body` makes of their indexes, then `tail`, which
    /// must make a file larger than [`MAX_READ_WHOLE`].
    pub fn new(
        name: &str,
        head: &[u8],
        count: u64,
        mut body: impl FnMut(u64) -> Vec<u8>,
        tail: &[u8],
    ) -> Self {
        let file = Self::written(name, |out| {
            out.write_all(head)?;
            for index in 0..count {
                out.write_all(&body(index))?;
            }
            out.write_all(tail)
        });
        assert!(std::fs::metadata(&file.path).unwrap().len() > MAX_READ_WHOLE);
        file
    }

    /// What `write` writes: a file larger than [`MAX_READ_WHOLE`], or one that decodes into
    /// more.
    pub fn written(
        name: &str,
        write: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
    ) -> Self {
        let dir =
            crate::process::temp_dir().join(format!("dijo-large-{name}-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join(name);
        let mut out = BufWriter::new(File::create(&path).unwrap());
        write(&mut out).unwrap();
        out.into_inner().unwrap().sync_all().unwrap();
        Self { dir, path }
    }

    /// `head`, then zeros to make a file larger than [`MAX_READ_WHOLE`]: the file of a parser
    /// that reads only a header, and what it points to, in place.
    pub fn padded(name: &str, head: &[u8]) -> Self {
        Self::new(
            name,
            head,
            bodies_past_limit(PADDING.len()),
            |_| PADDING.to_vec(),
            b"",
        )
    }

    /// How many zeros [`LargeFile::padded`] writes after its head.
    pub fn padding_length() -> u64 {
        bodies_past_limit(PADDING.len()) * PADDING.len() as u64
    }

    pub fn path(&self) -> &str {
        self.path.to_str().unwrap()
    }

    /// The budget of an extraction of the file: [`BUDGET`] for a file read mapped, and for one
    /// small enough to read whole, as a file that decodes into too much to hold may be, the
    /// file itself, held whole, besides.
    pub fn budget(&self) -> u64 {
        let length = std::fs::metadata(&self.path).unwrap().len();
        if length > MAX_READ_WHOLE {
            BUDGET
        } else {
            length + BUDGET
        }
    }

    /// The file's text and metadata, extracted under its [`LargeFile::budget`] and never holding
    /// more, which `parser` must have read.
    pub fn extract(&self, parser: &str) -> (String, Metadata) {
        self.extract_with(&Extractor::new(), self.budget(), parser)
    }

    /// The file's text and metadata, extracted by `extractor` under `budget` and never holding
    /// more, on its own thread or on any working for it, which `parser` must have read, itself
    /// or a document embedded in it.
    pub fn extract_with(
        &self,
        extractor: &Extractor,
        budget: u64,
        parser: &str,
    ) -> (String, Metadata) {
        let path = self.path();
        let ((extracted, budget_peak), peak) = peak_of(|| {
            memory::within(budget, || {
                let extracted = extractor.extract_file_to_string(path);
                (
                    extracted,
                    memory::peak().expect("the extraction has a budget"),
                )
            })
        });
        let (text, metadata) = extracted
            .unwrap_or_else(|error| panic!("{path} extracts under {budget} bytes: {error}"));
        assert!(peak <= budget, "{path} held {peak} bytes at its peak");
        assert!(
            budget_peak <= budget,
            "{path} held {budget_peak} bytes in large allocations and reservations at their peak"
        );
        let parsers = &metadata["tk:parsed-by"];
        assert!(
            parsers.iter().any(|by| by == parser),
            "{path} is parsed by {parsers:?}"
        );
        (text, metadata)
    }
}

impl LargeFile {
    /// The file's metadata, each line of its text handed to `each_line` as it is written, as
    /// [`LargeFile::extract_each`] extracts it.
    pub fn extract_lines(&self, parser: &str, mut each_line: impl FnMut(&str)) -> Metadata {
        let mut line = String::new();
        let metadata = self.extract_each(parser, |text| {
            for piece in text.split_inclusive('\n') {
                line.push_str(piece);
                if let Some(whole) = line.strip_suffix('\n') {
                    each_line(whole);
                    line.clear();
                }
            }
        });
        if !line.is_empty() {
            each_line(&line);
        }
        metadata
    }

    /// The file's metadata, extracted as [`LargeFile::extract_each`] extracts it, each of
    /// `texts` standing on a line of its text.
    pub fn extract_holding(&self, parser: &str, texts: &[&str]) -> Metadata {
        let mut missing = texts.to_vec();
        let metadata =
            self.extract_lines(parser, |line| missing.retain(|text| !line.contains(text)));
        assert!(missing.is_empty(), "{} lacks {missing:?}", self.path());
        metadata
    }

    /// The file's metadata, its text handed to `each` as it is written, extracted on this
    /// thread as a string extraction without a length limit, so that all of the file is read,
    /// under its [`LargeFile::budget`] and never holding more besides what `each` keeps, which
    /// `parser` must have read.
    pub fn extract_each(&self, parser: &str, each: impl FnMut(&str)) -> Metadata {
        let path = self.path();
        let budget = self.budget();
        let mut out = Each(each);
        let chain = Chain::String {
            output: Output::Text,
            max_length: usize::MAX,
        };
        let configs = ParserConfigs::default();
        let ((parsed, budget_peak), peak) = peak_of(|| {
            memory::within(budget, || {
                let routed =
                    route_file(path).unwrap_or_else(|why| panic!("{path} is routed: {why}"));
                let parsed = parse_through(
                    routed.parser,
                    &routed.input,
                    &routed.detected,
                    &configs,
                    chain,
                    &mut out,
                );
                (parsed, memory::peak().expect("the extraction has a budget"))
            })
        });
        let metadata = parsed
            .unwrap_or_else(|error| panic!("{path} extracts under {budget} bytes: {error}"))
            .unwrap_or_else(|why| panic!("{path} is read natively: {why}"));
        assert!(peak <= budget, "{path} held {peak} bytes at its peak");
        assert!(
            budget_peak <= budget,
            "{path} held {budget_peak} bytes in large allocations and reservations at their peak"
        );
        let parsers = &metadata["tk:parsed-by"];
        assert!(
            parsers.iter().any(|by| by == parser),
            "{path} is parsed by {parsers:?}"
        );
        metadata
    }
}

/// Hands each piece of text written to the closure it holds.
struct Each<F>(F);

impl<F: FnMut(&str)> Write for Each<F> {
    fn write(&mut self, text: &str) -> Result<(), Stop> {
        (self.0)(text);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Stop> {
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Stop> {
        Ok(())
    }
}

impl Drop for LargeFile {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}
