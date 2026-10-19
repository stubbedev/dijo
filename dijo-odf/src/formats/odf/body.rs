//! Tika's `OpenDocumentBodyHandler` behind its `NSNormalizerContentHandler`: the text of
//! `content.xml` and `styles.xml` as XHTML. Paragraphs, headings, lists, tables, links, notes,
//! annotations, text boxes and images become their XHTML counterparts, bold, italic and
//! underlined text styles become `b`, `i` and `u`, and templates, style definitions, covered
//! table cells and, unless deleted content is asked for, tracked changes are left out. A file
//! embedded as base64 in `binary-data` is parsed as an embedded document where it ends.
//!
//! Unlike Tika's handler, headings and notes end as blocks, `text:s` writes as many spaces as
//! its `text:c` counts, malformed font weights and outline levels are ignored rather than
//! failing the document, and the attributes of OpenOffice.org 1 documents are read in their
//! OpenDocument namespaces, as their elements are (#90).

use std::collections::HashMap;

use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::odf::{
    normalized_attributes, normalized_namespace, BinaryData, BINARY_DATA_ELEMENT, OFFICE_NS,
};
use crate::formats::xhtml::{Stop, Xhtml, XHTML_NAMESPACE};
use crate::formats::xml::{attribute, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::input::Input;
use crate::java;

const TEXT_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";
const TABLE_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";
const STYLE_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:style:1.0";
const FORMATTING_OBJECTS_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0";
const SVG_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0";
const PRESENTATION_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:presentation:1.0";
const DRAW_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:drawing:1.0";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const SPACE: &str = " ";
/// Spaces to write a `text:s` from, in pieces.
const SPACES: &str = "                                                                ";
const TAB: &str = "\t";
const MIN_BOLD_WEIGHT: i32 = 500;
/// The most spaces one `text:s` writes, whatever its `text:c` says.
const MAX_SPACES: usize = 65_535;

/// How the attributes of an ODF element become those of its XHTML element: the ODF attribute,
/// by namespace and local name, and the XHTML name it takes.
type AttributeMapping = &'static [(&'static str, &'static str, &'static str)];

const LINK_ATTRIBUTES: AttributeMapping =
    &[(XLINK_NS, "href", "href"), (XLINK_NS, "title", "title")];
const CELL_ATTRIBUTES: AttributeMapping = &[
    (TABLE_NS, "number-columns-spanned", "colspan"),
    (TABLE_NS, "number-rows-spanned", "rowspan"),
    (TABLE_NS, "number-columns-repeated", "colspan"),
];

/// Tika's `MAPPINGS`: the ODF elements written as an XHTML element of their own, by namespace and
/// local name, with how their attributes carry over.
const MAPPINGS: [(&str, &str, &str, AttributeMapping); 16] = [
    (TEXT_NS, "p", "p", &[]),
    (TEXT_NS, "line-break", "br", &[]),
    (TEXT_NS, "list-item", "li", &[]),
    (TEXT_NS, "note", "span", &[]),
    (OFFICE_NS, "annotation", "span", &[]),
    (PRESENTATION_NS, "notes", "span", &[]),
    (DRAW_NS, "object", "object", &[]),
    (DRAW_NS, "text-box", "div", &[]),
    (SVG_NS, "title", "span", &[]),
    (SVG_NS, "desc", "span", &[]),
    (TEXT_NS, "span", "span", &[]),
    (TEXT_NS, "a", "a", LINK_ATTRIBUTES),
    (DRAW_NS, "a", "a", LINK_ATTRIBUTES),
    (TABLE_NS, "table", "table", &[]),
    (TABLE_NS, "table-row", "tr", &[]),
    (TABLE_NS, "table-cell", "td", CELL_ATTRIBUTES),
];

/// The failure of a spooling of binary data, which `error` stopped.
fn unspooled(error: std::io::Error) -> Failure {
    Failure::Extraction(Error::IoError(format!(
        "spooling the binary data of an OpenDocument part fails: {error}"
    )))
}

/// The XHTML element an ODF element maps to, and how its attributes carry over.
fn mapping(namespace: &str, local_name: &str) -> Option<(&'static str, AttributeMapping)> {
    MAPPINGS
        .iter()
        .find(|(ns, local, _, _)| *ns == namespace && *local == local_name)
        .map(|(_, _, element, attributes)| (*element, *attributes))
}

/// A text style: whether its text is bold, italic or underlined.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TextStyle {
    italic: bool,
    bold: bool,
    underlined: bool,
}

/// A text style Tika holds by reference, as an index into [`Body::text_styles`].
type TextStyleId = usize;
/// A list style Tika holds by reference, as an index into [`Body::list_styles`].
type ListStyleId = usize;

/// A list as it is written: as `ul` unless its named list style numbers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListTag {
    Unnamed,
    Styled(ListStyleId),
}

/// The state of Tika's `OpenDocumentBodyHandler` over one XML part, which also keeps the
/// elements it has open, as the `XHTMLBalancingHandler` behind it in a flat document does.
pub(super) struct Body<'a, 'x> {
    xhtml: &'a mut Xhtml<'x>,
    include_deleted_content: bool,
    open: Vec<&'static str>,
    text_nodes: Vec<bool>,
    completely_filtered: usize,
    heading_stack: Vec<&'static str>,
    text_styles: Vec<TextStyle>,
    paragraph_text_style_names: HashMap<String, TextStyleId>,
    text_style_names: HashMap<String, TextStyleId>,
    list_styles: Vec<bool>,
    list_style_names: HashMap<String, ListStyleId>,
    current_paragraph_style_name: Option<String>,
    current_text_style: Option<TextStyleId>,
    current_text_style_name: Option<String>,
    list_style_stack: Vec<ListTag>,
    list_style: Option<ListStyleId>,
    has_written_start_style_tags: bool,
    underlined: bool,
    bold: bool,
    italic: bool,
    paragraph_depth: i64,
    anchor_depth: i64,
    /// The data of the `binary-data` element being read, if one is, decoded as far as its text
    /// has come.
    binary_data: Option<BinaryData>,
}

impl<'a, 'x> Body<'a, 'x> {
    pub(super) fn new(xhtml: &'a mut Xhtml<'x>, include_deleted_content: bool) -> Self {
        Self {
            xhtml,
            include_deleted_content,
            open: Vec::new(),
            text_nodes: Vec::new(),
            completely_filtered: 0,
            heading_stack: Vec::new(),
            text_styles: Vec::new(),
            paragraph_text_style_names: HashMap::new(),
            text_style_names: HashMap::new(),
            list_styles: Vec::new(),
            list_style_names: HashMap::new(),
            current_paragraph_style_name: None,
            current_text_style: None,
            current_text_style_name: None,
            list_style_stack: Vec::new(),
            list_style: None,
            has_written_start_style_tags: false,
            underlined: false,
            bold: false,
            italic: false,
            paragraph_depth: 0,
            anchor_depth: 0,
            binary_data: None,
        }
    }

    /// Whether it is inside a `binary-data` element, whose text it takes whatever the section.
    pub(super) fn reads_binary_data(&self) -> bool {
        self.binary_data.is_some()
    }

    /// The document's metadata.
    pub(super) fn metadata_mut(&mut self) -> &mut TikaMetadata {
        self.xhtml.metadata_mut()
    }

    /// `XHTMLBalancingHandler.drainOpenElements`: ends the elements still open.
    pub(super) fn drain_open_elements(&mut self) -> Result<(), Stop> {
        while let Some(name) = self.open.pop() {
            self.xhtml.end_element(name)?;
        }
        Ok(())
    }

    fn start(&mut self, name: &'static str, attributes: &[(&str, &str)]) -> Result<(), Failure> {
        self.open.push(name);
        self.xhtml.start_element_with(name, attributes)?;
        Ok(())
    }

    fn end_in(&mut self, namespace: &str, name: &'static str) -> Result<(), Failure> {
        self.open.pop();
        self.xhtml.end_element_in(namespace, name)?;
        Ok(())
    }

    /// The names of the text, paragraph and list styles defined so far.
    pub(super) fn defined_styles(&self) -> DefinedStyles {
        DefinedStyles {
            text: self.text_style_names.keys().cloned().collect(),
            paragraph: self.paragraph_text_style_names.keys().cloned().collect(),
            list: self.list_style_names.keys().cloned().collect(),
        }
    }

    /// Forgets the styles `names` holds, as the next part of an archive starts without the
    /// styles of the part before it.
    pub(super) fn forget_styles(&mut self, names: &DefinedStyles) {
        for name in &names.text {
            self.text_style_names.remove(name);
        }
        for name in &names.paragraph {
            self.paragraph_text_style_names.remove(name);
        }
        for name in &names.list {
            self.list_style_names.remove(name);
        }
    }

    /// The `needsCompleteFiltering` of dijo's substitution: elements whose whole content
    /// is left out.
    fn needs_complete_filtering(&self, namespace: &str, local_name: &str) -> bool {
        if namespace == TEXT_NS {
            if local_name == "tracked-changes" {
                return !self.include_deleted_content;
            }
            return local_name.ends_with("-template") || local_name.ends_with("-style");
        }
        namespace == TABLE_NS && local_name == "covered-table-cell"
    }

    fn new_text_style(&mut self) -> TextStyleId {
        self.text_styles.push(TextStyle::default());
        self.text_styles.len() - 1
    }

    fn start_list(&mut self, name: Option<&str>) -> Result<(), Failure> {
        let tag = match name.and_then(|name| self.list_style_names.get(name)) {
            Some(style) => ListTag::Styled(*style),
            None => ListTag::Unnamed,
        };
        let element = self.list_tag(tag);
        self.list_style_stack.push(tag);
        self.start(element, &[])?;
        Ok(())
    }

    fn list_tag(&self, tag: ListTag) -> &'static str {
        match tag {
            ListTag::Styled(style) if self.list_styles[style] => "ol",
            ListTag::Styled(_) | ListTag::Unnamed => "ul",
        }
    }

    fn end_list(&mut self) -> Result<(), Failure> {
        let element = match self.list_style_stack.pop() {
            Some(tag) => self.list_tag(tag),
            None => "ul",
        };
        self.end_in(XHTML_NAMESPACE, element)?;
        Ok(())
    }

    fn start_span(&mut self, name: Option<&str>) {
        let Some(name) = name else {
            return;
        };
        self.current_text_style = self.text_style_names.get(name).copied();
        self.has_written_start_style_tags = false;
    }

    fn start_paragraph(&mut self, style_name: Option<&str>) -> Result<(), Failure> {
        if self.paragraph_depth == 0 {
            self.start("p", &[])?;
            if let Some(name) = style_name {
                self.current_text_style = self.paragraph_text_style_names.get(name).copied();
            }
            self.has_written_start_style_tags = false;
        } else {
            self.xhtml.characters(SPACE)?;
        }
        self.paragraph_depth += 1;
        Ok(())
    }

    fn end_paragraph(&mut self) -> Result<(), Failure> {
        self.close_style_tags()?;
        if self.paragraph_depth == 1 {
            self.end_in(XHTML_NAMESPACE, "p")?;
        } else {
            self.xhtml.characters(SPACE)?;
        }
        self.paragraph_depth -= 1;
        Ok(())
    }

    fn update_style_tags(&mut self) -> Result<(), Failure> {
        if self.anchor_depth > 0 {
            return Ok(());
        }
        let Some(style) = self.current_text_style.map(|style| self.text_styles[style]) else {
            return self.close_style_tags();
        };
        if style.bold != self.bold {
            if self.underlined {
                self.end_in(XHTML_NAMESPACE, "u")?;
                self.underlined = false;
            }
            if self.italic {
                self.end_in(XHTML_NAMESPACE, "i")?;
                self.italic = false;
            }
            if style.bold {
                self.start("b", &[])?;
            } else {
                self.end_in(XHTML_NAMESPACE, "b")?;
            }
            self.bold = style.bold;
        }
        if style.italic != self.italic {
            if self.underlined {
                self.end_in(XHTML_NAMESPACE, "u")?;
                self.underlined = false;
            }
            if style.italic {
                self.start("i", &[])?;
            } else {
                self.end_in(XHTML_NAMESPACE, "i")?;
            }
            self.italic = style.italic;
        }
        if style.underlined != self.underlined {
            if style.underlined {
                self.start("u", &[])?;
            } else {
                self.end_in(XHTML_NAMESPACE, "u")?;
            }
            self.underlined = style.underlined;
        }
        Ok(())
    }

    fn close_style_tags(&mut self) -> Result<(), Failure> {
        if self.anchor_depth > 0 {
            return Ok(());
        }
        if self.underlined {
            self.end_in(XHTML_NAMESPACE, "u")?;
            self.underlined = false;
        }
        if self.italic {
            self.end_in(XHTML_NAMESPACE, "i")?;
            self.italic = false;
        }
        if self.bold {
            self.end_in(XHTML_NAMESPACE, "b")?;
            self.bold = false;
        }
        self.current_text_style = None;
        self.has_written_start_style_tags = false;
        Ok(())
    }

    /// `ElementMappingContentHandler.startElement`: the element's XHTML counterpart, if any.
    fn start_mapped(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let Some((element, mapping)) = mapping(namespace, local_name) else {
            return Ok(());
        };
        let mapped: Vec<(&str, &str)> = attributes
            .iter()
            .filter_map(|attribute| {
                mapping
                    .iter()
                    .find(|(ns, local, _)| {
                        *ns == attribute.namespace && *local == attribute.local_name
                    })
                    .map(|(_, _, name)| (*name, attribute.value.as_str()))
            })
            .collect();
        self.start(element, &mapped)?;
        Ok(())
    }

    fn end_mapped(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if let Some((element, _)) = mapping(namespace, local_name) {
            self.end_in(XHTML_NAMESPACE, element)?;
        }
        Ok(())
    }

    fn define_styles(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if namespace == STYLE_NS && local_name == "style" {
            match attribute(attributes, STYLE_NS, "family") {
                Some("text") => {
                    self.current_text_style = Some(self.new_text_style());
                    self.current_text_style_name =
                        attribute(attributes, STYLE_NS, "name").map(str::to_owned);
                }
                Some("paragraph") => {
                    self.current_text_style = Some(self.new_text_style());
                    self.current_paragraph_style_name =
                        attribute(attributes, STYLE_NS, "name").map(str::to_owned);
                }
                _ => {}
            }
        } else if namespace == TEXT_NS && local_name == "list-style" {
            self.list_styles.push(false);
            let style = self.list_styles.len() - 1;
            self.list_style = Some(style);
            if let Some(name) = attribute(attributes, STYLE_NS, "name") {
                self.list_style_names.insert(name.to_owned(), style);
            }
        } else if let (Some(style), true) = (
            self.current_text_style,
            namespace == STYLE_NS && local_name == "text-properties",
        ) {
            let font_style = attribute(attributes, FORMATTING_OBJECTS_NS, "font-style");
            if matches!(font_style, Some("italic" | "oblique")) {
                self.text_styles[style].italic = true;
            }
            let weight = attribute(attributes, FORMATTING_OBJECTS_NS, "font-weight");
            if weight.is_some_and(is_bold) {
                self.text_styles[style].bold = true;
            }
            let underline = attribute(attributes, STYLE_NS, "text-underline-style");
            if underline.is_some_and(|underline| underline != "none") {
                self.text_styles[style].underlined = true;
            }
        } else if let (Some(style), true) = (self.list_style, namespace == TEXT_NS) {
            match local_name {
                "list-level-style-bullet" => self.list_styles[style] = false,
                "list-level-style-number" => self.list_styles[style] = true,
                _ => {}
            }
        }
        Ok(())
    }

    fn end_style_definition(&mut self, namespace: &str, local_name: &str) {
        if namespace == STYLE_NS && local_name == "style" {
            let Some(style) = self.current_text_style else {
                return;
            };
            if let Some(name) = self.current_text_style_name.take() {
                self.text_style_names.insert(name, style);
                self.current_text_style = None;
            } else if let Some(name) = self.current_paragraph_style_name.take() {
                self.paragraph_text_style_names.insert(name, style);
                self.current_text_style = None;
            }
        } else if namespace == TEXT_NS && local_name == "list-style" {
            self.list_style = None;
        }
    }

    fn write_characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.completely_filtered == 0 && self.text_nodes.last() == Some(&true) {
            if !self.has_written_start_style_tags {
                self.update_style_tags()?;
                self.has_written_start_style_tags = true;
            }
            self.xhtml.characters(text)?;
        }
        Ok(())
    }
}

/// The names of the styles defined in a part, which [`Body::forget_styles`] forgets.
#[derive(Debug, Default)]
pub(super) struct DefinedStyles {
    text: Vec<String>,
    paragraph: Vec<String>,
    list: Vec<String>,
}

impl DefinedStyles {
    /// The styles of these that `before` does not hold.
    pub(super) fn since(self, before: &DefinedStyles) -> DefinedStyles {
        let new = |names: Vec<String>, old: &[String]| {
            names
                .into_iter()
                .filter(|name| !old.contains(name))
                .collect()
        };
        DefinedStyles {
            text: new(self.text, &before.text),
            paragraph: new(self.paragraph, &before.paragraph),
            list: new(self.list, &before.list),
        }
    }
}

/// Whether a `fo:font-weight` makes text bold: `bold`, `bolder` or a number above 500. A weight
/// that is empty, or starts with a digit but is no number, is not bold; Tika's check throws on
/// it, failing the whole document (#90).
fn is_bold(weight: &str) -> bool {
    matches!(weight, "bold" | "bolder")
        || java::parse_int(weight).is_some_and(|value| value > MIN_BOLD_WEIGHT)
}

/// The heading element of a `text:h`, from its outline level: `h1` where the level is missing
/// or no number, on which Tika fails the whole document (#90).
fn heading(attributes: &[Attribute]) -> &'static str {
    const HEADINGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
    let level = attribute(attributes, TEXT_NS, "outline-level")
        .and_then(java::parse_int)
        .unwrap_or(1);
    HEADINGS[usize::try_from(level.clamp(1, 6) - 1).expect("a clamped level is positive")]
}

/// How many spaces a `text:s` stands for: as many as its `text:c` counts, at most
/// [`MAX_SPACES`], and one where it counts none or is no number. Tika writes one space whatever
/// the count (#90).
fn spaces(attributes: &[Attribute]) -> usize {
    attribute(attributes, TEXT_NS, "c")
        .and_then(java::parse_int)
        .and_then(|count| usize::try_from(count).ok())
        .filter(|count| *count > 0)
        .map_or(1, |count| count.min(MAX_SPACES))
}

fn is_text_node(namespace: &str, local_name: &str) -> bool {
    if namespace == TEXT_NS && local_name != "page-number" && local_name != "page-count" {
        return true;
    }
    namespace == SVG_NS && (local_name == "title" || local_name == "desc")
}

/// Tika's `closeStylesBeforeOpen`: elements that must not open inside `b`, `i` or `u`.
fn closes_styles_before_open(namespace: &str, local_name: &str) -> bool {
    (namespace == DRAW_NS && matches!(local_name, "text-box" | "object"))
        || (namespace == TABLE_NS && matches!(local_name, "table" | "table-row" | "table-cell"))
        || (namespace == SVG_NS && matches!(local_name, "title" | "desc"))
        || (namespace == TEXT_NS && local_name == "list-item")
}

impl SaxHandler for Body<'_, '_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let namespace = normalized_namespace(namespace);
        let namespace = namespace.as_ref();
        let attributes = normalized_attributes(attributes);
        let attributes = attributes.as_ref();
        if namespace == DRAW_NS && local_name == "image" {
            self.close_style_tags()?;
            let source = attribute(attributes, XLINK_NS, "href")
                .filter(|link| !link.is_empty())
                .map(|link| format!("embedded:{link}"));
            match &source {
                Some(source) => self.start("img", &[("src", source)])?,
                None => self.start("img", &[])?,
            }
            self.end_in(XHTML_NAMESPACE, "img")?;
        }
        if local_name == BINARY_DATA_ELEMENT {
            self.binary_data = Some(BinaryData::new());
            return Ok(());
        }
        self.define_styles(namespace, local_name, attributes)?;
        self.text_nodes.push(is_text_node(namespace, local_name));
        if self.needs_complete_filtering(namespace, local_name) {
            self.completely_filtered += 1;
        }
        if self.completely_filtered != 0 {
            return Ok(());
        }
        match (namespace, local_name) {
            (TEXT_NS, "h") => {
                let element = heading(attributes);
                self.heading_stack.push(element);
                self.start(element, &[])?;
            }
            (TEXT_NS, "list") => self.start_list(attribute(attributes, TEXT_NS, "style-name"))?,
            (TEXT_NS, "span") => self.start_span(attribute(attributes, TEXT_NS, "style-name")),
            (TEXT_NS, "p") => self.start_paragraph(attribute(attributes, TEXT_NS, "style-name"))?,
            (TEXT_NS, "s") => {
                let mut spaces = spaces(attributes);
                while spaces > 0 {
                    let piece = spaces.min(SPACES.len());
                    self.xhtml.characters(&SPACES[..piece])?;
                    spaces -= piece;
                }
            }
            (_, "annotation" | "note" | "notes") => {
                self.close_style_tags()?;
                let class = match local_name {
                    "annotation" => "annotation",
                    "note" => "note",
                    _ => "notes",
                };
                self.start("p", &[("class", class)])?;
            }
            (_, "a") => {
                self.anchor_depth += 1;
                self.start_mapped(namespace, local_name, attributes)?;
            }
            _ => {
                if closes_styles_before_open(namespace, local_name) {
                    self.close_style_tags()?;
                }
                self.start_mapped(namespace, local_name, attributes)?;
            }
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if local_name == BINARY_DATA_ELEMENT {
            let data = match self.binary_data.take() {
                Some(data) => data.finish().map_err(unspooled)?,
                None => Input::Borrowed(&[]),
            };
            return self.xhtml.parse_embedded_input(
                &data,
                &TikaMetadata::new(),
                Wrapping::PackageEntry,
            );
        }
        let namespace = normalized_namespace(namespace);
        let namespace = namespace.as_ref();
        self.end_style_definition(namespace, local_name);
        if self.completely_filtered == 0 {
            match (namespace, local_name) {
                (TEXT_NS, "h") => {
                    let element = self
                        .heading_stack
                        .pop()
                        .expect("a heading ends after it started");
                    self.close_style_tags()?;
                    // Tika ends a heading in the ODF namespace, so no line break follows it.
                    self.end_in(XHTML_NAMESPACE, element)?;
                }
                (TEXT_NS, "list") => self.end_list()?,
                (TEXT_NS, "span") => {
                    self.current_text_style = None;
                    self.has_written_start_style_tags = false;
                }
                (TEXT_NS, "p") => self.end_paragraph()?,
                (_, "annotation" | "note" | "notes") => {
                    self.close_style_tags()?;
                    // Tika ends a note in the ODF namespace too.
                    self.end_in(XHTML_NAMESPACE, "p")?;
                }
                (_, "a") => {
                    self.close_style_tags()?;
                    self.end_mapped(namespace, local_name)?;
                    if self.anchor_depth > 0 {
                        self.anchor_depth -= 1;
                    }
                }
                (SVG_NS, "title" | "desc") => {
                    self.close_style_tags()?;
                    self.end_mapped(namespace, local_name)?;
                }
                _ => self.end_mapped(namespace, local_name)?,
            }
            if namespace == TEXT_NS && matches!(local_name, "tab-stop" | "tab") {
                self.write_characters(TAB)?;
            }
        }
        if self.needs_complete_filtering(namespace, local_name) {
            self.completely_filtered -= 1;
        }
        self.text_nodes.pop();
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if let Some(binary_data) = &mut self.binary_data {
            return binary_data.push(text).map_err(unspooled);
        }
        self.write_characters(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_attribute(local_name: &str, value: &str) -> Attribute {
        Attribute {
            namespace: TEXT_NS.to_owned(),
            local_name: local_name.to_owned(),
            qualified_name: format!("text:{local_name}"),
            value: value.into(),
        }
    }

    #[test]
    fn decides_boldness_headings_and_spaces_ignoring_malformed_values() {
        assert!(is_bold("bold"));
        assert!(is_bold("600"));
        assert!(!is_bold("500"));
        assert!(!is_bold("normal"));
        assert!(!is_bold("6x"));
        assert!(!is_bold(""));
        let level = |value: &str| heading(&[text_attribute("outline-level", value)]);
        assert_eq!(level("0"), "h1");
        assert_eq!(level("3"), "h3");
        assert_eq!(level("9"), "h6");
        assert_eq!(level("x"), "h1");
        assert_eq!(level(""), "h1");
        assert_eq!(heading(&[]), "h1");
        let count = |value: &str| spaces(&[text_attribute("c", value)]);
        assert_eq!(count("3"), 3);
        assert_eq!(count("0"), 1);
        assert_eq!(count("-2"), 1);
        assert_eq!(count("x"), 1);
        assert_eq!(count("999999999"), MAX_SPACES);
        assert_eq!(spaces(&[]), 1);
    }
}
