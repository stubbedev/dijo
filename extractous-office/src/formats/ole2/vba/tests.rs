use super::*;
use crate::ole2::{write, Node, Storage};

/// MS-OVBA's example of a chunk of literals only (3.2.1), and a chunk of a copy token.
#[test]
fn decompresses_literals_and_copy_tokens_as_ms_ovba_does() {
    let literals = [
        0x01, 0x19, 0xB0, 0x00, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x00, 0x69, 0x6A,
        0x6B, 0x6C, 0x6D, 0x6E, 0x6F, 0x70, 0x00, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x2E,
    ];
    assert_eq!(decompress(&literals).unwrap(), b"abcdefghijklmnopqrstuv.");
    let copied = [0x01, 0x05, 0xB0, 0x08, 0x61, 0x62, 0x63, 0x03, 0x20];
    assert_eq!(decompress(&copied).unwrap(), b"abcabcabc");
}

#[test]
fn fails_where_poi_throws_decompressing() {
    assert!(matches!(
        decompress(&[0x02, 0x05, 0xB0]),
        Err(Thrown::IllegalArgument(_))
    ));
    assert!(matches!(
        decompress(&[0x01, 0x05, 0x40]),
        Err(Thrown::IllegalArgument(_))
    ));
    assert!(matches!(
        decompress(&[0x01, 0x09, 0x30, b'a']),
        Err(Thrown::IllegalState(_))
    ));
    let before_the_chunk = [0x01, 0x03, 0xB0, 0x01, 0x03, 0x20];
    assert!(matches!(
        decompress(&before_the_chunk),
        Err(Thrown::Runtime(_))
    ));
}

/// A stream cut within a literal ends the decompression before the chunk, as POI's does.
#[test]
fn ends_where_a_chunk_is_cut_as_poi_does() {
    let cut = [0x01, 0x02, 0x30, b'a', b'b', b'c', 0x05, 0xB0, 0x00, b'd'];
    assert_eq!(decompress(&cut).unwrap(), b"abc");
}

/// `bytes` as one raw chunk after another, as MS-OVBA stores what it does not compress.
fn raw(bytes: &[u8]) -> Vec<u8> {
    let mut compressed = vec![0x01];
    for chunk in bytes.chunks(CHUNK) {
        compressed.extend((0x3000 | (chunk.len() as u16 - 1)).to_le_bytes());
        compressed.extend_from_slice(chunk);
    }
    compressed
}

/// A record of a `dir` stream of an id and the bytes after its size.
fn record(id: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = id.to_le_bytes().to_vec();
    bytes.extend((body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(body);
    bytes
}

/// The decompressed `dir` stream of a project in `code_page` of the modules `streams`, each with
/// where its source starts in its stream.
fn dir(code_page: u16, streams: &[(&str, u32)]) -> Vec<u8> {
    let mut bytes = record(0x0003, &code_page.to_le_bytes());
    bytes.extend(record(0x000F, &(streams.len() as u16).to_le_bytes()));
    for &(name, offset) in streams {
        bytes.extend(record(0x0019, name.as_bytes()));
        bytes.extend(record(0x001A, name.as_bytes()));
        bytes.extend(0x0032u16.to_le_bytes());
        let unicode: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        bytes.extend((unicode.len() as u32).to_le_bytes());
        bytes.extend(unicode);
        bytes.extend(record(0x0031, &offset.to_le_bytes()));
        bytes.extend(0x002Bu16.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
    }
    bytes.extend(0x0010u16.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    bytes
}

/// A module's stream: a performance cache of `cache` bytes, then the source compressed.
fn module(cache: usize, source: &str) -> Vec<u8> {
    let mut bytes = vec![0xAB; cache];
    bytes.extend(raw(&cp1252(source)));
    bytes
}

fn entry(name: &str, node: Node<'static>) -> (Vec<u16>, Node<'static>) {
    (name.encode_utf16().collect(), node)
}

/// A compound document whose project storage holds the `VBA` storage of `vba` and the PROJECT
/// stream `project`.
fn document(vba: Vec<(Vec<u16>, Node<'static>)>, project: &str) -> Vec<u8> {
    write(&Storage {
        class_id: [0; 16],
        entries: vec![entry(
            "_VBA_PROJECT_CUR",
            Node::Storage(Storage {
                class_id: [0; 16],
                entries: vec![
                    entry("PROJECT", Node::Owned(project.as_bytes().to_vec())),
                    entry(
                        "VBA",
                        Node::Storage(Storage {
                            class_id: [0; 16],
                            entries: vba,
                        }),
                    ),
                ],
            }),
        )],
    })
    .unwrap()
}

const PROJECT: &str = "ID=\"{00000000-0000-0000-0000-000000000000}\"\r\nDocument=ThisWorkbook/&H00000000\r\nModule=Logbog\r\n";
const THIS_WORKBOOK: &str = "Attribute VB_Name = \"ThisWorkbook\"\r\n";
const LOGBOG: &str =
    "Attribute VB_Name = \"Logbog\"\r\nSub Skriv()\r\n  MsgBox \"Bl\u{e5}b\u{e6}r\"\r\nEnd Sub\r\n";

fn cp1252(text: &str) -> Vec<u8> {
    text.chars().map(|c| c as u32 as u8).collect()
}

fn project(code_page: u16, vba_extra: Vec<(Vec<u16>, Node<'static>)>, properties: &str) -> Vec<u8> {
    let mut vba = vec![
        entry(
            "dir",
            Node::Owned(raw(&dir(code_page, &[("ThisWorkbook", 3), ("Logbog", 7)]))),
        ),
        entry("_VBA_PROJECT", Node::Owned(vec![0xCC, 0x61, 0xFF, 0xFF])),
        entry("ThisWorkbook", Node::Owned(module(3, THIS_WORKBOOK))),
        entry("Logbog", Node::Owned(module(7, LOGBOG))),
    ];
    vba.extend(vba_extra);
    document(vba, properties)
}

fn modules(macros: &Macros) -> Vec<(&str, &str)> {
    macros
        .read
        .as_ref()
        .unwrap()
        .iter()
        .map(|module| (module.name.as_str(), module.source.as_str()))
        .collect()
}

/// The modules of a project are read from where the `dir` stream says their source starts, in
/// the project's code page, in the order the project names them (#186).
#[test]
fn reads_each_module_of_a_project_in_its_code_page() {
    let data = project(1252, Vec::new(), PROJECT);
    let file = Ole2::open(&data).unwrap();
    let macros = Macros::read(&file);
    assert_eq!(
        modules(&macros),
        [("ThisWorkbook", THIS_WORKBOOK), ("Logbog", LOGBOG)]
    );
    if let Some(Difference(reason)) = macros.difference() {
        assert!(reason.contains("java.util.HashMap"), "{reason}");
    }
}

/// A stream of the `VBA` storage no project names as a module is left out, where POI hands it on
/// undecompressed (#186).
#[test]
fn leaves_out_a_stream_no_project_names() {
    let stray = vec![entry("Stray", Node::Owned(b"not a module".to_vec()))];
    let data = project(1252, stray, PROJECT);
    let file = Ole2::open(&data).unwrap();
    let macros = Macros::read(&file);
    assert_eq!(
        modules(&macros),
        [("ThisWorkbook", THIS_WORKBOOK), ("Logbog", LOGBOG)]
    );
    let Some(Difference(reason)) = macros.difference() else {
        panic!("a difference");
    };
    assert!(reason.contains("the stream Stray"), "{reason}");
}

/// A project in code page 1256 is read in it, where POI reads it in code page 1255 (#186).
#[test]
fn reads_a_project_in_code_page_1256_in_it() {
    let data = project(1256, Vec::new(), PROJECT);
    let file = Ole2::open(&data).unwrap();
    let macros = Macros::read(&file);
    let logbog = Charset::named("windows-1256")
        .decode(&cp1252(LOGBOG))
        .unwrap();
    assert_eq!(modules(&macros)[1], ("Logbog", logbog.as_str()));
    let Some(Difference(reason)) = macros.difference() else {
        panic!("a difference");
    };
    assert!(reason.contains("code page 1256"), "{reason}");
}

/// Where POI throws reading the macros, the reading fails and differs from Tika's path, which
/// records the failure in an entry of no content: a document module the PROJECT stream names
/// without its cookie, and a module the `dir` stream names but the storage lacks.
#[test]
fn fails_where_poi_throws_reading_the_macros() {
    let without_cookie = project(1252, Vec::new(), "Document=ThisWorkbook\r\n");
    let lacking = document(
        vec![entry(
            "dir",
            Node::Owned(raw(&dir(1252, &[("Missing", 0)]))),
        )],
        PROJECT,
    );
    for data in [without_cookie, lacking] {
        let file = Ole2::open(&data).unwrap();
        let macros = Macros::read(&file);
        assert!(macros.read.is_err(), "{macros:?}");
        let Some(Difference(reason)) = macros.difference() else {
            panic!("a difference");
        };
        assert!(reason.contains("POI fails to read the macros"), "{reason}");
    }
}

/// A module whose source does not start where the `dir` stream says is found by its first
/// chunk that decompresses into what starts like a module's source, as POI finds it.
#[test]
fn finds_a_module_whose_offset_is_wrong_by_brute_force() {
    let mut vba = vec![entry("dir", Node::Owned(raw(&dir(1252, &[("Logbog", 2)]))))];
    vba.push(entry("Logbog", Node::Owned(module(5, LOGBOG))));
    let data = document(vba, "Module=Logbog\r\n");
    let file = Ole2::open(&data).unwrap();
    assert_eq!(modules(&Macros::read(&file)), [("Logbog", LOGBOG)]);
}

/// The `\r\n` and `\n\r` POI splits the PROJECT stream at.
#[test]
fn splits_the_project_stream_as_poi_does() {
    assert_eq!(split_lines("a\r\nb\n\rc\nd\r\n\r\n"), ["a", "b", "c\nd"]);
    assert!(check_project_properties("[Host]\r\nDocument=A/&H1\r\n\"x\"").is_ok());
    assert!(check_project_properties("Document=\"A/&H1\"").is_ok());
    assert!(check_project_properties("Document=\"A\"").is_err());
}

/// Tika's test documents with macros, which the native parsers read with their macros as Tika's
/// path does, but where Tika hands the modules on in the order of a `java.util.HashMap`, which
/// leaves the document to Tika's path in a build with Tika (#186).
#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
fn extracts_macros_as_tikas_path_does() {
    use crate::config::OfficeParserConfig;
    use crate::formats::parity::{assert_same_as_tika_with, conformance_corpus};
    use crate::formats::{doc::DocParser, ppt::PptParser, xls::XlsParser, Parser};

    let configs = ParserConfigs {
        office: OfficeParserConfig::new().set_extract_macros(true),
        ..ParserConfigs::default()
    };
    let corpus = conformance_corpus();
    let documents: [(&str, &dyn Parser); 3] = [
        ("testWORD_macros.doc", &DocParser),
        ("testEXCEL_macro.xls", &XlsParser),
        ("testPPT_macros.ppt", &PptParser),
    ];
    let mut alike = Vec::new();
    for (name, parser) in documents {
        let path = walk(&corpus, name).unwrap_or_else(|| panic!("{name} in {corpus:?}"));
        let input = std::fs::read(&path).unwrap();
        let prepared = parser.prepare(&input, &TikaMetadata::new(), &configs);
        match prepared {
            Ok(_) => alike.push(path.clone()),
            Err(Decline::Differs {
                difference: Difference(reason),
                ..
            }) => assert!(
                reason.contains("java.util.HashMap") && !reason.contains(';'),
                "{name}: {reason}"
            ),
            Err(declined) => panic!("{name}: {declined:?}"),
        };
    }
    let alike: Vec<&std::path::Path> = alike.iter().map(std::path::PathBuf::as_path).collect();
    assert_same_as_tika_with(&alike, crate::formats::poi::OFFICE_PARSER, &configs);
}

/// The file named `name` under `directory`.
#[cfg(feature = "tika")]
fn walk(directory: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(directory).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(found) = walk(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
    }
    None
}
