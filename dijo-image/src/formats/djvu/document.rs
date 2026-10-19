//! The structure of a DjVu file (DjVu v3 specification, section 8), as our Java `DjvuDocument`
//! reads it: a single page FORM:DJVU, or a bundled FORM:DJVM whose DIRM chunk names the
//! component files that follow it, pages and the FORM:DJVI files they share by INCL.

use std::collections::HashMap;

use super::{bzz, DjvuError};

/// The largest chunk whose data is read.
const MAX_CHUNK_BYTES: u64 = 256 << 20;
const CHUNK_HEADER: u64 = 8;
const MAGIC: &[u8; 4] = b"AT&T";
/// The flag of a DIRM chunk of a bundled document.
const BUNDLED: u8 = 0x80;
const MAX_DIRECTORY_BYTES: usize = 16 << 20;

/// An IFF chunk: its id, where its data starts, its size, its end and the next chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Chunk {
    pub(super) id: [u8; 4],
    data: u64,
    pub(super) size: u64,
    end: u64,
    next: u64,
}

/// A FORM chunk: its type, and where its nested chunks start and end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Form {
    kind: [u8; 4],
    start: u64,
    end: u64,
}

/// A DjVu document: its pages, and its component files by id.
#[derive(Debug)]
pub(super) struct DjvuDocument<'a> {
    file: &'a [u8],
    pages: Vec<Form>,
    components: HashMap<String, Form>,
}

fn malformed(message: impl Into<String>) -> DjvuError {
    DjvuError::Malformed(message.into())
}

fn id_text(id: [u8; 4]) -> String {
    id.iter().map(|&byte| char::from(byte)).collect()
}

/// `read`: `length` bytes at `offset`, failing past the end of the file.
fn read(file: &[u8], offset: u64, length: u64) -> Result<&[u8], DjvuError> {
    usize::try_from(offset)
        .ok()
        .zip(usize::try_from(length).ok())
        .and_then(|(offset, length)| file.get(offset..offset.checked_add(length)?))
        .ok_or_else(|| malformed(format!("The DjVu file ends inside a chunk at {offset}")))
}

fn read_id(file: &[u8], offset: u64) -> Result<[u8; 4], DjvuError> {
    Ok(read(file, offset, 4)?.try_into().expect("four bytes"))
}

fn read_int(file: &[u8], offset: u64) -> Result<u32, DjvuError> {
    Ok(u32::from_be_bytes(read_id(file, offset)?))
}

fn chunk(file: &[u8], offset: u64, limit: u64) -> Result<Chunk, DjvuError> {
    if offset + CHUNK_HEADER > limit {
        return Err(malformed(format!(
            "A DjVu chunk header at {offset} passes its container"
        )));
    }
    let id = read_id(file, offset)?;
    let size = u64::from(read_int(file, offset + 4)?);
    let data = offset + CHUNK_HEADER;
    if data + size > limit {
        return Err(malformed(format!(
            "The DjVu chunk {} at {offset} passes its container",
            id_text(id)
        )));
    }
    let end = data + size;
    Ok(Chunk {
        id,
        data,
        size,
        end,
        next: limit.min(end + (end & 1)),
    })
}

fn form(file: &[u8], chunk: &Chunk) -> Result<Form, DjvuError> {
    if &chunk.id != b"FORM" || chunk.size < 4 {
        return Err(malformed(format!(
            "Expected a DjVu FORM chunk, not {}",
            id_text(chunk.id)
        )));
    }
    Ok(Form {
        kind: read_id(file, chunk.data)?,
        start: chunk.data + 4,
        end: chunk.end,
    })
}

impl<'a> DjvuDocument<'a> {
    /// The document a file holds.
    pub(super) fn read(file: &'a [u8]) -> Result<Self, DjvuError> {
        let size = file.len() as u64;
        if size < 4 + CHUNK_HEADER + 4 || &file[..4] != MAGIC {
            return Err(malformed("Not a DjVu file: no AT&T magic"));
        }
        let root = form(file, &chunk(file, 4, size)?)?;
        match &root.kind {
            b"DJVU" => {
                return Ok(Self {
                    file,
                    pages: vec![root],
                    components: HashMap::new(),
                })
            }
            b"DJVM" => {}
            kind => {
                return Err(malformed(format!(
                    "Unknown DjVu FORM type {}",
                    id_text(*kind)
                )))
            }
        }
        let mut pages = Vec::new();
        let mut files = Vec::new();
        let mut ids = None;
        let mut offset = root.start;
        while offset < root.end {
            let found = chunk(file, offset, root.end)?;
            match &found.id {
                b"DIRM" => ids = Some(directory(file, &found)?),
                b"FORM" => {
                    let component = form(file, &found)?;
                    files.push(component);
                    if &component.kind == b"DJVU" {
                        pages.push(component);
                    }
                }
                _ => {}
            }
            offset = found.next;
        }
        let ids =
            ids.ok_or_else(|| malformed("A multi-page DjVu document has no DIRM directory"))?;
        if ids.len() != files.len() {
            return Err(malformed(format!(
                "The DjVu directory names {} component files, but the document bundles {}",
                ids.len(),
                files.len()
            )));
        }
        Ok(Self {
            file,
            pages,
            components: ids.into_iter().zip(files).collect(),
        })
    }

    pub(super) fn pages(&self) -> &[Form] {
        &self.pages
    }

    /// The component file an INCL chunk names.
    pub(super) fn component(&self, id: &str) -> Result<Form, DjvuError> {
        self.components
            .get(id)
            .copied()
            .ok_or_else(|| malformed(format!("The DjVu document has no component file {id}")))
    }

    /// The chunks a FORM holds, in order.
    pub(super) fn chunks(&self, form: &Form) -> Result<Vec<Chunk>, DjvuError> {
        let mut chunks = Vec::new();
        let mut offset = form.start;
        while offset < form.end {
            let found = chunk(self.file, offset, form.end)?;
            offset = found.next;
            chunks.push(found);
        }
        Ok(chunks)
    }

    /// A chunk's data.
    pub(super) fn data(&self, chunk: &Chunk) -> Result<&'a [u8], DjvuError> {
        if chunk.size > MAX_CHUNK_BYTES {
            return Err(malformed(format!(
                "The DjVu chunk {} of {} bytes exceeds {MAX_CHUNK_BYTES}",
                id_text(chunk.id),
                chunk.size
            )));
        }
        read(self.file, chunk.data, chunk.size)
    }
}

/// The component ids of a bundled document's directory, in the order of its component files
/// (8.3.2): after the flags, the file count and the offsets, a BZZ stream of the sizes, the
/// flags and the id strings, each followed by a name and a title when its flags say so.
fn directory(file: &[u8], chunk: &Chunk) -> Result<Vec<String>, DjvuError> {
    if chunk.size < 3 {
        return Err(malformed("The DjVu directory ends inside its header"));
    }
    let header = read(file, chunk.data, 3)?;
    if header[0] & BUNDLED == 0 {
        return Err(DjvuError::Unsupported(
            "An indirect DjVu document keeps its pages in other files".to_owned(),
        ));
    }
    let count = usize::from(u16::from_be_bytes([header[1], header[2]]));
    let compressed = chunk.data + 3 + 4 * count as u64;
    if compressed > chunk.end {
        return Err(malformed("The DjVu directory ends inside its offsets"));
    }
    let listing = bzz::decode(
        read(file, compressed, chunk.end - compressed)?,
        MAX_DIRECTORY_BYTES,
    )?;
    let mut at = 4 * count;
    if at > listing.len() {
        return Err(malformed(
            "The DjVu directory ends inside its file sizes and flags",
        ));
    }
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let flags = *listing
            .get(3 * count + i)
            .ok_or_else(|| malformed("The DjVu directory ends inside its file flags"))?;
        let strings = 1 + usize::from(flags & 0x80 != 0) + usize::from(flags & 0x40 != 0);
        for s in 0..strings {
            let end = listing[at..]
                .iter()
                .position(|&byte| byte == 0)
                .map(|length| at + length)
                .ok_or_else(|| malformed("The DjVu directory ends inside a file name"))?;
            if s == 0 {
                ids.push(crate::java::utf8(&listing[at..end]));
            }
            at = end + 1;
        }
    }
    Ok(ids)
}
