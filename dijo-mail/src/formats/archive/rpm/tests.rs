//! Packages built entry by entry for what the generated test document cannot hold: each payload
//! compressor, stripped payloads with hard links, and hostile or unreadable payloads.

use super::header::tests::{header, package, Value};
use super::*;
use crate::compress::cpio::tests::{cpio, stripped};
use crate::compress::{bzip2, gzip, xz, zstd};

/// The main header of a package whose payload is compressed with `compressor`, if it says.
fn described(compressor: Option<&str>) -> Vec<u8> {
    let mut entries = vec![
        (NAME, Value::String("lighthouse-log")),
        (VERSION, Value::String("1.2")),
        (RELEASE, Value::String("3")),
        (SUMMARY, Value::I18n(&["The keeper's log", "Le journal"])),
        (
            DESCRIPTION,
            Value::I18n(&["What the lamp needed each night."]),
        ),
        (BUILD_TIME, Value::Int32(&[1_794_484_800])),
        (LICENSE, Value::String("CC0-1.0")),
        (URL, Value::String("https://example.org/lighthouse")),
        (PACKAGER, Value::String("The Keeper <keeper@example.org>")),
        (CHANGELOG_NAME, Value::Strings(&["The Keeper - 1.2-3"])),
        (CHANGELOG_TEXT, Value::Strings(&["- Record the storm."])),
        (PAYLOAD_FORMAT, Value::String("cpio")),
    ];
    if let Some(compressor) = compressor {
        entries.push((PAYLOAD_COMPRESSOR, Value::String(compressor)));
    }
    header(&entries)
}

/// The main header of a package listing a directory, a file, two hard links to one file, a
/// symbolic link and a ghost, for a stripped payload of them.
fn listing() -> Vec<u8> {
    header(&[
        (NAME, Value::String("large")),
        (PAYLOAD_COMPRESSOR, Value::String("gzip")),
        (FILE_SIZES, Value::Int32(&[4096, 3, 5, 5, 7, 0])),
        (
            FILE_MODES,
            Value::Int16(&[0o040755, 0o100644, 0o100644, 0o100644, 0o120777, 0o100644]),
        ),
        (FILE_MTIMES, Value::Int32(&[1_794_484_800; 6])),
        (FILE_FLAGS, Value::Int32(&[0, 0, 0, 0, 0, 1 << 6])),
        (FILE_DEVICES, Value::Int32(&[1; 6])),
        (FILE_INODES, Value::Int32(&[1, 2, 3, 3, 4, 5])),
        (DIR_INDEXES, Value::Int32(&[0, 1, 1, 1, 1, 1])),
        (
            BASENAMES,
            Value::Strings(&["doc", "odd.txt", "link-a", "link-b", "latest", "ghost"]),
        ),
        (
            DIRNAMES,
            Value::Strings(&["/usr/share/", "/usr/share/doc/"]),
        ),
        (LONG_FILE_SIZES, Value::Int64(&[4096, 3, 5, 5, 7, 0])),
    ])
}

/// The XHTML and metadata of the package `bytes`.
fn extract(bytes: &[u8]) -> (String, crate::Metadata) {
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(bytes)
        .unwrap()
}

/// What extracting the package `bytes` fails with.
fn failure(bytes: &[u8]) -> String {
    crate::Extractor::new()
        .extract_bytes_to_string(bytes)
        .unwrap_err()
        .to_string()
}

/// The paths of the files the XHTML `xml` holds, in order.
fn files(xml: &str) -> Vec<&str> {
    xml.split(r#"<div class="embedded" id=""#)
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect()
}

#[test]
fn reads_the_header_and_a_payload_compressed_with_each_compressor() {
    let archive = cpio(&[
        ("./usr/share/doc/", b""),
        ("./usr/share/doc/notes.txt", b"Oil the lamp weekly."),
    ]);
    let packages = [
        (Some("gzip"), gzip::tests::gzip(&archive, None)),
        (Some("bzip2"), bzip2::tests::bzip2(&archive)),
        (Some("xz"), xz::tests::xz(&archive)),
        (Some("lzma"), xz::tests::lzma(&archive)),
        (Some("zstd"), zstd::tests::zstd(&archive)),
        (None, gzip::tests::gzip(&archive, None)),
        (None, archive.clone()),
    ];
    for (compressor, payload) in packages {
        let (xml, metadata) = extract(&package(&described(compressor), &payload));
        assert_eq!(metadata["Content-Type"], ["application/x-rpm"]);
        assert_eq!(files(&xml), ["./usr/share/doc/notes.txt"], "{compressor:?}");
        for text in [
            "The keeper's log",
            "What the lamp needed each night.",
            "<h2>The Keeper - 1.2-3</h2>",
            "- Record the storm.",
            "Oil the lamp weekly.",
        ] {
            assert!(xml.contains(text), "{compressor:?} {text}: {xml}");
        }
        assert!(!xml.contains("Le journal"), "{xml}");
        assert_eq!(metadata["dc:title"], ["lighthouse-log"]);
        assert_eq!(metadata["rpm:version"], ["1.2"]);
        assert_eq!(metadata["rpm:release"], ["3"]);
        assert_eq!(metadata["dc:description"], ["The keeper's log"]);
        assert_eq!(metadata["dc:rights"], ["CC0-1.0"]);
        assert_eq!(metadata["rpm:url"], ["https://example.org/lighthouse"]);
        assert_eq!(metadata["dc:creator"], ["The Keeper <keeper@example.org>"]);
        assert_eq!(metadata["dcterms:created"], ["2026-11-12T12:00:00Z"]);
    }
}

#[test]
fn reads_stripped_payloads_by_the_files_the_header_lists() {
    let payload = stripped(&[
        (0, b""),
        (1, b"odd"),
        (2, b""),
        (3, b"twice"),
        (4, b"odd.txt"),
    ]);
    let (xml, _) = extract(&package(&listing(), &gzip::tests::gzip(&payload, None)));
    assert_eq!(
        files(&xml),
        [
            "./usr/share/doc/odd.txt",
            "./usr/share/doc/link-a",
            "./usr/share/doc/link-b",
            "./usr/share/doc/latest",
        ]
    );
    assert!(xml.contains("odd"), "{xml}");
    assert!(xml.contains("twice"), "{xml}");
}

#[test]
fn fails_on_a_payload_it_cannot_read() {
    let unknown = package(&described(Some("lzip")), b"LZIP");
    let failed = failure(&unknown);
    assert!(
        failed.contains("it is compressed with lzip, which is none of rpm's compressors"),
        "{failed}"
    );
    let corrupt = package(&described(Some("zstd")), b"\x28\xB5\x2F\xFDnot zstd");
    let failed = failure(&corrupt);
    assert!(failed.contains(WHAT), "{failed}");
    let unlisted = package(
        &described(Some("gzip")),
        &gzip::tests::gzip(&stripped(&[(0, b"")]), None),
    );
    let failed = failure(&unlisted);
    assert!(
        failed.contains("the header lists no file names"),
        "{failed}"
    );
    let beyond = package(&listing(), &gzip::tests::gzip(&stripped(&[(9, b"")]), None));
    let failed = failure(&beyond);
    assert!(
        failed.contains("a stripped entry is of file 9, but the header lists 6"),
        "{failed}"
    );
}

#[test]
fn fails_packages_whose_headers_cannot_be_read() {
    let configs = ParserConfigs::default();
    let reason = |bytes: &[u8]| match RpmParser.prepare(bytes, &TikaMetadata::new(), &configs) {
        Err(Decline::Fails(error)) => error.to_string(),
        _ => panic!("not failed"),
    };
    let whole = package(&described(Some("gzip")), b"");
    let cut = reason(&whole[..whole.len() - 1]);
    assert!(
        cut.contains("the RPM package: its main header: the input ends inside its store"),
        "{cut}"
    );
    let mut changelog = header(&[
        (CHANGELOG_NAME, Value::Strings(&["a", "b"])),
        (CHANGELOG_TEXT, Value::Strings(&["text"])),
    ]);
    changelog = package(&changelog, b"");
    assert!(reason(&changelog).contains("2 names for 1 texts"));
}
