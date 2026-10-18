#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika, assert_read_where_tika_fails, assert_same_as_tika, conformance_corpus,
    documents, Scratch,
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

/// A record of Quattro Pro's: its type, its length and `body`.
fn record(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = kind.to_le_bytes().to_vec();
    bytes.extend(u16::try_from(body.len()).unwrap().to_le_bytes());
    bytes.extend(body);
    bytes
}

/// A Quattro Pro 9 workbook's records: its beginning, its user, a sheet, a string table, labels
/// and a comment.
#[cfg(feature = "tika")]
fn quattro_records() -> Vec<u8> {
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

/// A WordPerfect document too large to read whole is read a character at a time, its text
/// written a chunk at a time.
#[test]
fn reads_documents_too_large_to_read_whole() {
    let words = b"ord ".repeat(256);
    let count = extractous_core::large::bodies_past_limit(words.len());
    let file = extractous_core::large::LargeFile::new(
        "long.wpd",
        &prefix(WP5_MAJOR_VERSION, 1, false),
        count,
        |_| words.clone(),
        b"",
    );
    let mut letters = 0u64;
    file.extract_each(WordPerfectParser.name(), |text| {
        letters += text.matches('o').count() as u64;
    });
    assert_eq!(letters, count * 256);
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

    let (read, deferred) = documents(&OldQuattroProParser, &[&corpus]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(read.len(), TIKAS_QUATTRO_CELLS.len(), "{read:?}");
    for path in &read {
        let name = path.file_name().unwrap().to_str().unwrap();
        let (_, cells) = TIKAS_QUATTRO_CELLS
            .iter()
            .find(|(spreadsheet, _)| *spreadsheet == name)
            .unwrap_or_else(|| panic!("{name} is no Quattro Pro test spreadsheet"));
        let (xml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_file_to_string(path.to_str().unwrap())
            .unwrap();
        for cell in *cells {
            assert!(xml.contains(cell), "{cell:?} in {name}: {xml}");
        }
    }
    let paths: Vec<&Path> = read.iter().map(PathBuf::as_path).collect();
    assert_read_where_tika_fails(&paths, "#158");
}

/// A spreadsheet of Quattro Pro for DOS (`.wq1`): a label, an integer, a number, a formula's
/// value and its text result, and an error.
fn quattro_dos() -> Vec<u8> {
    fn cell(column: u16, row: u16, data: &[u8]) -> Vec<u8> {
        let mut body = vec![0x80];
        body.extend(column.to_le_bytes());
        body.extend(row.to_le_bytes());
        body.extend(data);
        body
    }
    let mut formula = 30f64.to_le_bytes().to_vec();
    formula.extend([3, 0, 1, 2, 3]);
    [
        record(0x0000, &0x5120u16.to_le_bytes()),
        record(0x0006, &[0; 8]),
        record(0x000F, &cell(0, 0, b"'\x07Fangst\x91")),
        record(0x000D, &cell(1, 0, &7i16.to_le_bytes())),
        record(0x000E, &cell(0, 1, &2.5f64.to_le_bytes())),
        record(0x0010, &cell(3, 1, &formula)),
        record(0x0010, &cell(0, 2, &formula)),
        record(0x0033, &cell(0, 2, b"\x05torsk")),
        record(0x000E, &cell(1, 2, &f64::NAN.to_le_bytes())),
        record(0x0001, &[]),
    ]
    .concat()
}

/// A spreadsheet of Quattro Pro for Windows (`.wb1`) of two pages, a label on each, and what
/// follows its end.
fn quattro_windows() -> Vec<u8> {
    fn cell(column: u8, page: u8, row: u16, data: &[u8]) -> Vec<u8> {
        let mut body = vec![column, page];
        body.extend(row.to_le_bytes());
        body.extend(1u16.to_le_bytes());
        body.extend(data);
        body
    }
    [
        record(0x0000, &0x1001u16.to_le_bytes()),
        record(0x000F, &cell(0, 0, 0, b"'Sild\xe6\0x")),
        record(0x000D, &cell(0, 0, 1, &(-3i16).to_le_bytes())),
        record(0x000F, &cell(1, 1, 0, b"^Kuller\0")),
        record(0x0001, &[]),
        record(0x0000, b"QPW$ExtendedStorage"),
    ]
    .concat()
}

fn extract_xml(bytes: &[u8]) -> String {
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(bytes)
        .unwrap()
        .0
}

#[test]
fn reads_quattro_pro_for_dos_cells() {
    let xml = extract_xml(&quattro_dos());
    let rows = [
        "<tr>\t<td>Fangst\u{e6}</td>\t<td>7</td></tr>",
        "<tr>\t<td>2.5</td>\t<td />\t<td />\t<td>30</td></tr>",
        "<tr>\t<td>torsk</td></tr>",
    ];
    for row in rows {
        assert!(xml.contains(row), "{row:?} in {xml}");
    }
    assert_eq!(xml.matches("<tr>").count(), 3, "{xml}");
}

#[test]
fn reads_quattro_pro_for_windows_pages() {
    let xml = extract_xml(&quattro_windows());
    assert_eq!(xml.matches("<div class=\"page\">").count(), 2, "{xml}");
    assert!(xml.contains("<td>Sild\u{e6}</td>"), "{xml}");
    assert!(xml.contains("<td>-3</td>"), "{xml}");
    assert!(xml.contains("<tr>\t<td />\t<td>Kuller</td></tr>"), "{xml}");
}

#[test]
fn fails_quattro_pro_spreadsheets_that_do_not_read() {
    let fails = |bytes: &[u8]| {
        crate::Extractor::new()
            .extract_bytes_to_string(bytes)
            .unwrap_err()
            .to_string()
    };
    let mut cut = quattro_dos();
    cut.truncate(40);
    assert!(fails(&cut).contains("is cut"), "{}", fails(&cut));
    let mut short = quattro_dos();
    short[20] = 2;
    assert!(fails(&short).contains("is cut"), "{}", fails(&short));
}

/// The cells of Tika's Quattro Pro 1 to 6 test spreadsheets, which Tika's path fails on.
#[cfg(feature = "tika")]
const TIKAS_QUATTRO_CELLS: [(&str, &[&str]); 4] = [
    ("testQuattro.wq1", &["<td>OBSERV</td>", "<td>4001</td>"]),
    (
        "testQuattro.wq2",
        &[
            "<td>SATURATED HYDRAULIC CONDUCTIVITY: INVERSE AUGERHOLE METHOD</td>",
            "<td>DEPTH(D):</td>",
            "<td>sal</td>",
            "<td>1.012679207245471E-15</td>",
        ],
    ),
    (
        "testQuattro.wb1",
        &["<td>X</td>", "<td>Y</td>", "<td>18</td>"],
    ),
    ("testQuattro.wb2", &["<td>Z</td>", "<td>10</td>"]),
];

#[cfg(feature = "tika")]
#[test]
fn reads_quattro_pro_where_tikas_path_fails() {
    let scratch = Scratch::new("old-quattro");
    let spreadsheets = [
        scratch.write("sheet.wq1", &quattro_dos()),
        scratch.write("sheet.wb1", &quattro_windows()),
    ];
    let (read, deferred) = documents(&OldQuattroProParser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(read.len(), spreadsheets.len(), "{read:?}");
    let paths: Vec<&Path> = spreadsheets.iter().map(PathBuf::as_path).collect();
    assert_read_where_tika_fails(&paths, "#158");
}
