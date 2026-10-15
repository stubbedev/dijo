//! How Tika's `OOXMLTikaBodyPartHandler` writes what the text handler finds as XHTML:
//! paragraphs as their style's element, tables, list numbers, links and bookmarks, with
//! `FormattingTagManager` keeping bold, italic, strike-through, underline and links nested.

use super::numbering::ListManager;
use super::styles::{paragraph_tag_and_style, Styles};
use super::text::RunProperties;
use crate::formats::xhtml::{Stop, Xhtml};

/// The flags `OOXMLTikaBodyPartHandler` sets on the metadata.
pub(super) const HAS_LINKED_OLE_OBJECTS: &str = "msoffice:has-linked-ole-objects";
pub(super) const HAS_FIELD_HYPERLINKS: &str = "msoffice:has-field-hyperlinks";
const HAS_HOVER_HYPERLINKS: &str = "msoffice:has-hover-hyperlinks";
const HAS_VML_HYPERLINKS: &str = "msoffice:has-vml-hyperlinks";

/// The paragraph information the text handler gathers before a paragraph starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ParagraphProperties {
    pub(super) style_id: Option<String>,
    pub(super) level: i32,
    pub(super) num_id: i32,
}

impl ParagraphProperties {
    pub(super) fn new() -> Self {
        Self {
            style_id: None,
            level: -1,
            num_id: -1,
        }
    }
}

/// What a body handler writes besides the text itself.
pub(super) struct Context<'c> {
    pub(super) styles: Option<&'c Styles>,
    pub(super) lists: &'c mut ListManager,
    /// Whether the handler records features of the document in its metadata.
    pub(super) records_metadata: bool,
}

/// `OOXMLTikaBodyPartHandler`.
pub(super) struct Body<'x, 'h, 'c> {
    xhtml: &'x mut Xhtml<'h>,
    context: Context<'c>,
    formatting: Formatting,
    paragraph_depth: i32,
    table_depth: i32,
    cell_depth: i32,
    paragraphs_in_cell: i32,
    paragraph_tag: String,
}

impl<'x, 'h, 'c> Body<'x, 'h, 'c> {
    pub(super) fn new(xhtml: &'x mut Xhtml<'h>, context: Context<'c>) -> Self {
        Self {
            xhtml,
            context,
            formatting: Formatting::default(),
            paragraph_depth: 0,
            table_depth: 0,
            cell_depth: 0,
            paragraphs_in_cell: 0,
            paragraph_tag: "p".to_owned(),
        }
    }

    pub(super) fn run(&mut self, properties: &RunProperties, text: &str) -> Result<(), Stop> {
        self.formatting.apply(self.xhtml, properties)?;
        self.xhtml.characters(text)
    }

    pub(super) fn hyperlink_start(&mut self, link: Option<&str>) -> Result<(), Stop> {
        self.formatting.open_hyperlink(self.xhtml, link)
    }

    pub(super) fn hyperlink_end(&mut self) -> Result<(), Stop> {
        self.formatting.close_hyperlink(self.xhtml)
    }

    pub(super) fn start_paragraph(&mut self, properties: &ParagraphProperties) -> Result<(), Stop> {
        if self.cell_depth > 0 && self.paragraphs_in_cell > 0 {
            self.xhtml.characters("\n")?;
        }
        if self.paragraph_depth == 0 && self.table_depth == 0 {
            let mut tag = "p".to_owned();
            let mut class = None;
            if let (Some(id), Some(styles)) = (&properties.style_id, self.context.styles) {
                if let Some(name) = styles.name(id) {
                    let tag_and_style = paragraph_tag_and_style(name);
                    tag = tag_and_style.tag;
                    class = tag_and_style.class;
                }
            }
            match &class {
                Some(class) => self.xhtml.start_element_with(&tag, &[("class", class)])?,
                None => self.xhtml.start_element(&tag)?,
            }
            self.paragraph_tag = tag;
        }
        if properties.level >= 0 && properties.num_id >= 0 {
            let number = self
                .context
                .lists
                .formatted_number(properties.num_id, properties.level);
            self.xhtml.characters(&number)?;
        }
        self.paragraph_depth += 1;
        Ok(())
    }

    pub(super) fn end_paragraph(&mut self) -> Result<(), Stop> {
        self.formatting.close_all(self.xhtml)?;
        if self.paragraph_depth == 1 && self.table_depth == 0 {
            let tag = self.paragraph_tag.clone();
            self.xhtml.end_element(&tag)?;
        } else if self.cell_depth == 0 || self.paragraphs_in_cell > 0 {
            self.xhtml.characters("\n")?;
        }
        if self.cell_depth > 0 {
            self.paragraphs_in_cell += 1;
        }
        self.paragraph_depth -= 1;
        Ok(())
    }

    pub(super) fn start_table(&mut self) -> Result<(), Stop> {
        self.formatting.close_all(self.xhtml)?;
        self.xhtml.start_element("table")?;
        self.table_depth += 1;
        Ok(())
    }

    pub(super) fn end_table(&mut self) -> Result<(), Stop> {
        self.xhtml.end_element("table")?;
        self.table_depth -= 1;
        Ok(())
    }

    pub(super) fn start_row(&mut self) -> Result<(), Stop> {
        self.xhtml.start_element("tr")
    }

    pub(super) fn end_row(&mut self) -> Result<(), Stop> {
        self.xhtml.end_element("tr")
    }

    pub(super) fn start_cell(&mut self) -> Result<(), Stop> {
        self.xhtml.start_element("td")?;
        self.cell_depth += 1;
        Ok(())
    }

    pub(super) fn end_cell(&mut self) -> Result<(), Stop> {
        self.xhtml.end_element("td")?;
        self.paragraphs_in_cell = 0;
        self.cell_depth -= 1;
        Ok(())
    }

    pub(super) fn embedded_ole_reference(
        &mut self,
        relationship: Option<&str>,
    ) -> Result<(), Stop> {
        let Some(relationship) = relationship else {
            return Ok(());
        };
        self.xhtml
            .start_element_with("div", &[("class", "embedded"), ("id", relationship)])?;
        self.xhtml.end_element("div")
    }

    pub(super) fn linked_ole_reference(&mut self, relationship: Option<&str>) -> Result<(), Stop> {
        let Some(relationship) = relationship else {
            return Ok(());
        };
        if self.context.records_metadata {
            self.xhtml
                .metadata_mut()
                .set(HAS_LINKED_OLE_OBJECTS, "true");
        }
        self.xhtml.start_element_with(
            "a",
            &[("class", "external-ref-linkedOle"), ("id", relationship)],
        )?;
        self.xhtml.end_element("a")
    }

    pub(super) fn embedded_picture(
        &mut self,
        file_name: Option<&str>,
        description: Option<&str>,
    ) -> Result<(), Stop> {
        let source = file_name.map(|name| format!("embedded:{name}"));
        let mut attributes: Vec<(&str, &str)> = Vec::with_capacity(2);
        if let Some(source) = &source {
            attributes.push(("src", source));
        }
        if let Some(description) = description {
            attributes.push(("alt", description));
        }
        self.xhtml.start_element_with("img", &attributes)?;
        self.xhtml.end_element("img")
    }

    pub(super) fn field_hyperlink_start(&mut self, link: &str) -> Result<(), Stop> {
        if self.context.records_metadata {
            self.xhtml.metadata_mut().set(HAS_FIELD_HYPERLINKS, "true");
        }
        self.hyperlink_start(Some(link))
    }

    pub(super) fn external_reference(&mut self, kind: &str, url: Option<&str>) -> Result<(), Stop> {
        let Some(url) = url.filter(|url| !url.is_empty()) else {
            return Ok(());
        };
        if self.context.records_metadata {
            let flag = match kind {
                "hlinkHover" => HAS_HOVER_HYPERLINKS,
                "vml-shape-href" => HAS_VML_HYPERLINKS,
                _ => HAS_FIELD_HYPERLINKS,
            };
            self.xhtml.metadata_mut().set(flag, "true");
        }
        let class = format!("external-ref-{kind}");
        self.xhtml
            .start_element_with("a", &[("class", &class), ("href", url)])?;
        self.xhtml.end_element("a")
    }

    pub(super) fn bookmark(&mut self, name: Option<&str>) -> Result<(), Stop> {
        if let Some(name) = name {
            if !self.formatting.hyperlink_active() {
                self.xhtml.start_element_with("a", &[("name", name)])?;
                self.xhtml.end_element("a")?;
            }
        }
        Ok(())
    }
}

/// `FormattingTagManager`: the open link and formatting, nested link, bold, italic,
/// strike-through, underline from the outside in.
#[derive(Debug, Default)]
struct Formatting {
    hyperlink: Option<String>,
    wrapper_hyperlink: bool,
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
}

impl Formatting {
    fn open_hyperlink(&mut self, xhtml: &mut Xhtml, url: Option<&str>) -> Result<(), Stop> {
        let Some(url) = url else {
            return Ok(());
        };
        self.close_formatting(xhtml)?;
        if self.hyperlink.is_some() {
            xhtml.end_element("a")?;
        }
        xhtml.start_element_with("a", &[("href", url)])?;
        self.hyperlink = Some(url.to_owned());
        self.wrapper_hyperlink = true;
        Ok(())
    }

    fn close_hyperlink(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if self.hyperlink.is_some() && self.wrapper_hyperlink {
            self.close_formatting(xhtml)?;
            xhtml.end_element("a")?;
            self.hyperlink = None;
            self.wrapper_hyperlink = false;
        }
        Ok(())
    }

    fn hyperlink_active(&self) -> bool {
        self.hyperlink.is_some()
    }

    fn apply(&mut self, xhtml: &mut Xhtml, run: &RunProperties) -> Result<(), Stop> {
        if !self.wrapper_hyperlink && run.hyperlink != self.hyperlink {
            self.close_formatting(xhtml)?;
            if self.hyperlink.is_some() {
                xhtml.end_element("a")?;
            }
            if let Some(url) = &run.hyperlink {
                xhtml.start_element_with("a", &[("href", url)])?;
            }
            self.hyperlink.clone_from(&run.hyperlink);
        }
        if run.bold != self.bold {
            self.close_underline(xhtml)?;
            self.close_strike(xhtml)?;
            self.close_italic(xhtml)?;
            toggle(xhtml, "b", run.bold)?;
            self.bold = run.bold;
        }
        if run.italic != self.italic {
            self.close_underline(xhtml)?;
            self.close_strike(xhtml)?;
            toggle(xhtml, "i", run.italic)?;
            self.italic = run.italic;
        }
        if run.strike != self.strike {
            self.close_underline(xhtml)?;
            toggle(xhtml, "s", run.strike)?;
            self.strike = run.strike;
        }
        if run.underline != self.underline {
            toggle(xhtml, "u", run.underline)?;
            self.underline = run.underline;
        }
        Ok(())
    }

    fn close_all(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        self.close_formatting(xhtml)?;
        if self.hyperlink.is_some() {
            xhtml.end_element("a")?;
            self.hyperlink = None;
            self.wrapper_hyperlink = false;
        }
        Ok(())
    }

    fn close_formatting(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        self.close_underline(xhtml)?;
        self.close_strike(xhtml)?;
        self.close_italic(xhtml)?;
        if self.bold {
            xhtml.end_element("b")?;
            self.bold = false;
        }
        Ok(())
    }

    fn close_underline(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if self.underline {
            xhtml.end_element("u")?;
            self.underline = false;
        }
        Ok(())
    }

    fn close_strike(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if self.strike {
            xhtml.end_element("s")?;
            self.strike = false;
        }
        Ok(())
    }

    fn close_italic(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if self.italic {
            xhtml.end_element("i")?;
            self.italic = false;
        }
        Ok(())
    }
}

fn toggle(xhtml: &mut Xhtml, element: &str, on: bool) -> Result<(), Stop> {
    if on {
        xhtml.start_element(element)
    } else {
        xhtml.end_element(element)
    }
}
