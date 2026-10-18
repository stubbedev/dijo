#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::records::sid;
use super::stream::tests::record;
use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{self, assert_same_as_tika_with_passwords};

#[cfg(feature = "tika")]
const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";

/// The encrypted workbooks, by file name, with the password that opens them.
#[cfg(feature = "tika")]
const ENCRYPTED: [(&str, &str); 2] = [
    ("testEXCEL_protected_passtika.xls", "tika"),
    ("tika-password.xls", "tika"),
];

#[cfg(feature = "tika")]
fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
    let (mut ported, deferred) = parity::documents(&XlsParser, &[&parity::conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    ported.sort();
    assert_eq!(ported.len(), 29);
    let (departing, compared): (Vec<PathBuf>, Vec<PathBuf>) =
        ported.into_iter().partition(|document| {
            DEPARTING
                .iter()
                .any(|(name, _)| file_name(document) == *name)
        });
    assert_same_as_tika_with_passwords(&compared, TIKA_PARSER, &ENCRYPTED);
    assert_eq!(departing.len(), DEPARTING.len());
    for document in departing {
        let name = file_name(&document);
        let (xml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_file_to_string(document.to_str().unwrap())
            .unwrap();
        let (_, texts) = DEPARTING
            .iter()
            .find(|(departing, _)| *departing == name)
            .unwrap();
        for text in *texts {
            assert!(xml.contains(text), "{name}: {text:?} in {xml}");
        }
    }
}

/// Tika's own test documents the native parser reads differently from Tika's path on purpose
/// (#107), with XHTML it writes for each: a sheet kept together around its charts, whose cells
/// are text outside the table; headers and footers without their codes; linked cells once, in
/// the table.
#[cfg(feature = "tika")]
const DEPARTING: [(&str, &[&str]); 5] = [
    (
        "testEXCEL-charts.xls",
        &["<div class=\"outside\">fizzlepuff</div>\n<div class=\"outside\">1</div>"],
    ),
    (
        "testEXCEL_extra_metadata.xls",
        &["<div class=\"outside\">Page</div>"],
    ),
    (
        "testEXCEL_headers_footers.xls",
        &["<div class=\"outside\">Header - Corporate Spreadsheet\tHeader - For Internal Use Only\t\
           Header - Author: John Smith</div>"],
    ),
    (
        "testEXCEL_hyperlinks.xls",
        &["<td><a href=\"http://tika.apache.org/\">http_tika_link</a></td>"],
    ),
    (
        "testEXCEL_labels-govdocs-515858.xls",
        &["<h1>Cabbage and other brassicas</h1>"],
    ),
];

#[cfg(feature = "tika")]
#[test]
fn opens_the_repositorys_encrypted_workbook_as_tikas_path_does() {
    let encrypted = PathBuf::from("../test_files/documents/encrypted/tika-password.xls");
    assert_same_as_tika_with_passwords(&[encrypted], TIKA_PARSER, &ENCRYPTED);
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_what_tikas_old_excel_parser_extracts_from_tikas_test_documents() {
    let (mut documents, deferred) =
        parity::documents(&OldExcelParser, &[&parity::conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    documents.sort();
    assert_eq!(documents.len(), 1);
    let documents: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
    parity::assert_same_as_tika_with(
        &documents,
        "org.apache.tika.parser.microsoft.OldExcelParser",
        &ParserConfigs::default(),
    );
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

fn configs() -> ParserConfigs {
    ParserConfigs::default()
}

/// What planning the workbook `stream` finds, and its pictures.
fn plan(stream: &[u8]) -> Result<Planned, Decline<'static>> {
    super::plan(Content::from(stream), Pages::IN_MEMORY, &configs())
}

/// The pictures of the workbook `stream`, each with its type and the sheets showing it.
fn pictures(stream: &[u8]) -> Vec<(Vec<u8>, &'static str, BTreeSet<usize>)> {
    let planned = plan(stream).unwrap();
    let records = RecordStream::new(stream);
    let mut stored = 0;
    let mut pictures = Vec::new();
    for &index in &planned.found.drawing_groups {
        let group = spool_drawing_group(&records, &planned.drawing_groups[index]).unwrap();
        let escher = escher::decode(&group).unwrap();
        find_pictures(
            &escher,
            &planned.sheets_of_pictures,
            &mut stored,
            &mut |picture| {
                pictures.push((picture.data.to_vec(), picture.media_type, picture.sheets));
                Ok::<(), ()>(())
            },
        )
        .unwrap();
    }
    pictures
}

/// The XHTML the native path extracts from a workbook of the stream `stream`.
fn xhtml(stream: &[u8]) -> String {
    let workbook = crate::ole2::tests::compound(&["Workbook"], stream);
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(&workbook)
        .unwrap()
        .0
}

/// Asserts that `texts` stand in `xhtml` in their order.
fn assert_in_order(xhtml: &str, texts: &[&str]) {
    let mut from = 0;
    for text in texts {
        let at = xhtml[from..]
            .find(text)
            .unwrap_or_else(|| panic!("{text:?} after {from} in {xhtml}"));
        from += at + text.len();
    }
}

/// A sheet whose cells come out of order is held to be written in order, a cell of a taken
/// place written outside the table.
#[test]
fn writes_cells_in_a_table_per_sheet() {
    let mut stream = globals(&["One"], &["shared"]);
    stream.extend(bof(0x10));
    stream.extend(label(0, 1, " label "));
    let mut shared = cell(1, 0, 0);
    shared.extend(0i32.to_le_bytes());
    stream.extend(record(sid::LABEL_SST, &shared));
    stream.extend(number(2, 0, 1, 45_000.0));
    stream.extend(number(1, 1, 0, 2.5));
    stream.extend(label(0, 1, "again"));
    stream.extend(record(sid::EOF, &[]));
    let found = plan(&stream).unwrap().found;
    assert!(!found.plans[0].is_streamed());
    let xhtml = xhtml(&stream);
    assert_in_order(
        &xhtml,
        &[
            "<h1>One</h1>",
            "<td>label</td></tr>",
            "<tr><td>shared</td><td>2.5</td></tr>",
            "<tr><td>3/15/23</td></tr>",
            "</table>",
            "<div class=\"outside\">again</div>",
        ],
    );
}

/// A sheet whose cells come in order is written as they come, alike.
#[test]
fn writes_cells_that_come_in_order_as_they_come() {
    let mut stream = globals(&["One"], &["shared", "more"]);
    stream.extend(bof(0x10));
    stream.extend(label(0, 1, " label "));
    for (row, index) in [(1u16, 0i32), (3, 1)] {
        let mut shared = cell(row, 0, 0);
        shared.extend(index.to_le_bytes());
        stream.extend(record(sid::LABEL_SST, &shared));
    }
    stream.extend(number(3, 2, 1, 45_000.0));
    stream.extend(record(sid::EOF, &[]));
    let found = plan(&stream).unwrap().found;
    assert!(found.plans[0].is_streamed());
    let xhtml = xhtml(&stream);
    assert_in_order(
        &xhtml,
        &[
            "<h1>One</h1>",
            "<td>label</td></tr>",
            "<tr><td>shared</td></tr>",
            "<tr><td>more</td>",
            "<td>3/15/23</td></tr>",
            "</table>",
        ],
    );
}

/// A sheet is one page around its embedded charts, where Tika ends the page at a chart and
/// takes the cells after it for text outside any sheet (#107).
#[test]
fn keeps_a_sheet_together_around_its_embedded_charts() {
    let mut stream = globals(&["One", "Two"], &[]);
    stream.extend(bof(0x10));
    stream.extend(label(0, 0, "before"));
    for name in ["series", "second"] {
        stream.extend(bof(0x20));
        let mut series = vec![0, 0, 6, 0];
        series.extend(name.as_bytes());
        stream.extend(record(sid::SERIES_TEXT, &series));
        stream.extend(record(sid::EOF, &[]));
    }
    stream.extend(label(1, 0, "after"));
    stream.extend(record(sid::EOF, &[]));
    stream.extend(bof(0x20));
    stream.extend(record(sid::EOF, &[]));
    let xhtml = xhtml(&stream);
    assert_in_order(
        &xhtml,
        &[
            "<h1>One</h1>",
            "before",
            "after",
            "</table>",
            "<div class=\"outside\">series</div>",
            "<div class=\"outside\">second</div>",
            "<h1>Two</h1>",
        ],
    );
    assert_eq!(xhtml.matches("<div class=\"page\">").count(), 2);
}

fn formula(row: u16, column: u16, result: [u8; 8]) -> Vec<u8> {
    let mut data = cell(row, column, 0);
    data.extend(result);
    data.extend([0; 8]);
    record(sid::FORMULA, &data)
}

/// A hyperlink from the cell at `row` and `column` to the file `target`.
fn hyperlink(row: u16, column: u16, target: &str) -> Vec<u8> {
    let mut link = [row, row, column, column]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    link.extend([0; 16]);
    link.extend(2i32.to_le_bytes());
    link.extend(0x101i32.to_le_bytes());
    link.extend((target.len() as i32).to_le_bytes());
    link.extend(target.encode_utf16().flat_map(u16::to_le_bytes));
    record(sid::HYPERLINK, &link)
}

/// Formula results are written by their type, a linked cell once, where it is, headers and
/// footers without their codes, and dates in the workbook's date system (#107).
#[test]
fn writes_cells_as_their_records_mean_them() {
    let mut stream = bof(0x05);
    stream.extend(record(sid::DATE_WINDOW_1904, &[1, 0]));
    stream.extend(extended_format(0));
    stream.extend(extended_format(14));
    let mut sheet = vec![0, 0, 0, 0, 0, 0, 1, 0];
    sheet.push(b'S');
    stream.extend(record(sid::BOUND_SHEET, &sheet));
    stream.extend(record(sid::EOF, &[]));
    stream.extend(bof(0x10));
    let mut header = 12u16.to_le_bytes().to_vec();
    header.push(0);
    header.extend(b"&CPage &P&RQ");
    stream.extend(record(sid::HEADER, &header));
    stream.extend(formula(0, 0, [1, 0, 1, 0, 0, 0, 0xFF, 0xFF]));
    stream.extend(formula(0, 1, [1, 0, 0, 0, 0, 0, 0xFF, 0xFF]));
    stream.extend(formula(0, 2, [2, 0, 0x07, 0, 0, 0, 0xFF, 0xFF]));
    stream.extend(formula(0, 3, [3, 0, 0, 0, 0, 0, 0xFF, 0xFF]));
    stream.extend(hyperlink(1, 1, "early.xls"));
    stream.extend(number(1, 0, 1, 0.0));
    stream.extend(label(1, 1, "linked"));
    stream.extend(hyperlink(1, 1, "x.xls"));
    stream.extend(hyperlink(1, 1, "y.xls"));
    stream.extend(record(sid::EOF, &[]));
    let found = plan(&stream).unwrap().found;
    assert!(found.plans[0].is_streamed());
    let xhtml = xhtml(&stream);
    assert_in_order(
        &xhtml,
        &[
            "<h1>S</h1>",
            "<td>TRUE</td><td>FALSE</td><td>#DIV/0!</td>",
            "<td>1/1/04</td><td><a href=\"y.xls\"><a href=\"x.xls\">linked</a></a></td>",
            "<div class=\"outside\">Page \tQ</div>",
        ],
    );
    assert!(!xhtml.contains("early.xls"), "{xhtml}");
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
    let found = plan(&stream).unwrap().found;
    assert_eq!(found.hidden_sheets, ["H"]);
    assert_eq!(found.comment_authors.values().collect::<Vec<_>>(), ["Bob"]);
}

#[test]
fn fails_on_xor_obfuscated_workbooks_the_password_does_not_open() {
    let mut stream = bof(0x05);
    stream.extend(record(sid::FILE_PASS, &[0; 6]));
    assert!(matches!(
        plan(&stream),
        Err(Decline::Fails(crate::Error::Encrypted(reason))) if reason.contains("Default password is invalid")
    ));
}

/// Encrypts the records of `stream` from `start` with `key`, as RC4 encrypts what it
/// decrypts.
fn encrypt(stream: &mut [u8], start: usize, key: factory::WorkbookKey) {
    let mut decrypting = factory::Decrypting::new(key, start);
    let mut at = start;
    while at + 4 <= stream.len() {
        let sid = u16::from_le_bytes([stream[at], stream[at + 1]]);
        let length = usize::from(u16::from_le_bytes([stream[at + 2], stream[at + 3]]));
        let body = at + 4;
        let end = (body + length).min(stream.len());
        let mut data = std::borrow::Cow::Borrowed(&stream[body..end]);
        decrypting.decrypt(sid, at, length, &mut data);
        let data = data.into_owned();
        stream[body..end].copy_from_slice(&data);
        at = body + length;
    }
}

/// A workbook encrypted after a `WRITEPROTECT` of two bytes is decrypted from where its
/// records are, where POI measures that record as four bytes and decrypts out of step (#119).
#[test]
fn decrypts_from_where_the_encrypted_records_are() {
    use crate::formats::crypt::{BinaryRc4, EncryptionInfo, DEFAULT_PASSWORD};
    let rc4 = BinaryRc4::encrypting(DEFAULT_PASSWORD);
    let mut file_pass = 1u16.to_le_bytes().to_vec();
    file_pass.extend(rc4.written());
    let mut stream = bof(0x05);
    stream.extend(record(sid::WRITE_PROTECT, &[0, 0]));
    stream.extend(record(sid::FILE_PASS, &file_pass));
    let encrypted = stream.len();
    stream.extend(extended_format(0));
    let mut hidden = vec![0, 0, 0, 0, 1, 0, 1, 0];
    hidden.push(b'H');
    stream.extend(record(sid::BOUND_SHEET, &hidden));
    stream.extend(record(sid::EOF, &[]));
    stream.extend(bof(0x10));
    stream.extend(label(0, 0, "secret"));
    stream.extend(record(sid::EOF, &[]));
    let keys = EncryptionInfo::BinaryRc4(rc4)
        .verify(DEFAULT_PASSWORD)
        .unwrap();
    encrypt(&mut stream, encrypted, factory::WorkbookKey::Rc4(keys));
    assert_eq!(plan(&stream).unwrap().found.hidden_sheets, ["H"]);
    assert!(xhtml(&stream).contains("<td>secret</td>"));
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
    assert!(plan(&stream).is_err());
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
    let pictures = pictures(&stream);
    let [picture] = &pictures[..] else {
        panic!("one picture");
    };
    assert_eq!(picture.0, b"png?");
    assert_eq!(picture.1, "image/png");
    assert_eq!(picture.2, BTreeSet::from([2]));
}

/// An MS Graph chart, which Tika's detection of an embedded storage names a workbook, is read
/// natively, as `OfficeParser` reads it once it detects the chart: its summary information
/// only, where it was left to Tika (#119).
#[test]
fn reads_an_ms_graph_chart_as_office_parser_does_once_it_detects_one() {
    let chart = crate::ole2::tests::compound(&["Workbook", "\u{1}CompObj"], b"..MSGraph.Chart.8..");
    assert_eq!(
        XlsParser.defers(&chart, &TikaMetadata::new(), &configs()),
        None
    );
}

/// A shared string table of `strings`, run on into `CONTINUE` records between strings.
fn shared_string_table(strings: &[String]) -> Vec<u8> {
    let mut records = Vec::new();
    let mut data = (strings.len() as i32).to_le_bytes().to_vec();
    data.extend((strings.len() as i32).to_le_bytes());
    for text in strings {
        if data.len() + 3 + text.len() > 8224 {
            records.push(std::mem::take(&mut data));
        }
        data.extend((text.len() as u16).to_le_bytes());
        data.push(0);
        data.extend(text.as_bytes());
    }
    records.push(data);
    let mut bytes = record(sid::SST, &records[0]);
    for data in &records[1..] {
        bytes.extend(record(sid::CONTINUE, data));
    }
    bytes
}

/// A workbook too large to read whole is planned, then written as its records come, its cells
/// in order and its shared strings read where cells refer to them, in memory that does not grow
/// with it.
#[test]
fn extracts_workbooks_too_large_to_read_whole_in_bounded_memory() {
    use crate::input::MAX_READ_WHOLE;
    use extractous_core::large::LargeFile;
    const ROWS: u16 = 65_000;
    let strings: Vec<String> = (0..100_000)
        .map(|index| format!("Fyrets logbog, linje {index}"))
        .collect();
    let sheets = MAX_READ_WHOLE as usize / (usize::from(ROWS) * 32) + 1;
    let names: Vec<String> = (0..sheets).map(|sheet| format!("Ark {sheet}")).collect();
    let mut stream = bof(0x05);
    stream.extend(extended_format(0));
    for name in &names {
        let mut sheet = vec![0, 0, 0, 0, 0, 0, name.len() as u8, 0];
        sheet.extend(name.as_bytes());
        stream.extend(record(sid::BOUND_SHEET, &sheet));
    }
    stream.extend(shared_string_table(&strings));
    stream.extend(record(sid::EOF, &[]));
    for sheet in 0..sheets {
        stream.extend(bof(0x10));
        for row in 0..ROWS {
            let index = (sheet * usize::from(ROWS) + usize::from(row)) * 7 % strings.len();
            let mut shared = cell(row, 0, 0);
            shared.extend((index as i32).to_le_bytes());
            stream.extend(record(sid::LABEL_SST, &shared));
            stream.extend(number(row, 1, 0, f64::from(row)));
        }
        if sheet == sheets - 1 {
            let mut note = cell(0, 0, 0);
            note.extend([0, 0, 4, 0, 0]);
            note.extend(b"Last");
            stream.extend(record(sid::NOTE, &note));
        }
        stream.extend(record(sid::EOF, &[]));
    }
    let workbook = crate::ole2::tests::compound(&["Workbook"], &stream);
    drop(stream);
    let file = LargeFile::new("large.xls", &workbook, 0, |_| Vec::new(), b"");
    drop(workbook);
    let (text, metadata) = file.extract("extractous::OfficeParser");
    assert_in_order(
        &text,
        &[
            "Ark 0",
            "Fyrets logbog, linje 0\t0",
            "Fyrets logbog, linje 7\t1",
            "Fyrets logbog, linje 14\t2",
        ],
    );
    assert_eq!(metadata[excel::COMMENT_PERSONS], ["Last"]);
}
