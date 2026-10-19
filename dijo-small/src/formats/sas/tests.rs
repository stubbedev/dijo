use dijo_core::large::LargeFile;

use super::*;

/// Tika's `testSAS.sas7bdat`: a 32-bit, little-endian data set of two columns and eleven rows on
/// one page.
const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/sas/testSAS.sas7bdat");

fn extract(bytes: &[u8]) -> (String, crate::Metadata) {
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(bytes)
        .unwrap()
}

#[test]
fn reads_data_sets_as_parso_does() {
    let (xml, metadata) = extract(FIXTURE);
    assert!(xml.contains("<h1>TESTING</h1>"), "{xml}");
    assert!(
        xml.contains("<th title=\"recnum\">recnum</th>\t<th title=\"label\">label</th>"),
        "{xml}"
    );
    assert!(
        xml.contains("<td>10</td>\t<td>This is row           10 of           10</td>"),
        "{xml}"
    );
    assert_eq!(xml.matches("<tr>").count(), 12, "{xml}");
    assert_eq!(metadata["database:column-name"], ["recnum", "label"]);
    assert_eq!(metadata["database:row-count"], ["11"]);
    assert_eq!(metadata["dcterms:created"], ["2017-01-30T07:31:47Z"]);
    assert_eq!(metadata["extended-properties:AppVersion"], ["9.0301M2"]);
    assert_eq!(metadata["machine:endian"], ["Little"]);
    assert_eq!(metadata["Content-Encoding"], ["windows-1252"]);
}

/// The fixture's page as a mixed page with deletion markers, the fourth of its rows marked
/// deleted: the page type at 16 of the page, at 1024, and the first byte of the markers, which
/// its pointer at 12 places 2432 bytes on, past the subheader pointers and the rows.
fn with_fourth_row_deleted() -> Vec<u8> {
    let mut bytes = FIXTURE.to_vec();
    let page = 1024;
    bytes[page + 16..page + 18].copy_from_slice(&640i16.to_le_bytes());
    let pointer = i32::from_le_bytes(bytes[page + 12..page + 16].try_into().unwrap());
    let markers = page + usize::try_from(pointer).unwrap() + 2432;
    assert_eq!(bytes[markers], 0);
    bytes[markers] = 0b0001_0000;
    bytes
}

#[test]
fn reads_the_rows_after_a_deleted_one() {
    let (xml, metadata) = extract(&with_fourth_row_deleted());
    assert_eq!(xml.matches("<tr>").count(), 11, "{xml}");
    assert!(!xml.contains("<td>3</td>"), "{xml}");
    assert!(xml.contains("<td>2</td>"), "{xml}");
    assert!(
        xml.contains("<td>10</td>\t<td>This is row           10 of           10</td>"),
        "{xml}"
    );
    assert_eq!(metadata["database:row-count"], ["11"]);
}

const PAGE_LENGTH: usize = 16384;
const ROW_LENGTH: usize = 208;
const ROWS_PER_PAGE: usize = (PAGE_LENGTH - 24) / ROW_LENGTH;
/// Enough pages of data to make a file too large to read whole.
const DATA_PAGES: u64 = 4100;
/// Where the row size subheader the fixture's first page points to starts: the page also
/// holds a stale copy at 7984, which no pointer leads to.
const ROW_SIZE_SUBHEADER: usize = 16928;

fn large_row_count() -> i32 {
    i32::try_from(11 + DATA_PAGES * ROWS_PER_PAGE as u64).unwrap()
}

/// The label of row `index` of a data set of `count` rows.
fn label(index: i32, count: i32) -> String {
    format!("This is row {index:>12} of {count:>12}")
}

/// The data page at `index` after the fixture's page: its header, then its rows of a number
/// and a label, numbered on from the fixture's.
fn data_page(index: u64) -> Vec<u8> {
    let mut page = vec![0; PAGE_LENGTH];
    page[16..18].copy_from_slice(&256i16.to_le_bytes());
    page[18..20].copy_from_slice(&i16::try_from(ROWS_PER_PAGE).unwrap().to_le_bytes());
    let first = 11 + index * ROWS_PER_PAGE as u64;
    for (at, row) in page[24..]
        .as_chunks_mut::<ROW_LENGTH>()
        .0
        .iter_mut()
        .take(ROWS_PER_PAGE)
        .enumerate()
    {
        let number = i32::try_from(first + at as u64).unwrap();
        row[..8].copy_from_slice(&f64::from(number).to_le_bytes());
        let text = format!("{:<200}", label(number, large_row_count()));
        row[8..].copy_from_slice(text.as_bytes());
    }
    page
}

/// The fixture, its page of metadata holding only its eleven rows, then [`DATA_PAGES`] pages
/// of data, the row count raised to match.
fn long_data_set() -> LargeFile {
    let mut head = FIXTURE.to_vec();
    let row_count = ROW_SIZE_SUBHEADER + 24;
    head[row_count..row_count + 4].copy_from_slice(&large_row_count().to_le_bytes());
    let mix_page_rows = ROW_SIZE_SUBHEADER + 60;
    head[mix_page_rows..mix_page_rows + 4].copy_from_slice(&11i32.to_le_bytes());
    LargeFile::new("long.sas7bdat", &head, DATA_PAGES, data_page, b"")
}

/// A data set too large to read whole is read a page at a time, each row written as it is
/// read.
#[test]
fn reads_data_sets_too_large_to_read_whole_a_page_at_a_time() {
    let file = long_data_set();
    let count = large_row_count();
    let mut rows = 0;
    let mut check = |line: &str| {
        if !line.contains("This is row") {
            return;
        }
        let of = if rows <= 10 { 10 } else { count };
        assert_eq!(line, format!("\t{rows}\t{}", label(rows, of)));
        rows += 1;
    };
    let metadata = file.extract_lines(Sas7bdatParser.name(), &mut check);
    assert_eq!(rows, count);
    assert_eq!(metadata["database:row-count"], [count.to_string()]);
}
