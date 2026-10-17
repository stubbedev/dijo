//! Packages built for tests.

use std::io::Write;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// A package whose `main` part the package's relationships name as its office document, of
/// `parts`, each a name, a content type and the content.
pub(crate) fn package(main: &str, parts: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let office_document =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
    package_with(&relationship("rId1", office_document, main), parts)
}

/// A package of `parts` whose own relationships are `package_relationships`.
pub(crate) fn package_with(package_relationships: &str, parts: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let mut types = String::from(
        "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>",
    );
    for (name, content_type, _) in parts {
        types.push_str(&format!(
            "<Override PartName=\"/{name}\" ContentType=\"{content_type}\"/>"
        ));
    }
    types.push_str("</Types>");
    let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut add = |name: &str, content: &[u8]| {
        writer.start_file(name, options).unwrap();
        writer.write_all(content).unwrap();
    };
    add("[Content_Types].xml", types.as_bytes());
    add(
        "_rels/.rels",
        relationships(package_relationships).as_bytes(),
    );
    for (name, _, content) in parts {
        add(name, content);
    }
    writer.finish().unwrap().into_inner()
}

/// A relationship part of `relationships`.
pub(crate) fn relationships(relationships: &str) -> String {
    format!(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{relationships}</Relationships>"
    )
}

pub(crate) fn relationship(id: &str, kind: &str, target: &str) -> String {
    format!("<Relationship Id=\"{id}\" Type=\"{kind}\" Target=\"{target}\"/>")
}
