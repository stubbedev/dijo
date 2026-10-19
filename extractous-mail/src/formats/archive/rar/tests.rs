use super::*;

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

/// The documents of an entry the port cannot give whole, which Tika gives empty: the
/// port records why instead (#122).
const UNREAD: [(&str, &str); 3] = [
    ("corrupt-data.rar", "does not match its CRC"),
    ("method36.rar", "RAR version 36"),
    ("volume.part1.rar", "another volume"),
];

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

/// The files of the RAR archive `data` but its directories, with their data, empty for an
/// entry the port cannot give, if the port reads it.
fn archive_files(data: &[u8]) -> Option<Vec<ArchiveFile<'static>>> {
    let headers = headers::read(data).ok()?;
    let (names, _) = names(&headers);
    let mut files = Vec::new();
    let extracted = extract::each(data, &headers, |index, extracted| {
        files.push(file(&headers.files[index], &names[index], extracted).0);
        Ok::<(), std::convert::Infallible>(())
    });
    match extracted {
        Ok(()) => Some(files),
        Err(never) => match never {},
    }
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

#[test]
fn spools_an_entry_too_large_to_read_whole_one_entry_at_a_time() {
    use std::io::Read;

    use crate::input::MAX_READ_WHOLE;
    use extractous_core::large::{Binary, LargeFile, BUDGET, END};

    let mut large = Vec::new();
    Binary::new(MAX_READ_WHOLE + (16 << 20))
        .read_to_end(&mut large)
        .unwrap();
    let archive = rar(
        0,
        &[
            stored(b"large.bin", 0, &large),
            stored(b"end.txt", 0, END.as_bytes()),
        ],
    );
    drop(large);
    let file = LargeFile::written("large.rar", |out| out.write_all(&archive));
    drop(archive);
    let extractor = extractous_core::Extractor::new();
    let (text, _) = file.extract_with(&extractor, BUDGET, "extractous::RarParser");
    assert!(text.contains(END), "{text}");
}

/// RAR 1.4's 16-bit sum, `RarCRC.checkOldCrc` from zero.
fn old_sum(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |sum, &byte| {
        sum.wrapping_add(u16::from(byte)).rotate_left(1)
    })
}

/// A stored entry of a hand-made RAR 1.4 archive: its OEM name, DOS attributes, data and the
/// checksum its header gives.
struct Stored14<'a> {
    name: &'a [u8],
    attributes: u8,
    data: &'a [u8],
    sum: u16,
}

fn stored14<'a>(name: &'a [u8], attributes: u8, data: &'a [u8]) -> Stored14<'a> {
    Stored14 {
        name,
        attributes,
        data,
        sum: old_sum(data),
    }
}

/// A RAR 1.4 archive of stored entries: the marker, a main header of `main_flags`, and each
/// entry's 21-byte header, name and data.
fn rar14(main_flags: u8, entries: &[Stored14]) -> Vec<u8> {
    let mut archive = b"RE~^".to_vec();
    archive.extend_from_slice(&7u16.to_le_bytes());
    archive.push(main_flags);
    for entry in entries {
        archive.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        archive.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        archive.extend_from_slice(&entry.sum.to_le_bytes());
        archive.extend_from_slice(&(21 + entry.name.len() as u16).to_le_bytes());
        archive.extend_from_slice(&DOS_TIME.to_le_bytes());
        archive.push(entry.attributes);
        archive.push(0);
        archive.push(2);
        archive.push(entry.name.len() as u8);
        archive.push(0);
        archive.extend_from_slice(entry.name);
        archive.extend_from_slice(entry.data);
    }
    archive
}

/// The name, data and modification time of each file `archive_files` gives of `archive`.
fn read_files(archive: &[u8]) -> Vec<(String, Vec<u8>, Option<i64>)> {
    archive_files(archive)
        .expect("the port reads the archive")
        .into_iter()
        .map(|file| (file.name, file.content.to_vec(), file.modified_millis))
        .collect()
}

/// 2021-03-04 05:06:08 UTC, [`DOS_TIME`], in milliseconds since the epoch.
const DOS_TIME_MILLIS: Option<i64> = Some(1_614_834_368_000);

#[test]
fn reads_rar_1_4_archives_as_junrar_does() {
    let mut corrupt = stored14(b"corrupt.txt", 0x20, b"data whose sum is wrong");
    corrupt.sum ^= 1;
    let archive = rar14(
        0,
        &[
            stored14(b"DIR", 0x10, b""),
            stored14(b"DIR\\K\x84SE.TXT", 0x20, b"ost og br\xf8d"),
            corrupt,
            stored14(b"EMPTY.TXT", 0x20, b""),
        ],
    );
    let headers = headers::read(&archive).unwrap();
    assert_eq!(headers.files.len(), 4);
    assert!(headers.files[0].is_directory());
    assert_eq!(
        read_files(&archive),
        [
            (
                "DIR/K\u{e4}SE.TXT".to_owned(),
                b"ost og br\xf8d".to_vec(),
                DOS_TIME_MILLIS
            ),
            ("corrupt.txt".to_owned(), Vec::new(), DOS_TIME_MILLIS),
            ("EMPTY.TXT".to_owned(), Vec::new(), DOS_TIME_MILLIS),
        ]
    );
}

#[test]
fn fails_a_rar_1_4_archive_whose_header_is_cut_short() {
    let mut archive = rar14(0, &[stored14(b"A.TXT", 0x20, b"a")]);
    archive.extend_from_slice(&[1, 2, 3]);
    let unopened = headers::read(&archive).unwrap_err();
    assert!(
        unopened.reason.contains("Truncated RAR 1.4 file header"),
        "{unopened:?}"
    );
}

#[test]
fn finds_the_marker_of_a_self_extracting_archive_past_its_code() {
    let mut sfx = b"MZ".to_vec();
    sfx.resize(4096, 0x90);
    sfx.extend_from_slice(&rar(0, &[stored(b"inside.txt", 0, b"self-extracted")]));
    assert_eq!(
        read_files(&sfx),
        [(
            "inside.txt".to_owned(),
            b"self-extracted".to_vec(),
            DOS_TIME_MILLIS
        )]
    );
    let mut old = b"MZ".to_vec();
    old.resize(28, 0x90);
    old.extend_from_slice(b"RSFX");
    old.resize(512, 0x90);
    old.extend_from_slice(&rar14(0, &[stored14(b"OLD.TXT", 0x20, b"from 1.4")]));
    assert_eq!(read_files(&old)[0].1, b"from 1.4");
    let mut uncorroborated = old.clone();
    uncorroborated[28..32].copy_from_slice(b"XXXX");
    assert!(headers::read(&uncorroborated).is_err());
}
