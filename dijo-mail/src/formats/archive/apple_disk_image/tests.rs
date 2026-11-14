//! The test image `lighthouse.dmg`, which `tests/fixtures/apple_disk_image/lighthouse.py`
//! writes, read whole and read again with its volume corrupted, wrapped or made a bomb.

use super::udif::tests::{image, Run};
use super::*;
use crate::formats::embedded::EMBEDDED_STREAM_EXCEPTION;
use crate::formats::testing::Scratch;

const DOCUMENT: &str = "../test_files/documents/lighthouse.dmg";
/// Where the volume of the test image starts on its disk, and its length.
const VOLUME_SECTOR: u64 = 40;
const VOLUME_LENGTH: usize = 64 * 4096;
const CHARTS: u32 = 17;
const LOGBOOK: u32 = 18;

fn document() -> Vec<u8> {
    std::fs::read(DOCUMENT).unwrap()
}

/// The disk of the image `bytes` and its first volume.
fn opened(bytes: &[u8]) -> (Disk<'_>, Volume) {
    let image = udif::read(bytes).unwrap();
    let mut disk = Disk::new(bytes, Pages::IN_MEMORY, image.chunks);
    let partitions = if image.partitions.is_empty() {
        let sectors = disk.sectors();
        maps::partitions(&mut disk, sectors).unwrap()
    } else {
        image.partitions
    };
    let mut found = find_volumes(&mut disk, &partitions).unwrap();
    assert!(!found.volumes.is_empty(), "no volume: {:?}", found.unread);
    let (_, volume) = found.volumes.remove(0);
    (disk, volume)
}

/// The items of the folder `folder` of `volume`.
fn items(disk: &mut Disk, volume: &Volume, folder: u32) -> Result<Vec<Item>, Fault> {
    let mut items = Vec::new();
    volume.list::<Fault>(disk, folder, |_, item| {
        items.push(item);
        Ok(())
    })?;
    Ok(items)
}

/// The volume of the test image, as a raw disk of it alone holds it.
fn flat_volume() -> Vec<u8> {
    let bytes = document();
    let (mut disk, _) = opened(&bytes);
    let mut volume = vec![0u8; VOLUME_LENGTH];
    disk.read(VOLUME_SECTOR * SECTOR, &mut volume).unwrap();
    volume
}

/// Where in the flat volume `volume` the data of the catalog record of `name` in the folder
/// `parent` starts.
fn record_of(volume: &[u8], parent: u32, name: &str) -> usize {
    let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let mut key = parent.to_be_bytes().to_vec();
    key.extend_from_slice(&(units.len() as u16 / 2).to_be_bytes());
    key.extend_from_slice(&units);
    let at = volume
        .windows(key.len())
        .position(|window| window == key)
        .unwrap_or_else(|| panic!("no record of {name}"));
    at + key.len()
}

/// The XHTML and metadata of the disk image `bytes`, read from a file of the image's name.
fn extract(bytes: &[u8]) -> (String, crate::Metadata) {
    let scratch = Scratch::new("apple-disk-image");
    let path = scratch.write("lighthouse.dmg", bytes);
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string(path.to_str().unwrap())
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
fn reads_the_files_of_the_test_image() {
    let (xml, metadata) = extract(&document());
    assert_eq!(metadata["Content-Type"], ["application/x-apple-diskimage"]);
    assert_eq!(
        files(&xml),
        [
            "ReadMe.txt",
            "shared.txt",
            "charts/soundings.bin",
            "logbook/Caf\u{e9} visits.txt",
            "logbook/maintenance notes.txt",
            "logbook/shared again.txt",
            "logbook/1890s/F\u{f8}rste storm.txt",
        ]
    );
    for text in [
        "Every evening at dusk the keeper climbed the tower",
        "shared by two names through a hard link",
        "keeper's caf\u{e9} on Sundays",
        "Oil the lamp weekly",
        "the lamp burned until dawn",
    ] {
        assert!(xml.contains(text), "{text}: {xml}");
    }
    assert_eq!(
        xml.matches("shared by two names through a hard link")
            .count(),
        2,
        "{xml}"
    );
    assert_eq!(metadata[SYMBOLIC_LINK], ["latest.txt -> ReadMe.txt"]);
    assert_eq!(metadata["dc:title"], ["Lighthouse"]);
    assert_eq!(metadata["dcterms:created"], ["2026-11-14T12:00:00Z"]);
    assert_eq!(metadata["dcterms:modified"], ["2026-11-14T12:00:00Z"]);
    let failures = metadata[EMBEDDED_STREAM_EXCEPTION].join("\n");
    assert!(
        failures.contains("(compressed.txt): its data is compressed by HFS Plus"),
        "{failures}"
    );
    assert!(!xml.contains("HFS+ Private Data"), "{xml}");
    assert!(!xml.contains("iNode30"), "{xml}");
}

/// `charts/soundings.bin` is in ten extents, the last two in the extents overflow file, in
/// chunks stored, coded with bzip2, ADC and zlib.
#[test]
fn reads_a_file_through_the_extents_overflow_file() {
    let bytes = document();
    let (mut disk, volume) = opened(&bytes);
    let soundings = items(&mut disk, &volume, CHARTS).unwrap().remove(0);
    let Kind::File(file) = soundings.kind else {
        panic!("{soundings:?}");
    };
    let Ok(Ok(content)) = content(&mut disk, &volume, &file, "charts/soundings.bin") else {
        panic!("soundings.bin is unread");
    };
    assert_eq!(
        (content.len(), crate::compress::crc32(&content)),
        (37_864, 0x77fa_d55c)
    );
}

#[test]
fn reads_a_raw_image_of_the_volume_alone_in_place() {
    let volume_bytes = flat_volume();
    let (mut disk, volume) = opened(&volume_bytes);
    let root = items(&mut disk, &volume, ROOT_FOLDER).unwrap();
    let readme = root.iter().find(|item| item.name == "ReadMe.txt").unwrap();
    let Kind::File(file) = readme.kind else {
        panic!("{readme:?}");
    };
    let Ok(Ok(content)) = content(&mut disk, &volume, &file, "ReadMe.txt") else {
        panic!("ReadMe.txt is unread");
    };
    assert!(matches!(content, Input::Within(..)));
    assert!(content.starts_with(b"The Lighthouse Keeper"));
}

#[test]
fn records_folders_whose_leaves_loop_and_folders_read_twice() {
    let mut looped = flat_volume();
    looped[5 * 4096..5 * 4096 + 4].copy_from_slice(&1u32.to_be_bytes());
    let (mut disk, volume) = opened(&looped);
    let failed = items(&mut disk, &volume, LOGBOOK).unwrap_err();
    assert_eq!(
        failed,
        Fault::Corrupt("the catalog's leaf nodes link back to node 1".into())
    );
    let (xml, metadata) = extract(&looped);
    assert!(files(&xml).contains(&"logbook/1890s/F\u{f8}rste storm.txt"));
    let failures = metadata[EMBEDDED_STREAM_EXCEPTION].join("\n");
    assert!(
        failures.contains("(logbook): the catalog's leaf nodes link back to node 1"),
        "{failures}"
    );

    let mut twice = flat_volume();
    let logbook = record_of(&twice, ROOT_FOLDER, "logbook");
    twice[logbook + 8..logbook + 12].copy_from_slice(&ROOT_FOLDER.to_be_bytes());
    let (_, metadata) = extract(&twice);
    let failures = metadata[EMBEDDED_STREAM_EXCEPTION].join("\n");
    assert!(
        failures.contains("(logbook): it is the folder 2, which is read already"),
        "{failures}"
    );
}

#[test]
fn records_files_whose_extents_are_missing() {
    let mut volume_bytes = flat_volume();
    let key = [0u8, 0, 0, 0, 0, 24, 0, 0, 0, 8];
    let at = volume_bytes[2 * 4096..4 * 4096]
        .windows(key.len())
        .position(|window| window == key)
        .unwrap()
        + 2 * 4096;
    volume_bytes[at + 9] = 7;
    let (mut disk, volume) = opened(&volume_bytes);
    let soundings = items(&mut disk, &volume, CHARTS).unwrap().remove(0);
    let Kind::File(file) = soundings.kind else {
        panic!("{soundings:?}");
    };
    let Ok(Err(reason)) = content(&mut disk, &volume, &file, "charts/soundings.bin") else {
        panic!("soundings.bin is read");
    };
    assert_eq!(
        reason,
        "the extents overflow file holds no extents of file 24 from block 8"
    );
}

#[test]
fn fails_catalogs_that_point_past_their_nodes() {
    let mut volume_bytes = flat_volume();
    let index = 7 * 4096;
    let first = usize::from(u16::from_be_bytes([
        volume_bytes[index + 4094],
        volume_bytes[index + 4095],
    ]));
    let key = usize::from(u16::from_be_bytes([
        volume_bytes[index + first],
        volume_bytes[index + first + 1],
    ]));
    let child = index + first + 2 + key;
    volume_bytes[child..child + 4].copy_from_slice(&99u32.to_be_bytes());
    let (mut disk, volume) = opened(&volume_bytes);
    assert_eq!(
        items(&mut disk, &volume, hfs::ROOT_PARENT).unwrap_err(),
        Fault::Corrupt("the node 99 of the catalog is past its 8 nodes".into())
    );
    let mut sized = flat_volume();
    sized[4 * 4096 + 32..4 * 4096 + 34].copy_from_slice(&1000u16.to_be_bytes());
    let failure = refusal(&sized).unwrap_err().to_string();
    assert!(
        failure.contains("its catalog file: its nodes are 1000 bytes"),
        "{failure}"
    );
}

/// A volume wrapped in an HFS volume, as old Mac OS wrote them, is read for the volume inside.
#[test]
fn reads_a_volume_wrapped_in_an_hfs_volume() {
    let mut wrapped = vec![0u8; 8192];
    wrapped[1024..1026].copy_from_slice(b"BD");
    wrapped[1024 + 0x14..1024 + 0x18].copy_from_slice(&4096u32.to_be_bytes());
    wrapped[1024 + 0x1C..1024 + 0x1E].copy_from_slice(&16u16.to_be_bytes());
    wrapped[1024 + 0x7C..1024 + 0x7E].copy_from_slice(b"H+");
    wrapped.extend(flat_volume());
    let (mut disk, volume) = opened(&wrapped);
    let root = volume.root(&mut disk).unwrap().unwrap();
    assert_eq!(root.name, "Lighthouse");
}

#[test]
fn reads_an_lzfse_chunk() {
    let stream = std::fs::read("tests/fixtures/apple_disk_image/sectors.lzfse").unwrap();
    let bytes = image(&[(
        "whole disk",
        0,
        vec![Run {
            kind: 0x8000_0007,
            sector: 0,
            sectors: 64,
            data: stream,
        }],
    )]);
    let image = udif::read(&bytes).unwrap();
    let mut disk = Disk::new(&bytes, Pages::IN_MEMORY, image.chunks);
    let mut decoded = vec![0u8; 64 * 512];
    disk.read(0, &mut decoded).unwrap();
    assert_eq!(crate::compress::crc32(&decoded), 0xf282_caf4);
}

/// A file of 300 MiB of a chunk of zeros is not handed on: the files of the image stop there.
#[test]
fn stops_handing_on_files_past_the_decode_limit() {
    let mut volume_bytes = flat_volume();
    let blocks: u32 = 80_000;
    volume_bytes[1024 + 44..1024 + 48].copy_from_slice(&blocks.to_be_bytes());
    let soundings = record_of(&volume_bytes, CHARTS, "soundings.bin") + 88;
    let size: u64 = 300 << 20;
    let fork_blocks = (size / 4096) as u32;
    volume_bytes[soundings..soundings + 8].copy_from_slice(&size.to_be_bytes());
    volume_bytes[soundings + 12..soundings + 16].copy_from_slice(&fork_blocks.to_be_bytes());
    volume_bytes[soundings + 16..soundings + 24].copy_from_slice(&[0, 0, 0, 14, 0, 0, 0, 0]);
    volume_bytes[soundings + 20..soundings + 24].copy_from_slice(&fork_blocks.to_be_bytes());
    volume_bytes[soundings + 24..soundings + 80].fill(0);
    let bomb = image(&[(
        "disk image",
        0,
        vec![
            Run {
                kind: 1,
                sector: 0,
                sectors: (VOLUME_LENGTH / 512) as u64,
                data: volume_bytes,
            },
            Run {
                kind: 0,
                sector: (VOLUME_LENGTH / 512) as u64,
                sectors: u64::from(blocks) * 8 - (VOLUME_LENGTH / 512) as u64,
                data: Vec::new(),
            },
        ],
    )]);
    let scratch = Scratch::new("apple-disk-image-bomb");
    let path = scratch.write("bomb.dmg", &bomb);
    let failed = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap_err()
        .to_string();
    assert!(failed.contains(WHAT), "{failed}");
}

/// Why the parser does not prepare the image `bytes`: what it does not read of it, or the
/// error it fails with.
fn refusal(bytes: &[u8]) -> Result<String, Error> {
    let configs = ParserConfigs::default();
    let metadata = TikaMetadata::new();
    let prepared = AppleDiskImageParser.prepare(bytes, &metadata, &configs);
    match prepared {
        Ok(_) => panic!("the image is prepared"),
        Err(Decline::Unread(reason)) => Ok(reason),
        Err(Decline::Fails(error)) => Err(error),
        Err(other) => panic!("{other:?}"),
    }
}

#[test]
fn refuses_images_without_an_hfs_plus_volume() {
    let mut apfs = vec![0u8; 8 * 512];
    apfs[32..36].copy_from_slice(b"NXSB");
    assert_eq!(
        refusal(&apfs).unwrap(),
        "the Apple disk image holds no HFS Plus volume, only an APFS container in the whole \
         disk, which dijo does not read"
    );
    let mut hfs = vec![0u8; 8 * 512];
    hfs[1024..1026].copy_from_slice(b"BD");
    let reason = refusal(&hfs).unwrap();
    assert!(
        reason.contains("an HFS volume in the whole disk"),
        "{reason}"
    );
    assert_eq!(
        refusal(&[0u8; 8 * 512]).unwrap_err().to_string(),
        "the Apple disk image holds no HFS Plus volume"
    );
    assert!(matches!(
        refusal(b"encrcdsa\0\0\0\x02"),
        Err(Error::Encrypted(_))
    ));
    let reason = refusal(b"sprs and more").unwrap();
    assert!(reason.contains("sparse disk image"), "{reason}");
}
