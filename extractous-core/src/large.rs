//! Files too large to read whole, which the tests of the crates of the formats write and extract
//! under a memory budget far below their size, so that a parser that declares its memory bounded
//! is held to it: to what it holds where the extraction checks its budget, and to the most it
//! holds at any moment.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::PathBuf;

use extractous_base::config::ParserConfigs;
use extractous_base::formats::handlers::Write;
use extractous_base::formats::xhtml::Stop;
use extractous_base::formats::{Chain, Output};
use extractous_base::input::MAX_READ_WHOLE;
use extractous_base::isolate::temp_dir;

use crate::formats::{parse_through, route_file};
use crate::{memory, Metadata};

/// The memory an extraction of a large file may take: a fraction of the file.
const BUDGET: u64 = 16 << 20;

thread_local! {
    /// The bytes this thread holds of what it allocated.
    static HELD: Cell<i64> = const { Cell::new(0) };
    /// The most [`HELD`] has been since [`peak_of`] last started.
    static PEAK: Cell<i64> = const { Cell::new(0) };
}

/// An allocator that counts the bytes each thread holds, and the most it held, around the
/// allocator `A` that allocates: the tests' measure of an extraction's peak, which a budget,
/// checked only where the extraction checks it, does not see.
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
fn peak_of<R>(work: impl FnOnce() -> R) -> (R, u64) {
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
        let dir = temp_dir().join(format!("extractous-large-{name}-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join(name);
        let mut out = BufWriter::new(File::create(&path).unwrap());
        out.write_all(head).unwrap();
        for index in 0..count {
            out.write_all(&body(index)).unwrap();
        }
        out.write_all(tail).unwrap();
        let file = out.into_inner().unwrap();
        file.sync_all().unwrap();
        assert!(file.metadata().unwrap().len() > MAX_READ_WHOLE);
        Self { dir, path }
    }

    /// The file's text and metadata, extracted under [`BUDGET`] and never holding more, which
    /// `parser` must have read.
    pub fn extract(&self, parser: &str) -> (String, Metadata) {
        let mut text = String::new();
        let metadata = self.extract_each(parser, |written| text.push_str(written));
        (text, metadata)
    }

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
    /// thread as a string extraction without a length limit, under [`BUDGET`] and never holding
    /// more besides what `each` keeps, which `parser` must have read.
    pub fn extract_each(&self, parser: &str, each: impl FnMut(&str)) -> Metadata {
        let path = self.path.to_str().unwrap();
        let mut out = Each(each);
        let chain = Chain::String {
            output: Output::Text,
            max_length: usize::MAX,
        };
        let configs = ParserConfigs::default();
        let (parsed, peak) = peak_of(|| {
            memory::within(BUDGET, || {
                let routed =
                    route_file(path).unwrap_or_else(|why| panic!("{path} is routed: {why}"));
                assert!(routed.input.is_mapped(), "{path} is mapped");
                parse_through(
                    routed.parser,
                    &routed.input,
                    &routed.detected,
                    &configs,
                    chain,
                    &mut out,
                )
            })
        });
        let metadata = parsed
            .unwrap_or_else(|error| panic!("{path} extracts under {BUDGET} bytes: {error}"))
            .unwrap_or_else(|why| panic!("{path} is read natively: {why}"));
        assert!(peak <= BUDGET, "{path} held {peak} bytes at its peak");
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
