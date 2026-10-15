//! Tika's `OpenDocumentBodyHandler` behind its `NSNormalizerContentHandler`: the text of
//! `content.xml` and `styles.xml` as XHTML. Paragraphs, headings, lists, tables, links, notes,
//! annotations, text boxes and images become their XHTML counterparts, bold, italic and
//! underlined text styles become `b`, `i` and `u`, and templates, style definitions, covered
//! table cells and, unless deleted content is asked for, tracked changes are left out.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::formats::metadata::TikaMetadata;
use crate::formats::odf::{normalized_namespace, BINARY_DATA_ELEMENT, OFFICE_NS};
use crate::formats::xhtml::{Stop, Xhtml, XHTML_NAMESPACE};
use crate::formats::xml::{attribute, Attribute, SaxHandler};
use crate::formats::Failure;

const TEXT_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";
const TABLE_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:table:1.0";
const STYLE_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:style:1.0";
const FORMATTING_OBJECTS_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0";
const SVG_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0";
const PRESENTATION_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:presentation:1.0";
const DRAW_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:drawing:1.0";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const SPACE: &str = " ";
const TAB: &str = "\t";
const MIN_BOLD_WEIGHT: i32 = 500;

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

/// Which namespaces the handler sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Namespaces {
    /// Those of OpenOffice.org 1 elements mapped to OpenDocument's, as behind Tika's
    /// `NSNormalizerContentHandler` in the parts of an archive.
    Normalized,
    /// As written, as in a flat document, where Tika normalizes only what the handler writes.
    AsWritten,
}

/// The state of Tika's `OpenDocumentBodyHandler` over one XML part, which also keeps the
/// elements it has open, as the `XHTMLBalancingHandler` behind it in a flat document does.
pub(super) struct Body<'a, 'x> {
    xhtml: &'a mut Xhtml<'x>,
    include_deleted_content: bool,
    namespaces: Namespaces,
    open: Vec<&'static str>,
    unchecked_failure: bool,
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
}

impl<'a, 'x> Body<'a, 'x> {
    pub(super) fn new(
        xhtml: &'a mut Xhtml<'x>,
        include_deleted_content: bool,
        namespaces: Namespaces,
    ) -> Self {
        Self {
            xhtml,
            include_deleted_content,
            namespaces,
            open: Vec::new(),
            unchecked_failure: false,
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
        }
    }

    /// The document's metadata.
    pub(super) fn metadata_mut(&mut self) -> &mut TikaMetadata {
        self.xhtml.metadata_mut()
    }

    /// Whether the handler failed as with one of Java's unchecked exceptions, on a number that
    /// does not parse, rather than with a `SAXException`.
    pub(super) fn failed_unchecked(&self) -> bool {
        self.unchecked_failure
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

    fn namespace<'n>(&self, namespace: &'n str) -> Cow<'n, str> {
        match self.namespaces {
            Namespaces::Normalized => normalized_namespace(namespace),
            Namespaces::AsWritten => Cow::Borrowed(namespace),
        }
    }

    /// Notes when `result` failed as Java's unchecked exceptions fail.
    fn unchecked<T>(&mut self, result: Result<T, Failure>) -> Result<T, Failure> {
        self.unchecked_failure |= result.is_err();
        result
    }

    /// The `needsCompleteFiltering` of extractous's substitution: elements whose whole content
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
        attributes: &[Attribute<'_>],
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
                    .map(|(_, _, name)| (*name, attribute.value.as_ref()))
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
        attributes: &[Attribute<'_>],
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
            if let Some(weight) = attribute(attributes, FORMATTING_OBJECTS_NS, "font-weight") {
                let bold = is_bold(weight);
                if self.unchecked(bold)? {
                    self.text_styles[style].bold = true;
                }
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

/// Whether a `fo:font-weight` makes text bold, failing where Tika's check throws: on an empty
/// weight, and on one that starts with a digit but is no `int`.
fn is_bold(weight: &str) -> Result<bool, Failure> {
    if matches!(weight, "bold" | "bolder") {
        return Ok(true);
    }
    let Some(first) = weight.chars().next() else {
        return Err(Failure::Malformed("A font weight is empty".to_owned()));
    };
    if !is_java_digit(first) {
        return Ok(false);
    }
    let value = parse_java_int(weight)
        .ok_or_else(|| Failure::Malformed(format!("The font weight {weight:?} is not a number")))?;
    Ok(value > MIN_BOLD_WEIGHT)
}

/// The heading element of a `text:h`, from its outline level.
fn heading(attributes: &[Attribute<'_>]) -> Result<&'static str, Failure> {
    const HEADINGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
    let Some(level) = attribute(attributes, TEXT_NS, "outline-level") else {
        return Ok(HEADINGS[0]);
    };
    let level = parse_java_int(level).ok_or_else(|| {
        Failure::Malformed(format!("The outline level {level:?} is not a number"))
    })?;
    Ok(HEADINGS[usize::try_from(level.clamp(1, 6) - 1).expect("a clamped level is positive")])
}

/// Java's `Character.isDigit` on a `char`: a decimal digit of the Basic Multilingual Plane.
fn is_java_digit(character: char) -> bool {
    java_digit(character).is_some()
}

/// The value of a decimal digit as Java's `Character.digit(char, 10)` reads it: a character of
/// the Basic Multilingual Plane in the category Nd, whose digits come in runs from 0 to 9.
fn java_digit(character: char) -> Option<u32> {
    use unicode_general_category::{get_general_category, GeneralCategory};
    let is_digit = |character: char| {
        character < '\u{10000}' && get_general_category(character) == GeneralCategory::DecimalNumber
    };
    if !is_digit(character) {
        return None;
    }
    let mut zero = u32::from(character);
    while zero > 0 && char::from_u32(zero - 1).is_some_and(is_digit) {
        zero -= 1;
    }
    Some((u32::from(character) - zero) % 10)
}

/// Java's `Integer.parseInt(text)`: an optional sign and decimal digits that fit an `int`.
fn parse_java_int(text: &str) -> Option<i32> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() {
        return None;
    }
    let mut value: i64 = 0;
    for character in digits.chars() {
        value = value * 10 + i64::from(java_digit(character)?);
        if value > i64::from(i32::MAX) + 1 {
            return None;
        }
    }
    i32::try_from(if negative { -value } else { value }).ok()
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
        attributes: &[Attribute<'_>],
    ) -> Result<(), Failure> {
        let namespace = self.namespace(namespace);
        let namespace = namespace.as_ref();
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
            return Err(Failure::Malformed(
                "The document embeds binary data, which only Tika's path extracts".to_owned(),
            ));
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
                let element = self.unchecked(element)?;
                self.heading_stack.push(element);
                self.start(element, &[])?;
            }
            (TEXT_NS, "list") => self.start_list(attribute(attributes, TEXT_NS, "style-name"))?,
            (TEXT_NS, "span") => self.start_span(attribute(attributes, TEXT_NS, "style-name")),
            (TEXT_NS, "p") => self.start_paragraph(attribute(attributes, TEXT_NS, "style-name"))?,
            (TEXT_NS, "s") => self.xhtml.characters(SPACE)?,
            (_, "annotation" | "note" | "notes") => {
                self.close_style_tags()?;
                self.xhtml
                    .start_element_with("p", &[("class", local_name)])?;
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
        let namespace = self.namespace(namespace);
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
                    self.end_in(namespace, element)?;
                }
                (TEXT_NS, "list") => self.end_list()?,
                (TEXT_NS, "span") => {
                    self.current_text_style = None;
                    self.has_written_start_style_tags = false;
                }
                (TEXT_NS, "p") => self.end_paragraph()?,
                (_, "annotation" | "note" | "notes") => {
                    self.close_style_tags()?;
                    self.end_in(namespace, "p")?;
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
        self.write_characters(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ints_as_java_does() {
        assert_eq!(parse_java_int("700"), Some(700));
        assert_eq!(parse_java_int("+7"), Some(7));
        assert_eq!(parse_java_int("-2147483648"), Some(i32::MIN));
        assert_eq!(parse_java_int("2147483648"), None);
        assert_eq!(parse_java_int("٧٠٠"), Some(700));
        assert_eq!(parse_java_int("7.5"), None);
        assert_eq!(parse_java_int(""), None);
        assert_eq!(parse_java_int("-"), None);
    }

    #[test]
    fn decides_boldness_and_headings_as_tika_does() {
        assert!(is_bold("bold").unwrap());
        assert!(is_bold("600").unwrap());
        assert!(!is_bold("500").unwrap());
        assert!(!is_bold("normal").unwrap());
        assert!(is_bold("6x").is_err());
        assert!(is_bold("").is_err());
        let level = |value: &'static str| {
            heading(&[Attribute {
                namespace: TEXT_NS,
                local_name: "outline-level",
                qualified_name: "text:outline-level",
                value: value.into(),
            }])
        };
        assert_eq!(level("0").unwrap(), "h1");
        assert_eq!(level("3").unwrap(), "h3");
        assert_eq!(level("9").unwrap(), "h6");
        assert!(level("x").is_err());
        assert_eq!(heading(&[]).unwrap(), "h1");
    }
}
