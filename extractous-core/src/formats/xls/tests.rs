use std::path::{Path, PathBuf};

use super::records::sid;
use super::stream::tests::record;
use super::*;
use crate::formats::parity::{self, assert_same_as_tika_with};

const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";

/// The workbooks of Tika's own tests this port leaves to Tika, with why.
const DEFERRED_CONFORMANCE: [(&str, &str); 8] = [
    ("testEXCEL_5.xls", "before BIFF8"),
    ("testEXCEL_95.xls", "before BIFF8"),
    ("testEXCEL_embeddedPDF_windows.xls", "embeds documents"),
    ("testEXCEL_embeded.xls", "embeds documents"),
    ("testEXCEL_protected_passtika.xls", "encrypted"),
    ("testExcel_embeddedPDF.xls", "embeds documents"),
    ("testMSChart-govdocs-428996.xls", "embeds documents"),
    ("testWPSAttachment.xls", "embeds documents"),
];

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn deferral(path: &Path, configs: &ParserConfigs) -> Option<String> {
    let input = std::fs::read(path).unwrap();
    XlsParser.defers(&input, &TikaMetadata::new(), configs)
}

/// Of `documents`, those this port reads under `configs`, after checking that it leaves exactly
/// `deferred` to Tika, by file name and a part of the reason.
fn ported(
    documents: &[PathBuf],
    deferred: &[(&str, &str)],
    configs: &ParserConfigs,
) -> Vec<PathBuf> {
    let mut ported = Vec::new();
    let mut unexpected = Vec::new();
    for document in documents {
        let name = file_name(document);
        let expected = deferred.iter().find(|(deferred, _)| *deferred == name);
        match (deferral(document, configs), expected) {
            (None, None) => ported.push(document.clone()),
            (Some(reason), Some((_, why))) if reason.contains(why) => {}
            (reason, expected) => {
                unexpected.push(format!("{name}: {reason:?}, expected {expected:?}"))
            }
        }
    }
    assert!(unexpected.is_empty(), "{}", unexpected.join("\n"));
    ported
}

#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let (mut documents, deferred) = parity::documents(&XlsParser, &[&parity::conformance_corpus()]);
    documents.extend(deferred);
    documents.sort();
    let defaults = ParserConfigs::default();
    let ported = ported(&documents, &DEFERRED_CONFORMANCE, &defaults);
    assert_eq!(ported.len(), 20);
    let documents: Vec<&Path> = ported.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_with(&documents, TIKA_PARSER, &defaults);
}

fn bof(kind: i16) -> Vec<u8> {
    let mut data = 0x0600i16.to_le_bytes().to_vec();
    data.extend(kind.to_le_bytes());
    data.extend([0; 12]);
    record(sid::BOF, &data)
}

/// A style of format `format`.
fn extended_format(format: i16) -> Vec<u8> {
    let mut data = vec![0, 0];
    data.extend(format.to_le_bytes());
    data.extend([0; 16]);
    record(sid::EXTENDED_FORMAT, &data)
}

fn cell(row: u16, column: u16, xf: u16) -> Vec<u8> {
    [row, column, xf]
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn label(row: u16, column: u16, text: &str) -> Vec<u8> {
    let mut data = cell(row, column, 0);
    data.extend((text.len() as u16).to_le_bytes());
    data.push(0);
    data.extend(text.as_bytes());
    record(sid::LABEL, &data)
}

fn number(row: u16, column: u16, xf: u16, value: f64) -> Vec<u8> {
    let mut data = cell(row, column, xf);
    data.extend(value.to_le_bytes());
    record(sid::NUMBER, &data)
}

fn globals(sheets: &[&str], shared: &[&str]) -> Vec<u8> {
    let mut data = bof(0x05);
    data.extend(extended_format(0));
    data.extend(extended_format(14));
    for name in sheets {
        let mut sheet = vec![0, 0, 0, 0, 0, 0, name.len() as u8, 0];
        sheet.extend(name.as_bytes());
        data.extend(record(sid::BOUND_SHEET, &sheet));
    }
    let mut table = (shared.len() as i32).to_le_bytes().to_vec();
    table.extend((shared.len() as i32).to_le_bytes());
    for text in shared {
        table.extend((text.len() as u16).to_le_bytes());
        table.push(0);
        table.extend(text.as_bytes());
    }
    data.extend(record(sid::SST, &table));
    data.extend(record(sid::EOF, &[]));
    data
}

fn settings() -> OfficeParserConfig {
    OfficeParserConfig::new()
}

fn read(stream: &[u8]) -> Result<Extracted, Unported> {
    let zone = TimeZone::fixed(0);
    read_records(stream, &settings(), Box::leak(Box::new(zone))).map(|(read, _)| read)
}

fn texts(out: &[Out]) -> Vec<String> {
    out.iter()
        .filter_map(|event| match event {
            Out::Text(text) | Out::Element(_, text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn writes_cells_in_a_table_per_sheet() {
    let mut stream = globals(&["One"], &["shared"]);
    stream.extend(bof(0x10));
    stream.extend(label(0, 1, " label "));
    let mut shared = cell(1, 0, 0);
    shared.extend(0i32.to_le_bytes());
    stream.extend(record(sid::LABEL_SST, &shared));
    stream.extend(number(1, 1, 0, 2.5));
    stream.extend(number(2, 0, 1, 45_000.0));
    stream.extend(label(0, 1, "again"));
    stream.extend(record(sid::EOF, &[]));
    let read = read(&stream).unwrap();
    let [page] = &read.pages[..] else {
        panic!("one page: {:?}", read.pages);
    };
    assert_eq!(
        texts(&page.table),
        ["One", "label", "shared", "2.5", "3/15/23"]
    );
    assert_eq!(page.outside, [Cell::Text("again".to_owned())]);
    let row_starts = page
        .table
        .iter()
        .filter(|event| **event == Out::Start("tr"))
        .count();
    assert_eq!(row_starts, 3);
}

#[test]
fn splits_a_sheet_at_an_embedded_chart() {
    let mut stream = globals(&["One", "Two"], &[]);
    stream.extend(bof(0x10));
    stream.extend(label(0, 0, "before"));
    stream.extend(bof(0x20));
    let mut series = vec![0, 0, 6, 0];
    series.extend(b"series");
    stream.extend(record(sid::SERIES_TEXT, &series));
    stream.extend(record(sid::EOF, &[]));
    stream.extend(label(1, 0, "after"));
    stream.extend(record(sid::EOF, &[]));
    let read = read(&stream).unwrap();
    assert_eq!(read.pages.len(), 2);
    assert_eq!(texts(&read.pages[0].table), ["One", "before"]);
    assert_eq!(texts(&read.pages[1].table), ["One"]);
    assert_eq!(read.pages[1].outside, [Cell::Text("series".to_owned())]);
    assert_eq!(read.outside, [Cell::Text("after".to_owned())]);
}

#[test]
fn records_comment_authors_and_hidden_sheets() {
    let mut stream = bof(0x05);
    stream.extend(extended_format(0));
    let mut hidden = vec![0, 0, 0, 0, 1, 0, 1, 0];
    hidden.push(b'H');
    stream.extend(record(sid::BOUND_SHEET, &hidden));
    stream.extend(record(sid::EOF, &[]));
    stream.extend(bof(0x10));
    let mut note = cell(0, 0, 0);
    note.extend([0, 0, 3, 0, 0]);
    note.extend(b"Bob");
    stream.extend(record(sid::NOTE, &note));
    stream.extend(record(sid::EOF, &[]));
    let read = read(&stream).unwrap();
    assert_eq!(read.hidden_sheets, ["H"]);
    assert_eq!(read.comment_authors.values().collect::<Vec<_>>(), ["Bob"]);
}

#[test]
fn leaves_encrypted_workbooks_and_pictures_to_tika() {
    let mut stream = bof(0x05);
    stream.extend(record(sid::FILE_PASS, &[0; 6]));
    assert!(read(&stream).is_err_and(|Unported(reason)| reason.contains("encrypted")));
}

#[test]
fn fails_where_a_shared_string_comes_before_the_table() {
    let mut stream = bof(0x05);
    stream.extend(extended_format(0));
    stream.extend(record(sid::EOF, &[]));
    stream.extend(bof(0x10));
    let mut shared = cell(0, 0, 0);
    shared.extend(0i32.to_le_bytes());
    stream.extend(record(sid::LABEL_SST, &shared));
    stream.extend(record(sid::EOF, &[]));
    assert!(read(&stream).is_err());
}

fn escher(options: u16, kind: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = options.to_le_bytes().to_vec();
    bytes.extend(kind.to_le_bytes());
    bytes.extend((body.len() as i32).to_le_bytes());
    bytes.extend(body);
    bytes
}

#[test]
fn finds_the_pictures_and_the_sheets_showing_them() {
    let mut blip = vec![0; 17];
    blip.extend(b"png?");
    let mut stored = vec![0; 36];
    stored.extend(escher(0x6E00, 0xF01E, &blip));
    let group = escher(
        0x000F,
        0xF000,
        &escher(0x000F, 0xF001, &escher(0x0062, 0xF007, &stored)),
    );
    let mut options = 0x0104u16.to_le_bytes().to_vec();
    options.extend(1i32.to_le_bytes());
    let shapes = escher(
        0x000F,
        0xF002,
        &escher(0x000F, 0xF004, &escher(0x0013, 0xF00B, &options)),
    );
    let mut stream = globals(&["One", "Two"], &[]);
    let end = stream.len() - 4;
    stream.splice(end..end, record(sid::DRAWING_GROUP, &group));
    stream.extend(bof(0x10));
    stream.extend(record(sid::EOF, &[]));
    stream.extend(bof(0x10));
    stream.extend(record(sid::DRAWING, &shapes));
    stream.extend(record(sid::EOF, &[]));
    let zone = TimeZone::fixed(0);
    let (_, pictures) = read_records(&stream, &settings(), Box::leak(Box::new(zone))).unwrap();
    let [picture] = &pictures[..] else {
        panic!("one picture");
    };
    assert_eq!(picture.data, b"png?");
    assert_eq!(picture.media_type, "image/png");
    assert_eq!(picture.sheets, BTreeSet::from([2]));
}
