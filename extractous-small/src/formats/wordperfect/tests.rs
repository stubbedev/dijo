#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
};
#[cfg(feature = "tika")]
use crate::ole2::tests::compound;

/// A WordPerfect prefix area of `major` and `minor` version, its document area right after it.
fn prefix(major: u8, minor: u8, encrypted: bool) -> Vec<u8> {
    let length: i32 = if major == WP6_MAJOR_VERSION { 24 } else { 16 };
    let mut bytes = b"\xFFWPC".to_vec();
    bytes.extend(length.to_le_bytes());
    bytes.extend([
        1,
        if major == WP6_MAJOR_VERSION { 36 } else { 10 },
        major,
        minor,
    ]);
    bytes.extend(u16::from(encrypted).to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    if major == WP6_MAJOR_VERSION {
        bytes.extend([0; 4]);
        bytes.extend(4321i32.to_le_bytes());
    }
    bytes
}

#[cfg(feature = "tika")]
fn wp5() -> Vec<u8> {
    let mut bytes = prefix(WP5_MAJOR_VERSION, 1, false);
    bytes.extend(b"Hej fra K");
    bytes.extend([192, 24, 1, 192]);
    bytes.extend(b"benhavn.");
    bytes.extend([10]);
    bytes.extend(b"Linje");
    bytes.extend([13, 160, 170]);
    bytes.extend([195, 12, 195]);
    bytes.extend([208, 4, 3, 0, 1, 2, 3]);
    bytes.extend([192, 200, 1, 192, 192, 3, 99, 192]);
    bytes.extend([12]);
    bytes.extend(b"lang ".repeat(1000));
    bytes.extend([140]);
    bytes
}

fn wp6() -> Vec<u8> {
    let mut bytes = prefix(WP6_MAJOR_VERSION, 1, false);
    bytes.extend(b"S");
    bytes.extend([5, 3]);
    bytes.extend(b"ster ");
    bytes.extend([23, 0x0F, 32]);
    bytes.extend([240, 25, 1, 240]);
    bytes.extend([128, 129, 132, 198]);
    bytes.extend([208, 10, 6, 0, 9, 9]);
    bytes.extend([208, 5, 4, 0]);
    bytes.extend([241, 0, 1, 2, 3]);
    bytes.extend(b"slettet");
    bytes.extend([241, 1, 1, 2, 3]);
    bytes.extend([138, 1, 2, 139]);
    bytes.extend([213, 1, 4, 0, 224, 1, 4, 0]);
    bytes.extend([242, 1, 242]);
    bytes.extend([204]);
    bytes.extend(b"slut");
    bytes.extend([255, 7, 7, 255]);
    bytes
}

/// A Quattro Pro 9 workbook's records: its beginning, its user, a sheet, a string table, labels
/// and a comment.
#[cfg(feature = "tika")]
fn quattro_records() -> Vec<u8> {
    fn record(kind: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = kind.to_le_bytes().to_vec();
        bytes.extend((body.len() as u16).to_le_bytes());
        bytes.extend(body);
        bytes
    }
    fn label(text: &str) -> Vec<u8> {
        let mut bytes = ((text.len() - 1) as u16).to_le_bytes().to_vec();
        bytes.push(1);
        bytes.extend(text.as_bytes());
        bytes
    }
    let mut bof = b"QPW9".to_vec();
    for value in [9u16, 1234, 0, 7, 3] {
        bof.extend(value.to_le_bytes());
    }
    bof.extend([0; 6]);
    let mut user = label("Ane");
    user.extend(label("Bo"));
    let mut sheet = vec![0; 6];
    sheet.extend([0; 8]);
    sheet.extend([0; 4]);
    sheet.extend(label("Fangst"));
    let mut strings = 2i32.to_le_bytes().to_vec();
    strings.extend([0; 8]);
    strings.extend(label("Torsk"));
    strings.extend(label("Sild"));
    let mut cell = vec![0; 2];
    cell.extend([0; 4]);
    cell.extend([0; 2]);
    cell.extend(label("Kuller"));
    let mut comment = vec![0; 2];
    comment.extend([0; 8]);
    comment.extend(label("Ane"));
    comment.extend(label("Godt fiskeri"));
    [
        record(0x0001, &bof),
        record(0x0005, &user),
        record(0x0601, &sheet),
        record(0x0407, &strings),
        record(0x0c72, &cell),
        record(0x0c80, &comment),
        record(0x0002, &[1, 2, 3]),
    ]
    .concat()
}

#[test]
fn reads_prefix_areas_as_tika_does() {
    let prefix = PrefixArea::read(&wp6()).unwrap();
    assert_eq!(prefix.file_id, "\u{FF}WPC");
    assert_eq!(prefix.document_area, 24);
    assert_eq!(prefix.file_size, Some(4321));
    assert!(!prefix.encrypted);
    assert_eq!(PrefixArea::read(b"\xFFWPC\x10\x00"), Err(EndOfFile));
}

#[cfg(feature = "tika")]
#[test]
fn extracts_wordperfect_and_quattro_pro_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    for parser in [&WordPerfectParser as &dyn Parser, &QuattroProParser] {
        let (parsed, deferred) = documents(parser, &roots);
        assert!(
            parsed.is_empty() && deferred.is_empty(),
            "{parsed:?} {deferred:?}"
        );
    }

    let scratch = Scratch::new("wordperfect");
    let five = scratch.write("five.wp", &wp5());
    let six = scratch.write("six.wpd", &wp6());
    let mut cut = wp6();
    cut.extend([208, 1, 40, 0, 1]);
    let cut = scratch.write("cut.wpd", &cut);
    let mut encrypted = prefix(WP6_MAJOR_VERSION, 1, true);
    encrypted.extend(b"hemmelig");
    let encrypted = scratch.write("encrypted.wpd", &encrypted);
    let mut beyond = prefix(WP5_MAJOR_VERSION, 0, false);
    beyond[4] = 200;
    let beyond = scratch.write("beyond.wp", &beyond);
    let records = quattro_records();
    let quattro = scratch.write("quattro.qpw", &compound(&["NativeContent_MAIN"], &records));
    let cut_quattro = scratch.write(
        "cut-quattro.qpw",
        &compound(&["NativeContent_MAIN"], &records[..records.len() - 9]),
    );
    let (parsed, deferred) = documents(&WordPerfectParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 5, "{parsed:?}");
    let (parsed, deferred) = documents(&QuattroProParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 2, "{parsed:?}");
    assert_same_as_tika(
        &[five.as_path(), six.as_path()],
        WordPerfectParser.tika_name(),
    );
    assert_same_as_tika(&[quattro.as_path()], QuattroProParser.tika_name());
    assert_fails_as_tika(&[
        cut.as_path(),
        encrypted.as_path(),
        beyond.as_path(),
        cut_quattro.as_path(),
    ]);
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_wordperfect_and_quattro_pro_as_tikas_path_does() {
    let corpus = conformance_corpus();
    let (parsed, deferred) = documents(&WordPerfectParser, &[&corpus]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 3, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, WordPerfectParser.tika_name());

    let (parsed, deferred) = documents(&QuattroProParser, &[&corpus]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, QuattroProParser.tika_name());

    let (failing, deferred) = documents(&OldQuattroProParser, &[&corpus]);
    assert_eq!(deferred.len(), 1, "{deferred:?}");
    assert!(deferred[0].ends_with("testQUATTRO.wb3"), "{deferred:?}");
    assert_eq!(failing.len(), 4, "{failing:?}");
    let paths: Vec<&Path> = failing.iter().map(PathBuf::as_path).collect();
    assert_fails_as_tika(&paths);
}

/// The first records of a Quattro Pro 1 spreadsheet for Windows, as `testQuattro.wb1` starts.
const QUATTRO_WB1: [u8; 16] = [
    0x00, 0x00, 0x02, 0x00, 0x01, 0x10, 0xC9, 0x00, 0x02, 0x00, 0x00, 0x06, 0x4C, 0x00, 0x02, 0x00,
];

/// The first records of a Quattro Pro spreadsheet for DOS, as `testQuattro.wq1` starts.
const QUATTRO_WQ1: [u8; 16] = [
    0x00, 0x00, 0x02, 0x00, 0x20, 0x51, 0x06, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0D, 0x00,
];

#[test]
fn fails_old_quattro_pro_as_office_parser_does() {
    let error = crate::Extractor::new()
        .extract_bytes_to_string(&QUATTRO_WB1)
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "TIKA-198: Illegal IOException from org.apache.tika.parser.microsoft.OfficeParser <- \
         caused by org.apache.poi.poifs.filesystem.NotOLE2FileException: Invalid header \
         signature; read 0x00C9100100020000, expected 0xE11AB1A1E011CFD0 - Your file appears \
         not to be a valid OLE2 document"
    );
}

#[cfg(feature = "tika")]
#[test]
fn fails_old_quattro_pro_as_tikas_path_does() {
    let scratch = Scratch::new("old-quattro");
    let spreadsheets = [
        scratch.write("sheet.wb1", &QUATTRO_WB1),
        scratch.write("sheet.wq1", &QUATTRO_WQ1),
    ];
    let (failing, deferred) = documents(&OldQuattroProParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(failing.len(), spreadsheets.len(), "{failing:?}");
    let paths: Vec<&Path> = spreadsheets.iter().map(PathBuf::as_path).collect();
    assert_fails_as_tika(&paths);
}
