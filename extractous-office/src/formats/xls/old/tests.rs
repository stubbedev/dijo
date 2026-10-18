use super::*;
use crate::formats::xls::stream::tests::record;

/// `getText`: the text of each record, each ending its line.
fn text(stream: &[u8]) -> Result<String, Unread> {
    let mut texts = Texts::new(stream)?;
    let mut text = String::new();
    while let Some(line) = texts.next()? {
        text.push_str(&line);
        text.push('\n');
    }
    Ok(text)
}

fn bof(sid: u16) -> Vec<u8> {
    let mut data = 0x0500i16.to_le_bytes().to_vec();
    data.extend(0x10i16.to_le_bytes());
    data.extend([0; 4]);
    record(sid, &data)
}

fn label(sid: u16, text: &[u8]) -> Vec<u8> {
    let mut data = vec![0; 6];
    data.extend((text.len() as u16).to_le_bytes());
    data.extend(text);
    record(sid, &data)
}

#[test]
fn reads_the_text_of_an_excel_95_workbook_as_poi_does() {
    let mut stream = bof(sid::BOF);
    stream.extend(record(old_sid::CODEPAGE, &1251u16.to_le_bytes()));
    let mut sheet = vec![0; 6];
    sheet.extend([4, 0]);
    sheet.extend([0xCB, 0xE8, 0xF1, 0xF2]);
    stream.extend(record(old_sid::SHEET, &sheet[..sheet.len() - 1]));
    stream.extend(label(old_sid::LABEL, &[0xC0, 0xC1]));
    let mut number = vec![0; 6];
    number.extend(1.5f64.to_le_bytes());
    stream.extend(record(sid::NUMBER, &number));
    let mut rk = vec![0; 6];
    rk.extend(((7i32 << 2) | 2).to_le_bytes());
    stream.extend(record(sid::RK, &rk));
    let mut formula = vec![0; 6];
    formula.extend(1e20f64.to_le_bytes());
    formula.extend([0; 8]);
    stream.extend(record(sid::FORMULA, &formula));
    stream.extend(record(sid::EOF, &[]));
    assert_eq!(text(&stream).unwrap(), "Sheet: Лис\nАБ\n1.5\n7.0\n1.0E20\n");
}

#[test]
fn reads_an_excel_2_label_short_of_its_length_as_far_as_it_goes() {
    let mut stream = bof(old_sid::BOF_BIFF2);
    let mut label = vec![0; 7];
    label.extend([3, b'a']);
    stream.extend(record(old_sid::LABEL_BIFF2, &label));
    assert_eq!(text(&stream).unwrap(), "a\n");
}

#[test]
fn fails_where_poi_throws() {
    assert!(matches!(
        text(&record(sid::EOF, &[])),
        Err(Unread::Thrown(_))
    ));
    let mut encrypted = bof(sid::BOF);
    encrypted.extend(record(sid::FILE_PASS, &[0; 4]));
    assert!(matches!(
        text(&encrypted),
        Err(Unread::Thrown(Thrown(reason))) if reason.contains("encrypted")
    ));
}

/// An Excel 2 workbook too large to read whole is read a record at a time, each label written
/// as it comes, in memory that does not grow with it.
#[test]
fn extracts_workbooks_too_large_to_read_whole_in_bounded_memory() {
    use extractous_core::large::{bodies_past_limit, LargeFile};

    let label = |index: u64| {
        let text = format!("Linje {index} i logbogen");
        let mut data = vec![0; 7];
        data.push(text.len() as u8);
        data.extend(text.as_bytes());
        record(old_sid::LABEL_BIFF2, &data)
    };
    let count = bodies_past_limit(label(0).len());
    let head = record(old_sid::BOF_BIFF2, &[0, 0, 0x10, 0]);
    let file = LargeFile::new("large.xls", &head, count, label, b"");
    let last = format!("Linje {} i logbogen", count - 1);
    let (mut first_seen, mut last_seen) = (false, false);
    file.extract_lines("extractous::OldExcelParser", |line| match line.trim() {
        "Linje 0 i logbogen" => first_seen = true,
        line if line == last => last_seen = first_seen,
        _ => {}
    });
    assert!(first_seen && last_seen);
}
