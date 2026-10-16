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
use crate::compress::seven_z::tests::seven_z;
use crate::compress::snappy::tests::snappy;
use crate::compress::tar_header::tests::{pax, tar};
use crate::compress::xz::tests::{lzma, xz};
use crate::compress::zip::file::tests::zip;
use crate::compress::zlib::tests::zlib;
use crate::compress::zstd::tests::zstd;
use crate::formats::parity::ZSTD_WITHOUT_ZSTD_JNI;
use crate::formats::parity::{
    assert_same_as_tika, assert_same_as_tika_departing, conformance_corpus, documents,
    ComparedMetadata, Departure, Scratch,
};
use crate::formats::Parser;

const ROOTS: [&str; 2] = ["../test_files/documents", "../quality/corpus"];
const ZIP_PARSER: &str = "org.apache.tika.parser.pkg.ZipParser";
const COMPRESSOR_PARSER: &str = "org.apache.tika.parser.pkg.CompressorParser";
const PACKAGE_PARSER: &str = "org.apache.tika.parser.pkg.PackageParser";
const SEVEN_Z_PARSER: &str = "org.apache.tika.parser.pkg.SevenZParser";

/// Why the native path extracts a document of Tika's tests differently until `EventRecorder`
/// records the namespaces of the elements of the embedded parses it hands to Tika.
const FOREIGN_PARAGRAPHS: &str = "holds a document whose parser ends paragraphs outside the \
                                  XHTML namespace, which EventRecorder drops, so their replay \
                                  ends each with a line break";

/// The metadata of the ZIP integrity check.
const INTEGRITY_CHECK: [&str; 4] = [
    "zip:integrity-check-result",
    "zip:duplicate-entry-names",
    "zip:local-header-only-entries",
    "zip:central-directory-only-entries",
];

/// A native ZIP result as Tika's path writes it, with the integrity check as late as Tika runs
/// it (#99): after the document has ended, so not in its head, and not at all when the parse
/// failed, as at the write limit.
fn as_tika_checks_zips(
    text: &mut String,
    metadata: &mut ComparedMetadata,
    tika: &ComparedMetadata,
) {
    for name in INTEGRITY_CHECK {
        let meta = format!(r#"<meta name="{name}" content=""#);
        while let Some(start) = text.find(&meta) {
            let end = start
                + text[start..]
                    .find('\n')
                    .map_or(text.len() - start, |end| end + 1);
            text.replace_range(start..end, "");
        }
    }
    if !tika.iter().any(|(name, _)| name == INTEGRITY_CHECK[0]) {
        metadata.retain(|(name, _)| !INTEGRITY_CHECK.contains(&name.as_str()));
    }
}

/// Asserts parity on the documents of `parser` under `roots` it neither defers nor names in
/// `limited`, each of which it must parse, and returns the names of those it defers.
fn assert_parity(
    parser: &dyn Parser,
    tika_name: &str,
    roots: &[&Path],
    limited: &[(&str, &str)],
) -> Vec<String> {
    assert_parity_departing(parser, tika_name, roots, limited, None)
}

/// [`assert_parity`], with each native result rewritten by `departure`, if any.
fn assert_parity_departing(
    parser: &dyn Parser,
    tika_name: &str,
    roots: &[&Path],
    limited: &[(&str, &str)],
    departure: Option<Departure>,
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
    match departure {
        Some(departure) => assert_same_as_tika_departing(&paths, tika_name, departure),
        None => assert_same_as_tika(&paths, tika_name),
    }
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
    if tika_name == ZIP_PARSER {
        assert_same_as_tika_departing(&paths, tika_name, as_tika_checks_zips);
    } else {
        assert_same_as_tika(&paths, tika_name);
    }
}

/// Why the native path extracts a ZIP archive with an encrypted entry differently: Tika writes
/// the entry's name in a paragraph besides recording its failure (#99).
const ENCRYPTED_ENTRY_AS_TEXT: &str = "Tika writes the name of an encrypted entry as text";

#[test]
fn extracts_zip_archives_as_tikas_path_does() {
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let deferred = assert_parity_departing(
        &ZipParser,
        ZIP_PARSER,
        &roots,
        &[("encrypted-entry.zip", ENCRYPTED_ENTRY_AS_TEXT)],
        Some(as_tika_checks_zips),
    );
    assert_eq!(deferred, ["spanned.z01"]);
}

#[test]
fn records_encrypted_zip_entries_as_failures_only() {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string("../test_files/documents/encrypted/encrypted-entry.zip")
        .unwrap();
    assert!(xml.contains("unencrypted.txt"), "{xml}");
    assert!(!xml.contains("<p>encrypted.txt</p>"), "{xml}");
    let integrity = xml.find("zip:integrity-check-result").unwrap();
    assert!(integrity < xml.find("<body>").unwrap(), "{xml}");
    let failures = &metadata["tk:exception:embedded-stream-exception"];
    assert!(
        failures[0].contains("stream (encrypted.txt) is encrypted"),
        "{failures:?}"
    );
}

#[test]
fn reads_zip_entries_as_flagged_and_dated_without_a_zone() {
    use std::io::Write;
    // Tika takes a JAR only by its first entry, reads DOS times in the JVM's zone and takes
    // the UTF-8 flag only as a hint (#99).
    let mut writer = ::zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let time = ::zip::DateTime::from_date_and_time(2020, 6, 15, 12, 30, 10).unwrap();
    let options = ::zip::write::SimpleFileOptions::default().last_modified_time(time);
    for (name, content) in [
        ("caf\u{e9} \u{65e5}\u{672c}.txt", "text"),
        ("META-INF/MANIFEST.MF", "Manifest-Version: 1.0\n"),
    ] {
        writer.start_file(name, options).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    let archive = writer.finish().unwrap().into_inner();
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(&archive)
        .unwrap();
    assert_eq!(metadata["Content-Type"], ["application/java-archive"]);
    assert!(
        xml.contains("<h1>caf\u{e9} \u{65e5}\u{672c}.txt</h1>"),
        "{xml}"
    );
    let entry = crate::compress::zip::file::ZipFile::open(&archive).unwrap();
    let modified = entry.entries()[0].modified().unwrap();
    assert_eq!(
        crate::java::time::instant_string(modified.seconds, modified.nanos),
        "2020-06-15T12:30:10Z"
    );
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
    let deferred = assert_parity_departing(
        &ZipParser,
        ZIP_PARSER,
        &[&conformance_corpus()],
        &[
            ("testOpenOfficeInAZip.zip", FOREIGN_PARAGRAPHS),
            ("testZipEncrypted.zip", ENCRYPTED_ENTRY_AS_TEXT),
        ],
        Some(as_tika_checks_zips),
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
            ("notes.txt.xz", xz(text)),
            ("notes.lzma", lzma(text)),
        ],
    );
}

#[test]
fn decodes_zstandard_which_tikas_path_leaves_empty() {
    let scratch = Scratch::new("archive-zstd");
    let path = scratch.write("notes.txt.zst", &zstd(b"Zstandard text."));
    let (parsed, _) = crate::formats::parity::documents(&CompressorParser, &[scratch.directory()]);
    assert_eq!(
        parsed,
        std::slice::from_ref(&path),
        "{ZSTD_WITHOUT_ZSTD_JNI}"
    );
    let text = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap()
        .0;
    assert!(text.contains("Zstandard text."), "{text}");
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_compressed_files_as_tikas_path_does() {
    let deferred = assert_parity(
        &CompressorParser,
        COMPRESSOR_PARSER,
        &[&conformance_corpus()],
        &[("testZSTD.zst", ZSTD_WITHOUT_ZSTD_JNI)],
    );
    assert_eq!(
        deferred,
        ["TIKA-216.tgz", "quine.gz", "testLZMA_oom", "testZ_oom.Z"]
    );
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

/// A backslash is a character of a TAR name, which Tika turns into a slash (#101).
#[test]
fn keeps_backslashes_in_tar_names() {
    let archive = tar(&[(b"back\\slash.txt", b'0', b"text")]);
    let (xml, _) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(&archive)
        .unwrap();
    assert!(xml.contains("id=\"back\\slash.txt\""), "{xml}");
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
    assert_eq!(
        deferred,
        ["test-documents.cpio"],
        "an old binary cpio archive"
    );
}

#[test]
fn extracts_7z_archives_as_tikas_path_does() {
    let text = "Archived text, caf\u{e9}.\nSecond line.\n".as_bytes();
    let html = b"<html><head><title>Inner</title></head><body><p>Inner page</p></body></html>";
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let deferred = assert_parity(&SevenZParser, SEVEN_Z_PARSER, &roots, &[]);
    assert_eq!(deferred, ["password.7z"]);
    assert_synthetic_parity(
        "archive-7z",
        &SevenZParser,
        SEVEN_Z_PARSER,
        &[(
            "files.7z",
            seven_z(&[
                ("dir/", b""),
                ("dir/a.txt", text),
                ("empty.txt", b""),
                ("page.html", html),
            ]),
        )],
    );
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_7z_archives_as_tikas_path_does() {
    let deferred = assert_parity(&SevenZParser, SEVEN_Z_PARSER, &[&conformance_corpus()], &[]);
    assert_eq!(
        deferred,
        ["full_encrypted.7z", "test7Z_protected_passTika.7z"]
    );
}
