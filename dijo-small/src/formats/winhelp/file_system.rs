//! The file system a help file keeps its internal files in: the help file header, the
//! directory, a B+ tree of the internal files' names and offsets, and the file header before
//! each internal file. The B+ trees of the directory and of internal files such as `|KWBTREE`
//! are read leaf by leaf, from the first leaf the index pages lead to along the leaves' chain.

use super::reader::Reader;
use super::Malformed;

const HELP_MAGIC: u32 = 0x0003_5F3F;
const BTREE_MAGIC: u16 = 0x293B;
/// The bytes of a file header: its reserved space, used space and flags.
const FILE_HEADER: usize = 9;
/// Where an index page holds the page of the entries before its first.
const PREVIOUS_PAGE: usize = 4;
const LEAF_PAGE_HEADER: usize = 8;
const NO_PAGE: i16 = -1;

/// The internal files of a help file, by name.
pub(super) struct FileSystem<'a> {
    data: &'a [u8],
    files: Vec<(&'a [u8], u32)>,
}

impl<'a> FileSystem<'a> {
    /// Reads the help file header and the directory of `data`.
    pub(super) fn open(data: &'a [u8]) -> Result<Self, Malformed> {
        let mut header = Reader::new(data, "the help file header");
        let magic = header.u32()?;
        if magic != HELP_MAGIC {
            return Err(Malformed(format!(
                "the help file starts with {magic:#010x}, not the magic number {HELP_MAGIC:#010x}"
            )));
        }
        let directory = internal_file(data, header.u32()?, "the directory")?;
        let mut files = Vec::new();
        for leaf in BTree::read(directory, "the directory")?.leaves() {
            let mut entries = leaf?;
            while let Some(entry) = entries.next_entry() {
                let name = entry.string();
                files.push((name, entry.u32()?));
            }
        }
        Ok(Self { data, files })
    }

    /// The content of the internal file `name`, if there is one.
    pub(super) fn file(&self, name: &str) -> Result<Option<&'a [u8]>, Malformed> {
        self.files
            .iter()
            .find(|(file, _)| *file == name.as_bytes())
            .map(|&(_, offset)| internal_file(self.data, offset, name))
            .transpose()
    }
}

/// The content of the internal file `what` whose file header is at `offset` of `data`.
fn internal_file<'a>(data: &'a [u8], offset: u32, what: &str) -> Result<&'a [u8], Malformed> {
    let start = usize::try_from(offset)
        .ok()
        .filter(|&start| start < data.len());
    let header = start.and_then(|start| data.get(start..)).ok_or_else(|| {
        Malformed(format!(
            "{what} is at {offset}, beyond the {} bytes of the help file",
            data.len()
        ))
    })?;
    let mut fields = Reader::new(header, "a file header");
    let _reserved = fields.u32()?;
    let used = fields.u32()? as usize;
    let _flags = fields.u8()?;
    header
        .get(FILE_HEADER..)
        .and_then(|content| content.get(..used))
        .ok_or_else(|| {
            Malformed(format!(
                "{what} of {used} bytes at {offset} runs past the {} bytes of the help file",
                data.len()
            ))
        })
}

/// A B+ tree: its pages, the root page and the levels of index pages above the leaves.
pub(super) struct BTree<'a> {
    pages: &'a [u8],
    page_size: usize,
    page_count: usize,
    root: i16,
    levels: usize,
    what: &'static str,
}

impl<'a> BTree<'a> {
    /// The B+ tree of the internal file `content`, which holds `what`.
    pub(super) fn read(content: &'a [u8], what: &'static str) -> Result<Self, Malformed> {
        let mut header = Reader::new(content, what);
        let magic = header.u16()?;
        if magic != BTREE_MAGIC {
            return Err(Malformed(format!(
                "{what} starts with {magic:#06x}, not the B+ tree magic number {BTREE_MAGIC:#06x}"
            )));
        }
        let _flags = header.u16()?;
        let page_size = usize::from(header.u16()?);
        header.skip(16 + 2 + 2)?;
        let root = header.i16()?;
        header.skip(2)?;
        let page_count = count(header.i16()?, "pages", what)?;
        let levels = count(header.i16()?, "levels", what)?;
        let _entries = header.i32()?;
        if page_size < LEAF_PAGE_HEADER {
            return Err(Malformed(format!(
                "{what} has pages of {page_size} bytes, too few for a page header"
            )));
        }
        Ok(Self {
            pages: header.rest(),
            page_size,
            page_count,
            root,
            levels,
            what,
        })
    }

    fn page(&self, number: i16) -> Result<&'a [u8], Malformed> {
        usize::try_from(number)
            .ok()
            .filter(|&number| number < self.page_count)
            .and_then(|number| {
                let start = number.checked_mul(self.page_size)?;
                self.pages.get(start..start.checked_add(self.page_size)?)
            })
            .ok_or_else(|| {
                Malformed(format!(
                    "{} has no page {number} of its {} pages of {} bytes",
                    self.what, self.page_count, self.page_size
                ))
            })
    }

    /// The entries of each leaf page in order, up to the first page that cannot be read. Each
    /// page is visited once at most, so that pages linked in a cycle end in an error.
    pub(super) fn leaves(&self) -> impl Iterator<Item = Result<Entries<'a>, Malformed>> + '_ {
        let mut next = self.first_leaf();
        let mut visited = 0;
        std::iter::from_fn(move || {
            let number = match next.take()? {
                Ok(number) if number == NO_PAGE => return None,
                Ok(number) => number,
                Err(problem) => return Some(Err(problem)),
            };
            visited += 1;
            if visited > self.page_count {
                return Some(Err(Malformed(format!(
                    "the leaf pages of {} link in a cycle",
                    self.what
                ))));
            }
            Some(self.page(number).and_then(|page| {
                let mut header = Reader::new(page, self.what);
                let _unused = header.u16()?;
                let count = count(header.i16()?, "entries in a page", self.what)?;
                let _previous = header.i16()?;
                next = Some(Ok(header.i16()?));
                Ok(Entries {
                    reader: Reader::new(&page[LEAF_PAGE_HEADER..], self.what),
                    count,
                })
            }))
        })
    }

    /// The first leaf page, which the first entry of each index page from the root leads to,
    /// or none in a tree without levels.
    fn first_leaf(&self) -> Option<Result<i16, Malformed>> {
        if self.levels == 0 {
            return None;
        }
        if self.levels > self.page_count {
            return Some(Err(Malformed(format!(
                "{} has {} levels of its {} pages",
                self.what, self.levels, self.page_count
            ))));
        }
        let mut number = self.root;
        for _ in 1..self.levels {
            number = match self.page(number) {
                Ok(page) => i16::from_le_bytes([page[PREVIOUS_PAGE], page[PREVIOUS_PAGE + 1]]),
                Err(problem) => return Some(Err(problem)),
            };
        }
        Some(Ok(number))
    }
}

/// The count `value` of `things` in `what`, which cannot be negative.
fn count(value: i16, things: &str, what: &str) -> Result<usize, Malformed> {
    usize::try_from(value)
        .map_err(|_| Malformed(format!("{what} has {value} {things}, fewer than none")))
}

/// The entries of a leaf page, each read by a [`Reader`] that starts at it: reading one entry
/// moves the next one on.
pub(super) struct Entries<'a> {
    reader: Reader<'a>,
    count: usize,
}

impl<'a> Entries<'a> {
    /// The reader of the next entry, while the page holds more.
    pub(super) fn next_entry(&mut self) -> Option<&mut Reader<'a>> {
        if self.count == 0 {
            return None;
        }
        self.count -= 1;
        Some(&mut self.reader)
    }
}
