use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, deferral, documents,
    spools_as_tika, Scratch,
};

const SOLID: u16 = 0x0008;
const FILE_SOLID: u16 = 0x0010;
const DIRECTORY: u16 = 0x00e0;
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
    let mut archive = b"Rar!\x1a\x07\x00".to_vec();
    block(&mut archive, 0x73, main_flags, &[0; 6]);
    for entry in entries {
        let mut body = Vec::new();
        body.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        body.push(2);
        body.extend_from_slice(&crate::compress::crc32(entry.data).to_le_bytes());
        body.extend_from_slice(&DOS_TIME.to_le_bytes());
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

#[test]
fn extracts_rar_archives_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&RarParser, &roots);
    let names = |paths: &[PathBuf]| -> Vec<String> {
        paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(
        names(&deferred),
        ["corrupt-data.rar", "method36.rar", "volume.part1.rar"]
    );
    let reasons: Vec<String> = deferred
        .iter()
        .map(|path| deferral(&RarParser, path).unwrap())
        .collect();
    assert!(reasons[0].contains("CRC"), "{reasons:?}");
    assert!(reasons[1].contains("method 36"), "{reasons:?}");
    assert!(reasons[2].contains("another volume"), "{reasons:?}");
    let failing = ["encrypted-headers.rar", "no-main-header.rar", "rar5.rar"];
    let (failed, same): (Vec<PathBuf>, Vec<PathBuf>) = parsed
        .into_iter()
        .partition(|path| failing.contains(&path.file_name().unwrap().to_string_lossy().as_ref()));
    assert_eq!(names(&failed), failing);
    assert_eq!(same.len(), 24, "{:?}", names(&same));
    let same: Vec<&Path> = same.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&same, RarParser.tika_name());
    let failed: Vec<&Path> = failed.iter().map(PathBuf::as_path).collect();
    assert_fails_as_tika(&failed);
    let encrypted = std::fs::read("../test_files/documents/rar/encrypted-headers.rar").unwrap();
    assert!(matches!(
        RarParser.prepare(&encrypted, &TikaMetadata::new(), &ParserConfigs::default()),
        Err(Declined::Fails(Error::Encrypted(_)))
    ));
}

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

#[test]
fn leaves_solid_entries_after_one_larger_than_junrars_pipe_to_tika() {
    let scratch = Scratch::new("rar-pipe");
    let large = "Dette er en lang tekst om vinter i Danmark.\n".repeat(1000);
    let first = stored(b"lang.txt", 0, large.as_bytes());
    let second = stored(b"kort.txt", FILE_SOLID, b"Kort.\n");
    let solid = scratch.write("solid.rar", &rar(SOLID, &[first, second]));
    let reason = deferral(&RarParser, &solid).unwrap();
    assert!(reason.contains("pipe"), "{reason}");
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
    assert_eq!(deferral(&RarParser, &separate), None);
    assert_same_as_tika(
        &[separate.as_path(), small.as_path()],
        RarParser.tika_name(),
    );
}

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
