#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, deferral, documents,
    spools_as_tika, Scratch,
};

#[cfg(feature = "tika")]
const SOLID: u16 = 0x0008;
#[cfg(feature = "tika")]
const FILE_SOLID: u16 = 0x0010;
#[cfg(feature = "tika")]
const DIRECTORY: u16 = 0x00e0;
#[cfg(feature = "tika")]
const UNICODE: u16 = 0x0200;
/// 2021-03-04 05:06:08 as a DOS date and time.
const DOS_TIME: u32 = (41 << 25) | (3 << 21) | (4 << 16) | (5 << 11) | (6 << 5) | 4;

/// A stored entry of a hand-made RAR 4 archive.
struct Stored<'a> {
    name: &'a [u8],
    flags: u16,
    data: &'a [u8],
}

/// A RAR 4 archive of stored entries, with every header's CRC, as RAR 2 writes one.
fn rar(main_flags: u16, entries: &[Stored]) -> Vec<u8> {
    rar_dated(main_flags, entries, DOS_TIME)
}

/// [`rar`], its entries dated by the DOS time `time`.
fn rar_dated(main_flags: u16, entries: &[Stored], time: u32) -> Vec<u8> {
    let mut archive = b"Rar!\x1a\x07\x00".to_vec();
    block(&mut archive, 0x73, main_flags, &[0; 6]);
    for entry in entries {
        let mut body = Vec::new();
        body.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        body.push(2);
        body.extend_from_slice(&crate::compress::crc32(entry.data).to_le_bytes());
        body.extend_from_slice(&time.to_le_bytes());
        body.push(20);
        body.push(0x30);
        body.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        body.extend_from_slice(&0x20u32.to_le_bytes());
        body.extend_from_slice(entry.name);
        block(&mut archive, 0x74, 0x8000 | entry.flags, &body);
        archive.extend_from_slice(entry.data);
    }
    archive.extend_from_slice(&[0xc4, 0x3d, 0x7b, 0x00, 0x40, 0x07, 0x00]);
    archive
}

fn block(archive: &mut Vec<u8>, header_type: u8, flags: u16, body: &[u8]) {
    let mut header = vec![0, 0, header_type];
    header.extend_from_slice(&flags.to_le_bytes());
    header.extend_from_slice(&(7 + body.len() as u16).to_le_bytes());
    header.extend_from_slice(body);
    let crc = crate::compress::crc32(&header[2..]) as u16;
    header[..2].copy_from_slice(&crc.to_le_bytes());
    archive.extend_from_slice(&header);
}

fn stored<'a>(name: &'a [u8], flags: u16, data: &'a [u8]) -> Stored<'a> {
    Stored { name, flags, data }
}

/// The documents of an entry the port cannot give whole, which Tika's path gives empty: the
/// port records why instead (#122).
const UNREAD: [(&str, &str); 3] = [
    ("corrupt-data.rar", "does not match its CRC"),
    ("method36.rar", "RAR version 36"),
    ("volume.part1.rar", "another volume"),
];

#[cfg(feature = "tika")]
#[test]
fn extracts_rar_archives_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&RarParser, &roots);
    assert!(deferred.is_empty(), "{deferred:?}");
    let named = |path: &PathBuf, names: &[&str]| {
        names.contains(&path.file_name().unwrap().to_string_lossy().as_ref())
    };
    let failing = ["encrypted-headers.rar", "no-main-header.rar", "rar5.rar"];
    let unread: Vec<&str> = UNREAD.iter().map(|(name, _)| *name).collect();
    let (failed, rest): (Vec<PathBuf>, Vec<PathBuf>) =
        parsed.into_iter().partition(|path| named(path, &failing));
    let (unread, same): (Vec<PathBuf>, Vec<PathBuf>) =
        rest.into_iter().partition(|path| named(path, &unread));
    assert_eq!(names(&failed), failing);
    assert_eq!(unread.len(), UNREAD.len());
    assert_eq!(same.len(), 24, "{:?}", names(&same));
    let same: Vec<&Path> = same.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&same, RarParser.tika_name());
    let failed: Vec<&Path> = failed.iter().map(PathBuf::as_path).collect();
    assert_fails_as_tika(&failed);
    let encrypted = std::fs::read("../test_files/documents/rar/encrypted-headers.rar").unwrap();
    assert!(matches!(
        RarParser.prepare(&encrypted, &TikaMetadata::new(), &ParserConfigs::default()),
        Err(crate::formats::Decline::Fails(Error::Encrypted(_)))
    ));
}

#[test]
fn records_the_entries_it_cannot_give_whole() {
    for (name, reason) in UNREAD {
        let path = format!("../test_files/documents/rar/{name}");
        let (xml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_file_to_string(&path)
            .unwrap();
        assert_eq!(
            metadata["tk:parsed-by"],
            ["extractous::RarParser"],
            "{name}"
        );
        let failures = &metadata["tk:exception:embedded-stream-exception"];
        assert_eq!(failures.len(), 1, "{name}: {failures:?}");
        assert!(
            failures[0].starts_with(
                "org.apache.tika.exception.TikaException: Can't read archive stream ("
            ) && failures[0].contains(reason),
            "{name}: {failures:?}"
        );
        assert!(
            xml.contains(r#"<div class="embedded" id=""#),
            "{name}: {xml}"
        );
        assert!(!xml.contains("package-entry"), "{name}: {xml}");
    }
}

#[cfg(feature = "tika")]
fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[cfg(feature = "tika")]
#[test]
fn reads_names_times_and_directories_as_junrar_does() {
    let scratch = Scratch::new("rar-headers");
    let latin1 = scratch.write(
        "latin1.rar",
        &rar(
            0,
            &[
                stored(b"mappe", DIRECTORY, b""),
                stored(
                    b"mappe\\r\xf8dgr\xf8d.txt",
                    0,
                    "Rødgrød med fløde.\n".as_bytes(),
                ),
                stored("Ærø.txt".as_bytes(), 0, "Ærøskøbing\n".as_bytes()),
                stored(b"tom.txt", 0, b""),
            ],
        ),
    );
    let mut unicode_name = b"smorrebrod.txt\0\0".to_vec();
    for chunk in "smørrebrød.txt"
        .encode_utf16()
        .collect::<Vec<u16>>()
        .chunks(4)
    {
        unicode_name.push(0x00);
        unicode_name.extend(chunk.iter().map(|&unit| unit as u8));
    }
    let unicode = scratch.write(
        "unicode.rar",
        &rar(
            0,
            &[stored(
                &unicode_name,
                UNICODE,
                "Smørrebrød er dansk.\n".as_bytes(),
            )],
        ),
    );
    let mut broken = rar(0, &[stored(b"brudt.txt", 0, b"Et brudt hoved.\n")]);
    broken[20] ^= 0x01;
    let broken = scratch.write("broken.rar", &broken);
    let read = |path: &PathBuf| -> Vec<(String, Vec<u8>)> {
        archive_files(&std::fs::read(path).unwrap())
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.content))
            .collect()
    };
    assert_eq!(
        read(&latin1),
        [
            (
                "mappe/rødgrød.txt".to_owned(),
                "Rødgrød med fløde.\n".as_bytes().to_vec()
            ),
            ("Ærø.txt".to_owned(), "Ærøskøbing\n".as_bytes().to_vec()),
            ("tom.txt".to_owned(), Vec::new()),
        ]
    );
    assert_eq!(
        read(&unicode),
        [(
            "smørrebrød.txt".to_owned(),
            "Smørrebrød er dansk.\n".as_bytes().to_vec()
        )]
    );
    assert_eq!(read(&broken), [("brudt.txt".to_owned(), Vec::new())]);
    let (parsed, deferred) = documents(&RarParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 3);
    assert_same_as_tika(
        &[latin1.as_path(), unicode.as_path(), broken.as_path()],
        RarParser.tika_name(),
    );
}

#[cfg(feature = "tika")]
#[test]
fn decodes_solid_entries_after_one_larger_than_junrars_pipe() {
    let scratch = Scratch::new("rar-pipe");
    let large = "Dette er en lang tekst om vinter i Danmark.\n".repeat(1000);
    let first = stored(b"lang.txt", 0, large.as_bytes());
    let second = stored(b"kort.txt", FILE_SOLID, b"Kort.\n");
    let solid = scratch.write("solid.rar", &rar(SOLID, &[first, second]));
    let first = stored(b"lang.txt", 0, large.as_bytes());
    let second = stored(b"kort.txt", 0, b"Kort.\n");
    let separate = scratch.write("separate.rar", &rar(0, &[first, second]));
    let small = scratch.write(
        "small-solid.rar",
        &rar(
            SOLID,
            &[
                stored(b"a.txt", 0, b"Forste fil.\n"),
                stored(b"b.txt", FILE_SOLID, b"Anden fil.\n"),
            ],
        ),
    );
    for path in [&solid, &separate, &small] {
        assert_eq!(deferral(&RarParser, path), None);
    }
    assert_same_as_tika(
        &[solid.as_path(), separate.as_path(), small.as_path()],
        RarParser.tika_name(),
    );
}

#[test]
fn dates_entries_in_utc_and_drops_dates_out_of_range() {
    let entries = [stored(b"a.txt", 0, b"tekst")];
    let files = archive_files(&rar(0, &entries)).unwrap();
    assert_eq!(files[0].modified_millis, Some(1_614_834_368_000));
    let february_30 = (41 << 25) | (2 << 21) | (30 << 16);
    let files = archive_files(&rar_dated(0, &entries, february_30)).unwrap();
    assert_eq!(files[0].modified_millis, None);
}

#[test]
fn reads_names_junrar_may_take_otherwise_as_differing() {
    let long = "a".repeat(300);
    let archive = rar(0, &[stored(long.as_bytes(), 0, b"tekst")]);
    let configs = ParserConfigs::default();
    let Err(crate::formats::Decline::Differs { difference, .. }) =
        RarParser.prepare(&archive, &TikaMetadata::new(), &configs)
    else {
        panic!("a name past what junrar surely takes is not read as differing");
    };
    assert!(difference.0.contains("so long"), "{difference:?}");
    assert_eq!(archive_files(&archive).unwrap()[0].name, long);
}

#[test]
fn fails_a_rar_5_archive_as_junrar_does() {
    let configs = ParserConfigs::default();
    let declined = RarParser.prepare(b"Rar!\x1a\x07\x01\x00rest", &TikaMetadata::new(), &configs);
    let Err(crate::formats::Decline::Fails(Error::ParseError(reason))) = declined else {
        panic!("a RAR 5 archive is not failed");
    };
    assert!(reason.starts_with("UnsupportedRarV5Exception"), "{reason}");
}

#[test]
fn leaves_a_solid_stream_alone_across_a_stored_entry() {
    let files = archive_files(include_bytes!(
        "../../../../tests/fixtures/rar/stored-in-solid-stream.rar"
    ))
    .unwrap();
    let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, ["a.txt", "b.png", "c.txt"]);
    let text = "R\u{f8}dgr\u{f8}d med fl\u{f8}de og vinter i Danmark. ";
    assert_eq!(files[0].content, text.repeat(300).as_bytes());
    assert_eq!(files[1].content.len(), 3000);
    assert_eq!(
        files[2].content,
        format!("{}Slut.\n", text.repeat(200)).as_bytes()
    );
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&RarParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    let (failing, parsed): (Vec<PathBuf>, Vec<PathBuf>) = parsed
        .into_iter()
        .partition(|path| path.to_string_lossy().ends_with("-enc.rar"));
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    assert_eq!(failing.len(), 1, "{failing:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, RarParser.tika_name());
    for budget in [u64::MAX, 3000] {
        assert!(matches!(spools_as_tika(&parsed[0], budget), Ok(1..)));
    }
    let failing: Vec<&Path> = failing.iter().map(PathBuf::as_path).collect();
    assert_fails_as_tika(&failing);
}
