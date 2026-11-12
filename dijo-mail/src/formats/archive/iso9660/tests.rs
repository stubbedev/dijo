//! Images written sector by sector for what a generated test document cannot hold: Rock Ridge
//! continuation areas and relocations, files in several extents, and hostile directories.

use super::directory::tests::record;
use super::rock_ridge::tests::{continuation, entry, extension, name, sharing};
use super::volume::tests::structure;
use super::volume::SECTOR;
use super::*;
use crate::formats::embedded::EMBEDDED_STREAM_EXCEPTION;
use crate::formats::testing::Scratch;

const DIRECTORY: u8 = 0x02;
const MULTI_EXTENT: u8 = 0x80;
const PRIMARY_ROOT: u32 = 20;
const JOLIET_ROOT: u32 = 21;
const DOCS: u32 = 22;
const RELOCATED: u32 = 23;
const MOVED: u32 = 24;
const CONTINUATION: u32 = 25;
const README: u32 = 30;
const BIG: u32 = 31;
const SPLIT: u32 = 33;
const NOTE: u32 = 36;
const DEEP: u32 = 37;
const BLOCKS: usize = 40;

/// An image of `blocks`, each written at its logical block.
fn image(blocks: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = vec![0u8; BLOCKS * SECTOR];
    for (block, data) in blocks {
        let at = *block as usize * SECTOR;
        bytes[at..at + data.len()].copy_from_slice(data);
    }
    bytes
}

/// A primary (1) or supplementary (2) volume descriptor whose root directory is at `root`,
/// identified by `volume`, encoded as Joliet in a supplementary one.
fn descriptor(kind: u8, root: u32, volume: &str) -> Vec<u8> {
    let mut sector = structure(kind, b"CD001");
    let text = |field: &mut [u8], text: &str| {
        let encoded: Vec<u8> = if kind == 2 {
            text.encode_utf16().flat_map(u16::to_be_bytes).collect()
        } else {
            text.bytes().collect()
        };
        let pad: &[u8] = if kind == 2 { &[0, b' '] } else { b" " };
        for (at, byte) in field.iter_mut().enumerate() {
            *byte = *encoded.get(at).unwrap_or(&pad[at % pad.len()]);
        }
    };
    text(&mut sector[40..72], volume);
    text(&mut sector[318..446], "Lighthouse Press");
    text(&mut sector[446..574], "The Keeper");
    text(&mut sector[574..702], "hand-built");
    if kind == 2 {
        sector[88..91].copy_from_slice(b"%/E");
    }
    sector[128..130].copy_from_slice(&2048u16.to_le_bytes());
    sector[130..132].copy_from_slice(&2048u16.to_be_bytes());
    sector[156..190].copy_from_slice(&record(b"\0", root, 2048, DIRECTORY, b""));
    sector[813..830].copy_from_slice(b"2026111220414800\x04");
    sector
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// The records of a directory, its own and its parent's first.
fn directory(own: u32, parent: u32, own_use: &[u8], records: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = record(b"\0", own, 2048, DIRECTORY, own_use);
    bytes.extend(record(b"\x01", parent, 2048, DIRECTORY, b""));
    for record in records {
        bytes.extend_from_slice(record);
    }
    bytes
}

/// An image with Rock Ridge, unless `plain`, and a Joliet tree, if `joliet`: in its root a text
/// file, a symbolic link, a file in two adjacent extents, one in two extents apart, and `docs`,
/// holding a note, a directory pointing back to the root, a file past the image's end and a
/// relocated directory whose record is in `moved`.
fn disc(plain: bool, joliet: bool) -> Vec<u8> {
    let rock = |area: Vec<u8>| if plain { Vec::new() } else { area };
    let mut root_use = sharing();
    root_use.extend(continuation(CONTINUATION, 100, 30));
    let mut times = vec![0x03];
    times.extend_from_slice(&[110, 3, 4, 5, 6, 7, 0]);
    times.extend_from_slice(&[126, 11, 12, 20, 41, 48, 4]);
    let mut readme_use = name(0, "ReadMe");
    readme_use.extend(name(1, ".txt"));
    readme_use.extend(entry(b"TF", &times));
    let mut link_use = name(0, "latest");
    link_use.extend(entry(
        b"SL",
        &[
            0, 0, 10, b'R', b'e', b'a', b'd', b'M', b'e', b'.', b't', b'x', b't',
        ],
    ));
    let primary_root = directory(
        PRIMARY_ROOT,
        PRIMARY_ROOT,
        &rock(root_use),
        &[
            record(
                b"BIG.TXT;1",
                BIG,
                2048,
                MULTI_EXTENT,
                &rock(name(0, "big.txt")),
            ),
            record(b"BIG.TXT;1", BIG + 1, 9, 0, &rock(name(0, "big.txt"))),
            record(b"DOCS", DOCS, 2048, DIRECTORY, &rock(name(0, "docs"))),
            record(b"LATEST.;1", 0, 0, 0, &rock(link_use)),
            record(b"README.TXT;1", README, 26, 0, &rock(readme_use)),
            record(
                b"RR_MOVED",
                MOVED,
                2048,
                DIRECTORY,
                &rock(name(0, "rr_moved")),
            ),
            record(b"SPLIT.TXT;1", SPLIT, 2048, MULTI_EXTENT, b""),
            record(b"SPLIT.TXT;1", SPLIT + 2, 6, 0, b""),
        ],
    );
    let mut relocation = name(0, "deep");
    relocation.extend(entry(b"CL", &[23, 0, 0, 0, 0, 0, 0, 23]));
    let docs = directory(
        DOCS,
        PRIMARY_ROOT,
        b"",
        &[
            record(
                b"BACK",
                PRIMARY_ROOT,
                2048,
                DIRECTORY,
                &rock(name(0, "back")),
            ),
            record(b"DEEP", 0, 0, 0, &rock(relocation)),
            record(b"NOTE.TXT;1", NOTE, 21, 0, b""),
            record(b"PAST.TXT;1", 1000, 10, 0, b""),
        ],
    );
    let relocated = directory(
        RELOCATED,
        MOVED,
        b"",
        &[record(b"DEEP.TXT;1", DEEP, 14, 0, b"")],
    );
    let moved = directory(
        MOVED,
        PRIMARY_ROOT,
        b"",
        &[record(
            b"DEEP",
            RELOCATED,
            2048,
            DIRECTORY,
            &rock(entry(b"RE", &[])),
        )],
    );
    let mut continued = vec![0u8; 100];
    continued.extend(extension());
    let mut big = vec![b'a'; 2048];
    big[..9].copy_from_slice(b"big start");
    big.extend_from_slice(b"big end\n\n");
    let mut split = vec![b' '; 2048];
    split[..11].copy_from_slice(b"split start");
    let mut blocks = vec![
        (16, descriptor(1, PRIMARY_ROOT, "LIGHTHOUSE")),
        (18, structure(255, b"CD001")),
        (PRIMARY_ROOT, primary_root),
        (DOCS, docs),
        (RELOCATED, relocated),
        (MOVED, moved),
        (CONTINUATION, continued),
        (README, b"The lamp is lit at dusk.\n\n".to_vec()),
        (BIG, big),
        (SPLIT, split),
        (SPLIT + 2, b"apart\n".to_vec()),
        (NOTE, b"Oil the lamp weekly.\n".to_vec()),
        (DEEP, b"Deep in rocks\n".to_vec()),
    ];
    if joliet {
        blocks.push((17, descriptor(2, JOLIET_ROOT, "Lighthouse")));
        blocks.push((
            JOLIET_ROOT,
            directory(
                JOLIET_ROOT,
                JOLIET_ROOT,
                b"",
                &[record(&utf16("Read Me.txt;1"), README, 26, 0, b"")],
            ),
        ));
    } else {
        blocks.push((17, structure(255, b"CD001")));
    }
    image(&blocks)
}

/// The XHTML and metadata of the image `bytes`.
fn extract(bytes: &[u8]) -> (String, crate::Metadata) {
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(bytes)
        .unwrap()
}

/// The paths of the files the XHTML `xml` holds, in order.
fn files(xml: &str) -> Vec<&str> {
    xml.split(r#"<div class="embedded" id=""#)
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect()
}

#[test]
fn reads_rock_ridge_names_times_links_and_relocations() {
    let (xml, metadata) = extract(&disc(false, true));
    assert_eq!(metadata["Content-Type"], ["application/x-iso9660-image"]);
    assert_eq!(
        files(&xml),
        [
            "big.txt",
            "ReadMe.txt",
            "SPLIT.TXT",
            "docs/NOTE.TXT",
            "docs/deep/DEEP.TXT",
        ]
    );
    for text in [
        "The lamp is lit at dusk.",
        "Oil the lamp weekly.",
        "Deep in rocks",
        "big start",
        "big end",
        "split start",
        "apart",
    ] {
        assert!(xml.contains(text), "{text}: {xml}");
    }
    assert_eq!(metadata[SYMBOLIC_LINK], ["latest -> ReadMe.txt"]);
    let failures = metadata[EMBEDDED_STREAM_EXCEPTION].join("\n");
    assert!(
        failures.contains("(docs/back): it is the directory at block 20, which is read already"),
        "{failures}"
    );
    assert!(
        failures.contains("(docs/PAST.TXT): its extent of 10 bytes at byte 2048000 ends past"),
        "{failures}"
    );
    assert_eq!(metadata["dc:title"], ["Lighthouse"]);
    assert_eq!(metadata["dc:publisher"], ["Lighthouse Press"]);
    assert_eq!(metadata["dc:creator"], ["The Keeper"]);
    assert_eq!(metadata["xmp:CreatorTool"], ["hand-built"]);
    assert_eq!(metadata["dcterms:created"], ["2026-11-12T19:41:48Z"]);
}

#[test]
fn dates_and_sizes_each_file() {
    let bytes = disc(false, false);
    let tree = {
        let Recognized::Iso9660 { primary, joliet } = volume::recognize(&bytes).unwrap() else {
            panic!("no ISO 9660 file system");
        };
        Tree::of(&bytes, &primary, joliet.as_ref()).unwrap()
    };
    let (entries, broken) = tree.entries(tree.root).unwrap();
    assert_eq!(broken, None);
    let readme = entries
        .iter()
        .find(|entry| entry.name == "ReadMe.txt")
        .unwrap();
    let dated = |millis: Option<i64>| millis.map(crate::java::time::format_instant);
    assert_eq!(
        dated(readme.created).as_deref(),
        Some("2010-03-04T05:06:07Z")
    );
    assert_eq!(
        dated(readme.modified).as_deref(),
        Some("2026-11-12T19:41:48Z")
    );
    let notes = tree
        .entries(Location {
            block: u64::from(DOCS),
            length: Some(2048),
        })
        .unwrap()
        .0;
    let note = notes.iter().find(|entry| entry.name == "NOTE.TXT").unwrap();
    assert_eq!(
        dated(note.modified).as_deref(),
        Some("2020-06-15T12:30:00Z")
    );
    assert_eq!(note.created, None);
    let sizes: Vec<(String, u64, usize)> = entries
        .into_iter()
        .filter_map(|entry| match entry.kind {
            Kind::File { pieces, size } => Some((entry.name, size, pieces.len())),
            _ => None,
        })
        .collect();
    assert_eq!(
        sizes,
        [
            ("big.txt".to_owned(), 2057, 1),
            ("ReadMe.txt".to_owned(), 26, 1),
            ("SPLIT.TXT".to_owned(), 2054, 2),
        ]
    );
}

#[test]
fn reads_joliet_names_without_rock_ridge_and_iso_names_without_either() {
    let (joliet, _) = extract(&disc(true, true));
    assert_eq!(files(&joliet), ["Read Me.txt"]);
    let (plain, metadata) = extract(&disc(true, false));
    assert_eq!(
        files(&plain),
        [
            "BIG.TXT",
            "LATEST",
            "README.TXT",
            "SPLIT.TXT",
            "DOCS/DEEP",
            "DOCS/NOTE.TXT",
            "RR_MOVED/DEEP/DEEP.TXT",
        ]
    );
    assert_eq!(metadata["dc:title"], ["LIGHTHOUSE"]);
    let failures = metadata[EMBEDDED_STREAM_EXCEPTION].join("\n");
    assert!(failures.contains("(DOCS/BACK)"), "{failures}");
}

#[test]
fn records_files_it_cannot_read() {
    let mut bytes = disc(true, false);
    let at = PRIMARY_ROOT as usize * SECTOR;
    let interleaved = record(b"WOVEN.TXT;1", README, 26, 0, b"");
    let mut woven = interleaved.clone();
    woven[26] = 1;
    woven[27] = 1;
    let mut root = directory(PRIMARY_ROOT, PRIMARY_ROOT, b"", &[woven]);
    root.extend(record(b"ODD.TXT;1", BIG, 2048, MULTI_EXTENT, b""));
    root.extend(record(b"OTHER.TXT;1", BIG + 1, 9, 0, b""));
    root.extend(record(b"LAST.TXT;1", BIG, 2048, MULTI_EXTENT, b""));
    bytes[at..at + SECTOR].fill(0);
    bytes[at..at + root.len()].copy_from_slice(&root);
    let (_, metadata) = extract(&bytes);
    let failures = metadata[EMBEDDED_STREAM_EXCEPTION].join("\n");
    for expected in [
        "(WOVEN.TXT): it is recorded interleaved",
        "(OTHER.TXT): its extents' records name it differently",
        "(/): its last file's record says it goes on in another",
    ] {
        assert!(failures.contains(expected), "{expected}: {failures}");
    }
}

/// Thousands of records of one extent of zeros are handed on only up to the decode limit.
#[test]
fn stops_handing_on_files_past_the_decode_limit() {
    const FILES: u32 = 4000;
    const ROOT_BLOCKS: u32 = 84;
    const ZEROS: u32 = 20 + ROOT_BLOCKS;
    const ZERO_BLOCKS: u32 = 400;
    let mut root = directory(PRIMARY_ROOT, PRIMARY_ROOT, b"", &[]);
    for index in 0..FILES {
        let record = record(
            format!("F{index:04}.;1").as_bytes(),
            ZEROS,
            ZERO_BLOCKS * SECTOR as u32,
            0,
            b"",
        );
        if root.len() % SECTOR + record.len() > SECTOR {
            root.resize(root.len().next_multiple_of(SECTOR), 0);
        }
        root.extend(record);
    }
    let mut primary = descriptor(1, PRIMARY_ROOT, "BOMB");
    primary[156..190].copy_from_slice(&record(
        b"\0",
        PRIMARY_ROOT,
        ROOT_BLOCKS * SECTOR as u32,
        DIRECTORY,
        b"",
    ));
    let mut bytes = vec![0u8; (ZEROS + ZERO_BLOCKS) as usize * SECTOR];
    bytes[16 * SECTOR..17 * SECTOR].copy_from_slice(&primary);
    bytes[17 * SECTOR..18 * SECTOR].copy_from_slice(&structure(255, b"CD001"));
    let at = PRIMARY_ROOT as usize * SECTOR;
    assert!(root.len() <= ROOT_BLOCKS as usize * SECTOR);
    bytes[at..at + root.len()].copy_from_slice(&root);
    let failed = crate::Extractor::new()
        .extract_bytes_to_string(&bytes)
        .unwrap_err()
        .to_string();
    assert!(failed.contains(WHAT), "{failed}");
}

#[test]
fn notes_an_image_of_udf_alone() {
    let mut bytes = vec![0u8; 16 * SECTOR];
    for identifier in [b"BEA01", b"NSR03", b"TEA01"] {
        bytes.extend(structure(0, identifier));
    }
    let scratch = Scratch::new("iso9660-udf");
    let path = scratch.write("dvd.iso", &bytes);
    let (text, metadata) = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap();
    assert_eq!(text.trim(), "");
    assert_eq!(metadata[EXCEPTION_WARNING], [UDF_ONLY]);
}

#[test]
fn fails_an_image_without_a_file_system() {
    let configs = ParserConfigs::default();
    let bytes = vec![0u8; 20 * SECTOR];
    let failure = match Iso9660Parser.prepare(&bytes, &TikaMetadata::new(), &configs) {
        Err(Decline::Fails(error)) => error.to_string(),
        _ => panic!("not failed"),
    };
    assert!(
        failure.contains("no primary volume descriptor"),
        "{failure}"
    );
}
