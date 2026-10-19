//! Web archives too large to read whole, or that inflate into more than can be held in memory,
//! extracted under a memory budget far below their size: a record at a time.

use std::io::{self, Write};

use dijo_core::large::{bytes, zip_archive, Binary, LargeFile, BUDGET, END};
use dijo_core::Extractor;
use zip::CompressionMethod::Deflated;

use crate::formats::archive::large::LARGE;
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
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, "dijo::WARCParser");
    assert!(text.contains(END), "{text}");
}

#[test]
fn inflates_a_gzipped_archive_past_the_memory_limit_to_a_spool() {
    let file = LargeFile::written("large.warc.gz", |out| {
        let mut gzip = flate2::write::GzEncoder::new(out, flate2::Compression::fast());
        archive_of_large_payload(&mut gzip, LARGE)?;
        gzip.finish()?.flush()
    });
    let (text, _) = file.extract_with(&Extractor::new(), file.budget(), "dijo::WARCParser");
    assert!(text.contains(END), "{text}");
}

/// A collection whose archive is deflated, and inflates past what a stream may decode into
/// memory, is read with the archive spooled, a record at a time.
#[test]
fn spools_a_collection_archive_past_the_memory_limit() {
    let file = LargeFile::written("large.wacz", |out| {
        zip_archive(
            out,
            vec![
                (
                    "datapackage.json",
                    Deflated,
                    bytes(br#"{"profile": "data-package", "resources": []}"#),
                ),
                (
                    "archive/data.warc",
                    Deflated,
                    Box::new(|out| archive_of_large_payload(out, LARGE)),
                ),
            ],
        )
    });
    let (text, metadata) = file.extract_with(&Extractor::new(), file.budget(), "dijo::WACZParser");
    assert!(text.contains(END), "{text}");
    assert!(metadata["tk:parsed-by-full-set"]
        .iter()
        .any(|parser| parser == "dijo::WARCParser"));
}
