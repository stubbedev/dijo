//! Directory Interchange Format records, as Tika's `DIFParser` reads them: every leaf element's
//! last piece of text as `dif:` metadata named by its path, and the title and the bounding box of
//! the spatial coverage written to the content handler Tika was given, around its own XHTML.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Declined, Failure, Parser, Prepared};

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
pub(crate) struct DifParser;

impl Parser for DifParser {
    fn name(&self) -> &'static str {
        "extractous::DIFParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.dif.DIFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/dif+xml"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        if xml::has_document_type(input) {
            return Err(Declined::Deferred(
                "a document type declaration, under which Xerces may report whitespace as \
                 ignorable, which DIFContentHandler writes as it is"
                    .to_owned(),
            ));
        }
        Ok(Box::new(move |xhtml| {
            xhtml.start_element("p")?;
            let mut handler = Dif {
                xhtml: &mut *xhtml,
                path: Vec::new(),
                texts: Vec::new(),
                leaf: false,
            };
            let parsed = xml::parse_sax(input, "the DIF record", &mut handler);
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

/// `DIFContentHandler`: the open elements, every piece of text pushed and one popped per end
/// tag, as its two stacks keep them.
struct Dif<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    path: Vec<String>,
    texts: Vec<String>,
    leaf: bool,
}

/// `Stack.peek` or `Stack.pop` of an empty stack.
fn empty_stack() -> Failure {
    Failure::Malformed("EmptyStackException".to_owned())
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
        self.path.push(local_name.to_owned());
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if local_name == "Spatial_Coverage" {
            self.xhtml.raw_characters("\n")?;
            self.xhtml.raw_characters("\t")?;
            self.xhtml.raw_end_element("table")?;
        }
        if self.leaf {
            let value = self.texts.last().ok_or_else(empty_stack)?;
            self.xhtml
                .metadata_mut()
                .add_prefixed(PREFIX, &self.path.join("-"), value.as_str());
            self.leaf = false;
        }
        self.path.pop().ok_or_else(empty_stack)?;
        self.texts.pop().ok_or_else(empty_stack)?;
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.texts.push(text.to_owned());
        let element = self.path.last().ok_or_else(empty_stack)?.clone();
        if element == "Entry_Title" {
            self.heading(&format!("Title: {text}"))?;
        }
        if BOUNDS.contains(&element.as_str()) {
            self.bound(&element, text)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

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

    #[test]
    fn extracts_dif_records_as_tikas_path_does() {
        let scratch = Scratch::new("dif");
        let record = scratch.write("havis.dif", RECORD.as_bytes());
        let empty = scratch.write(
            "empty.dif",
            RECORD
                .replace("<Version>2</Version>", "<Version/>")
                .as_bytes(),
        );
        let leading = scratch.write(
            "leading.dif",
            br#"<?xml version="1.0"?><DIF xmlns="http://gcmd.gsfc.nasa.gov/Aboutus/xml/dif/"><Entry_ID/></DIF>"#,
        );
        let typed = scratch.write(
            "typed.dif",
            RECORD
                .replace("<DIF ", "<!DOCTYPE DIF [<!ELEMENT DIF ANY>]><DIF ")
                .as_bytes(),
        );
        let (parsed, deferred) = documents(&DifParser, &[scratch.directory()]);
        assert_eq!(deferred, [typed]);
        assert_eq!(parsed.len(), 3, "{parsed:?}");
        assert_same_as_tika(
            &[record.as_path(), empty.as_path()],
            "org.apache.tika.parser.dif.DIFParser",
        );
        assert_fails_as_tika(&[leading.as_path()]);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_dif_records_as_tikas_path_does() {
        let (parsed, deferred) = documents(&DifParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 3, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, DifParser.tika_name());
    }
}
