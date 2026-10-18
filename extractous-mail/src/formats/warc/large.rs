//! Web archives too large to read whole, or that inflate into more than can be held in memory,
//! extracted under a memory budget far below their size: a record at a time.

use std::io::{self, Write};

use extractous_core::large::LargeFile;
use extractous_core::Extractor;

use crate::formats::archive::large::{Binary, BUDGET, END, LARGE};
use crate::input::MAX_READ_WHOLE;

/// Writes to `out` a response record of `id` whose payload of `length` bytes and `content_type`
/// `payload` writes.
fn response(
    out: &mut dyn Write,
    id: &str,
    content_type: &str,
    length: u64,
    payload: &mut dyn io::Read,
) -> io::Result<()> {
    let http = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\r\n");
    write!(
        out,
        "WARC/1.0\r\nWARC-Type: response\r\nWARC-Record-ID: {id}\r\n\
         WARC-Target-URI: http://example.com/\r\n\
         Content-Type: application/http; msgtype=response\r\nContent-Length: {}\r\n\r\n{http}",
        http.len() as u64 + length
    )?;
    io::copy(payload, out)?;
    out.write_all(b"\r\n\r\n")
}

/// Writes to `out` an archive of a binary payload of `large` bytes, then a text payload of
/// [`END`].
fn archive_of_large_payload(out: &mut dyn Write, large: u64) -> io::Result<()> {
    let binary = "application/octet-stream";
    response(out, "<urn:uuid:a>", binary, large, &mut Binary::new(large))?;
    let text = END.as_bytes();
    response(
        out,
        "<urn:uuid:b>",
        "text/plain",
        text.len() as u64,
        &mut &text[..],
    )
}

#[test]
fn reads_an_archive_too_large_to_read_whole_in_place() {
    let file = LargeFile::written("large.warc", |out| {
        archive_of_large_payload(out, MAX_READ_WHOLE + (16 << 20))
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, "extractous::WARCParser");
    assert!(text.contains(END), "{text}");
}

#[test]
fn inflates_a_gzipped_archive_past_the_memory_limit_to_a_spool() {
    let file = LargeFile::written("large.warc.gz", |out| {
        let mut gzip = flate2::write::GzEncoder::new(out, flate2::Compression::fast());
        archive_of_large_payload(&mut gzip, LARGE)?;
        gzip.finish()?.flush()
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, "extractous::WARCParser");
    assert!(text.contains(END), "{text}");
}
