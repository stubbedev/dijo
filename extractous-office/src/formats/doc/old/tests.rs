use super::*;

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// A Word 6 main stream holding `text` as its one piece, with no runs or sections and one
/// font of character set `chs`.
fn word6(text: &[u8], chs: u8) -> Vec<u8> {
    let mut main = vec![0; 0x400];
    let mut put = |at: usize, value: i32| main[at..at + 4].copy_from_slice(&value.to_le_bytes());
    put(0x18, 0x200);
    put(0x1C, 0x200 + text.len() as i32);
    for (at, offset) in [(0x88, 0x300), (0xB8, 0x310), (0xC0, 0x320)] {
        put(at, offset);
        put(at + 4, 4);
    }
    put(0xD0, 0x330);
    put(0xD4, 10);
    main[0x332] = 7;
    main[0x336] = chs;
    main[0x338] = b'A';
    main[0x200..0x200 + text.len()].copy_from_slice(text);
    main
}

#[test]
fn reads_the_paragraphs_in_the_code_page_the_fonts_name() {
    let mut text = vec![0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, b'\r'];
    text.extend(b"World\r");
    let document = OldDocument::read(&word6(&text, 0xCC)).unwrap();
    assert_eq!(
        document.paragraph_text(),
        [units("Привет\n"), units("World\n")]
    );
}

#[test]
fn reads_code_page_1252_where_the_fonts_name_no_other() {
    let document = OldDocument::read(&word6(&[0xE6, b'\r'], 0x02)).unwrap();
    assert_eq!(document.paragraph_text(), [units("æ\n")]);
}

#[test]
fn reads_double_byte_text_in_the_code_page_the_fonts_name() {
    let mut text = vec![0x93, 0xFA, 0x96, 0x7B, b'\r'];
    text.extend(b"World\r");
    let document = OldDocument::read(&word6(&text, 0x80)).unwrap();
    assert_eq!(
        document.paragraph_text(),
        [units("\u{65E5}\u{672C}\n"), units("World\n")]
    );
}

#[test]
fn ends_the_lines_of_a_piece_as_word6_extractor_falls_back_to() {
    let piece = units("a\rb");
    assert_eq!(piece_text(&piece), units("a\r\nb"));
}

/// Text in Big5 is read two bytes a character, as POI's reader of code page 950 reads it, its
/// ASCII padded with a zero byte (#186).
#[test]
fn reads_big5_text_as_pois_code_page_950_reader_does() {
    let mut text = vec![0x40, 0xA4, 0x40, 0xF9, b'\r', 0x00];
    for byte in b"World\r" {
        text.extend([*byte, 0x00]);
    }
    let document = OldDocument::read(&word6(&text, 0x88)).unwrap();
    assert_eq!(
        document.paragraph_text(),
        [units("\u{4E00}\u{7E98}\n"), units("World\n")]
    );
}

#[test]
fn leaves_what_poi_throws_on_to_tika() {
    assert!(OldDocument::read(&[0; 0x80]).is_err());
}
