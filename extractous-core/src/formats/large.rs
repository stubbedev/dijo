//! Files too large to read whole, which the tests of the crates of the formats write and extract
//! under a memory budget far below their size, so that a parser that declares its memory bounded
//! is held to it: to what it holds where the extraction checks its budget, to the most the
//! extraction's thread holds at any moment, and to the most its budget's large allocations and
//! reservations hold on any thread.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use crate::config::ParserConfigs;
use crate::formats::handlers::Write;
use crate::formats::xhtml::Stop;
use crate::formats::{parse_through, route_file, Chain, Output};
use crate::input::MAX_READ_WHOLE;
use crate::{memory, Extractor, Metadata};

/// The memory an extraction of a large file may take by default: a fraction of the file.
pub const BUDGET: u64 = 16 << 20;
/// The budget of an extraction that decodes a stream of unknown length, which holds as much of
/// what it decodes in memory as a file read whole before it spools the rest to a file, or that
/// reads whole a file that decodes into more than that.
pub const SPOOLING_BUDGET: u64 = MAX_READ_WHOLE + BUDGET;
/// What [`LargeFile::padded`] pads a header with, a body at a time.
const PADDING: &[u8] = &[0; 1 << 20];

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
        let dir = crate::isolate::temp_dir()
            .join(format!("extractous-large-{name}-{}", std::process::id()));
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

    /// The file's text and metadata, extracted under [`BUDGET`] and never holding more, which
    /// `parser` must have read.
    pub fn extract(&self, parser: &str) -> (String, Metadata) {
        self.extract_with(&Extractor::new(), BUDGET, parser)
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

    /// The file's metadata, its text handed to `each` as it is written, extracted on this
    /// thread as a string extraction without a length limit, so that all of the file is read,
    /// under [`BUDGET`] and never holding more besides what `each` keeps, which `parser` must
    /// have read.
    pub fn extract_each(&self, parser: &str, each: impl FnMut(&str)) -> Metadata {
        let path = self.path();
        let mut out = Each(each);
        let chain = Chain::String {
            output: Output::Text,
            max_length: usize::MAX,
        };
        let configs = ParserConfigs::default();
        let ((parsed, budget_peak), peak) = peak_of(|| {
            memory::within(BUDGET, || {
                let routed =
                    route_file(path).unwrap_or_else(|why| panic!("{path} is routed: {why}"));
                assert!(routed.input.is_mapped(), "{path} is mapped");
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
            .unwrap_or_else(|error| panic!("{path} extracts under {BUDGET} bytes: {error}"))
            .unwrap_or_else(|why| panic!("{path} is read natively: {why}"));
        assert!(peak <= BUDGET, "{path} held {peak} bytes at its peak");
        assert!(
            budget_peak <= BUDGET,
            "{path} held {budget_peak} bytes in large allocations and reservations at their peak"
        );
        assert_eq!(metadata["tk:parsed-by"].last().unwrap(), parser);
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
