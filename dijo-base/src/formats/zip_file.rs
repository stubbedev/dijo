//! Entries of a ZIP archive as the zip crate reads them, for the OOXML packages read with it: an
//! entry's content, stored data read in place, and the records of the central directory, which
//! tell apart several entries of one name that the crate lists as one.

use std::io::{self, Cursor};

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
    let raw = archive.by_index_raw(index).map_err(|e| e.to_string())?;
    if raw.compression() == CompressionMethod::Stored && !raw.encrypted() {
        let start = raw
            .data_start()
            .and_then(|start| usize::try_from(start).ok());
        let length = usize::try_from(raw.compressed_size()).ok();
        let stored = start
            .zip(length)
            .and_then(|(start, length)| input.get(start..start.checked_add(length)?))
            .ok_or("its data lies outside the archive")?;
        let paging = Paging::new(pages);
        let mut crc = flate2::Crc::new();
        paging.scan(stored, |window| crc.update(window));
        if crc.sum() != raw.crc32() {
            return Err("Invalid checksum".to_owned());
        }
        return Ok(Input::Within(stored, pages));
    }
    drop(raw);
    let mut file = archive.by_index(index).map_err(|e| e.to_string())?;
    let mut spool = Spool::new(Some(file.size()));
    io::copy(&mut file, &mut spool).map_err(|e| e.to_string())?;
    spool.finish().map_err(|e| e.to_string())
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
