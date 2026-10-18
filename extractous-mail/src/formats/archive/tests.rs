//! The archive parsers compared with Tika's path: on the archives of the repository's corpora,
//! on synthetic ones written for what the corpora lack, and on Tika's own test documents.

#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::compress::ar::tests::ar;
#[cfg(feature = "tika")]
use crate::compress::arj::tests::{arj, with_stray_bytes};
#[cfg(feature = "tika")]
use crate::compress::brotli::tests::brotli;
#[cfg(feature = "tika")]
use crate::compress::bzip2::tests::bzip2;
#[cfg(feature = "tika")]
use crate::compress::cpio::tests::cpio;
use crate::compress::gzip::tests::gzip;
#[cfg(feature = "tika")]
use crate::compress::lz4::tests::lz4;
#[cfg(feature = "tika")]
use crate::compress::lzw::tests::encode as compress;
#[cfg(feature = "tika")]
use crate::compress::seven_z::tests::seven_z;
#[cfg(feature = "tika")]
use crate::compress::snappy::tests::snappy;
use crate::compress::tar_header::tests::tar;
#[cfg(feature = "tika")]
use crate::compress::tar_header::tests::{
    ended, old_gnu_sparse, pax, records, sparse_1_0_filling_a_record,
};
#[cfg(feature = "tika")]
use crate::compress::xz::tests::{lzma, xz};
use crate::compress::zip::file::tests::zip;
#[cfg(feature = "tika")]
use crate::compress::zlib::tests::zlib;
use crate::compress::zstd::tests::zstd;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_same_as_tika, assert_same_as_tika_departing_rewriting, assert_same_as_tika_rewriting,
    conformance_corpus, documents, NativeRewrite,
};
use crate::formats::parity::{Scratch, ZSTD_WITHOUT_ZSTD_JNI};
#[cfg(feature = "tika")]
use crate::formats::{class_file, Parser};

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

/// [`assert_parity`], with each native result rewritten by `rewrite`, if any, and each archive
/// compared [by its contents](assert_same_as_tika_by_contents).
#[cfg(feature = "tika")]
fn assert_parity_departing(
    parser: &dyn Parser,
    tika_name: &str,
    roots: &[&Path],
    limited: &[(&str, &str)],
    rewrite: Option<NativeRewrite>,
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
    let compared: Vec<&Path> = compared.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_by_contents(&compared, tika_name, rewrite);
    names(&deferred)
}

/// How the comparison of an archive departs for the documents it holds.
#[cfg(feature = "tika")]
#[derive(PartialEq)]
struct Contents {
    /// Whether it is one of the
    /// [`ANCHORED_BLOCK_DOCUMENTS`](crate::formats::parity::ANCHORED_BLOCK_DOCUMENTS), whose
    /// blocks fall otherwise (#136).
    anchored_blocks: bool,
    /// Whether it holds class files, whose access keywords Tika misreads (#132).
    class_files: bool,
}

/// Asserts that the native parser of each of `archives`, which Tika's path parses with
/// `tika_name`, extracts what Tika's path does, each native result rewritten by `rewrite`, if
/// any, and each archive departing as the documents it holds depart, by its [`Contents`].
#[cfg(feature = "tika")]
fn assert_same_as_tika_by_contents(
    archives: &[&Path],
    tika_name: &str,
    rewrite: Option<NativeRewrite>,
) {
    let mut groups: Vec<(Contents, Vec<&Path>)> = Vec::new();
    for archive in archives {
        let contents = Contents {
            anchored_blocks: crate::formats::parity::ANCHORED_BLOCK_DOCUMENTS
                .iter()
                .any(|name| archive.ends_with(name)),
            class_files: parsers_of(archive)
                .iter()
                .any(|name| name == class_file::ClassParser.name()),
        };
        match groups.iter_mut().find(|(group, _)| *group == contents) {
            Some((_, archives)) => archives.push(archive),
            None => groups.push((contents, vec![archive])),
        }
    }
    for (contents, archives) in &groups {
        let rewrite = if contents.class_files {
            Some((
                rewrite.unwrap_or(class_file::as_written),
                Some(class_file::without_misread_access as fn(&mut String)),
            ))
        } else {
            rewrite.map(|rewrite| (rewrite, None))
        };
        let departures = if contents.anchored_blocks {
            crate::formats::parity::ANCHORED_BLOCKS
        } else {
            &[]
        };
        assert_same_as_tika_departing_rewriting(archives, tika_name, departures, rewrite);
    }
}

/// The native names of the parsers the native path hands the document at `path`, and those it
/// holds at any depth, to; none where the native path fails on it, which leaves no text to
/// compare.
#[cfg(feature = "tika")]
fn parsers_of(path: &Path) -> Vec<String> {
    crate::Extractor::new()
        .extract_file_to_string(path.to_str().expect("a UTF-8 path"))
        .map(|(_, mut metadata)| {
            metadata
                .remove("tk:parsed-by-full-set")
                .unwrap_or_else(|| panic!("no parsers recorded for {}", path.display()))
        })
        .unwrap_or_default()
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
            TIKAS_SPLIT_SEGMENT,
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
            ("drawing.emz", gzip(text, None)),
            ("page.html-gz", gzip(html, None)),
            ("notes.txt.bz2", bzip2(text)),
            ("notes.zlib", zlib(text)),
            ("notes.txt.Z", compress(&text.repeat(3))),
            ("notes.sz", snappy(text)),
            ("notes.lz4", lz4(text)),
            ("notes.txt.xz", xz(text)),
            ("notes.lzma", lzma(text)),
            ("notes.txt.br", brotli(text)),
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
    assert_eq!(deferred, ["TIKA-216.tgz", "quine.gz"]);
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
                "global-and-sparse.tar",
                tar(&[
                    (
                        b"pax_global_header",
                        b'g',
                        &pax(&[("mtime", "1700000123.25"), ("comment", "a commit")]),
                    ),
                    (b"a.txt", b'0', text),
                    (
                        b"././@PaxHeader",
                        b'x',
                        &pax(&[
                            ("GNU.sparse.size", "10"),
                            ("GNU.sparse.map", "0,2,6,2,10,0"),
                            ("GNU.sparse.name", "sparse.txt"),
                        ]),
                    ),
                    (b"./GNUSparseFile.1/sparse.txt", b'0', b"abcd"),
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

/// TAR archives whose headers commons-compress applies again as its recursion unwinds, which
/// throws on them, and the native parser applies once (#184): a global size again over an
/// entry's own, which leaves the stream in the entry's data, and an old GNU sparse file's
/// extension records read again from its data after a long name or a global header.
#[cfg(feature = "tika")]
#[test]
fn reads_tar_archives_whose_headers_commons_compress_applies_again() {
    let scratch = Scratch::new("archive-tar-headers-once");
    let sized = tar(&[
        (b"pax_global_header", b'g', &pax(&[("size", "0")])),
        (b"././@PaxHeader", b'x', &pax(&[("size", "600")])),
        (b"sized.txt", b'0', &[b'x'; 600]),
    ]);
    let structs = [(0, 1), (2, 1), (4, 1), (6, 1), (8, 1), (10, 0)];
    let mut long_name = records(&[(b"././@LongLink", b'L', b"long-sparse.txt\0")]);
    long_name.extend(old_gnu_sparse(b"cut", 10, &structs, b"abcde"));
    let mut global = records(&[(b"pax_global_header", b'g', &pax(&[("comment", "a commit")]))]);
    global.extend(old_gnu_sparse(b"sparse.txt", 10, &structs, b"abcde"));
    let written = [
        scratch.write("global-size.tar", &sized),
        scratch.write("long-name-sparse.tar", &ended(long_name)),
        scratch.write("global-sparse.tar", &ended(global)),
    ];
    let paths: Vec<&Path> = written.iter().map(PathBuf::as_path).collect();
    crate::formats::parity::assert_read_where_tika_fails(&paths, "#184");
}

/// A TAR archive whose 1.0 sparse map fills its record, after which commons-compress skips the
/// file's data, reads its data from the next header and that header from the next file's data,
/// on which it throws, and the native parser reads the data that follows the map (#184).
#[cfg(feature = "tika")]
#[test]
fn reads_a_tar_archive_whose_1_0_sparse_map_fills_its_record() {
    let scratch = Scratch::new("archive-tar-filled-sparse-map");
    let archive = ended(
        [
            sparse_1_0_filling_a_record("filled.txt", &[b'A'; 512]),
            records(&[(b"after.txt", b'0', &[b'x'; 512])]),
        ]
        .concat(),
    );
    let written = scratch.write("filled-sparse-map.tar", &archive);
    crate::formats::parity::assert_read_where_tika_fails(&[written.as_path()], "#184");
}

/// ARJ archives with a stray byte of the header magic before a local header, at which
/// commons-compress stops its search and throws on the header size it reads after it, past the
/// archive's end, where the native parser searches on for the whole magic (#184).
#[cfg(feature = "tika")]
#[test]
fn reads_arj_archives_with_a_stray_byte_of_the_header_magic() {
    let scratch = Scratch::new("archive-arj-stray-magic");
    let archive = arj(&[(b"A.TXT", 0, 0, b"Stored text after a stray byte.")]);
    let written = [
        scratch.write("stray-first.arj", &with_stray_bytes(&archive, [0x60, 0])),
        scratch.write("stray-second.arj", &with_stray_bytes(&archive, [0, 0xEA])),
    ];
    let paths: Vec<&Path> = written.iter().map(PathBuf::as_path).collect();
    crate::formats::parity::assert_read_where_tika_fails(&paths, "#184");
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
    let protected = ("test7Z_protected_passTika.7z", "Tika");
    let (protected_paths, plain): (Vec<PathBuf>, Vec<PathBuf>) = parsed
        .into_iter()
        .partition(|path| path.ends_with(protected.0));
    let plain: Vec<&Path> = plain.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_by_contents(&plain, SEVEN_Z_PARSER, None);
    crate::formats::parity::assert_same_as_tika_with_passwords_but(
        &protected_paths,
        SEVEN_Z_PARSER,
        &[protected],
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
/// zip bomb in a build that has it, fails with its exception or records it, as its wrapping of
/// the guard's exception has it.
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
    let declined = undecoded("the Gzip stream", Undecoded::TooLarge, "CompressorParser");
    if cfg!(feature = "tika") {
        assert!(
            matches!(&declined, Decline::Deferred(reason) if reason.contains("Tika's path streams")),
            "{declined:?}"
        );
    } else {
        assert!(
            matches!(&declined, Decline::Fails(Error::LimitExceeded(reason))
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

/// `length` bytes no compressor shrinks, the same each time.
fn incompressible(length: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn records_a_bomb_in_an_entry_and_goes_on_with_the_entries_after_it() {
    let line = "All work and no play makes Jack a dull boy.\n";
    let bomb = gzip(line.repeat(70_000).as_bytes(), Some("bomb.txt"));
    let archive = zip(&[
        ("before.txt", b"Before the bomb."),
        ("padding.bin", &incompressible(50_000)),
        ("bomb.txt.gz", &bomb),
        ("after.txt", b"After the bomb."),
    ]);
    assert!(bomb.len() * 100 < line.len() * 70_000, "{}", bomb.len());
    assert!(
        archive.len() * 100 > line.len() * 70_000,
        "{}",
        archive.len()
    );
    let (text, ended) = extract(&archive);
    assert!(text.contains("Before the bomb."), "{}", &text[..100]);
    assert!(text.contains(line), "{}", &text[..100]);
    if cfg!(feature = "tika") {
        assert_zip_bomb(ended, "output characters");
        return;
    }
    assert!(
        text.len().abs_diff(bomb.len() * 100) < 10_000,
        "{} characters of {} bytes",
        text.len(),
        bomb.len()
    );
    assert!(
        text.contains("After the bomb."),
        "{}",
        &text[text.len() - 100..]
    );
    let recorded = ended.expect("the archive goes on past the bomb").concat();
    assert!(
        recorded.contains("could not parse the embedded document \"bomb.txt.gz\""),
        "{recorded}"
    );
    assert!(recorded.contains("Zip bomb detected!"), "{recorded}");
    assert!(recorded.contains("output characters"), "{recorded}");
}

#[cfg(not(feature = "tika"))]
#[test]
fn writes_an_entry_up_to_the_most_the_native_path_decodes_and_stops_there() {
    let bomb = vec![0; crate::compress::MAX_DECODED + 1000];
    let archive = zip(&[
        ("before.txt", b"Before the large entry."),
        ("large.bin", &bomb),
        ("after.txt", b"After the large entry."),
    ]);
    let (text, ended) = extract(&archive);
    assert!(text.contains("Before the large entry."), "{text}");
    assert!(!text.contains("After the large entry."), "{text}");
    match ended {
        Err(Error::LimitExceeded(message)) => {
            assert!(message.starts_with("the entry large.txt: "), "{message}");
            assert!(
                message.contains("the most the native parser decodes"),
                "{message}"
            );
        }
        other => panic!("not stopped at the limit: {other:?}"),
    }
}

#[cfg(not(feature = "tika"))]
#[test]
fn writes_a_7z_archive_up_to_the_entry_past_the_most_the_native_path_decodes() {
    let bomb = vec![0; crate::compress::MAX_DECODED + 1000];
    let archive = crate::compress::seven_z::tests::seven_z(&[
        ("before.txt", b"Before the large entry."),
        ("large.bin", &bomb),
        ("after.txt", b"After the large entry."),
    ]);
    let (text, ended) = extract(&archive);
    assert!(text.contains("Before the large entry."), "{text}");
    assert!(!text.contains("After the large entry."), "{text}");
    match ended {
        Err(Error::LimitExceeded(message)) => {
            assert!(message.starts_with("the 7z archive: "), "{message}");
            assert!(
                message.contains("the most the native parser decodes"),
                "{message}"
            );
        }
        other => panic!("not stopped at the limit: {other:?}"),
    }
}

#[cfg(not(feature = "tika"))]
#[test]
fn writes_an_entry_cut_short_as_far_as_it_decodes_and_goes_on() {
    let mut archive = zip(&[
        ("broken.txt", &[7u8; 5000]),
        ("after.txt", b"After the broken entry."),
    ]);
    let start =
        30 + "broken.txt".len() + usize::from(u16::from_le_bytes([archive[28], archive[29]]));
    archive[start] = 0xFF;
    let (text, ended) = extract(&archive);
    assert!(text.contains("After the broken entry."), "{text}");
    let recorded = ended.expect("the archive goes on past the entry");
    let (unreadable, rest): (Vec<&String>, Vec<&String>) = recorded
        .iter()
        .partition(|recorded| recorded.contains("Can't read archive stream"));
    assert_eq!(unreadable.len(), 1, "{recorded:?}");
    assert!(
        unreadable[0].contains("(broken.txt): Truncated ZIP file"),
        "{recorded:?}"
    );
    assert!(
        rest.iter()
            .all(|recorded| recorded.contains("read the document differently from Tika")),
        "{recorded:?}"
    );
}
