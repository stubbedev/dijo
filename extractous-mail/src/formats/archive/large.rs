//! Archives and compressed files too large to read whole, or holding entries too large to
//! decode into memory, extracted under a memory budget far below their size.

use std::io::{self, Read, Write};

use extractous_core::large::{LargeFile, BUDGET};

use crate::compress::MAX_DECODED;
use crate::input::MAX_READ_WHOLE;

/// The text of the entry after the large one, which only an archive read to its end writes.
pub(crate) const END: &str = "the end of the archive";

/// The size of the large entry: well past what a stream may decode into memory.
pub(crate) const LARGE: u64 = MAX_DECODED as u64 + (64 << 20);

/// The budget of an extraction that decodes a stream of unknown length, which holds as much of
/// what it decodes in memory as a file read whole before it spools the rest to a file.
pub(crate) const SPOOLING_BUDGET: u64 = MAX_READ_WHOLE + BUDGET;

/// A large entry's data: binary, so that no parser writes it out, and compressing well, lines
/// of 64 bytes that each hold their number among bytes that repeat.
pub(crate) struct Binary {
    at: u64,
    length: u64,
}

impl Binary {
    pub(crate) fn new(length: u64) -> Self {
        Self { at: 0, length }
    }
}

impl Read for Binary {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
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

/// Writes to `out` a TAR archive of a binary entry of `large` bytes, then a text entry of
/// [`END`].
pub(crate) fn tar_of_large_entry(out: impl Write, large: u64) -> io::Result<()> {
    let mut builder = tar::Builder::new(out);
    let mut header = tar::Header::new_gnu();
    header.set_size(large);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "large.bin", Binary::new(large))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(END.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "end.txt", END.as_bytes())?;
    builder.into_inner()?.flush()
}

#[test]
fn reads_a_tar_archive_too_large_to_read_whole_in_place() {
    let file = LargeFile::written("large.tar", |out| {
        tar_of_large_entry(out, MAX_READ_WHOLE + (16 << 20))
    });
    let (text, _) = file.extract("extractous::PackageParser");
    assert!(text.contains(END), "{text}");
}

#[test]
fn decodes_a_gzipped_entry_past_the_memory_limit_to_a_spool() {
    let file = LargeFile::written("large.tar.gz", |out| {
        let mut gzip = flate2::write::GzEncoder::new(out, flate2::Compression::fast());
        tar_of_large_entry(&mut gzip, LARGE)?;
        gzip.finish()?.flush()
    });
    let extractor = extractous_core::Extractor::new();
    let (text, metadata) =
        file.extract_with(&extractor, SPOOLING_BUDGET, "extractous::CompressorParser");
    assert!(text.contains(END), "{text}");
    assert!(metadata["tk:parsed-by"]
        .iter()
        .any(|parser| parser == "extractous::PackageParser"));
}
