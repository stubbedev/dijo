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
#[test]
fn extracts_metafiles_as_tikas_path_does() {
    let scratch = Scratch::new("metafile");
    let russian = wmf(&[
        wmf_font(0xCC),
        wmf_text_out(b"\xcf\xf0\xe8\xe2\xe5\xf2"),
        wmf_font(0x02),
        wmf_ext_text_out(b"Caf\xe9"),
        wmf_text_out(b"odd"),
    ]);
    let icon = emf(&[
        emf_comment(&utf16("IconOnly\0")),
        emf_comment(&utf16("Quarterly report.docx\0")),
        emf_text("Quarterly", 10, 20, true),
        emf_text("report", 75, 20, true),
        emf_text("near", 128, 20, true),
        emf_text("Next line", 10, 40, true),
        emf_text("ANSI caf\u{e9}", 10, 60, false),
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
        scratch.write("unknown.emf", &emf(&[emf_record(0x45, &[])])),
        scratch.write(
            "negative.emf",
            &emf(&[emf_text("ok", 0, 0, true), {
                let mut text = emf_text("bad", 0, 9, true);
                text[8 + 36..8 + 40].copy_from_slice(&int(-1));
                text
            }]),
        ),
        scratch.write("cut.wmf", &russian[..russian.len() - 10]),
        scratch.write("unknown.wmf", &wmf(&[wmf_record(0x0999, &[])])),
    ];
    for (parser, count) in [(&EmfParser as &dyn Parser, 6), (&WmfParser, 4)] {
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

#[test]
fn decodes_every_charset_a_wmf_font_names() {
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
fn reads_the_name_an_icon_only_comment_gives() {
    let name = utf16("?Report.docx\0");
    assert_eq!(unicode_string(&name).as_deref(), Some("?Report.docx"));
    let marked = [&[0u8, 0][..], &name[2..]].concat();
    assert_eq!(unicode_string(&marked).as_deref(), Some("?Report.docx"));
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
