//! The archive parsers compared with Tika's path: on the archives of the repository's corpora,
//! on synthetic ones written for what the corpora lack, and on Tika's own test documents.

#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::compress::ar::tests::ar;
#[cfg(feature = "tika")]
use crate::compress::bzip2::tests::bzip2;
#[cfg(feature = "tika")]
use crate::compress::cpio::tests::cpio;
#[cfg(feature = "tika")]
use crate::compress::gzip::tests::gzip;
#[cfg(feature = "tika")]
use crate::compress::lz4::tests::lz4;
#[cfg(feature = "tika")]
use crate::compress::lzw::tests::encode as compress;
#[cfg(feature = "tika")]
use crate::compress::seven_z::tests::seven_z;
#[cfg(feature = "tika")]
use crate::compress::snappy::tests::snappy;
#[cfg(feature = "tika")]
use crate::compress::tar_header::tests::pax;
use crate::compress::tar_header::tests::tar;
#[cfg(feature = "tika")]
use crate::compress::xz::tests::{lzma, xz};
use crate::compress::zip::file::tests::zip;
#[cfg(feature = "tika")]
use crate::compress::zlib::tests::zlib;
use crate::compress::zstd::tests::zstd;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_same_as_tika, assert_same_as_tika_rewriting, conformance_corpus, documents,
    NativeRewrite,
};
use crate::formats::parity::{Scratch, ZSTD_WITHOUT_ZSTD_JNI};
#[cfg(feature = "tika")]
use crate::formats::Parser;

#[cfg(feature = "tika")]
const ROOTS: [&str; 2] = ["../test_files/documents", "../quality/corpus"];
#[cfg(feature = "tika")]
const ZIP_PARSER: &str = "org.apache.tika.parser.pkg.ZipParser";
#[cfg(feature = "tika")]
const COMPRESSOR_PARSER: &str = "org.apache.tika.parser.pkg.CompressorParser";
#[cfg(feature = "tika")]
const PACKAGE_PARSER: &str = "org.apache.tika.parser.pkg.PackageParser";
#[cfg(feature = "tika")]
const SEVEN_Z_PARSER: &str = "org.apache.tika.parser.pkg.SevenZParser";

/// Why the native path extracts a document of Tika's tests differently until `EventRecorder`
/// records the namespaces of the elements of the embedded parses it hands to Tika.
#[cfg(feature = "tika")]
const FOREIGN_PARAGRAPHS: &str = "holds a document whose parser ends paragraphs outside the \
                                  XHTML namespace, which EventRecorder drops, so their replay \
                                  ends each with a line break";

/// Asserts parity on the documents of `parser` under `roots` it neither defers nor names in
/// `limited`, each of which it must parse, and returns the names of those it defers.
#[cfg(feature = "tika")]
fn assert_parity(
    parser: &dyn Parser,
    tika_name: &str,
    roots: &[&Path],
    limited: &[(&str, &str)],
) -> Vec<String> {
    assert_parity_departing(parser, tika_name, roots, limited, None)
}

/// [`assert_parity`], with each native result rewritten by `departure`, if any.
#[cfg(feature = "tika")]
fn assert_parity_departing(
    parser: &dyn Parser,
    tika_name: &str,
    roots: &[&Path],
    limited: &[(&str, &str)],
    departure: Option<NativeRewrite>,
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
        Some(departure) => assert_same_as_tika_rewriting(&paths, tika_name, departure),
        None => assert_same_as_tika(&paths, tika_name),
    }
    names(&deferred)
}

#[cfg(feature = "tika")]
fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect()
}

/// Writes `documents` and asserts that `parser` parses each exactly as Tika's path does.
#[cfg(feature = "tika")]
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
        assert_same_as_tika_rewriting(&paths, tika_name, as_tika_checks_zips);
    } else {
        assert_same_as_tika(&paths, tika_name);
    }
}

/// Why the native path extracts a ZIP archive with an encrypted entry differently: Tika writes
/// the entry's name in a paragraph besides recording its failure (#99).
#[cfg(feature = "tika")]
const ENCRYPTED_ENTRY_AS_TEXT: &str = "Tika writes the name of an encrypted entry as text";

#[cfg(feature = "tika")]
const PLIST_UIDS: &str = "holds keyed archives, whose UIDs the native property list parser \
                          names by value where Tika names them by their object (#104)";

/// Why the native path extracts the first segment of a split ZIP archive where Tika's path
/// fails on it: Tika's stream fails the document at the entry the segment cuts short, the
/// native parser records that entry as unreadable and keeps the entries before it.
#[cfg(feature = "tika")]
const SPLIT_SEGMENT: &str = "Tika fails the document at the entry the segment cuts short";

#[cfg(feature = "tika")]
#[test]
fn extracts_zip_archives_as_tikas_path_does() {
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let deferred = assert_parity_departing(
        &ZipParser,
        ZIP_PARSER,
        &roots,
        &[
            ("encrypted-entry.zip", ENCRYPTED_ENTRY_AS_TEXT),
            ("spanned.z01", SPLIT_SEGMENT),
        ],
        Some(as_tika_checks_zips),
    );
    assert!(deferred.is_empty(), "{deferred:?}");
}

#[test]
fn reads_the_first_segment_of_a_split_zip_up_to_the_entry_it_cuts_short() {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string("../test_files/documents/spanned.z01")
        .unwrap();
    let entries: Vec<&str> = xml
        .split(r#"<div class="embedded" id=""#)
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect();
    assert_eq!(
        entries,
        [
            "testEXCEL.xls",
            "testHTML.html",
            "testOpenOffice2.odt",
            "testPDF.pdf",
            "testPPT.ppt"
        ]
    );
    assert!(xml.contains("Sample Excel Worksheet"), "{xml}");
    assert_eq!(metadata["zip:integrity-check-result"], ["PARTIAL"]);
    let failures = &metadata["tk:exception:embedded-stream-exception"];
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("(testRTF.rtf): Truncated ZIP file"),
        "{failures:?}"
    );
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

#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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
            ("testIPA.ipa", PLIST_UIDS),
            ("test-documents-spanned.z01", SPLIT_SEGMENT),
        ],
        Some(as_tika_checks_zips),
    );
    assert_eq!(deferred, ["droste.zip"]);
}

#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
#[test]
fn extracts_7z_archives_as_tikas_path_does() {
    let text = "Archived text, caf\u{e9}.\nSecond line.\n".as_bytes();
    let html = b"<html><head><title>Inner</title></head><body><p>Inner page</p></body></html>";
    let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
    let (parsed, deferred) = documents(&SevenZParser, &roots);
    assert!(deferred.is_empty(), "{deferred:?}");
    crate::formats::parity::assert_same_as_tika_with_passwords_but(
        &parsed,
        SEVEN_Z_PARSER,
        &[("password.7z", "extractous")],
        crate::formats::parity::WrongPassword::Defers,
    );
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

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_7z_archives_as_tikas_path_does() {
    let (parsed, deferred) = documents(&SevenZParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    // Its headers are encrypted, with a password Tika's tests do not name.
    let (headers_encrypted, parsed): (Vec<PathBuf>, Vec<PathBuf>) = parsed
        .into_iter()
        .partition(|path| path.ends_with("full_encrypted.7z"));
    assert_eq!(headers_encrypted.len(), 1);
    crate::formats::parity::assert_fails_as_tika(&[headers_encrypted[0].as_path()]);
    crate::formats::parity::assert_same_as_tika_with_passwords_but(
        &parsed,
        SEVEN_Z_PARSER,
        &[("test7Z_protected_passTika.7z", "Tika")],
        crate::formats::parity::WrongPassword::Defers,
    );
}

/// What extracting `archive` streams, and how the extraction ends: the error it fails with, or
/// the failures its metadata records.
fn extract(archive: &[u8]) -> (String, Result<Vec<String>, crate::Error>) {
    let (mut reader, _) = crate::Extractor::new()
        .extract_bytes(archive)
        .expect("the extraction starts");
    let mut text = Vec::new();
    let read = std::io::Read::read_to_end(&mut reader, &mut text);
    let text = String::from_utf8(text).expect("the text is UTF-8");
    let ended = match read {
        Ok(_) => {
            let metadata = reader
                .metadata()
                .expect("the metadata of a finished extraction");
            Ok(metadata
                .into_iter()
                .filter(|(key, _)| key.starts_with("tk:exception:"))
                .flat_map(|(_, values)| values)
                .collect())
        }
        Err(error) => Err(*error
            .into_inner()
            .expect("the stream fails with the extraction's error")
            .downcast::<crate::Error>()
            .expect("the stream fails with an extraction error")),
    };
    (text, ended)
}

/// Asserts that the extraction `ended` at the zip bomb limit of Tika's `SecureContentHandler`
/// for `cause`. The native path fails with [`Error::LimitExceeded`]. Tika's path, which takes a
/// zip bomb in a build that has it, fails with its exception where the limit is reached in an
/// entry of the archive, and records it where it is reached deeper.
fn assert_zip_bomb(ended: Result<Vec<String>, crate::Error>, cause: &str) {
    let message = match ended {
        Err(Error::LimitExceeded(message)) => message,
        Err(error) if cfg!(feature = "tika") => error.to_string(),
        Ok(recorded) if cfg!(feature = "tika") => recorded.concat(),
        other => panic!("not stopped as a zip bomb: {other:?}"),
    };
    assert!(message.contains("Suspected zip bomb"), "{message}");
    assert!(message.contains(cause), "{message}");
}

#[test]
fn leaves_a_stream_decoding_past_the_most_the_native_path_decodes_to_a_limit() {
    let declined = undecoded("the Gzip stream", Undecoded::TooLarge);
    if cfg!(feature = "tika") {
        assert!(
            matches!(&declined, Declined::Deferred(reason) if reason.contains("Tika's path streams")),
            "{declined:?}"
        );
    } else {
        assert!(
            matches!(&declined, Declined::Fails(Error::LimitExceeded(reason))
                if reason.starts_with("the Gzip stream: it decodes into more than")),
            "{declined:?}"
        );
    }
}

#[test]
fn stops_archives_nested_ten_package_entries_deep_with_what_they_wrote() {
    let mut archive = zip(&[("level1.txt", b"Text at nesting level 1.")]);
    for level in 2..=300 {
        let text = format!("Text at nesting level {level}.");
        archive = zip(&[
            (&format!("level{level}.txt"), text.as_bytes()),
            ("inner.zip", &archive),
        ]);
    }
    let (text, ended) = extract(&archive);
    assert!(text.contains("Text at nesting level 300."), "{text}");
    assert!(text.contains("Text at nesting level 292."), "{text}");
    assert!(!text.contains("Text at nesting level 291."), "{text}");
    assert_zip_bomb(ended, "10 levels of package entry nesting");
}

#[test]
fn stops_an_entry_that_inflates_a_hundredfold_past_a_million_characters() {
    let line = "All work and no play makes Jack a dull boy.\n";
    let bomb = zip(&[("bomb.txt", line.repeat(700_000).as_bytes())]);
    assert!(bomb.len() * 100 < line.len() * 700_000, "{}", bomb.len());
    let (text, ended) = extract(&bomb);
    assert!(text.contains(line), "{}", &text[..100.min(text.len())]);
    assert!(text.len() > 1_000_000, "{}", text.len());
    assert!(text.len() < line.len() * 700_000);
    assert_zip_bomb(ended, "output characters");
}
