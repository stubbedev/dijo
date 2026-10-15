//! Tika's `OOXMLWordAndPowerPointTextHandler`: reads a part of a Word document or a presentation
//! element by element, by local name, and hands paragraphs, runs, tables, links, notes, fields,
//! pictures and tracked changes to the [`Body`] that writes them, noting whether a slide is hidden
//! or animated.

use std::collections::HashMap;

use super::body::{Body, ParagraphProperties};
use super::W_NAMESPACE;
use crate::formats::xhtml::Stop;
use crate::formats::xml::{attribute, attribute_named, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java::parse_int;

const MC_NAMESPACE: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
const O_NAMESPACE: &str = "urn:schemas-microsoft-com:office:office";
const V_NAMESPACE: &str = "urn:schemas-microsoft-com:vml";
const C_NAMESPACE: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
pub(super) const R_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// The formatting of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RunProperties {
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) strike: bool,
    pub(super) underline: bool,
    /// A link on the run itself, as presentations have.
    pub(super) hyperlink: Option<String>,
}

impl RunProperties {
    /// `setUnderline`: only the exact value `NONE` turns underlining off.
    fn set_underline(&mut self, value: &str) {
        self.underline = value != "NONE";
    }
}

/// The settings of a text handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextOptions {
    pub(super) include_text_boxes: bool,
    pub(super) concatenate_phonetic_runs: bool,
    pub(super) prefer_alternate_content_choice: bool,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self {
            include_text_boxes: true,
            concatenate_phonetic_runs: true,
            prefer_alternate_content_choice: true,
        }
    }
}

/// The kind of tracked change being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    None,
    Insert,
    Delete,
    MoveTo,
    MoveFrom,
}

/// `OOXMLPictureTracker`.
#[derive(Debug, Default)]
struct Picture {
    description: Option<String>,
    relationship: Option<String>,
    last_image_data: Option<String>,
}

pub(crate) struct TextHandler<'x, 'h, 'c, 'r> {
    body: Body<'x, 'h, 'c>,
    relationships: &'r HashMap<String, String>,
    options: TextOptions,
    run: RunProperties,
    paragraph: ParagraphProperties,
    run_buffer: String,
    ruby_buffer: String,
    in_run: bool,
    in_text: bool,
    in_run_properties: bool,
    in_numbering_properties: bool,
    in_ruby_text: bool,
    last_start_was_paragraph: bool,
    paragraphs_started: Vec<bool>,
    choice_depth: i32,
    fallback_depth: i32,
    in_text_box: bool,
    in_chart_value: bool,
    in_paragraph_properties: bool,
    in_field: bool,
    in_instruction: bool,
    in_field_hyperlink: bool,
    instruction: String,
    edit: Edit,
    picture: Picture,
    hidden_slide: bool,
    has_animations: bool,
}

impl<'x, 'h, 'c, 'r> TextHandler<'x, 'h, 'c, 'r> {
    pub(crate) fn new(
        body: Body<'x, 'h, 'c>,
        relationships: &'r HashMap<String, String>,
        options: TextOptions,
    ) -> Self {
        Self {
            body,
            relationships,
            options,
            run: RunProperties::default(),
            paragraph: ParagraphProperties::new(),
            run_buffer: String::new(),
            ruby_buffer: String::new(),
            in_run: false,
            in_text: false,
            in_run_properties: false,
            in_numbering_properties: false,
            in_ruby_text: false,
            last_start_was_paragraph: false,
            paragraphs_started: Vec::new(),
            choice_depth: 0,
            fallback_depth: 0,
            in_text_box: false,
            in_chart_value: false,
            in_paragraph_properties: false,
            in_field: false,
            in_instruction: false,
            in_field_hyperlink: false,
            instruction: String::new(),
            edit: Edit::None,
            picture: Picture::default(),
            hidden_slide: false,
            has_animations: false,
        }
    }

    /// Whether the part is a slide its `show` attribute hides.
    pub(crate) fn is_hidden_slide(&self) -> bool {
        self.hidden_slide
    }

    /// Whether the part has a slide's animation timing.
    pub(crate) fn has_animations(&self) -> bool {
        self.has_animations
    }

    pub(super) fn into_body(self) -> Body<'x, 'h, 'c> {
        self.body
    }

    fn in_skipped_alternate_content(&self) -> bool {
        if self.options.prefer_alternate_content_choice {
            self.fallback_depth > 0
        } else {
            self.choice_depth > 0
        }
    }

    fn is_text_box(local: &str) -> bool {
        local == "textbox" || local == "txbx"
    }

    fn mark_paragraph_started(&mut self) {
        self.paragraphs_started.pop();
        self.paragraphs_started.push(true);
    }

    fn end_run(&mut self) -> Result<(), Stop> {
        let text = std::mem::take(&mut self.run_buffer);
        self.body.run(&self.run, &text)?;
        self.in_run = false;
        self.run = RunProperties::default();
        Ok(())
    }

    fn end_ruby(&mut self) -> Result<(), Stop> {
        if self.ruby_buffer.is_empty() {
            return Ok(());
        }
        let ruby = std::mem::take(&mut self.ruby_buffer);
        if self.options.concatenate_phonetic_runs {
            self.body.run(&self.run, &format!(" ({ruby})"))?;
        }
        Ok(())
    }

    fn append(&mut self, text: &str) {
        if self.in_ruby_text {
            self.ruby_buffer.push_str(text);
        } else {
            self.run_buffer.push_str(text);
        }
    }

    fn linked(&self, id: Option<&str>) -> Option<&'r str> {
        let relationships: &'r HashMap<String, String> = self.relationships;
        id.and_then(|id| relationships.get(id)).map(String::as_str)
    }

    fn end_picture(&mut self) -> Result<(), Stop> {
        let picture = std::mem::take(&mut self.picture);
        let file_name = self.linked(picture.relationship.as_deref());
        self.body
            .embedded_picture(file_name, picture.description.as_deref())?;
        self.picture.last_image_data = picture.last_image_data;
        Ok(())
    }

    fn start_edit(&mut self, edit: Edit) {
        self.edit = edit;
    }

    fn field_character(&mut self, kind: Option<&str>) -> Result<(), Stop> {
        match kind {
            Some("begin") => {
                self.in_field = true;
                self.instruction.clear();
            }
            Some("separate") => {
                if let Some(url) = hyperlink_of_field(&self.instruction) {
                    self.body.field_hyperlink_start(url)?;
                    self.in_field_hyperlink = true;
                } else if let Some((kind, url)) = external_reference_of_field(&self.instruction) {
                    self.body.external_reference(kind, Some(url))?;
                }
            }
            Some("end") => {
                if self.in_field_hyperlink {
                    self.body.hyperlink_end()?;
                    self.in_field_hyperlink = false;
                }
                self.in_field = false;
                self.instruction.clear();
            }
            _ => {}
        }
        Ok(())
    }

    fn ole_object(&mut self, attributes: &[Attribute]) -> Result<(), Stop> {
        let mut kind = None;
        let mut relationship = None;
        for attribute in attributes {
            if attribute.local_name == "Type" {
                kind = Some(attribute.value.as_ref());
            } else if attribute.namespace == R_NAMESPACE && attribute.local_name == "id" {
                relationship = Some(attribute.value.as_ref());
            }
        }
        match kind {
            Some("Embed") => self.body.embedded_ole_reference(relationship),
            Some("Link") => self.body.linked_ole_reference(relationship),
            _ => Ok(()),
        }
    }
}

/// `getOnOff`: `0`, `false` and `off` turn a toggle off, anything else or nothing turns it on.
fn on_off(attributes: &[Attribute]) -> bool {
    !matches!(
        attribute(attributes, W_NAMESPACE, "val"),
        Some("0" | "false" | "off")
    )
}

fn int_value(attributes: &[Attribute]) -> i32 {
    attribute(attributes, W_NAMESPACE, "val")
        .and_then(parse_int)
        .unwrap_or(-1)
}

impl SaxHandler for TextHandler<'_, '_, '_, '_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if self.last_start_was_paragraph && local == "pPr" {
            self.in_paragraph_properties = true;
        } else if self.last_start_was_paragraph {
            self.body.start_paragraph(&self.paragraph)?;
            self.mark_paragraph_started();
        }
        self.last_start_was_paragraph = false;
        if namespace == MC_NAMESPACE {
            if local == "Choice" {
                self.choice_depth += 1;
            } else if local == "Fallback" {
                self.fallback_depth += 1;
            }
        }
        if self.in_skipped_alternate_content() {
            return Ok(());
        }
        if !self.options.include_text_boxes && Self::is_text_box(local) {
            self.in_text_box = true;
            return Ok(());
        }
        let w = |local| attribute(attributes, W_NAMESPACE, local);
        match local {
            "rPr" => self.in_run_properties = true,
            "r" => self.in_run = true,
            "t" => self.in_text = true,
            "tab" => self.run_buffer.push('\t'),
            "p" => {
                self.last_start_was_paragraph = true;
                self.paragraphs_started.push(false);
            }
            "b" if self.in_run && self.in_run_properties => self.run.bold = on_off(attributes),
            "b" => {}
            "tc" => self.body.start_cell()?,
            "pStyle" => self.paragraph.style_id = w("val").map(str::to_owned),
            "i" if self.in_run && self.in_run_properties => self.run.italic = on_off(attributes),
            "i" => {}
            "strike" if self.in_run && self.in_run_properties => {
                self.run.strike = on_off(attributes);
            }
            "strike" => {}
            "u" if self.in_run && self.in_run_properties => {
                self.run.set_underline(w("val").unwrap_or_default());
            }
            "u" => {}
            "tr" => self.body.start_row()?,
            "numPr" => self.in_numbering_properties = true,
            "ilvl" if self.in_numbering_properties => self.paragraph.level = int_value(attributes),
            "ilvl" => {}
            "numId" if self.in_numbering_properties => {
                self.paragraph.num_id = int_value(attributes);
            }
            "numId" => {}
            "br" | "cr" => self.run_buffer.push('\n'),
            "noBreakHyphen" => self.run_buffer.push('\u{2011}'),
            "softHyphen" => self.run_buffer.push('\u{ad}'),
            "bookmarkStart" => self.body.bookmark(w("name"))?,
            "bookmarkEnd" => {}
            "hyperlink" => match attribute(attributes, R_NAMESPACE, "id") {
                Some(id) => {
                    let link = self.linked(Some(id));
                    self.body.hyperlink_start(link)?;
                }
                None => {
                    let anchor = w("anchor").map(|anchor| format!("#{anchor}"));
                    self.body.hyperlink_start(anchor.as_deref())?;
                }
            },
            "hlinkClick" => {
                if let Some(id) = attribute(attributes, R_NAMESPACE, "id") {
                    let link = self.linked(Some(id));
                    if self.in_run {
                        self.run.hyperlink = link.map(str::to_owned);
                    } else if link.is_some() {
                        self.body.external_reference("hlinkClick", link)?;
                    }
                }
            }
            "tbl" => self.body.start_table()?,
            "blip" => {
                self.picture.relationship =
                    attribute(attributes, R_NAMESPACE, "embed").map(str::to_owned);
            }
            "cNvPr" => {
                self.picture.description = attribute(attributes, "", "descr").map(str::to_owned);
            }
            "pic" => {}
            "footnoteReference" => self.body.footnote_reference(w("id"))?,
            "imagedata" => {
                let id = attribute(attributes, R_NAMESPACE, "id").map(str::to_owned);
                self.picture.relationship.clone_from(&id);
                self.picture.last_image_data = id;
                self.picture.description =
                    attribute(attributes, O_NAMESPACE, "title").map(str::to_owned);
            }
            "ins" => self.start_edit(Edit::Insert),
            "delText" => {}
            "del" => self.start_edit(Edit::Delete),
            "moveTo" => self.start_edit(Edit::MoveTo),
            "moveFrom" => self.start_edit(Edit::MoveFrom),
            "OLEObject" => self.ole_object(attributes)?,
            "endnoteReference" => self.body.endnote_reference(w("id"))?,
            "commentReference" => self.body.comment_reference(w("id")),
            "v" if namespace == C_NAMESPACE => self.in_chart_value = true,
            "rt" => self.in_ruby_text = true,
            "sld" => {
                if matches!(attribute_named(attributes, "show"), Some("0" | "false")) {
                    self.hidden_slide = true;
                }
            }
            "timing" => self.has_animations = true,
            "fldChar" => self.field_character(w("fldCharType"))?,
            "instrText" => self.in_instruction = true,
            "hlinkHover" => {
                if let Some(id) = attribute(attributes, R_NAMESPACE, "id") {
                    let link = self.linked(Some(id));
                    if link.is_some() {
                        self.body.external_reference("hlinkHover", link)?;
                    }
                }
            }
            "shape" if namespace == V_NAMESPACE => {
                let href = attribute_named(attributes, "href")
                    .or_else(|| attribute(attributes, O_NAMESPACE, "href"));
                if href.is_some_and(|href| !href.is_empty()) {
                    self.body.external_reference("vml-shape-href", href)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local: &str) -> Result<(), Failure> {
        if local == "Choice" {
            self.choice_depth -= 1;
        } else if local == "Fallback" {
            self.fallback_depth -= 1;
        }
        if self.in_skipped_alternate_content() {
            return Ok(());
        }
        if !self.options.include_text_boxes && Self::is_text_box(local) {
            self.in_text_box = false;
            return Ok(());
        }
        match local {
            "pic" => self.end_picture()?,
            "rPr" => self.in_run_properties = false,
            "r" | "fld" => self.end_run()?,
            "t" => self.in_text = false,
            "pPr" if self.in_paragraph_properties => {
                if !self.paragraphs_started.last().copied().unwrap_or(false) {
                    self.body.start_paragraph(&self.paragraph)?;
                    self.mark_paragraph_started();
                }
                self.paragraph = ParagraphProperties::new();
                self.in_paragraph_properties = false;
            }
            "p" => {
                if !self.run_buffer.is_empty() {
                    let text = std::mem::take(&mut self.run_buffer);
                    self.body.run(&self.run, &text)?;
                }
                if self.paragraphs_started.pop().unwrap_or(false) {
                    self.body.end_paragraph()?;
                }
                self.last_start_was_paragraph = false;
            }
            "tc" => self.body.end_cell()?,
            "tr" => self.body.end_row()?,
            "tbl" => self.body.end_table()?,
            "ins" | "del" | "moveTo" | "moveFrom" => self.edit = Edit::None,
            "hyperlink" => self.body.hyperlink_end()?,
            "pict" => self.end_picture()?,
            "v" if namespace == C_NAMESPACE => {
                self.in_chart_value = false;
                self.end_run()?;
            }
            "rt" => self.in_ruby_text = false,
            "ruby" => self.end_ruby()?,
            "instrText" => self.in_instruction = false,
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_skipped_alternate_content()
            || (!self.options.include_text_boxes && self.in_text_box)
        {
            return Ok(());
        }
        if self.edit == Edit::MoveFrom && self.in_text {
            if self.body.include_move_from() {
                self.append(text);
            }
        } else if self.in_text || (self.body.include_deleted() && self.edit == Edit::Delete) {
            self.append(text);
        } else if self.in_chart_value {
            self.append(text);
            self.append("\t");
        } else if self.in_instruction && self.in_field {
            self.instruction.push_str(text);
        }
        Ok(())
    }
}

/// `FieldCodeParser.parseHyperlinkFromInstrText`.
fn hyperlink_of_field(instruction: &str) -> Option<&str> {
    find_quoted_field(java_trim(instruction), "HYPERLINK", false)
}

/// `FieldCodeParser.parseExternalRefFromInstrText`: the field and its URL.
fn external_reference_of_field(instruction: &str) -> Option<(&'static str, &str)> {
    let trimmed = java_trim(instruction);
    ["INCLUDEPICTURE", "INCLUDETEXT", "IMPORT"]
        .into_iter()
        .find_map(|field| find_quoted_field(trimmed, field, false).map(|url| (field, url)))
        .or_else(|| find_quoted_field(trimmed, "LINK", true).map(|url| ("LINK", url)))
}

fn java_trim(text: &str) -> &str {
    crate::java::trim(text)
}

/// Java's `\s`.
fn is_regex_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// The first match of `NAME\s{1,100}"([^"]{1,10000})"`, ignoring ASCII case, with
/// `[\w.]{1,50}\s{1,100}` after the spaces when `with_class`, as `LINK` fields name a class.
fn find_quoted_field<'t>(text: &'t str, name: &str, with_class: bool) -> Option<&'t str> {
    let bytes = text.as_bytes();
    (0..bytes.len()).find_map(|start| {
        let after_name = start + name.len();
        if !bytes
            .get(start..after_name)?
            .eq_ignore_ascii_case(name.as_bytes())
        {
            return None;
        }
        let mut position = run_of(bytes, after_name, 1, 100, is_regex_space)?;
        if with_class {
            position = run_of(bytes, position, 1, 50, |byte| {
                byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
            })?;
            position = run_of(bytes, position, 1, 100, is_regex_space)?;
        }
        if bytes.get(position) != Some(&b'"') {
            return None;
        }
        let content_start = position + 1;
        let content_end = content_start + bytes[content_start..].iter().position(|&b| b == b'"')?;
        let length = text[content_start..content_end].chars().count();
        (1..=10_000)
            .contains(&length)
            .then(|| &text[content_start..content_end])
    })
}

/// The end of a run of `minimum..=maximum` bytes matching `matches` at `start`, which must not
/// go on past `maximum`.
fn run_of(
    bytes: &[u8],
    start: usize,
    minimum: usize,
    maximum: usize,
    matches: impl Fn(u8) -> bool,
) -> Option<usize> {
    let length = bytes[start.min(bytes.len())..]
        .iter()
        .take_while(|&&byte| matches(byte))
        .count();
    (minimum..=maximum)
        .contains(&length)
        .then_some(start + length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_field_codes_as_tika_does() {
        assert_eq!(
            hyperlink_of_field(" HYPERLINK \"http://x.org/\" \\o tip"),
            Some("http://x.org/")
        );
        assert_eq!(hyperlink_of_field("hyperlink\t\"a\""), Some("a"));
        assert_eq!(hyperlink_of_field("HYPERLINK \\l \"a\""), None);
        assert_eq!(hyperlink_of_field("HYPERLINK \"\""), None);
        assert_eq!(
            external_reference_of_field("INCLUDEPICTURE \"http://x/y.png\" \\d"),
            Some(("INCLUDEPICTURE", "http://x/y.png"))
        );
        assert_eq!(
            external_reference_of_field("LINK Excel.Sheet.12 \"C:\\a.xlsx\" \"S!R1\""),
            Some(("LINK", "C:\\a.xlsx"))
        );
        assert_eq!(external_reference_of_field("PAGE"), None);
    }
}
