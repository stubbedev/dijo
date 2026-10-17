#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::testing::*;
use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
};

#[cfg(feature = "tika")]
fn paths(files: &[PathBuf]) -> Vec<&Path> {
    files.iter().map(PathBuf::as_path).collect()
}

#[cfg(feature = "tika")]
/// A WMF picture of Cyrillic text in the font it makes and selects, and of Latin-1 text in a
/// symbol font, the last font made selected as Tika takes it.
fn russian() -> Vec<u8> {
    wmf(&[
        wmf_font(0xCC),
        wmf_select(0),
        wmf_text_out(b"\xcf\xf0\xe8\xe2\xe5\xf2"),
        wmf_font(0x02),
        wmf_select(1),
        wmf_ext_text_out(b"Caf\xe9"),
        wmf_text_out(b"odd"),
    ])
}

#[cfg(feature = "tika")]
#[test]
fn extracts_metafiles_as_tikas_path_does() {
    let scratch = Scratch::new("metafile");
    let russian = russian();
    let icon = emf(&[
        emf_comment(&utf16("IconOnly\0")),
        emf_comment(&utf16("Quarterly report.docx\0")),
        emf_text("Quarterly", 10, 20, true),
        emf_text("report", 75, 20, true),
        emf_text("near", 128, 20, true),
        emf_text("Next line", 10, 40, true),
        emf_text("ANSI text", 10, 60, false),
    ]);
    let embedding = emf(&[
        emf_text("Before", 0, 0, true),
        emf_wmf_comment(&russian),
        emf_formats_comment(&[b"plain text in a format", b""]),
        emf_text("After", 0, 30, true),
    ]);
    let read = [
        scratch.write("icon.emf", &icon),
        scratch.write("embedding.emf", &embedding),
        scratch.write("russian.wmf", &russian),
        scratch.write("empty.emf", &emf(&[])),
        scratch.write("empty.wmf", &wmf(&[])),
    ];
    let failing = [
        scratch.write("cut.emf", &icon[..icon.len() - 30]),
        scratch.write(
            "negative.emf",
            &emf(&[emf_text("ok", 0, 0, true), {
                let mut text = emf_text("bad", 0, 9, true);
                text[8 + 36..8 + 40].copy_from_slice(&int(-1));
                text
            }]),
        ),
        scratch.write("cut.wmf", &russian[..russian.len() - 10]),
    ];
    for (parser, count) in [(&EmfParser as &dyn Parser, 5), (&WmfParser, 3)] {
        let (parsed, deferred) = documents(parser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), count, "{parsed:?}");
    }
    assert_same_as_tika(&paths(&read[..2]), EmfParser.tika_name());
    assert_same_as_tika(&paths(&read[2..3]), WmfParser.tika_name());
    assert_same_as_tika(&paths(&read[3..4]), EmfParser.tika_name());
    assert_same_as_tika(&paths(&read[4..]), WmfParser.tika_name());
    assert_fails_as_tika(&paths(&failing));
}

/// The body of the XHTML the native parser extracts of `picture`.
fn body(picture: &[u8]) -> String {
    let (xml, _) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(picture)
        .unwrap();
    xml[xml.find("<body>").unwrap()..].to_owned()
}

/// Where Tika's path fails on a record POI cannot read, decodes ANSI text as UTF-8 or a WMF's
/// text in the last font made, the port reads on and decodes text in the font selected (#127).
#[test]
fn reads_records_tika_fails_on_and_text_in_the_font_selected() {
    let unknown = emf(&[
        emf_record(0x45, &[]),
        emf_record(0x08, &[&[0; 16][..], &int(1), &int(0), &int(0)].concat()),
        emf_text("After them", 0, 0, true),
    ]);
    assert_eq!(body(&unknown), "<body><p>After them</p>\n</body></html>");
    let ansi = emf(&[
        emf_text("caf\u{e9}", 0, 0, false),
        emf_font(1, 0xCC),
        emf_select(1),
        emf_text("\u{cf}\u{f0}\u{e8}\u{e2}\u{e5}\u{f2}", 0, 20, false),
    ]);
    assert_eq!(
        body(&ansi),
        "<body><p>caf\u{e9}</p>\n<p>\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}</p>\n</body></html>"
    );
    let selected = wmf(&[
        wmf_font(0xCC),
        wmf_font(0xEE),
        wmf_select(0),
        wmf_text_out(b"\xcf\xf0\xe8\xe2\xe5\xf2"),
        wmf_record(0x0999, &[]),
    ]);
    assert_eq!(
        body(&selected),
        "<body><p>\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}</p>\n</body></html>"
    );
}

#[test]
fn decodes_every_charset_a_font_names() {
    for id in 0..=u8::MAX {
        if let Some(name) = font_charset(id) {
            assert!(
                Charset::named(name).decode(b"abc").is_some(),
                "{name} is not decoded"
            );
        }
    }
}

#[test]
fn reads_the_name_an_icon_only_comment_gives_up_to_its_end() {
    let name = utf16("Report.docx\0");
    assert_eq!(unicode_string(&name).as_deref(), Some("Report.docx"));
    assert_eq!(
        unicode_string(&utf16("Report.docx")).as_deref(),
        Some("Report.docx")
    );
    let marked = [&[0u8, 0][..], &name].concat();
    assert_eq!(unicode_string(&marked).as_deref(), Some(""));
    assert_eq!(unicode_string(&[0]), None);
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_metafiles_as_tikas_path_does() {
    for (parser, count) in [(&EmfParser as &dyn Parser, 3), (&WmfParser, 2)] {
        let (parsed, deferred) = documents(parser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), count, "{parsed:?}");
        assert_same_as_tika(&paths(&parsed), parser.tika_name());
    }
}
