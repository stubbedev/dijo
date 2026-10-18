//! A document's bytes as native detection and parsing read them: the caller's, read into memory,
//! or mapped from a file too large to read whole, of which only the parts read are paged in.

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use memmap2::Mmap;

use crate::errors::{Error, ExtractResult};

/// The largest file read into memory whole. A larger one is mapped instead, and only a parser
/// whose memory stays bounded whatever its input's size reads it.
pub const MAX_READ_WHOLE: u64 = 64 * 1024 * 1024;

/// Where a document is read from.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    File(&'a str),
    Bytes(&'a [u8]),
    Url(&'a str),
}

/// A document's bytes.
#[derive(Debug, Clone)]
pub enum Input<'a> {
    /// The caller's.
    Borrowed(&'a [u8]),
    /// Read into memory.
    Read(Arc<Vec<u8>>),
    /// Mapped from a file larger than [`MAX_READ_WHOLE`].
    Mapped(Arc<Mmap>),
    /// Part of other bytes, read in place, whose pages are theirs: an entry stored in an
    /// archive.
    Within(&'a [u8], Pages<'a>),
}

impl Deref for Input<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Input::Borrowed(bytes) => bytes,
            Input::Read(bytes) => bytes,
            Input::Mapped(map) => map,
            Input::Within(bytes, _) => bytes,
        }
    }
}

impl Input<'_> {
    /// The same bytes, owned: the caller's copied, the others shared.
    pub fn to_static(&self) -> Input<'static> {
        match self {
            Input::Borrowed(bytes) | Input::Within(bytes, _) => {
                Input::Read(Arc::new(bytes.to_vec()))
            }
            Input::Read(bytes) => Input::Read(Arc::clone(bytes)),
            Input::Mapped(map) => Input::Mapped(Arc::clone(map)),
        }
    }

    /// Whether the bytes are mapped from a file too large to read whole.
    pub fn is_mapped(&self) -> bool {
        matches!(self, Input::Mapped(_))
    }

    /// The pages of the bytes: those of the map, if they are mapped.
    pub fn pages(&self) -> Pages<'_> {
        match self {
            Input::Borrowed(_) | Input::Read(_) => Pages::IN_MEMORY,
            Input::Mapped(map) => Pages(Some(map)),
            Input::Within(_, pages) => *pages,
        }
    }
}

/// The pages of a document's bytes, as a parser that reads them in bounded memory lets go of
/// them: the pages of a file mapped too large to read whole stay the process' memory once read,
/// until they are released; bytes held in memory have nothing to release.
#[derive(Debug, Clone, Copy)]
pub struct Pages<'a>(#[cfg_attr(not(unix), expect(dead_code))] Option<&'a Mmap>);

impl Pages<'_> {
    /// The pages of bytes held in memory.
    pub const IN_MEMORY: Pages<'static> = Pages(None);

    /// Lets go of every page of the map read so far: they stop counting as the process' memory,
    /// and reading them again pages them in afresh, from the operating system's cache of the
    /// file or from the file. A parser reading a mapped file releases what it has read as it
    /// goes, so that its memory stays bounded whatever the file's size.
    ///
    /// The map is a read-only shared map of a file, so `MADV_DONTNEED` loses none of its bytes.
    /// Elsewhere than on Unix the operating system trims the pages of a map no longer read by
    /// itself, and nothing is asked of it.
    pub fn release(self) {
        #[cfg(unix)]
        if let Some(map) = self.0 {
            unsafe { map.unchecked_advise(memmap2::UncheckedAdvice::DontNeed) }
                .expect("a read-only map of a file releases its pages");
        }
    }
}

/// The bytes of the file at `path`: read whole, or mapped when it is larger than
/// [`MAX_READ_WHOLE`].
pub fn read_file(path: &str) -> ExtractResult<Input<'static>> {
    let unreadable = |e: io::Error| Error::IoError(format!("Could not read {path}: {e}"));
    let mut file = File::open(path).map_err(unreadable)?;
    let length = file.metadata().map_err(unreadable)?.len();
    if length > MAX_READ_WHOLE {
        return map(&file).map_err(unreadable);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length).expect("at most MAX_READ_WHOLE"));
    file.read_to_end(&mut bytes).map_err(unreadable)?;
    Ok(Input::Read(Arc::new(bytes)))
}

/// Maps `file` into memory read-only.
///
/// A mapped file that another process truncates while it is mapped makes reading the pages past
/// its new end fault (`SIGBUS` on Unix), which ends the process, as with any program that maps
/// the files it reads. Only files larger than [`MAX_READ_WHOLE`] and downloads spooled to files
/// of our own are mapped.
fn map(file: &File) -> io::Result<Input<'static>> {
    let map = unsafe { Mmap::map(file) }?;
    Ok(Input::Mapped(Arc::new(map)))
}

/// How many bytes of a mapped document a parser reads between releases of its pages.
const RELEASE_EVERY: usize = 64 << 20;
/// The bytes [`Paging::find`] and [`Paging::scan`] read at a time.
pub(crate) const WINDOW: usize = 16 << 20;

/// The pages of a document's bytes as a parser reading all of them uses them: released each
/// time it has read [`RELEASE_EVERY`] bytes since, and once it is done with them.
#[derive(Debug)]
pub struct Paging<'a> {
    pages: Pages<'a>,
    read: Cell<usize>,
}

impl<'a> Paging<'a> {
    pub fn new(pages: Pages<'a>) -> Self {
        Self {
            pages,
            read: Cell::new(0),
        }
    }

    /// Counts `bytes` more read, and releases the pages once enough have been.
    pub fn read(&self, bytes: usize) {
        let read = self.read.get().saturating_add(bytes);
        if read < RELEASE_EVERY {
            self.read.set(read);
            return;
        }
        self.release();
    }

    /// Where `needle` first stands in `haystack`, searched a window at a time, each window
    /// counted read, so that a search of all of a mapped file stays bounded too.
    pub fn find(&self, haystack: &[u8], needle: &[u8]) -> Option<usize> {
        let finder = memchr::memmem::Finder::new(needle);
        let mut start = 0;
        while start < haystack.len() {
            let end = haystack.len().min(start + WINDOW + needle.len());
            if let Some(found) = finder.find(&haystack[start..end]) {
                self.read(found + needle.len());
                return Some(start + found);
            }
            self.read(end - start);
            if end == haystack.len() {
                break;
            }
            start += WINDOW;
        }
        None
    }

    /// Hands `each` the bytes of `bytes` a window at a time, each window counted read.
    pub(crate) fn scan(&self, bytes: &[u8], mut each: impl FnMut(&[u8])) {
        for window in bytes.chunks(WINDOW) {
            each(window);
            self.read(window.len());
        }
    }

    /// Releases the pages read so far.
    pub fn release(&self) {
        self.pages.release();
        self.read.set(0);
    }
}

impl Drop for Paging<'_> {
    fn drop(&mut self) {
        if self.read.get() > 0 {
            self.release();
        }
    }
}

/// Collects a stream of a document's bytes, as Tika's `CachingSource` caches a stream: in memory
/// up to [`MAX_READ_WHOLE`], and beyond it in a temporary file of its own, mapped once complete,
/// which is deleted once its map is dropped.
pub struct Spool {
    memory: Vec<u8>,
    file: Option<File>,
}

impl Spool {
    /// A spool for a stream of `expected` bytes, if the stream says how many.
    pub fn new(expected: Option<u64>) -> Self {
        let capacity = expected
            .filter(|&length| length <= MAX_READ_WHOLE)
            .unwrap_or(0);
        Self {
            memory: Vec::with_capacity(usize::try_from(capacity).expect("at most MAX_READ_WHOLE")),
            file: None,
        }
    }

    /// The bytes collected, once the stream has ended.
    pub fn finish(self) -> io::Result<Input<'static>> {
        match self.file {
            None => Ok(Input::Read(Arc::new(self.memory))),
            Some(mut file) => {
                file.flush()?;
                map(&file)
            }
        }
    }

    fn file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            let mut file = temporary_file()?;
            file.write_all(&self.memory)?;
            self.memory = Vec::new();
            self.file = Some(file);
        }
        Ok(self
            .file
            .as_mut()
            .expect("the spool's file was just created"))
    }
}

impl Write for Spool {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.file.is_none() && self.memory.len() as u64 + bytes.len() as u64 <= MAX_READ_WHOLE {
            self.memory.extend_from_slice(bytes);
            return Ok(bytes.len());
        }
        self.file()?.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// A new file in the temporary directory that no other process opens and that is deleted once
/// it is closed: unlinked at once on Unix, deleted on close on Windows.
fn temporary_file() -> io::Result<File> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path: PathBuf = crate::isolate::temp_dir().join(format!(
        "extractous-spool-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        options.custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
    }
    let file = options.open(&path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("Could not create {}: {e}", path.display()),
        )
    })?;
    #[cfg(unix)]
    std::fs::remove_file(&path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("Could not unlink {}: {e}", path.display()),
        )
    })?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_small_files_whole_and_maps_large_ones() {
        let path = crate::isolate::temp_dir()
            .join(format!("extractous-input-test-{}", std::process::id()));
        let file = File::create(&path).unwrap();
        file.set_len(MAX_READ_WHOLE).unwrap();
        let name = path.to_str().unwrap();
        let read = read_file(name).unwrap();
        assert!(!read.is_mapped());
        assert_eq!(read.len() as u64, MAX_READ_WHOLE);
        file.set_len(MAX_READ_WHOLE + 1).unwrap();
        let mapped = read_file(name).unwrap();
        assert!(mapped.is_mapped());
        assert_eq!(mapped.len() as u64, MAX_READ_WHOLE + 1);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn finds_across_search_windows() {
        let paging = Paging::new(Pages::IN_MEMORY);
        let mut haystack = vec![b'.'; WINDOW + 8];
        haystack[WINDOW - 2..WINDOW + 3].copy_from_slice(b"found");
        assert_eq!(paging.find(&haystack, b"found"), Some(WINDOW - 2));
        assert_eq!(paging.find(&haystack, b"missing"), None);
        assert_eq!(paging.find(b"a found b", b"found"), Some(2));
        assert_eq!(paging.find(b"", b"found"), None);
    }

    #[test]
    fn spools_streams_past_the_limit_to_a_mapped_file() {
        let mut small = Spool::new(Some(3));
        small.write_all(b"abc").unwrap();
        let small = small.finish().unwrap();
        assert!(!small.is_mapped());
        assert_eq!(&*small, b"abc");

        let chunk = vec![7u8; 1 << 20];
        let mut large = Spool::new(None);
        for _ in 0..=MAX_READ_WHOLE >> 20 {
            large.write_all(&chunk).unwrap();
        }
        large.write_all(b"end").unwrap();
        let large = large.finish().unwrap();
        assert!(large.is_mapped());
        assert_eq!(large.len() as u64, MAX_READ_WHOLE + (1 << 20) + 3);
        assert!(large.ends_with(b"\x07end"));
    }
}
