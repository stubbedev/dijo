//! Translation memories and translation files, as Tika's `TMXParser` and `XLIFF12Parser` read
//! them: each translation unit a div of its variants or of its source and target, the header's
//! languages and tools as metadata, and the units counted once the document has ended.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::jdk_zip::{self, Refusal};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Nested, Xhtml};
use crate::formats::xml::{self, attribute_named, Attribute, SaxHandler};
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java;

/// The attributes `TMXContentHandler.extractAttributes` copies onto a unit's or variant's
/// element, each under the name it writes it as.
const TMX_ATTRIBUTES: [(&str, &str); 11] = [
    ("xml:lang", "lang"),
    ("o-encoding", "original-encoding"),
    ("datatype", "datatype"),
    ("usagecount", "usagecount"),
    ("lastusagedate", "lastusagedate"),
    ("creationdate", "creationdate"),
    ("creationid", "creationid"),
    ("changedate", "changedate"),
    ("changeid", "changeid"),
    ("tuid", "tuid"),
    ("o-tmf", "o-tmf"),
];

/// The attributes of a TMX header Tika adds as metadata, each under the name it adds it as.
const TMX_HEADER: [(&str, &str); 7] = [
    ("creationtool", "creation-tool"),
    ("creationtoolversion", "creation-tool-version"),
    ("segtype", "segment-type"),
    ("o-tmf", "original-format-type"),
    ("datatype", "data-type"),
    ("srclang", "source-language"),
    ("adminlang", "admin-language"),
];

/// `Metadata.add` of an attribute that may be missing, which adds nothing.
fn add_present(metadata: &mut TikaMetadata, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        metadata.add(name, value);
    }
}

/// Tika's `TMXParser`.
pub struct TmxParser;

impl Parser for TmxParser {
    fn name(&self) -> &'static str {
        "extractous::TMXParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.tmx.TMXParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-tmx"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        Ok(Box::new(move |xhtml| {
            xhtml
                .metadata_mut()
                .set("Content-Type", "application/x-tmx");
            let mut handler = Tmx {
                xhtml: &mut *xhtml,
                source_language: None,
                target_languages: Vec::new(),
                units: 0,
                variants: 0,
                in_segment: false,
            };
            let parsed = xml::parse_sax(input, "the TMX document", &mut handler);
            if parsed.is_ok() {
                handler.end_document();
            }
            xhtml.end_document()?;
            parsed
        }))
    }
}

/// `TMXContentHandler`.
struct Tmx<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    source_language: Option<String>,
    target_languages: Vec<String>,
    units: u32,
    variants: u32,
    in_segment: bool,
}

impl Tmx<'_, '_> {
    /// `extractAttributes`: the attributes a unit or variant is written with, and its
    /// language among the target languages where it is not the source language.
    fn attributes(&mut self, attributes: &[Attribute]) -> Vec<(&'static str, String)> {
        if let Some(language) = attribute_named(attributes, "xml:lang") {
            let is_source = self
                .source_language
                .as_deref()
                .is_some_and(|source| java::equals_ignore_case(language, source));
            if !is_source && !self.target_languages.iter().any(|known| known == language) {
                self.target_languages.push(language.to_owned());
            }
        }
        TMX_ATTRIBUTES
            .iter()
            .filter_map(|&(read, written)| {
                attribute_named(attributes, read).map(|value| (written, value.to_owned()))
            })
            .collect()
    }

    fn start(&mut self, name: &str, attributes: &[Attribute]) -> Result<(), Failure> {
        let attributes = self.attributes(attributes);
        let borrowed: Vec<(&str, &str)> = attributes
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        Ok(self.xhtml.start_element_with(name, &borrowed)?)
    }

    /// `endDocument`: the target languages, in the order of a `HashSet`, and the counts.
    fn end_document(&mut self) {
        let metadata = self.xhtml.metadata_mut();
        for language in java::hash_set_order(self.target_languages.iter().map(String::as_str)) {
            metadata.add("target-language", language);
        }
        metadata.set("tu-count", self.units.to_string());
        metadata.set("tuv-count", self.variants.to_string());
    }
}

impl SaxHandler for Tmx<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match local_name {
            "header" => {
                let metadata = self.xhtml.metadata_mut();
                for (read, written) in TMX_HEADER {
                    add_present(metadata, written, attribute_named(attributes, read));
                }
                self.source_language = attribute_named(attributes, "srclang").map(str::to_owned);
            }
            "tu" => {
                self.units += 1;
                self.start("div", attributes)?;
            }
            "tuv" => {
                self.variants += 1;
                self.start("p", attributes)?;
            }
            "seg" => self.in_segment = true,
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        match local_name {
            "tu" => self.xhtml.end_element("div")?,
            "tuv" => self.xhtml.end_element("p")?,
            "seg" => self.in_segment = false,
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_segment && !text.is_empty() {
            self.xhtml.characters(text)?;
        }
        Ok(())
    }
}

/// Tika's `XLIFF12Parser`.
pub struct Xliff12Parser;

impl Parser for Xliff12Parser {
    fn name(&self) -> &'static str {
        "extractous::XLIFF12Parser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.xliff.XLIFF12Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-xliff+xml"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let mut finder = NamespacedUnitAttribute(false);
        let _ = xml::parse_sax(input, "the XLIFF document", &mut finder);
        if finder.0 {
            return Err(Declined::Deferred(
                "a trans-unit attribute in a namespace, which Tika copies onto the XHTML, whose \
                 writer fails on a namespace no element declares"
                    .to_owned(),
            ));
        }
        Ok(Box::new(move |xhtml| {
            xhtml
                .metadata_mut()
                .set("Content-Type", "application/x-xliff+xml");
            let mut handler = Xliff {
                xhtml: &mut *xhtml,
                files: 0,
                units: 0,
                in_unit: false,
            };
            let parsed = xml::parse_sax(input, "the XLIFF document", &mut handler);
            if parsed.is_ok() {
                let metadata = handler.xhtml.metadata_mut();
                metadata.set("file-count", handler.files.to_string());
                metadata.set("tu-count", handler.units.to_string());
            }
            xhtml.end_document()?;
            parsed
        }))
    }
}

/// Finds a `trans-unit` with an attribute in a namespace, before the first error, and stops.
struct NamespacedUnitAttribute(bool);

impl SaxHandler for NamespacedUnitAttribute {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name == "trans-unit"
            && attributes
                .iter()
                .any(|attribute| !attribute.namespace.is_empty())
        {
            self.0 = true;
            return Err(Failure::Malformed("found".to_owned()));
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

/// `XLIFF12ContentHandler`.
struct Xliff<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    files: u32,
    units: u32,
    in_unit: bool,
}

impl SaxHandler for Xliff<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match local_name {
            "file" => {
                self.files += 1;
                self.xhtml.start_element("div")?;
                self.xhtml.start_element("h1")?;
                if let Some(original) = attribute_named(attributes, "original") {
                    self.xhtml.characters(original)?;
                }
                self.xhtml.end_element("h1")?;
                let metadata = self.xhtml.metadata_mut();
                add_present(
                    metadata,
                    "source-language",
                    attribute_named(attributes, "source-language"),
                );
                add_present(
                    metadata,
                    "target-language",
                    attribute_named(attributes, "target-language"),
                );
            }
            "trans-unit" => {
                self.units += 1;
                self.in_unit = true;
                let copied: Vec<(&str, &str)> = attributes
                    .iter()
                    .map(|attribute| (attribute.qualified_name.as_str(), attribute.value.as_str()))
                    .collect();
                self.xhtml.start_element_with("div", &copied)?;
            }
            "source" | "target" => {
                let language: Vec<(&str, &str)> = attribute_named(attributes, "xml:lang")
                    .map(|language| ("lang", language))
                    .into_iter()
                    .collect();
                self.xhtml.start_element_with("p", &language)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        match local_name {
            "file" => self.xhtml.end_element("div")?,
            "trans-unit" => {
                self.in_unit = false;
                self.xhtml.end_element("div")?;
            }
            "source" | "target" => self.xhtml.end_element("p")?,
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_unit && !text.is_empty() {
            self.xhtml.characters(text)?;
        }
        Ok(())
    }
}

/// Tika's `XLZParser`: each entry of the archive named with `.xlf`, in the archive's order,
/// parsed by the XLIFF 1.2 parser into the document's handler, shielded from its end, so each
/// writes its own `html`, head and `body` into the document, with the metadata they share.
pub struct XlzParser;

impl Parser for XlzParser {
    fn name(&self) -> &'static str {
        "extractous::XLZParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.xliff.XLZParser"
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-xliff+zip"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let refused = |refusal| match refusal {
            Refusal::Unread(reason) => Declined::Deferred(reason.to_owned()),
            Refusal::Failed(reason) => Declined::Fails(Error::ParseError(reason)),
        };
        let zip = jdk_zip::open(input).map_err(refused)?;
        let mut documents = Vec::new();
        for entry in zip
            .entries()
            .iter()
            .filter(|entry| entry.name.contains(".xlf"))
        {
            let document = jdk_zip::read_whole(&zip, entry).map_err(refused)?;
            if let Err(Declined::Deferred(reason)) =
                Xliff12Parser.prepare(&document, metadata, configs)
            {
                return Err(Declined::Deferred(format!("{}: {reason}", entry.name)));
            }
            documents.push(document);
        }
        if documents.is_empty() {
            return Err(Declined::Deferred(
                "no XLIFF entry, of which Tika's path starts no document".to_owned(),
            ));
        }
        let start = metadata.clone();
        Ok(Box::new(move |xhtml| {
            let context = xhtml.context();
            for document in &documents {
                let prepared = Xliff12Parser
                    .prepare(document, &start, configs)
                    .map_err(|_| {
                        Failure::Malformed("the XLIFF entry is not prepared".to_owned())
                    })?;
                let shared = std::mem::take(xhtml.metadata_mut());
                let mut nested = Nested { outer: &mut *xhtml };
                let (shared, _, result) = Xhtml::write(&mut nested, shared, context, prepared);
                *xhtml.metadata_mut() = shared;
                result?;
            }
            Ok(())
        }))
    }
}

/// Tika's XLIFF handler stores each file's source language as `original` too (#118).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const SOURCE_LANGUAGE_AS_ORIGINAL: crate::formats::parity::Departures =
    &[crate::formats::parity::Departure::Unset("original", "#118")];

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, assert_same_as_tika_departing,
        conformance_corpus, documents, Scratch,
    };

    const TMX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tmx version="1.4">
  <header creationtool="memoQ" creationtoolversion="9.2" segtype="sentence" o-tmf="memoQ"
          adminlang="en-US" srclang="da-DK" datatype="plaintext"/>
  <body>
    <tu tuid="1" creationdate="20240101T120000Z" usagecount="2">
      <tuv xml:lang="da-DK"><seg>Hav og <bpt i="1">&lt;b&gt;</bpt>kyst<ept i="1">&lt;/b&gt;</ept></seg></tuv>
      <tuv xml:lang="sv-SE" changeid="x"><seg>Hav och kust</seg></tuv>
      <tuv xml:lang="DA-dk"><seg>Igen</seg></tuv>
    </tu>
    <tu><tuv xml:lang="nb-NO" o-encoding="utf-8"><seg>Hav og kyst</seg></tuv>
        <tuv xml:lang="fi-FI"><seg>Meri</seg></tuv></tu>
  </body>
</tmx>
"#;

    const XLIFF: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xliff version="1.2" xmlns="urn:oasis:names:tc:xliff:document:1.2">
  <file original="ordbog.properties" source-language="da" target-language="de" datatype="plaintext">
    <body>
      <trans-unit id="1" resname="hilsen" approved="yes">
        <source xml:lang="da">Goddag, &amp; velkommen</source>
        <target xml:lang="de">Guten Tag</target>
        <note>Ikke oversat</note>
      </trans-unit>
    </body>
  </file>
  <file original="andet.properties" datatype="plaintext">
    <body><trans-unit id="2"><source>Farvel</source></trans-unit></body>
  </file>
</xliff>
"#;

    #[test]
    fn extracts_translations_as_tikas_path_does() {
        let scratch = Scratch::new("translation");
        let tmx = scratch.write("memory.tmx", TMX.as_bytes());
        let xliff = scratch.write("ordbog.xlf", XLIFF.as_bytes());
        let broken_tmx = scratch.write("broken.tmx", &TMX.as_bytes()[..TMX.len() - 30]);
        let broken_xliff = scratch.write("broken.xlf", &XLIFF.as_bytes()[..250]);
        let spaced = scratch.write(
            "spaced.xlf",
            XLIFF
                .replace("approved=\"yes\"", "xml:space=\"preserve\"")
                .as_bytes(),
        );
        let (parsed, deferred) = documents(&TmxParser, &[scratch.directory()]);
        assert!(deferred.is_empty());
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        let (parsed, deferred) = documents(&Xliff12Parser, &[scratch.directory()]);
        assert_eq!(deferred, [spaced]);
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        assert_same_as_tika(&[tmx.as_path()], "org.apache.tika.parser.tmx.TMXParser");
        assert_same_as_tika_departing(
            &[xliff.as_path()],
            "org.apache.tika.parser.xliff.XLIFF12Parser",
            SOURCE_LANGUAGE_AS_ORIGINAL,
        );
        assert_fails_as_tika(&[broken_tmx.as_path(), broken_xliff.as_path()]);
    }

    /// An XLZ archive of `entries`, each a name and its content.
    fn xlz(entries: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_xlz_archives_as_tikas_path_does() {
        let scratch = Scratch::new("xlz");
        let second = XLIFF.replace("ordbog.properties", "andet.properties");
        let archive = scratch.write(
            "ordbog.xlz",
            &xlz(&[
                ("ordbog.xlf", XLIFF),
                ("noter.txt", "ikke XLIFF"),
                ("mappe/andet.xlf", &second),
            ]),
        );
        let empty = scratch.write("tom.xlz", &xlz(&[("noter.txt", "ikke XLIFF")]));
        let spaced = scratch.write(
            "spaced.xlz",
            &xlz(&[(
                "ordbog.xlf",
                &XLIFF.replace("approved=\"yes\"", "xml:space=\"preserve\""),
            )]),
        );
        let (parsed, deferred) = documents(&XlzParser, &[scratch.directory()]);
        assert_eq!(deferred, [spaced, empty]);
        assert_eq!(parsed, std::slice::from_ref(&archive));
        assert_same_as_tika_departing(
            &[archive.as_path()],
            XlzParser.tika_name(),
            SOURCE_LANGUAGE_AS_ORIGINAL,
        );
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_translations_as_tikas_path_does() {
        for parser in [&TmxParser as &dyn Parser, &Xliff12Parser, &XlzParser] {
            let (parsed, deferred) = documents(parser, &[&conformance_corpus()]);
            assert!(deferred.is_empty(), "{deferred:?}");
            assert_eq!(parsed.len(), 1, "{parsed:?}");
            let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
            assert_same_as_tika_departing(&paths, parser.tika_name(), SOURCE_LANGUAGE_AS_ORIGINAL);
        }
    }

    #[test]
    fn keeps_the_source_language_out_of_original() {
        let (_, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(XLIFF.as_bytes())
            .unwrap();
        assert!(!metadata.contains_key("original"), "{metadata:?}");
        assert_eq!(metadata["source-language"], ["da"]);
    }
}
