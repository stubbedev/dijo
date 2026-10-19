//! Directory Interchange Format records, as Tika's `DIFParser` reads them: every leaf element's
//! text as `dif:` metadata named by its path, and the title and the bounding box of the spatial
//! coverage written to the content handler Tika was given, around its own XHTML.
//!
//! A leaf keeps its own text; the text of an element with elements in it, which no leaf takes,
//! is not kept. Tika's `DIFContentHandler` keeps one stack of every piece
//! of text, popping one per end tag, so a leaf may take the text of another element, an
//! element whose text comes in pieces (around an entity, say) yields its last piece and a
//! title one heading per piece, and an empty leaf may fail on the empty stack (#118).

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;

/// `DIF`: the prefix of the metadata of each leaf element.
const PREFIX: &str = "dif:";
/// The elements whose text `DIFContentHandler` writes as a row of the bounding box.
const BOUNDS: [&str; 4] = [
    "Southernmost_Latitude",
    "Northernmost_Latitude",
    "Westernmost_Longitude",
    "Easternmost_Longitude",
];

/// Tika's `DIFParser`.
pub struct DifParser;

impl Parser for DifParser {
    fn name(&self) -> &'static str {
        "dijo::DIFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.dif.DIFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/dif+xml"]
    }

    /// Proportional: every leaf element's text is kept as `dif:` metadata, which the document's
    /// head holds until its end, so the record is as large as its leaves.
    fn footprint(&self) -> Footprint {
        Footprint::Proportional
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        if xml::has_document_type(input) {
            return Err(Decline::Unread(
                "a document type declaration, under which Xerces may report whitespace as \
                 ignorable, which DIFContentHandler writes as it is"
                    .to_owned(),
            ));
        }
        Ok(Box::new(move |xhtml| {
            xhtml.start_element("p")?;
            let mut handler = Dif {
                xhtml: &mut *xhtml,
                open: Vec::new(),
                text: String::new(),
                leaf: false,
            };
            let parsed = xml::parse_paged(input, pages, "the DIF record", &mut handler);
            xhtml.end_element("p")?;
            xhtml.end_document()?;
            parsed.map_err(|failure| match failure {
                Failure::Malformed(reason) => {
                    Failure::Malformed(format!("XML parse error: {reason}"))
                }
                other => other,
            })
        }))
    }
}

/// `DIFContentHandler`: the names of the open elements, whether the last one opened has no
/// element in it yet, and its text so far while it has none.
struct Dif<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    open: Vec<String>,
    text: String,
    leaf: bool,
}

impl Dif<'_, '_> {
    /// A heading of `text`, on a line of its own, indented once.
    fn heading(&mut self, text: &str) -> Result<(), Failure> {
        self.xhtml.raw_characters("\n")?;
        self.xhtml.raw_characters("\t")?;
        self.xhtml.raw_start_element("h3")?;
        self.xhtml.raw_characters(text)?;
        self.xhtml.raw_end_element("h3")?;
        Ok(())
    }

    /// A row of the bounding box: the element's name and its text.
    fn bound(&mut self, name: &str, value: &str) -> Result<(), Failure> {
        let xhtml = &mut *self.xhtml;
        xhtml.raw_characters("\n")?;
        xhtml.raw_characters("\t")?;
        xhtml.raw_characters("\t")?;
        xhtml.raw_start_element("tr")?;
        xhtml.raw_start_element("td")?;
        xhtml.raw_characters(&format!("{name} : "))?;
        xhtml.raw_end_element("td")?;
        xhtml.raw_start_element("td")?;
        xhtml.raw_characters(value)?;
        xhtml.raw_end_element("td")?;
        xhtml.raw_end_element("tr")?;
        Ok(())
    }
}

impl SaxHandler for Dif<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.leaf = true;
        if local_name == "Spatial_Coverage" {
            self.heading("Geographic Data: ")?;
            self.xhtml.raw_characters("\n")?;
            self.xhtml.raw_characters("\t")?;
            self.xhtml.raw_start_element("table")?;
        }
        self.open.push(local_name.to_owned());
        self.text.clear();
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if local_name == "Spatial_Coverage" {
            self.xhtml.raw_characters("\n")?;
            self.xhtml.raw_characters("\t")?;
            self.xhtml.raw_end_element("table")?;
        }
        if self.leaf {
            self.leaf = false;
            let element = self
                .open
                .last()
                .expect("the element ending is open")
                .clone();
            let text = std::mem::take(&mut self.text);
            if element == "Entry_Title" {
                self.heading(&format!("Title: {text}"))?;
            }
            if BOUNDS.contains(&element.as_str()) {
                self.bound(&element, &text)?;
            }
            if !text.is_empty() {
                self.xhtml
                    .metadata_mut()
                    .add_prefixed(PREFIX, &self.open.join("-"), text);
            }
        }
        self.open.pop();
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.leaf {
            self.text.push_str(text);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    const RECORD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<DIF xmlns="http://gcmd.gsfc.nasa.gov/Aboutus/xml/dif/">
  <Entry_ID>havis-2024</Entry_ID>
  <Entry_Title>Havis i Øresund &amp; Kattegat</Entry_Title>
  <Data_Set_Citation><Dataset_Creator>DMI</Dataset_Creator><Version>2</Version></Data_Set_Citation>
  <Spatial_Coverage>
    <Southernmost_Latitude>54.5</Southernmost_Latitude>
    <Northernmost_Latitude>57.8</Northernmost_Latitude>
    <Westernmost_Longitude>10.1</Westernmost_Longitude>
    <Easternmost_Longitude>12.9</Easternmost_Longitude>
  </Spatial_Coverage>
  <Keyword>is</Keyword><Keyword>hav</Keyword>
</DIF>
"#;

    /// The XHTML and metadata the native path extracts from the DIF record `record`.
    fn extract(record: &str) -> (String, crate::Metadata) {
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(record.as_bytes())
            .unwrap()
    }

    #[test]
    fn keeps_each_elements_own_text() {
        // Tika's one stack of text pieces gives a leaf the last piece pushed, which may be
        // another element's, makes a heading of each piece of a title, and fails on an empty
        // first leaf (#118).
        let (xml, metadata) = extract(&RECORD.replace("<Version>2</Version>", "<Version/>"));
        assert_eq!(xml.matches("<h3>Title: ").count(), 1, "{xml}");
        assert!(
            xml.contains("<h3>Title: Havis i \u{d8}resund &amp; Kattegat</h3>"),
            "{xml}"
        );
        assert_eq!(
            metadata["dif:DIF-Entry_Title"],
            ["Havis i \u{d8}resund & Kattegat"]
        );
        assert!(!metadata.contains_key("dif:DIF-Data_Set_Citation-Version"));
        assert_eq!(
            metadata["dif:DIF-Data_Set_Citation-Dataset_Creator"],
            ["DMI"]
        );
        assert_eq!(metadata["dif:DIF-Keyword"], ["is", "hav"]);
        let (_, metadata) = extract(
            r#"<?xml version="1.0"?><DIF xmlns="http://gcmd.gsfc.nasa.gov/Aboutus/xml/dif/"><Entry_ID/><Entry_Title>T</Entry_Title></DIF>"#,
        );
        assert!(!metadata.contains_key("dif:DIF-Entry_ID"));
        assert_eq!(metadata["dif:DIF-Entry_Title"], ["T"]);
    }
}
