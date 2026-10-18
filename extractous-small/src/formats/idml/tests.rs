use std::io::Write;
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::*;
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika_departing, conformance_corpus, documents, Scratch,
};

const XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/">
<xmp:CreatorTool>Adobe InDesign 19.0</xmp:CreatorTool><xmp:CreateDate>2026-03-04T05:06:07+01:00</xmp:CreateDate>
<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Havnebladet</rdf:li></rdf:Alt></dc:title>
</rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="r"?>"#;

const NAMESPACE: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";

fn package(parts: &[(&str, String)]) -> Vec<u8> {
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer.start_file("mimetype", options).unwrap();
    writer
        .write_all(b"application/vnd.adobe.indesign-idml-package")
        .unwrap();
    for (name, content) in parts {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn spread(element: &str, pages: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><idPkg:{element} xmlns:idPkg="{NAMESPACE}" DOMVersion="19.0"><{element} Self="u1" PageCount="{pages}"><Page Self="u2"/></{element}></idPkg:{element}>"#
    )
}

fn story(paragraphs: &[&str]) -> String {
    let content: String = paragraphs
        .iter()
        .map(|text| {
            format!("<ParagraphStyleRange><CharacterStyleRange><Content>{text}</Content><Br/></CharacterStyleRange></ParagraphStyleRange>")
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><idPkg:Story xmlns:idPkg="{NAMESPACE}" DOMVersion="19.0"><Story Self="u3">{content}</Story></idPkg:Story>"#
    )
}

#[test]
fn extracts_idml_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&IdmlParser, &roots);
    assert!(
        parsed.is_empty() && deferred.is_empty(),
        "{parsed:?} {deferred:?}"
    );

    let scratch = Scratch::new("idml");
    let full = scratch.write(
        "havn.idml",
        &package(&[
            (
                "designmap.xml",
                "<?xml version=\"1.0\"?><Document/>".to_owned(),
            ),
            (
                "MasterSpreads/MasterSpread_a.xml",
                spread("MasterSpread", " 2 "),
            ),
            ("Spreads/Spread_b.xml", spread("Spread", "x")),
            ("Spreads/Spread_c.xml", spread("Spread", "3")),
            (
                "Stories/Story_d.xml",
                story(&["Bølgerne går højt", "i Øresund &amp; Kattegat"]),
            ),
            ("META-INF/metadata.xml", XMP.to_owned()),
        ]),
    );
    let plain = scratch.write(
        "plain.idml",
        &package(&[("Stories/Story_a.xml", story(&["Kun tekst"]))]),
    );
    let pageless = scratch.write(
        "pageless.idml",
        &package(&[(
            "Spreads/Spread_a.xml",
            "<?xml version=\"1.0\"?><Document/>".to_owned(),
        )]),
    );
    let typed = scratch.write(
        "typed.idml",
        &package(&[(
            "Stories/Story_a.xml",
            story(&["tekst"]).replace("?><idPkg", "?><!DOCTYPE idPkg:Story><idPkg"),
        )]),
    );
    let (parsed, deferred) = documents(&IdmlParser, &[scratch.directory()]);
    assert_eq!(deferred, [typed]);
    assert_eq!(parsed.len(), 3, "{parsed:?}");
    assert_same_as_tika_departing(
        &[full.as_path(), plain.as_path()],
        IdmlParser.tika_name(),
        ACCUMULATED_PAGE_COUNTS,
    );
    assert_fails_as_tika(&[pageless.as_path()]);
}

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&IdmlParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 1, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_departing(&paths, IdmlParser.tika_name(), ACCUMULATED_PAGE_COUNTS);
}
