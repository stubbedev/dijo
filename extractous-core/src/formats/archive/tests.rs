//! The archive parsers compared with Tika's path: on the archives of the repository's corpora,
//! on synthetic ones written for what the corpora lack, and on Tika's own test documents.

use std::path::{Path, PathBuf};

use super::*;
use crate::compress::ar::tests::ar;
use crate::compress::bzip2::tests::bzip2;
use crate::compress::cpio::tests::cpio;
use crate::compress::gzip::tests::gzip;
use crate::compress::lz4::tests::lz4;
use crate::compress::lzw::tests::encode as compress;
use crate::compress::snappy::tests::snappy;
use crate::compress::tar::tests::{pax, tar};
use crate::compress::zip::file::tests::zip;
use crate::compress::zlib::tests::zlib;
use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};
use crate::formats::Parser;

const ROOTS: [&str; 2] = ["../test_files/documents", "../quality/corpus"];
const ZIP_PARSER: &str = "org.apache.tika.parser.pkg.ZipParser";
const COMPRESSOR_PARSER: &str = "org.apache.tika.parser.pkg.CompressorParser";
const PACKAGE_PARSER: &str = "org.apache.tika.parser.pkg.PackageParser";

/// Why the native path extracts a document of Tika's tests differently until `EventRecorder`
/// records the namespaces of the elements of the embedded parses it hands to Tika.
const FOREIGN_PARAGRAPHS: &str = "holds a document whose parser ends paragraphs outside the \
                                  XHTML namespace, which EventRecorder drops, so their replay \
                                  ends each with a line break";

/// Asserts parity on the documents of `parser` under `roots` it neither defers nor names in
/// `limited`, each of which it must parse, and returns the names of those it defers.
fn assert_parity(
    parser: &dyn Parser,
    tika_name: &str,
    roots: &[&Path],
    limited: &[(&str, &str)],
) -> Vec<String> {
    let (parsed, deferred) = documents(parser, roots);
    eprintln!(
        "{tika_name}: {} parsed, deferred {deferred:?}",
        parsed.len()
    );
    let (excluded, compared): (Vec<PathBuf>, Vec<PathBuf>) = parsed
        .into_iter()
        .partition(|path| limited.iter().any(|(name, _)| path.ends_with(name)));
    assert_eq!(
        excluded.len(),
        limited.len(),
        "parsed only {excluded:?} of the documents excluded from the comparison"
    );
    let paths: Vec<&Path> = compared.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, tika_name);
    names(&deferred)
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect()
}

/// Writes `documents` and asserts that `parser` parses each exactly as Tika's path does.
fn assert_synthetic_parity(
    test: &str,
    parser: &dyn Parser,
    tika_name: &str,
    documents: &[(&str, Vec<u8>)],
) {
    let scratch = Scratch::new(test);
    for (name, bytes) in documents {
        scratch.write(name, bytes);
    }
    let (parsed, deferred) = crate::formats::parity::documents(parser, &[scratch.directory()]);
    assert_eq!(names(&deferred), Vec::<String>::new());
    let mut expected: Vec<&str> = documents.iter().map(|(name, _)| *name).collect();
    expected.sort_unstable();
    assert_eq!(names(&parsed), expected);
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, tika_name);
}

#[test]
fn extracts_zip_archives_as_tikas_path_does() {
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let deferred = assert_parity(&ZipParser, ZIP_PARSER, &roots, &[]);
    assert_eq!(deferred, ["spanned.z01"]);
}

#[test]
fn extracts_synthetic_zip_archives_as_tikas_path_does() {
    let long = "A line of text that the short write limit cuts.\n".repeat(4);
    assert_synthetic_parity(
        "archive-zips",
        &ZipParser,
        ZIP_PARSER,
        &[
            ("long.zip", zip(&[("long.txt", long.as_bytes())])),
            (
                "folders.zip",
                zip(&[
                    ("a/", b""),
                    ("a/b.txt", b"In a folder."),
                    ("c.html", b"<p>Page</p>"),
                ]),
            ),
        ],
    );
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_zip_archives_as_tikas_path_does() {
    let deferred = assert_parity(
        &ZipParser,
        ZIP_PARSER,
        &[&conformance_corpus()],
        &[("testOpenOfficeInAZip.zip", FOREIGN_PARAGRAPHS)],
    );
    assert_eq!(
        deferred,
        [
            "droste.zip",
            "moby.zip",
            "test-documents-spanned.z01",
            "test-documents-spanned.zip",
            "testZip_with_DataDescriptor.zip"
        ]
    );
}

#[test]
fn extracts_compressed_files_as_tikas_path_does() {
    let text = "Compressed text, caf\u{e9}.\nSecond line.\n".as_bytes();
    let html = b"<html><head><title>Inner</title></head><body><p>Inner page</p></body></html>";
    let svg =
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><title>Drawing</title><text>Label</text></svg>";
    let mut latin1_name = gzip(text, Some("caf\u{e9}.txt"));
    let at = latin1_name
        .windows(2)
        .position(|w| w == [0xC3, 0xA9])
        .unwrap();
    latin1_name.splice(at..at + 2, [0xE9]);
    let mut two_members = gzip(b"first member", Some("a"));
    two_members.extend_from_slice(&gzip(b"second member", Some("b")));
    assert_synthetic_parity(
        "archive-compressors",
        &CompressorParser,
        COMPRESSOR_PARSER,
        &[
            ("named.gz", gzip(html, Some("inner.html"))),
            ("plain.txt.gz", gzip(text, None)),
            ("blank-name.gz", gzip(text, Some(" "))),
            ("latin1-name.gz", latin1_name),
            ("two-members.gz", two_members),
            ("drawing.svgz", gzip(svg, None)),
            ("page.html-gz", gzip(html, None)),
            ("notes.txt.bz2", bzip2(text)),
            ("notes.zlib", zlib(text)),
            ("notes.txt.Z", compress(&text.repeat(3))),
            ("notes.sz", snappy(text)),
            ("notes.lz4", lz4(text)),
        ],
    );
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_compressed_files_as_tikas_path_does() {
    let deferred = assert_parity(
        &CompressorParser,
        COMPRESSOR_PARSER,
        &[&conformance_corpus()],
        &[],
    );
    assert_eq!(deferred, ["quine.gz", "testZ_oom.Z"]);
}

#[test]
fn extracts_tar_ar_and_cpio_archives_as_tikas_path_does() {
    let text = "Archived text, caf\u{e9}.\nSecond line.\n".as_bytes();
    let html = b"<html><head><title>Inner</title></head><body><p>Inner page</p></body></html>";
    let long = format!("{}long.txt\0", "folder/".repeat(20));
    let pax_path = pax(&[("path", "/caf\u{e9}/pax.txt"), ("mtime", "1700000000.75")]);
    assert_synthetic_parity(
        "archive-packages",
        &PackageParser,
        PACKAGE_PARSER,
        &[
            (
                "files.tar",
                tar(&[
                    (b"dir/", b'5', b""),
                    (b"dir/a.txt", b'0', text),
                    (b"page.html", b'0', html),
                    (b"././@PaxHeader", b'x', &pax_path),
                    (b"pax", b'0', text),
                    (b"././@LongLink", b'L', long.as_bytes()),
                    (b"cut", b'0', html),
                    (b"back\\slash.txt", b'0', text),
                    (b"link", b'2', b""),
                ]),
            ),
            (
                "files.a",
                ar(&[("a.txt", text), ("a-rather-long-file-name.html", html)]),
            ),
            (
                "files.cpio",
                cpio(&[("dir/", b""), ("dir/a.txt", text), ("page.html", html)]),
            ),
        ],
    );
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_tar_ar_and_cpio_archives_as_tikas_path_does() {
    let deferred = assert_parity(
        &PackageParser,
        PACKAGE_PARSER,
        &[&conformance_corpus()],
        &[],
    );
    assert_eq!(deferred, Vec::<String>::new());
}
