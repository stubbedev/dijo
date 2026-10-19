use super::*;

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

/// A spreadsheet too large to read whole, its cells in the order of their addresses: they are
/// written as their records are read again, under a budget far below an index of them.
#[test]
fn writes_quattro_pro_spreadsheets_too_large_to_read_whole_in_order() {
    const COLUMNS: u16 = 256;
    let row = |index: u64| {
        let row = u16::try_from(index).unwrap();
        (0..COLUMNS)
            .flat_map(|column| {
                let mut body = vec![0x80];
                body.extend(column.to_le_bytes());
                body.extend(row.to_le_bytes());
                body.extend([b'\'', 1, b'Q']);
                record(0x000F, &body)
            })
            .collect::<Vec<u8>>()
    };
    let count = extractous_core::large::bodies_past_limit(row(0).len());
    assert!(count < 1 << 16, "{count} rows fit a sheet");
    let file = extractous_core::large::LargeFile::new(
        "large.wq1",
        &record(0x0000, &0x5120u16.to_le_bytes()),
        count,
        row,
        &record(0x0001, &[]),
    );
    let mut cells = 0u64;
    file.extract_each(OldQuattroProParser.name(), |text| {
        cells += text.matches('Q').count() as u64;
    });
    assert_eq!(cells, count * u64::from(COLUMNS));
}

/// A Quattro Pro 9 workbook too large to read whole, its record stream read in place in the
/// compound document, under a budget far below it.
#[test]
fn reads_quattro_pro_workbooks_too_large_to_read_whole_in_place() {
    use crate::input::MAX_READ_WHOLE;

    let label = {
        let text = b"Q".repeat(255);
        let mut body = vec![0; 6];
        body.extend(254u16.to_le_bytes());
        body.push(0);
        body.extend(&text);
        record(0x0c02, &body)
    };
    let count = MAX_READ_WHOLE as usize / label.len() + 1;
    let mut records = record(0x0001, &[0; 14]);
    records.extend(label.repeat(count));
    let workbook = crate::ole2::tests::compound(&["NativeContent_MAIN"], &records);
    drop(records);
    let file =
        extractous_core::large::LargeFile::written("large.qpw", |out| out.write_all(&workbook));
    drop(workbook);
    let mut letters = 0usize;
    file.extract_each(QuattroProParser.name(), |text| {
        letters += text.matches('Q').count();
    });
    assert_eq!(letters, count * 255);
}

#[test]
fn indexes_cells_out_of_the_order_of_their_addresses() {
    fn cell(column: u16, row: u16, text: &[u8]) -> Vec<u8> {
        let mut body = vec![0x80];
        body.extend(column.to_le_bytes());
        body.extend(row.to_le_bytes());
        body.push(b'\'');
        body.push(u8::try_from(text.len()).unwrap());
        body.extend(text);
        record(0x000F, &body)
    }
    let sheet = [
        record(0x0000, &0x5120u16.to_le_bytes()),
        cell(1, 1, b"d"),
        cell(0, 0, b"a"),
        cell(1, 0, b"x"),
        cell(0, 1, b"c"),
        cell(1, 0, b"b"),
        record(0x0001, &[]),
    ]
    .concat();
    let xml = extract_xml(&sheet);
    assert!(
        xml.contains("<tr>\t<td>a</td>\t<td>b</td></tr>\n<tr>\t<td>c</td>\t<td>d</td></tr>"),
        "{xml}"
    );
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
