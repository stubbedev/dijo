use std::borrow::Cow;

use super::phrases::{Expanded, Table};
use super::reader::Reader;
use super::*;
use crate::text::codepage;

/// The WinHelp 4 file `tests/fixtures/winhelp/harbour.py winhelp4` writes: LZ77-compressed
/// topic blocks and old phrases.
const HARBOUR: &[u8] = include_bytes!("../../../../test_files/documents/harbour.hlp");
/// The same help file of `harbour.py hall`, Hall-compressed.
const HALL: &[u8] = include_bytes!("../../../tests/fixtures/winhelp/hall.hlp");
/// The WinHelp 3.0 file of `harbour.py winhelp30`, whose topic links run on across blocks.
const WINHELP_30: &[u8] = include_bytes!("../../../tests/fixtures/winhelp/winhelp30.hlp");

/// The body of the XHTML extracted from `input`, and its metadata.
fn extract(input: &[u8]) -> (String, dijo_core::Metadata) {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(input)
        .unwrap();
    assert_eq!(metadata["Content-Type"], [MEDIA_TYPE]);
    assert!(metadata["tk:parsed-by"].iter().any(|parser| parser == NAME));
    (crate::formats::testing::body(&xml).to_owned(), metadata)
}

/// Why the parser declines `input`.
fn declined(input: &[u8]) -> String {
    let configs = ParserConfigs::default();
    let reason = match WinHelpParser.prepare(input, &TikaMetadata::new(), &configs) {
        Ok(_) => panic!("the parser reads the help file"),
        Err(decline) => format!("{decline:?}"),
    };
    reason
}

/// Why writing `input` fails.
fn failure(input: &[u8]) -> String {
    let help = HelpFile::open(input).unwrap();
    let failed = TopicFile::new(help.topic, help.layout)
        .links()
        .find_map(Result::err);
    format!("{:?}", failed.expect("a topic link cannot be read"))
}

/// The offset of the internal file `name` of `input`.
fn offset_of(input: &[u8], name: &str) -> usize {
    let files = FileSystem::open(input).unwrap();
    let file = files.file(name).unwrap().unwrap();
    file.as_ptr() as usize - input.as_ptr() as usize
}

fn with_bytes(input: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut changed = input.to_vec();
    changed[at..at + bytes.len()].copy_from_slice(bytes);
    changed
}

const HARBOUR_TOPICS: &str = "<h1>The Lighthouse</h1>\n\
    <p>The lighthouse keeper lights the lamp at dusk.</p>\n\
    <p>Lamp:\tFresnel lens<br />\nRange:\t18\u{a0}nautical miles</p>\n\
    <p>Fyret lyser om natten, og Ove Gr\u{f8}n holder vagt. See the tide table.</p>\n\
    <h1>Tide Table</h1>\n\
    <p>The harbour tides for the first of May.</p>\n\
    <table><tr>\t<td><p>Hour</p>\n</td>\t<td><p>Height</p>\n</td></tr>\n\
    <tr>\t<td><p>06:00</p>\n</td>\t<td><p>1.2 m</p>\n</td></tr>\n\
    <tr>\t<td><p>18:00</p>\n</td>\t<td><p>1.4 m</p>\n<p>Spring tide</p>\n</td></tr>\n\
    <tr>\t<td />\t<td><p>calm</p>\n</td></tr>\n</table>\n\
    <p>Tides are given in metres above chart datum.</p>\n";

#[test]
fn reads_the_topics_of_a_winhelp_4_file_with_lz77_and_old_phrases() {
    let (body, metadata) = extract(HARBOUR);
    assert_eq!(body, HARBOUR_TOPICS);
    assert_eq!(metadata["dc:title"], ["Harbour Help"]);
    assert_eq!(metadata["dc:rights"], ["\u{a9} 2026 Harbour Office"]);
    assert_eq!(
        metadata[KEYWORDS],
        ["harbour", "lamp", "lighthouse", "tides"]
    );
}

#[test]
fn reads_hall_compressed_phrases_as_old_ones() {
    let help = HelpFile::open(HALL).unwrap();
    assert!(matches!(help.phrases, Phrases::Hall(_)));
    let (body, _) = extract(HALL);
    assert_eq!(body, HARBOUR_TOPICS);
}

#[test]
fn reads_a_winhelp_30_file_whose_links_run_on_across_blocks() {
    let help = HelpFile::open(WINHELP_30).unwrap();
    assert_eq!(help.layout, Layout::WinHelp30);
    assert!(help.topic.len() > 0x800, "the links span blocks");
    let (body, metadata) = extract(WINHELP_30);
    let log: String = (1..=60)
        .map(|day| format!("Day {day}: the lamp was lit at dusk and the harbour was calm.<br />\n"))
        .collect();
    assert_eq!(
        body,
        format!(
            "<h1>The Lighthouse</h1>\n\
             <p>The lighthouse keeper lights the lamp at dusk.</p>\n\
             <p>Lamp:\tFresnel lens<br />\nRange:\t18\u{a0}nautical miles</p>\n\
             <p>Fyret lyser om natten, og Ove Gr\u{f8}n holder vagt. See the tide table.</p>\n\
             <h1>Keeper's Log</h1>\n<p>{log}End of the log.</p>\n"
        )
    );
    assert_eq!(metadata["dc:title"], ["Harbour Help"]);
    assert!(!metadata.contains_key("dc:rights"));
}

#[test]
fn decodes_text_in_the_ansi_code_page_of_the_lcid() {
    for (lcid, name) in [
        (0x0409, "windows-1252"),
        (0x0405, "windows-1250"),
        (0x0419, "windows-1251"),
        (0x0C1A, "windows-1251"),
        (0x041A, "windows-1250"),
        (0x0408, "windows-1253"),
        (0x041F, "windows-1254"),
        (0x042C, "windows-1254"),
        (0x082C, "windows-1251"),
        (0x040D, "windows-1255"),
        (0x0401, "windows-1256"),
        (0x0427, "windows-1257"),
        (0x042A, "windows-1258"),
        (0x041E, "x-windows-874"),
        (0x0411, "windows-31j"),
        (0x0412, "x-windows-949"),
        (0x0804, "GBK"),
        (0x0404, "x-windows-950"),
        (0x0C04, "x-windows-950"),
    ] {
        let charset = codepage::windows(system::ansi_code_page(lcid));
        assert_eq!(charset.map(Charset::name), Some(name), "{lcid:#06x}");
    }
    for lcid in 0..=u16::MAX {
        let code_page = system::ansi_code_page(lcid);
        assert!(
            codepage::windows(code_page).is_some_and(Charset::decodes),
            "{code_page}"
        );
    }
    let at = offset_of(HARBOUR, "|SYSTEM");
    let lcid = HARBOUR[at..]
        .windows(4)
        .position(|window| window == [9, 0, 10, 0])
        .unwrap();
    let russian = with_bytes(HARBOUR, at + lcid + 4 + 8, &0x0419u16.to_le_bytes());
    let (body, _) = extract(&russian);
    assert!(body.contains("Ove Gr\u{448}n"), "{body}");
}

#[test]
fn reads_compressed_numbers() {
    let mut reader = Reader::new(
        &[
            0x0C, 0x81, 0x01, 0x8C, 0x03, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00, 0x80,
        ],
        "numbers",
    );
    assert_eq!(reader.compressed_u16().unwrap(), 6);
    assert_eq!(reader.compressed_u16().unwrap(), 0xC0);
    assert_eq!(reader.compressed_i16().unwrap(), 6);
    assert_eq!(reader.compressed_i16().unwrap(), 1 - 0x4000);
    assert_eq!(reader.compressed_i32().unwrap(), 0);
    assert_eq!(reader.compressed_i32().unwrap(), 0);
    assert!(reader.is_empty());
}

/// A table of `phrases`.
fn table(phrases: &[&str]) -> Table<'static> {
    let mut offsets = vec![0];
    for phrase in phrases {
        offsets.push(offsets.last().unwrap() + phrase.len());
    }
    Table::new(Cow::Owned(phrases.concat().into_bytes()), offsets)
}

/// The text `stored` expands to, given its `length`.
fn expanded(phrases: &Phrases<'_>, stored: &[u8], length: usize) -> Result<Vec<u8>, Malformed> {
    let mut text = Expanded::new(phrases, stored, length);
    let mut out = Vec::new();
    while let Some(byte) = text.next_byte()? {
        out.push(byte);
    }
    Ok(out)
}

#[test]
fn expands_old_phrases_with_the_space_an_odd_code_adds() {
    let phrases = Phrases::Old(table(&["keeper", "lamp"]));
    let stored = [b'A', 0x01, 0x01, 0x01, 0x02, 0, b'!'];
    assert_eq!(expanded(&phrases, &stored, 14).unwrap(), b"Akeeper lamp\0!");
    assert!(expanded(&phrases, &stored, 13).is_err());
    assert_eq!(expanded(&phrases, &stored, 3).unwrap(), b"A\x01\x01");
    let missing = expanded(&phrases, &[0x01, 0x04], 20).unwrap_err();
    assert!(
        missing.0.contains("the phrase 2 of 2 phrases"),
        "{missing:?}"
    );
    assert!(expanded(&phrases, &[0x01], 20).is_err());
    assert!(expanded(&Phrases::None, &[0x01, 0x00], 20).is_err());
}

#[test]
fn expands_hall_codes_of_phrases_runs_spaces_and_nuls() {
    let mut names: Vec<String> = (0..130).map(|n| format!("p{n}")).collect();
    names[129] = "far".to_owned();
    let phrases = Phrases::Hall(table(&names.iter().map(String::as_str).collect::<Vec<_>>()));
    let stored = [
        0x02,
        0x01,
        0x01,
        (2 << 3) | 3,
        b'a',
        b'b',
        b'c',
        (3 << 4) | 7,
        (1 << 4) | 0x0F,
    ];
    assert_eq!(
        expanded(&phrases, &stored, 100).unwrap(),
        b"p1farabc    \0\0"
    );
    assert!(expanded(&phrases, &[(4 << 3) | 3, b'a'], 100).is_err());
    assert!(expanded(&phrases, &[0x05, 0xFF], 100).is_err());
}

#[test]
fn reads_hall_phrase_lengths_from_their_bits() {
    let help = HelpFile::open(HALL).unwrap();
    let Phrases::Hall(table) = &help.phrases else {
        panic!("Hall compression");
    };
    assert_eq!(table.phrase(0).unwrap(), b"lighthouse");
    assert_eq!(table.phrase(130).unwrap(), b"harbour");
    assert!(table.phrase(131).is_err());
    let at = offset_of(HALL, "|PhrIndex");
    let endless = with_bytes(HALL, at + 4, &u32::MAX.to_le_bytes());
    assert!(declined(&endless).contains("the phrase lengths run past"));
}

#[test]
fn declines_what_is_no_help_file() {
    assert!(declined(b"PK\x03\x04").contains("not the magic number"));
    assert!(declined(&HARBOUR[..6]).contains("ends before"));
    let beyond = with_bytes(HARBOUR, 4, &u32::MAX.to_le_bytes());
    assert!(declined(&beyond).contains("beyond the"));
    let without_topics = HARBOUR
        .windows(7)
        .position(|window| window == b"|TOPIC\0")
        .unwrap();
    let renamed = with_bytes(HARBOUR, without_topics, b"|TOPIX");
    assert!(declined(&renamed).contains("has no |TOPIC file"));
}

#[test]
fn fails_on_hostile_sizes_without_reading_past_the_file() {
    let topic = offset_of(WINHELP_30, "|TOPIC");
    let link = topic + 12;
    let huge = with_bytes(
        WINHELP_30,
        link,
        &[0x7FFF_FFFFi32.to_le_bytes(), 0x7FFF_FFF0i32.to_le_bytes()].concat(),
    );
    let huge = with_bytes(&huge, link + 16, &0x7FFF_FFF0i32.to_le_bytes());
    assert!(failure(&huge).contains("ends within the topic link"));
    let short = with_bytes(WINHELP_30, link + 16, &3i32.to_le_bytes());
    assert!(failure(&short).contains("do not fit"));
    let stuck = with_bytes(WINHELP_30, link + 12, &0i32.to_le_bytes());
    assert!(failure(&stuck).contains("puts the next 0 bytes on"));
}

#[test]
fn fails_on_links_that_lead_back() {
    let layout = Layout::WinHelp31 {
        block_size: 0x1000,
        lz77: false,
    };
    let mut content = vec![0xFF; 4];
    content.extend([12, 0, 0, 0, 0, 0, 0, 0]);
    for field in [21i32, 0, -1, 12, 21] {
        content.extend(field.to_le_bytes());
    }
    content.push(0x20);
    let failed = TopicFile::new(&content, layout)
        .links()
        .find_map(Result::err)
        .unwrap();
    assert!(
        failed.0.contains("puts the next one back at 12"),
        "{failed:?}"
    );
}

#[test]
fn fails_on_btree_pages_linked_in_a_cycle() {
    let at = offset_of(HARBOUR, "|KWBTREE");
    let first_leaf = at + 38;
    let cycle = with_bytes(HARBOUR, first_leaf + 6, &0i16.to_le_bytes());
    assert!(declined(&cycle).contains("link in a cycle"));
    let missing = with_bytes(HARBOUR, first_leaf + 6, &7i16.to_le_bytes());
    assert!(declined(&missing).contains("has no page 7"));
}
