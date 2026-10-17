#[cfg(feature = "tika")]
use std::io::Write;
#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

#[cfg(feature = "tika")]
use zip::write::SimpleFileOptions;
#[cfg(feature = "tika")]
use zip::{CompressionMethod, ZipWriter};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

/// A 1×1 PNG.
#[cfg(feature = "tika")]
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

#[cfg(feature = "tika")]
const WORKSHEET: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<geogebra format="5.0" version="5.2.900.0" app="classic" id="a1b2c3" xml:lang="da">
<construction title=" Pythagoras på dansk " author="Ane Bøgh" date="1. marts 2026">
<expression label="txt" exp="&quot;I en retvinklet trekant &quot; + &quot;a² + b² = c²&quot;"/>
<expression label="f" exp="x^2 + 1"/>
<element type="text" label="t1">
<content val="[{&quot;text&quot;:&quot;Træk i hjørnerne.\nPrøv selv!&quot;},{&quot;text&quot;:&quot;\r\nSidste linje&quot;,&quot;bold&quot;:true}]"/>
<caption val="  Hypotenusen c  "/>
</element>
<element type="text" label="t2"><content val="ikke JSON"/></element>
</construction>
</geogebra>
"#;

#[cfg(feature = "tika")]
const MACROS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<geogebra format="5.0">
<macro cmdName="Midtpunkt" toolName="" toolHelp="Vælg to punkter" iconFile="ikon.png">
<construction title="ignored"><expression exp="&quot;i makroen&quot;"/></construction>
</macro>
</geogebra>
"#;

#[cfg(feature = "tika")]
fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, content) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[cfg(feature = "tika")]
fn slide(text: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><geogebra format="5.0" app="notes"><construction title="{text}"><element type="text"><caption val="{text}"/></element></construction></geogebra>"#
    )
}

#[test]
fn orders_slides_as_tika_does() {
    let mut ids = vec!["_slide10", "_slide2", "_slide02", "_slide1"];
    ids.sort_by(|a, b| compare_slides(a, b));
    assert_eq!(ids, ["_slide1", "_slide02", "_slide2", "_slide10"]);
    assert_eq!(slide_id("_slide12/geogebra.xml"), Some("_slide12"));
    assert_eq!(slide_id("_slide/geogebra.xml"), None);
    assert_eq!(slide_id("x/_slide1/geogebra.xml"), None);
    let json =
        json::read(br#"{"a":{"text":"1"},"b":[{"text":"2"}],"a":{"text":"3"}} trailing"#).unwrap();
    let mut texts = Vec::new();
    json.find_values_as_text("text", &mut texts).unwrap();
    assert_eq!(texts, ["3", "2"]);
}

#[cfg(feature = "tika")]
#[test]
fn extracts_geogebra_files_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&GeoGebraParser, &roots);
    assert!(
        parsed.is_empty() && deferred.is_empty(),
        "{parsed:?} {deferred:?}"
    );

    let scratch = Scratch::new("geogebra");
    let worksheet = scratch.write(
        "trekant.ggb",
        &archive(&[
            ("geogebra_thumbnail.png", PNG),
            ("geogebra_javascript.js", b"function ggbOnInit() {}\n"),
            ("geogebra_defaults2d.xml", b"<defaults/>"),
            ("geogebra.xml", WORKSHEET.as_bytes()),
            ("geogebra_macro.xml", MACROS.as_bytes()),
            ("noter.txt", "Noter om trekanten, på dansk.\n".as_bytes()),
        ]),
    );
    let tool = scratch.write(
        "midtpunkt.ggt",
        &archive(&[("geogebra_macro.xml", MACROS.as_bytes()), ("ikon.png", PNG)]),
    );
    let first = slide("Første side");
    let second = slide("Anden side");
    let slides = scratch.write(
        "noter.ggs",
        &archive(&[
            ("_slide1/geogebra.xml", first.as_bytes()),
            ("_slide2/geogebra.xml", second.as_bytes()),
            ("_slide2/geogebra_thumbnail.png", PNG),
            ("_slide1/noter.txt", b"Noter til side et.\n"),
            (
                "structure.json",
                br#"{"chapters":[{"pages":[{"elements":[{"id":"_slide2"},{"id":"_slide9"}]}]}]}"#,
            ),
        ]),
    );
    let numbered = scratch.write(
        "tal.ggb",
        &archive(&[(
            "geogebra.xml",
            WORKSHEET
                .replace(
                    "&quot;Træk",
                    "&quot;Træk&quot;,&quot;size&quot;:3,&quot;x&quot;:&quot;",
                )
                .replace("\\nPrøv selv!", "")
                .as_bytes(),
        )]),
    );
    let broken = scratch.write(
        "brudt.ggb",
        &archive(&[("geogebra.xml", b"<geogebra><construction></geogebra>")]),
    );
    let (parsed, deferred) = documents(&GeoGebraParser, &[scratch.directory()]);
    assert_eq!(deferred, [broken], "{parsed:?}");
    assert_eq!(parsed.len(), 4, "{parsed:?}");
    assert_same_as_tika(
        &[
            worksheet.as_path(),
            tool.as_path(),
            slides.as_path(),
            numbered.as_path(),
        ],
        GeoGebraParser.tika_name(),
    );
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&GeoGebraParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 10, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, GeoGebraParser.tika_name());
}
