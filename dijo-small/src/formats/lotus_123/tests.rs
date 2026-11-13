use super::letters;
use crate::errors::Error;

/// A record of `kind` with `body`.
fn record(kind: u16, body: &[u8]) -> Vec<u8> {
    let length = u16::try_from(body.len()).unwrap();
    [&kind.to_le_bytes()[..], &length.to_le_bytes(), body].concat()
}

/// A cell record of Release 2: its format byte, column, row and `payload`.
fn release2_cell(kind: u16, format: u8, column: u16, row: u16, payload: &[u8]) -> Vec<u8> {
    let head = [&[format][..], &column.to_le_bytes(), &row.to_le_bytes()].concat();
    record(kind, &[&head[..], payload].concat())
}

/// A cell record of Release 3: its row, sheet, column and `payload`.
fn release3_cell(kind: u16, row: u16, sheet: u8, column: u8, payload: &[u8]) -> Vec<u8> {
    let head = [&row.to_le_bytes()[..], &[sheet, column]].concat();
    record(kind, &[&head[..], payload].concat())
}

/// A 1-2-3 Release 2 worksheet of `records`, after the BOF and range records its magic needs.
fn worksheet(records: &[Vec<u8>]) -> Vec<u8> {
    let mut file = record(0x00, &0x0406u16.to_le_bytes());
    file.extend(record(0x06, &[0; 8]));
    file.extend(records.concat());
    file.extend(record(0x01, &[]));
    file
}

/// A 1-2-3 workbook of the BOF `version`, Release 3's or later, of `records`, whose text is in
/// LMBCS `group`.
fn workbook(version: u16, group: u8, records: &[Vec<u8>]) -> Vec<u8> {
    let mut bof = vec![0u8; 26];
    bof[..2].copy_from_slice(&version.to_le_bytes());
    bof[2] = 0x04;
    bof[16] = group;
    bof[17] = 10;
    let mut file = record(0x00, &bof);
    file.extend(records.concat());
    file.extend(record(0x01, &[]));
    file
}

/// The body of the XHTML the native path extracts from `file`, without its line breaks and
/// indentation.
fn body(file: &[u8]) -> String {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(file)
        .unwrap();
    assert_eq!(
        metadata["tk:parsed-by"].last().unwrap(),
        "dijo::Lotus123Parser"
    );
    crate::formats::testing::body(&xml).replace(['\n', '\t'], "")
}

#[test]
fn writes_a_release2_worksheet_row_by_row_in_its_formats() {
    let na = 0xFFF0_0000_0000_0000u64.to_le_bytes();
    let err = 0x7FF0_0000_0000_0000u64.to_le_bytes();
    let nan = f64::NAN.to_le_bytes();
    let file = worksheet(&[
        record(0x07, &[0, 0, 0, 0, 0x82, 0, 9, 0]),
        release2_cell(0x0F, 0xFF, 0, 1, b"\"Net\x9B\0"),
        release2_cell(0x0D, 0xA2, 1, 1, &(-1234i16).to_le_bytes()),
        release2_cell(0x0E, 0xF2, 3, 1, &45000.0f64.to_le_bytes()),
        release2_cell(0x0F, 0xFF, 0, 0, b"'Name\0"),
        release2_cell(0x0E, 0xFF, 1, 0, &2.5f64.to_le_bytes()),
        release2_cell(0x10, 0xFF, 2, 0, &[&nan[..], &[0, 0]].concat()),
        release2_cell(0x33, 0xFF, 2, 0, b"text result\0"),
        release2_cell(0x0E, 0xFF, 2, 1, &na),
        release2_cell(0x0E, 0x76, 4, 1, &7.0f64.to_le_bytes()),
        release2_cell(0x0D, 0x33, 0, 2, &5i16.to_le_bytes()),
        release2_cell(0x0E, 0xFF, 1, 2, &err),
    ]);
    assert_eq!(
        body(&file),
        "<div class=\"page\"><table><tbody>\
         <tr><td>Name</td><td>2.50</td><td>text result</td></tr>\
         <tr><td>Net\u{f8}</td><td>($1,234.00)</td><td>NA</td><td>15-Mar-23</td></tr>\
         <tr><td>500.000%</td><td>ERR</td></tr>\
         </tbody></table></div>"
    );
}

#[test]
fn writes_the_sheets_of_a_release3_workbook_in_the_formats_of_their_rows() {
    let format_row = |sheet: u8, row: u16, entries: &[u8]| {
        record(
            0x13,
            &[&[sheet, 0][..], &row.to_le_bytes(), entries].concat(),
        )
    };
    let one = [0, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x3F];
    let file = workbook(
        0x1000,
        0x01,
        &[
            record(
                0x1B,
                &[&14000u16.to_le_bytes()[..], &[0, 0], b"Budget\0"].concat(),
            ),
            format_row(0, 0, &[0xFF, 0, 0, 0x80, 1, 0x01, 0x02, 0, 0]),
            record(0x13, &[0, 2, 1, 0, 0, 0, 0, 0]),
            record(0x13, &[0, 1, 0, 0, 0x72, 0, 0, 0, 0, 0, 0, 0]),
            release3_cell(0x16, 0, 0, 0, b"^K\x01\x26b\0"),
            release3_cell(0x17, 0, 0, 1, &one),
            release3_cell(0x18, 0, 0, 2, &[0x35, 0x00]),
            release3_cell(0x18, 1, 0, 2, &[0x35, 0x00]),
            release3_cell(0x18, 2, 0, 0, &[0x10, 0x00]),
            release3_cell(
                0x19,
                2,
                0,
                1,
                &[0, 0, 0, 0, 0, 0, 0, 0xE0, 0xFF, 0xFF, 0x03],
            ),
            release3_cell(0x1A, 2, 0, 1, b"from a formula\0"),
            release3_cell(0x14, 0, 1, 3, &[]),
        ],
    );
    assert_eq!(
        body(&file),
        "<div class=\"page\"><h1>Budget</h1><table><tbody>\
         <tr><td>K\u{201c}b</td><td>01-Jan-00</td><td>(0.2)</td></tr>\
         <tr><td /><td /><td>(0.2)</td></tr>\
         <tr><td>08-Jan-00</td><td>from a formula</td></tr>\
         </tbody></table></div>\
         <div class=\"page\"><h1>B</h1><table><tbody>\
         <tr><td /><td /><td /><td>ERR</td></tr>\
         </tbody></table></div>"
    );
}

#[test]
fn writes_a_workbook_of_1_2_3_97_in_the_formats_of_its_database_with_comments() {
    let node = |length: u16| record(0x0800, &length.to_le_bytes());
    let data = |format: [u8; 4]| record(0x0801, &format);
    let file = workbook(
        0x1003,
        0x01,
        &[
            record(0x0293, &[]),
            record(0x0804, &[3, 0, 3, 0, 0x00, 0x20, 0x00, 0x01, 0x00, 0x01]),
            node(1),
            node(1),
            node(1),
            data([0x22, 0, 0, 0]),
            node(1),
            data([0x02, 0, 0x63, 0x80]),
            node(0x1FFE),
            node(1),
            node(1),
            data([0x31, 0x02, 0, 0]),
            node(0x1FFF),
            node(254),
            node(0x2000),
            node(255),
            node(256),
            node(0x2000),
            record(0x0293, &[]),
            release3_cell(0x27, 0, 0, 0, &1234.5f64.to_le_bytes()),
            release3_cell(0x27, 1, 0, 0, &45000.0f64.to_le_bytes()),
            release3_cell(0x27, 0, 0, 1, &0.25f64.to_le_bytes()),
            release3_cell(0x26, 0, 0, 1, b"\x01Checked\0"),
            release3_cell(0x26, 2, 0, 3, b"\x01 Alone \0"),
            release3_cell(0x26, 2, 0, 4, b"\x01\0"),
        ],
    );
    assert_eq!(
        body(&file),
        "<div class=\"page\"><h1>A</h1><table><tbody>\
         <tr><td>$1,234.50</td><td>(25.0%)<br />Checked</td></tr>\
         <tr><td>03/15/23</td></tr>\
         <tr><td /><td /><td /><td>Alone</td></tr>\
         </tbody></table></div>"
    );
}

#[test]
fn starts_a_sheet_of_cells_past_the_first_row_with_an_empty_row() {
    let file = workbook(0x1002, 0x01, &[release3_cell(0x16, 2, 0, 1, b"'late\0")]);
    assert_eq!(
        body(&file),
        "<div class=\"page\"><h1>A</h1><table><tbody>\
         <tr><td /></tr><tr><td /><td>late</td></tr>\
         </tbody></table></div>"
    );
}

fn extraction_error(file: &[u8]) -> Error {
    crate::Extractor::new()
        .extract_bytes_to_string(file)
        .unwrap_err()
}

#[test]
fn fails_a_file_a_password_protects_as_encrypted() {
    let file = worksheet(&[record(0x4B, &[0x12, 0x34])]);
    let error = extraction_error(&file);
    assert!(matches!(error, Error::Encrypted(_)), "{error:?}");
}

#[test]
fn fails_a_record_that_runs_past_the_end() {
    let mut file = worksheet(&[]);
    file.truncate(file.len() - 4);
    file.extend([0x0E, 0x00, 0x0D, 0x00, 0xFF]);
    let error = extraction_error(&file);
    assert!(
        error.to_string().contains("runs past the end of the file"),
        "{error}"
    );
}

#[test]
fn names_sheets_by_their_letters() {
    assert_eq!(letters(0), "A");
    assert_eq!(letters(25), "Z");
    assert_eq!(letters(26), "AA");
    assert_eq!(letters(255), "IV");
}
