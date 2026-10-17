//! Tika's `WordMLParser`: the paragraphs, tables, line breaks and text of the body, the
//! hyperlinks with their text, and the pictures' binary data handed to the embedded document
//! extractor, as its `WordMLHandler`, `HyperlinkHandler` and `PictHandler` write them, in that
//! order for each event.

use super::{Body, OFFICE_URN};
use crate::config::ParserConfigs;
use crate::formats::embedded::{Wrapping, RESOURCE_NAME};
use crate::formats::metadata::office::ORIGINAL_RESOURCE_NAME;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{attribute, Attribute};
use crate::formats::{Declined, Failure, Opened, Parser, Prepared};
use crate::java;

const MEDIA_TYPE: &str = "application/vnd.ms-wordml";
/// `WORD_ML_URL`, the namespace of the document's elements.
const WORD_ML: &str = "http://schemas.microsoft.com/office/word/2003/wordml";
/// `MS_VML_URN`, the namespace of the pictures' image data.
const VML_URN: &str = "urn:schemas-microsoft-com:vml";

/// Tika's `WordMLParser`.
pub(crate) struct WordMlParser;

impl Parser for WordMlParser {
    fn name(&self) -> &'static str {
        "extractous::WordMLParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.xml.WordMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        super::prepare(input, MEDIA_TYPE, Word::default())
    }
}

/// The XHTML element `WORDML_TO_XHTML` maps a WordprocessingML element to, by its lower-cased
/// local name.
fn xhtml_element(name: &str) -> Option<&'static str> {
    match name {
        "p" => Some("p"),
        "tbl" => Some("table"),
        "tr" => Some("tr"),
        "tc" => Some("td"),
        _ => None,
    }
}

/// The name `WordMLHandler` matches an element by: its local name, lower-cased for one in the
/// WordprocessingML namespace.
fn matched_name(namespace: &str, local_name: &str) -> String {
    if namespace == WORD_ML {
        java::to_lower_case(local_name)
    } else {
        local_name.to_owned()
    }
}

/// Whether `WordMLHandler` ignores the characters in an element it matches by `name`: a link's
/// or a picture's, and the document properties'. `binData` is in its set too, but as written,
/// which its lower-cased name never matches.
fn ignores_characters(namespace: &str, name: &str) -> bool {
    (namespace == WORD_ML && matches!(name, "hlink" | "pict"))
        || (namespace == OFFICE_URN && name == "DocumentProperties")
}

/// The state of `WordMLHandler`, `HyperlinkHandler` and `PictHandler`.
#[derive(Debug, Default)]
struct Word {
    ignore_characters: bool,
    in_body: bool,
    in_p: bool,
    link: Link,
    picture: Picture,
}

/// `HyperlinkHandler`'s link: its destination and its text so far.
#[derive(Debug, Default)]
struct Link {
    open: bool,
    text: String,
    href: Option<String>,
}

/// `PictHandler`'s picture: its binary data, as it reads and once decoded, its name and the
/// original name of its image.
#[derive(Debug, Default)]
struct Picture {
    in_pict: bool,
    in_bin: bool,
    encoded: String,
    data: Option<Vec<u8>>,
    name: Option<String>,
    source: Option<String>,
}

impl Word {
    /// `WordMLHandler.startElement`.
    fn start_body(
        &mut self,
        xhtml: &mut Xhtml,
        namespace: &str,
        name: &str,
    ) -> Result<(), Failure> {
        if namespace == WORD_ML {
            if name == "body" {
                self.in_body = true;
                return Ok(());
            }
            if let Some(element) = xhtml_element(name) {
                if self.in_p && (name == "p" || element == "table") {
                    xhtml.end_element("p")?;
                    self.in_p = false;
                }
                xhtml.start_element(element)?;
                if element == "table" {
                    xhtml.start_element("tbody")?;
                }
                if name == "p" {
                    self.in_p = true;
                }
            }
            if name == "br" {
                xhtml.characters_chunk("\n")?;
            }
        }
        if ignores_characters(namespace, name) {
            self.ignore_characters = true;
        }
        Ok(())
    }

    /// `WordMLHandler.endElement`.
    fn end_body(&mut self, xhtml: &mut Xhtml, namespace: &str, name: &str) -> Result<(), Failure> {
        if namespace == WORD_ML {
            if let Some(element) = xhtml_element(name) {
                if element == "table" {
                    xhtml.end_element("tbody")?;
                }
                if element == "p" && !self.in_p {
                    xhtml.start_element("p")?;
                }
                xhtml.end_element(element)?;
                if element == "p" {
                    self.in_p = false;
                }
            }
        }
        if ignores_characters(namespace, name) {
            self.ignore_characters = false;
        }
        Ok(())
    }
}

impl Link {
    /// `HyperlinkHandler.startElement`.
    fn start(&mut self, namespace: &str, local_name: &str, attributes: &[Attribute]) {
        if namespace == WORD_ML && local_name == "hlink" {
            self.href =
                attribute(attributes, WORD_ML, "dest").map(|href| java::trim(href).to_owned());
            self.open = true;
        }
    }

    /// `HyperlinkHandler.endElement`: the link and its trimmed text.
    fn end(&mut self, xhtml: &mut Xhtml, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if namespace != WORD_ML || local_name != "hlink" {
            return Ok(());
        }
        match self.href.take() {
            Some(href) => xhtml.start_element_with("a", &[("href", &href)])?,
            None => xhtml.start_element("a")?,
        }
        xhtml.characters_chunk(java::trim(&self.text))?;
        xhtml.end_element("a")?;
        self.text.clear();
        self.open = false;
        Ok(())
    }
}

impl Picture {
    /// `PictHandler.startElement`.
    fn start(&mut self, namespace: &str, local_name: &str, attributes: &[Attribute]) {
        if namespace == WORD_ML {
            if local_name == "pict" {
                self.in_pict = true;
            } else if local_name == "binData" {
                self.in_bin = true;
                self.name = attribute(attributes, WORD_ML, "name")
                    .map(|name| name.replacen("wordml://", "", 1));
            }
            return;
        }
        if namespace != VML_URN || local_name != "imagedata" {
            return;
        }
        let Some(title) =
            attribute(attributes, OFFICE_URN, "title").filter(|title| !title.is_empty())
        else {
            return;
        };
        let mut source = title.to_owned();
        if let Some(src) = attribute(attributes, "", "src") {
            if let Some(dot) = src.rfind('.').filter(|&dot| dot + 1 < src.len()) {
                source.push_str(&src[dot..]);
            }
        }
        self.source = Some(source);
    }

    /// `PictHandler.endElement`: the image of a picture, and the decoded binary data handed on
    /// at the end of the picture, or of the binary data outside one.
    fn end(&mut self, xhtml: &mut Xhtml, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if namespace != WORD_ML {
            return Ok(());
        }
        if local_name == "pict" {
            self.in_pict = false;
            match &self.name {
                Some(name) => xhtml.start_element_with("img", &[("href", name)])?,
                None => xhtml.start_element("img")?,
            }
            xhtml.end_element("img")?;
            self.embed(xhtml, Wrapping::Bare)
        } else if local_name == "binData" {
            self.in_bin = false;
            self.data = Some(java::commons_base64_decode(&self.encoded));
            self.encoded.clear();
            if self.in_pict {
                return Ok(());
            }
            self.embed(xhtml, Wrapping::PackageEntry)
        } else {
            Ok(())
        }
    }

    /// `handleEmbedded`: the decoded data, if any, handed to the embedded document extractor in
    /// a stream opened on its bytes alone, named as the picture is.
    fn embed(&mut self, xhtml: &mut Xhtml, wrapping: Wrapping) -> Result<(), Failure> {
        let name = self.name.take();
        let source = self.source.take();
        let Some(data) = self.data.take() else {
            return Ok(());
        };
        let mut metadata = TikaMetadata::new();
        if let Some(name) = name {
            metadata.set(RESOURCE_NAME, name);
        }
        if let Some(source) = source {
            metadata.set(ORIGINAL_RESOURCE_NAME, source);
        }
        xhtml.parse_embedded_opened(&data, &metadata, wrapping, Opened::Alone)
    }
}

impl Body for Word {
    fn start(
        &mut self,
        xhtml: &mut Xhtml,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.start_body(xhtml, namespace, &matched_name(namespace, local_name))?;
        self.link.start(namespace, local_name, attributes);
        self.picture.start(namespace, local_name, attributes);
        Ok(())
    }

    fn end(&mut self, xhtml: &mut Xhtml, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.end_body(xhtml, namespace, &matched_name(namespace, local_name))?;
        self.link.end(xhtml, namespace, local_name)?;
        self.picture.end(xhtml, namespace, local_name)
    }

    fn characters(&mut self, xhtml: &mut Xhtml, text: &str) -> Result<(), Failure> {
        if !self.ignore_characters && self.in_body {
            xhtml.characters_chunk(text)?;
        }
        if self.link.open {
            self.link.text.push_str(text);
        }
        if self.picture.in_bin {
            self.picture.encoded.push_str(text);
        } else if self.picture.in_pict {
            xhtml.characters_chunk(text)?;
        }
        Ok(())
    }
}
