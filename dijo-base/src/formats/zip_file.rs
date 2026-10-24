//! Entries of a ZIP archive as the zip crate reads them, for the OOXML packages read with it: an
//! entry's content, stored data read in place, and the records of the central directory, which
//! tell apart several entries of one name that the crate lists as one.

use std::io::{self, Cursor, Read};

use zip::{CompressionMethod, ZipArchive};

use crate::input::{Input, Pages, Paging, Spool};

const CENTRAL_DIRECTORY_SIGNATURE: &[u8] = b"PK\x01\x02";
const CENTRAL_DIRECTORY_HEADER_LENGTH: usize = 46;

/// The uncompressed content of the entry `index` of `archive`, which reads `input`, whose pages
/// are `pages`: the archive's own bytes where the entry is stored, checked against its CRC as the zip crate
/// checks what it reads, so that reading a large archive copies none of its stored data; an
/// inflated entry spooled as a stream is, to a file of its own past what is read whole.
pub fn read_entry<'a>(
    archive: &mut ZipArchive<Cursor<&'a [u8]>>,
    input: &'a [u8],
    pages: Pages<'a>,
    index: usize,
) -> Result<Input<'a>, String> {
    if let Some(stored) = stored_entry(archive, input, pages, index, |_| ())? {
        return Ok(Input::Within(stored, pages));
    }
    let mut file = archive.by_index(index).map_err(|e| e.to_string())?;
    let mut spool = Spool::new(Some(file.size()));
    io::copy(&mut file, &mut spool).map_err(|e| e.to_string())?;
    spool.finish().map_err(|e| e.to_string())
}

/// [`read_entry`] handing `each` the content a chunk at a time as it is checked or inflated,
/// keeping none of it: a scan of a large entry that spools nothing.
pub fn scan_entry<'a>(
    archive: &mut ZipArchive<Cursor<&'a [u8]>>,
    input: &'a [u8],
    pages: Pages<'a>,
    index: usize,
    mut each: impl FnMut(&[u8]),
) -> Result<(), String> {
    if stored_entry(archive, input, pages, index, &mut each)?.is_some() {
        return Ok(());
    }
    let mut file = archive.by_index(index).map_err(|e| e.to_string())?;
    let mut chunk = vec![0; SCAN_CHUNK];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(read) => each(&chunk[..read]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// The bytes inflated at a time by [`scan_entry`].
const SCAN_CHUNK: usize = 64 << 10;

/// The data of the entry `index` where it is stored, handed to `each` a window at a time as its
/// CRC is checked, or none where it is compressed or encrypted.
fn stored_entry<'a>(
    archive: &mut ZipArchive<Cursor<&'a [u8]>>,
    input: &'a [u8],
    pages: Pages<'a>,
    index: usize,
    mut each: impl FnMut(&[u8]),
) -> Result<Option<&'a [u8]>, String> {
    let raw = archive.by_index_raw(index).map_err(|e| e.to_string())?;
    if raw.compression() != CompressionMethod::Stored || raw.encrypted() {
        return Ok(None);
    }
    let start = raw
        .data_start()
        .and_then(|start| usize::try_from(start).ok());
    let length = usize::try_from(raw.compressed_size()).ok();
    let stored = start
        .zip(length)
        .and_then(|(start, length)| input.get(start..start.checked_add(length)?))
        .ok_or("its data lies outside the archive")?;
    let mut crc = flate2::Crc::new();
    Paging::new(pages).scan(stored, |window| {
        crc.update(window);
        each(window);
    });
    if crc.sum() != raw.crc32() {
        return Err("Invalid checksum".to_owned());
    }
    Ok(Some(stored))
}

/// How many records the central directory starting at `start` in `input` holds, or `None` where
/// one is cut short. The zip crate lists one entry for several records of one name, which this
/// count tells apart.
pub fn central_directory_records(input: &[u8], start: u64) -> Option<usize> {
    let mut position = usize::try_from(start).ok()?;
    let mut records = 0;
    while input.get(position..position + CENTRAL_DIRECTORY_SIGNATURE.len())
        == Some(CENTRAL_DIRECTORY_SIGNATURE)
    {
        let header = input.get(position..position + CENTRAL_DIRECTORY_HEADER_LENGTH)?;
        let length = |at: usize| usize::from(u16::from_le_bytes([header[at], header[at + 1]]));
        position += CENTRAL_DIRECTORY_HEADER_LENGTH + length(28) + length(30) + length(32);
        records += 1;
    }
    Some(records)
}
