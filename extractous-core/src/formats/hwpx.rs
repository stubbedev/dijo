//! Hancom Office documents in HWPX (KS X 6101), as our Java `HwpxParser` reads them: the
//! sections the package lists, in their order, as paragraphs and tables, footnotes, endnotes,
//! running headers, footers and master pages where they are anchored, and the title, language,
//! authors and dates of the package as metadata. The package is read as [`jdk_zip`] reads the
//! JDK's `ZipFile`, its XML parts as Tika's SAX parser reads them.
//!
//! Our Java parser leaves headers, footers and master pages out, ends the collection of a
//! metadata value at any end tag, and fails on an end tag it has no open element for (#105).

use std::collections::HashMap;

use crate::compress::zip::file::ZipFile;
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::jdk_zip::{self, failed, Refusal};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler};
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java::is_blank;
use crate::java::strip;

const CONTAINER: &str = "META-INF/container.xml";
/// What HWPX anchors in a paragraph, read as paragraphs of their own: the notes, and the running
/// headers, footers and master pages.
const ANCHORED: [&str; 5] = ["footNote", "endNote", "header", "footer", "masterPage"];

/// A change `Package` makes to the document's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Change {
    Set(&'static str, String),
    Add(&'static str, String),
}

/// The package file: the changes it makes to the metadata, the parts it lists by id and the
/// order of its sections. A `HashMap` that, as Java's, takes a missing id or href.
struct Package<'a> {
    zip: ZipFile<'a>,
    changes: Vec<Change>,
    manifest: HashMap<Option<String>, Option<String>>,
    spine: Vec<Option<String>>,
}

impl Package<'_> {
    /// The part of the section `idref` names, or the failure `parse` throws on it.
    fn section(&self, idref: &Option<String>) -> Result<Vec<u8>, Refusal> {
        let href = self.manifest.get(idref).cloned().flatten().ok_or_else(|| {
            failed(format!(
                "The HWPX spine names the missing item {}",
                idref.as_deref().unwrap_or("null")
            ))
        })?;
        part(&self.zip, &href)
    }
}

/// `parsePart`'s bytes: the part at `path` from the archive's root.
fn part(zip: &ZipFile<'_>, path: &str) -> Result<Vec<u8>, Refusal> {
    let entry = jdk_zip::entry(zip, path)?
        .ok_or_else(|| failed(format!("The HWPX archive has no part {path}")))?;
    jdk_zip::read_whole(zip, entry)
}

/// A part parsed by `handler`, failing as the SAX parser fails.
fn parse_part(bytes: &[u8], path: &str, handler: &mut impl SaxHandler) -> Result<(), Refusal> {
    xml::parse_sax(bytes, path, handler).map_err(|failure| failed(format!("{failure:?}")))
}

/// What `parse` reads before it starts the document: the container, then the package file.
fn package(input: &[u8]) -> Result<Package<'_>, Refusal> {
    let zip = jdk_zip::open(input)?;
    let mut container = Container(Vec::new());
    parse_part(&part(&zip, CONTAINER)?, CONTAINER, &mut container)?;
    let path = container
        .0
        .into_iter()
        .next()
        .ok_or_else(|| failed("The HWPX container names no package file"))?;
    let mut handler = PackageHandler::default();
    parse_part(&part(&zip, &path)?, &path, &mut handler)?;
    Ok(Package {
        zip,
        changes: handler.changes,
        manifest: handler.manifest,
        spine: handler.spine,
    })
}

/// The `full-path` of each `rootfile` of the container.
struct Container(Vec<String>);

impl SaxHandler for Container {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name == "rootfile" {
            if let Some(path) = attribute_named(attributes, "full-path") {
                self.0.push(path.to_owned());
            }
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// `HwpxParser.Package`: the manifest, the spine and the metadata the package file holds.
#[derive(Default)]
struct PackageHandler {
    changes: Vec<Change>,
    manifest: HashMap<Option<String>, Option<String>>,
    spine: Vec<Option<String>>,
    text: String,
    /// The name of the value being collected, and how many elements are open inside the
    /// element that holds it.
    collecting: Option<(String, usize)>,
}

impl SaxHandler for PackageHandler {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if let Some((_, depth)) = &mut self.collecting {
            *depth += 1;
            return Ok(());
        }
        let value = |name: &str| attribute_named(attributes, name).map(str::to_owned);
        match local_name {
            "item" => {
                self.manifest.insert(value("id"), value("href"));
            }
            "itemref" => self.spine.push(value("idref")),
            "title" | "language" => self.collecting = Some((local_name.to_owned(), 0)),
            "meta" => self.collecting = value("name").map(|name| (name, 0)),
            _ => return Ok(()),
        }
        self.text.clear();
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.collecting.is_some() {
            self.text.push_str(text);
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        match &mut self.collecting {
            Some((_, depth)) if *depth > 0 => {
                *depth -= 1;
                return Ok(());
            }
            Some(_) => {}
            None => return Ok(()),
        }
        let Some((collecting, _)) = self.collecting.take() else {
            return Ok(());
        };
        let value = strip(&self.text).to_owned();
        if value.is_empty() {
            return Ok(());
        }
        self.changes.extend(match collecting.as_str() {
            "title" => Some(Change::Set("dc:title", value)),
            "language" => Some(Change::Set("dc:language", value)),
            "creator" => Some(Change::Add("dc:creator", value)),
            "CreatedDate" => Some(Change::Set("dcterms:created", value)),
            "ModifiedDate" => Some(Change::Set("dcterms:modified", value)),
            _ => None,
        });
        Ok(())
    }
}

/// `HwpxParser.Cell`: a table cell, whose paragraphs are those one level deeper than the open
/// ones it started inside, or what is anchored inside one, which takes paragraphs again.
struct Cell {
    depth: Option<usize>,
    lines: usize,
}

impl Cell {
    fn holds(&self, paragraph_depth: usize) -> bool {
        self.depth.is_some_and(|depth| paragraph_depth == depth + 1)
    }
}

/// `HwpxParser.Section`: paragraphs, and the tables and notes HWPX nests inside the paragraph
/// they are anchored in, the text of a paragraph before one ending a paragraph of its own.
struct Section<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    paragraphs: Vec<String>,
    cells: Vec<Cell>,
    text_depth: i64,
    in_section: bool,
}

impl Section<'_, '_> {
    fn append(&mut self, text: &str) {
        if self.text_depth > 0 {
            if let Some(paragraph) = self.paragraphs.last_mut() {
                paragraph.push_str(text);
            }
        }
    }

    /// `endParagraph`: the text the innermost open paragraph has collected so far, as a
    /// paragraph or as a line of the table cell it is in.
    fn end_paragraph(&mut self) -> Result<(), Failure> {
        let Some(paragraph) = self.paragraphs.last_mut() else {
            return Ok(());
        };
        let text = std::mem::take(paragraph);
        if is_blank(&text) {
            return Ok(());
        }
        let depth = self.paragraphs.len();
        match self.cells.last_mut() {
            Some(cell) if cell.holds(depth) => {
                if cell.lines > 0 {
                    self.xhtml.characters("\n")?;
                }
                cell.lines += 1;
                self.xhtml.characters(&text)?;
            }
            _ => self.xhtml.element("p", &text)?,
        }
        Ok(())
    }
}

impl SaxHandler for Section<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name == "sec" {
            self.in_section = true;
        }
        if !self.in_section {
            return Ok(());
        }
        match local_name {
            "p" => self.paragraphs.push(String::new()),
            "t" => self.text_depth += 1,
            "tab" => self.append("\t"),
            "lineBreak" => self.append("\n"),
            "tbl" => {
                self.end_paragraph()?;
                self.xhtml.start_element("table")?;
            }
            "tr" => self.xhtml.start_element("tr")?,
            "tc" => {
                self.xhtml.start_element("td")?;
                self.cells.push(Cell {
                    depth: Some(self.paragraphs.len()),
                    lines: 0,
                });
            }
            _ if ANCHORED.contains(&local_name) => {
                self.end_paragraph()?;
                self.cells.push(Cell {
                    depth: None,
                    lines: 0,
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.text_depth > 0 {
            if let Some(paragraph) = self.paragraphs.last_mut() {
                paragraph.push_str(text);
            }
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if !self.in_section {
            return Ok(());
        }
        match local_name {
            "p" => {
                self.end_paragraph()?;
                self.paragraphs.pop();
            }
            "t" => self.text_depth -= 1,
            "tbl" => self.xhtml.end_element("table")?,
            "tr" => self.xhtml.end_element("tr")?,
            "tc" => {
                self.cells.pop();
                self.xhtml.end_element("td")?;
            }
            "sec" => self.in_section = false,
            _ if ANCHORED.contains(&local_name) => {
                self.cells.pop();
            }
            _ => {}
        }
        Ok(())
    }
}

/// Our Java `HwpxParser`.
pub(crate) struct HwpxParser;

impl Parser for HwpxParser {
    fn name(&self) -> &'static str {
        "extractous::HwpxParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "ai.yobix.HwpxParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/hwp+zip"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let package = package(input).map_err(|refusal| match refusal {
            Refusal::Unread(why) => Declined::Deferred(why.to_owned()),
            Refusal::Failed(reason) => Declined::Fails(Error::ParseError(reason)),
        })?;
        let sections = package
            .spine
            .iter()
            .map(|idref| package.section(idref))
            .collect::<Vec<_>>();
        if let Some(why) = sections.iter().find_map(|section| match section {
            Err(Refusal::Unread(why)) => Some((*why).to_owned()),
            _ => None,
        }) {
            return Err(Declined::Deferred(why));
        }
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            for change in &package.changes {
                match change {
                    Change::Set(name, value) => metadata.set(name, value.as_str()),
                    Change::Add(name, value) => metadata.add(name, value.as_str()),
                }
            }
            for section in sections {
                let section =
                    section.map_err(|refusal| Failure::Malformed(format!("{refusal:?}")))?;
                let mut handler = Section {
                    xhtml,
                    paragraphs: Vec::new(),
                    cells: Vec::new(),
                    text_depth: 0,
                    in_section: false,
                };
                xml::parse_sax(&section, "an HWPX section", &mut handler)?;
            }
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::compress::zip::file::tests::zip;
    use crate::formats::parity::{assert_fails_as_tika, assert_same_as_tika, documents, Scratch};

    const TIKA_NAME: &str = "ai.yobix.HwpxParser";
    const CONTAINER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ocf:container xmlns:ocf="urn:oasis:names:tc:opendocument:xmlns:container">
  <ocf:rootfiles>
    <ocf:rootfile full-path="Contents/content.hpf" media-type="application/hwpml-package+xml"/>
  </ocf:rootfiles>
</ocf:container>"#;
    const PACKAGE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<opf:package xmlns:opf="http://www.idpf.org/2007/opf/">
  <opf:metadata>
    <opf:title> A title </opf:title>
    <opf:language>ko</opf:language>
    <opf:meta name="creator" content="text">Ada</opf:meta>
    <opf:meta name="creator" content="text">Charles</opf:meta>
    <opf:meta name="CreatedDate" content="text">2024-01-02T03:04:05Z</opf:meta>
    <opf:meta name="keyword" content="text">ignored</opf:meta>
    <opf:meta name="ModifiedDate" content="text">   </opf:meta>
  </opf:metadata>
  <opf:manifest>
    <opf:item id="header" href="Contents/header.xml" media-type="application/xml"/>
    <opf:item id="section0" href="Contents/section0.xml" media-type="application/xml"/>
    <opf:item id="section1" href="Contents/section1.xml" media-type="application/xml"/>
  </opf:manifest>
  <opf:spine>
    <opf:itemref idref="section0"/>
    <opf:itemref idref="section1"/>
  </opf:spine>
</opf:package>"#;
    const SECTION0: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<hs:sec xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section" xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph">
  <hp:p><hp:run><hp:t>First paragraph</hp:t><hp:t><hp:tab/>tabbed<hp:lineBreak/>broken</hp:t></hp:run></hp:p>
  <hp:p><hp:run><hp:t>Before the table</hp:t><hp:tbl><hp:tr><hp:tc><hp:subList><hp:p><hp:run><hp:t>Cell one</hp:t></hp:run></hp:p><hp:p><hp:run><hp:t>second line</hp:t></hp:run></hp:p></hp:subList></hp:tc><hp:tc><hp:subList><hp:p><hp:run><hp:t>Cell two</hp:t><hp:footNote><hp:subList><hp:p><hp:run><hp:t>A note</hp:t></hp:run></hp:p></hp:subList></hp:footNote></hp:run></hp:p></hp:subList></hp:tc></hp:tr></hp:tbl><hp:t>after the table</hp:t></hp:run></hp:p>
  <hp:p><hp:run><hp:t>   </hp:t></hp:run></hp:p>
</hs:sec>"#;
    const SECTION1: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<hs:sec xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section" xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph">
  <hp:p><hp:run><hp:t>Second section, 한국어 텍스트.</hp:t><hp:endNote><hp:subList><hp:p><hp:run><hp:t>An end note</hp:t></hp:run></hp:p></hp:subList></hp:endNote></hp:run></hp:p>
</hs:sec>"#;

    /// A section of less text than the write limit, so that a failure after it fails every
    /// extraction.
    const SHORT_SECTION: &str = r#"<hs:sec xmlns:hs="http://www.hancom.co.kr/hwpml/2011/section" xmlns:hp="http://www.hancom.co.kr/hwpml/2011/paragraph"><hp:p><hp:run><hp:t>Short</hp:t></hp:run></hp:p></hs:sec>"#;

    fn hwpx(package: &str, sections: &[(&str, &str)]) -> Vec<u8> {
        package_in(CONTAINER_XML, package, sections)
    }

    fn package_in(container: &str, package: &str, sections: &[(&str, &str)]) -> Vec<u8> {
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("mimetype", b"application/hwp+zip"),
            ("META-INF/container.xml", container.as_bytes()),
            ("Contents/content.hpf", package.as_bytes()),
        ];
        entries.extend(sections.iter().map(|(name, xml)| (*name, xml.as_bytes())));
        zip(&entries)
    }

    #[test]
    fn reads_headers_and_metadata_by_their_structure() {
        // Our Java parser leaves headers out and ends a value at any end tag (#105).
        let section = SECTION0.replace(
            "<hp:p><hp:run><hp:t>First paragraph</hp:t>",
            "<hp:p><hp:run><hp:t>Before</hp:t><hp:secPr><hp:header><hp:p><hp:run><hp:t>Running header</hp:t></hp:run></hp:p></hp:header></hp:secPr><hp:t>First paragraph</hp:t>",
        );
        let package = PACKAGE_XML.replace(
            "<opf:title> A title </opf:title>",
            "<opf:title> A <opf:b>bold</opf:b> title </opf:title>",
        );
        let document = hwpx(
            &package,
            &[
                ("Contents/section0.xml", section.as_str()),
                ("Contents/section1.xml", SECTION1),
            ],
        );
        let (xml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(&document)
            .unwrap();
        assert!(
            xml.contains("<p>Before</p>\n<p>Running header</p>\n<p>First paragraph"),
            "{xml}"
        );
        assert_eq!(metadata["dc:title"], ["A bold title"]);
    }

    #[test]
    fn extracts_documents_as_tikas_path_does() {
        let scratch = Scratch::new("hwpx");
        let sections = [
            ("Contents/section0.xml", SECTION0),
            ("Contents/section1.xml", SECTION1),
        ];
        let read = [scratch.write("document.hwpx", &hwpx(PACKAGE_XML, &sections))];
        let short = [
            ("Contents/section0.xml", SHORT_SECTION),
            ("Contents/section1.xml", SHORT_SECTION),
        ];
        let failing = [
            scratch.write("missing-section.hwpx", &hwpx(PACKAGE_XML, &short[..1])),
            scratch.write(
                "missing-item.hwpx",
                &hwpx(
                    &PACKAGE_XML.replace(r#"idref="section1""#, r#"idref="nowhere""#),
                    &short,
                ),
            ),
            scratch.write(
                "broken-section.hwpx",
                &hwpx(
                    PACKAGE_XML,
                    &[short[0], ("Contents/section1.xml", "<hs:sec><p>")],
                ),
            ),
            scratch.write(
                "no-package.hwpx",
                &package_in(
                    &CONTAINER_XML.replace("content.hpf", "elsewhere.hpf"),
                    PACKAGE_XML,
                    &sections,
                ),
            ),
            scratch.write(
                "no-root-file.hwpx",
                &package_in(
                    &CONTAINER_XML.replace("full-path", "path"),
                    PACKAGE_XML,
                    &sections,
                ),
            ),
        ];
        let (parsed, deferred) = documents(&HwpxParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len() + failing.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }
}
