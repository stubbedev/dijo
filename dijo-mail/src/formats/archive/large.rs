//! Archives and compressed files too large to read whole, or holding entries too large to
//! decode into memory, extracted under a memory budget far below their size.

use std::io::{self, Write};

use dijo_core::large::{bytes, zip_archive, Binary, LargeFile, END};

use crate::compress::MAX_DECODED;
use crate::input::MAX_READ_WHOLE;

/// The size of the large entry: well past what a stream may decode into memory.
pub(crate) const LARGE: u64 = MAX_DECODED as u64 + (64 << 20);

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
    let (text, _) = file.extract("dijo::PackageParser");
    assert!(text.contains(END), "{text}");
}

#[test]
fn decodes_a_gzipped_entry_past_the_memory_limit_to_a_spool() {
    let file = LargeFile::written("large.tar.gz", |out| {
        let mut gzip = flate2::write::GzEncoder::new(out, flate2::Compression::fast());
        tar_of_large_entry(&mut gzip, LARGE)?;
        gzip.finish()?.flush()
    });
    let extractor = dijo_core::Extractor::new();
    let (text, metadata) = file.extract_with(&extractor, file.budget(), "dijo::CompressorParser");
    assert!(text.contains(END), "{text}");
    assert!(metadata["tk:parsed-by-full-set"]
        .iter()
        .any(|parser| parser == "dijo::PackageParser"));
}

/// Writes to `out` a ZIP archive of a binary entry of `large` bytes, then a text entry of
/// [`END`], both compressed with `method`.
fn zip_of_large_entry(
    out: &mut dyn Write,
    large: u64,
    method: zip::CompressionMethod,
) -> io::Result<()> {
    zip_archive(
        out,
        vec![
            (
                "large.bin",
                method,
                Box::new(move |out| io::copy(&mut Binary::new(large), out).map(drop)),
            ),
            ("end.txt", method, bytes(END.as_bytes())),
        ],
    )
}

#[test]
fn reads_a_zip_archive_too_large_to_read_whole_storing_its_entry_in_place() {
    let file = LargeFile::written("large.zip", |out| {
        zip_of_large_entry(
            out,
            MAX_READ_WHOLE + (16 << 20),
            zip::CompressionMethod::Stored,
        )
    });
    let (text, _) = file.extract("dijo::ZipParser");
    assert!(text.contains(END), "{text}");
}

#[test]
fn spools_a_deflated_zip_entry_past_the_memory_limit() {
    let file = LargeFile::written("deflated.zip", |out| {
        zip_of_large_entry(out, LARGE, zip::CompressionMethod::Deflated)
    });
    let extractor = dijo_core::Extractor::new();
    let (text, _) = file.extract_with(&extractor, file.budget(), "dijo::ZipParser");
    assert!(text.contains(END), "{text}");
}

/// A 7z entry too large to hold in memory, though within what the native path decodes of so
/// small an archive, is spooled to a file, and the entry after it read.
#[test]
fn spools_a_7z_entry_past_the_memory_limit_one_entry_at_a_time() {
    let file = LargeFile::written("large.7z", |out| {
        let archive = crate::compress::seven_z::tests::seven_z_streamed(&mut [
            ("large.bin", &mut Binary::new(MAX_READ_WHOLE + (16 << 20))),
            ("end.txt", &mut END.as_bytes()),
        ]);
        out.write_all(&archive)
    });
    let extractor = dijo_core::Extractor::new();
    let (text, _) = file.extract_with(&extractor, file.budget(), "dijo::SevenZParser");
    assert!(text.contains(END), "{text}");
}
