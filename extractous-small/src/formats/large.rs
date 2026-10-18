//! What the tests of the parsers of ZIP packages share to write packages too large to read
//! whole: a part of noise past what a spool holds in memory, which spools to a file, and the
//! package itself, too large to read whole as deflate barely compresses the noise.

use std::io::{self, Write};

use crate::input::MAX_READ_WHOLE;

/// The text after the large part, which only a package read to its end writes.
pub(crate) const END: &str = "the end of the document";
/// Twice what a spool holds in memory, so that a part this large spools to a file.
pub(crate) const LARGE: u64 = 2 * MAX_READ_WHOLE;
/// The characters [`noise`] draws from.
const NOISE_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789 .";

/// Writes to `out` at least `length` bytes of `element` elements, each on a line of its own,
/// holding characters that deflate barely compresses, drawn from a fixed sequence.
pub(crate) fn noise(out: &mut dyn Write, element: &str, length: u64) -> io::Result<()> {
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
pub(crate) type EntryData<'a> = Box<dyn FnOnce(&mut dyn Write) -> io::Result<()> + 'a>;

/// The data of an entry that holds `bytes`.
pub(crate) fn bytes(bytes: &[u8]) -> EntryData<'_> {
    Box::new(move |out| out.write_all(bytes))
}

/// Writes to `out` a ZIP archive of `entries`, each with its name and what writes its data, the
/// first stored, as a package's `mimetype` is, the others deflated.
pub(crate) fn package(out: &mut dyn Write, entries: Vec<(&str, EntryData<'_>)>) -> io::Result<()> {
    zip_archive(
        out,
        entries
            .into_iter()
            .enumerate()
            .map(|(index, (name, data))| {
                let method = if index == 0 {
                    zip::CompressionMethod::Stored
                } else {
                    zip::CompressionMethod::Deflated
                };
                (name, method, data)
            })
            .collect(),
    )
}

/// Writes to `out` a ZIP archive of `entries`, each with its name, how it is compressed and
/// what writes its data.
pub(crate) fn zip_archive(
    out: &mut dyn Write,
    entries: Vec<(&str, zip::CompressionMethod, EntryData<'_>)>,
) -> io::Result<()> {
    let mut zip = zip::ZipWriter::new_stream(out);
    for (name, method, data) in entries {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(method)
            .compression_level(Some(1));
        zip.start_file(name, options).map_err(io::Error::other)?;
        data(&mut zip)?;
    }
    zip.finish().map_err(io::Error::other)?.flush()
}
