//! The SAX handlers `XSSFExcelExtractorDecorator` and POI's `XSSFReader` read the workbook's
//! other parts with: the sheets the workbook lists, its hidden sheets and code name, the people
//! of threaded comments, the threaded comments and the text of drawn shapes, and the links to
//! outside data of external links, connections and pivot caches.

use std::collections::HashMap;

use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::extractor::{external_reference, name};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{attribute, attribute_named, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java::is_blank;
use crate::java::{equals_ignore_case, trim};

/// The namespace of relationship ids.
const RELATIONSHIPS_NS: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

mod meta {
    pub(super) const ORIGINAL_RESOURCE_NAME: &str = "tk:orig-resource-name";
    pub(super) const HAS_HIDDEN_SHEETS: &str = "msoffice:excel:has-hidden-sheets";
    pub(super) const HIDDEN_SHEET_NAMES: &str = "msoffice:excel:hidden-sheet-names";
    pub(super) const HAS_VERY_HIDDEN_SHEETS: &str = "msoffice:excel:has-very-hidden-sheets";
    pub(super) const VERY_HIDDEN_SHEET_NAMES: &str = "msoffice:excel:very-hidden-sheet-names";
    pub(super) const WORKBOOK_CODENAME: &str = "msoffice:excel:workbook-codename";
}

/// The value of the first attribute whose local name is `local_name`, as
/// `XMLReaderUtils.getAttrValue` finds it.
fn local_attribute<'a>(attributes: &'a [Attribute], local_name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.local_name == local_name)
        .map(|attribute| attribute.value.as_str())
}

macro_rules! ignore_rest {
    () => {
        fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
            Ok(())
        }

        fn characters(&mut self, _text: &str) -> Result<(), Failure> {
            Ok(())
        }
    };
}

/// A sheet the workbook lists: its name and relationship id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SheetRef {
    pub(crate) name: String,
    pub(crate) id: String,
}

/// `XSSFReader.XMLSheetRefReader`: each `sheet` element with a name and an id, whichever
/// namespace and case they are in, the sheets with an empty id dropped.
#[derive(Debug, Default)]
pub(crate) struct SheetRefs {
    pub(crate) sheets: Vec<SheetRef>,
}

impl SaxHandler for SheetRefs {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if !equals_ignore_case(local_name, "sheet") {
            return Ok(());
        }
        let mut name = None;
        let mut id = None;
        for attribute in attributes {
            if equals_ignore_case(&attribute.local_name, "name") {
                name = Some(attribute.value.clone());
            } else if equals_ignore_case(&attribute.local_name, "id") {
                id = Some(attribute.value.clone());
            }
            if let (Some(name), Some(id)) = (&name, &id) {
                if !id.is_empty() {
                    self.sheets.push(SheetRef {
                        name: name.clone(),
                        id: id.clone(),
                    });
                }
                break;
            }
        }
        Ok(())
    }

    ignore_rest!();
}

/// A sheet `WorkbookMetadataHandler` notes as hidden, by its name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hidden {
    Hidden(Option<String>),
    VeryHidden(Option<String>),
}

/// `WorkbookMetadataHandler`: the workbook's original path, hidden sheets and code name.
#[derive(Debug, Default)]
pub(crate) struct WorkbookMetadata {
    path: Option<String>,
    hidden: Vec<Hidden>,
    code_name: Option<String>,
}

impl WorkbookMetadata {
    /// Whether a hidden sheet has no name, which Tika adds as a null value.
    pub(crate) fn hides_an_unnamed_sheet(&self) -> bool {
        self.hidden.iter().any(|hidden| match hidden {
            Hidden::Hidden(name) | Hidden::VeryHidden(name) => name.is_none(),
        })
    }

    /// Records what was found, as the handler records it while it reads.
    pub(crate) fn write(&self, metadata: &mut TikaMetadata) {
        if let Some(path) = &self.path {
            metadata.set(meta::ORIGINAL_RESOURCE_NAME, path.clone());
        }
        for hidden in &self.hidden {
            match hidden {
                Hidden::Hidden(Some(name)) => {
                    metadata.set(meta::HAS_HIDDEN_SHEETS, "true");
                    metadata.add(meta::HIDDEN_SHEET_NAMES, name.clone());
                }
                Hidden::VeryHidden(Some(name)) => {
                    metadata.set(meta::HAS_VERY_HIDDEN_SHEETS, "true");
                    metadata.set(meta::VERY_HIDDEN_SHEET_NAMES, name.clone());
                }
                Hidden::Hidden(None) | Hidden::VeryHidden(None) => {}
            }
        }
        if let Some(code_name) = &self.code_name {
            metadata.set(meta::WORKBOOK_CODENAME, code_name.clone());
        }
    }
}

impl SaxHandler for WorkbookMetadata {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match local_name {
            "absPath" => {
                if let Some(url) = local_attribute(attributes, "url") {
                    self.path = Some(url.to_owned());
                }
            }
            "sheet" => {
                let name = local_attribute(attributes, "name").map(str::to_owned);
                match local_attribute(attributes, "state") {
                    Some("hidden") => self.hidden.push(Hidden::Hidden(name)),
                    Some("veryHidden") => self.hidden.push(Hidden::VeryHidden(name)),
                    _ => {}
                }
            }
            "workbookPr" => {
                if let Some(code_name) = local_attribute(attributes, "codeName") {
                    if !is_blank(code_name) {
                        self.code_name = Some(code_name.to_owned());
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    ignore_rest!();
}

/// `CommentPersonHandler`: the display names of the people of threaded comments.
#[derive(Debug, Default)]
pub(crate) struct Persons {
    pub(crate) names: Vec<String>,
}

impl Persons {
    pub(crate) fn write(&self, metadata: &mut TikaMetadata) {
        for person in &self.names {
            metadata.add(name::COMMENT_PERSONS, person.clone());
        }
    }
}

impl SaxHandler for Persons {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name == "person" {
            if let Some(display_name) = local_attribute(attributes, "displayName") {
                if !is_blank(display_name) {
                    self.names.push(display_name.to_owned());
                }
            }
        }
        Ok(())
    }

    ignore_rest!();
}

/// `ThreadedCommentHandler`: each comment's text in a `threaded-comment` div. Once a `text`
/// element has started, all character data is kept, as the handler never stops keeping it.
pub(crate) struct ThreadedComments<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    text: String,
    in_text: bool,
}

impl<'x, 'h> ThreadedComments<'x, 'h> {
    pub(crate) fn new(xhtml: &'x mut Xhtml<'h>) -> Self {
        Self {
            xhtml,
            text: String::new(),
            in_text: false,
        }
    }
}

impl SaxHandler for ThreadedComments<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name == "text" {
            self.in_text = true;
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if local_name == "text" {
            self.xhtml
                .start_element_with("div", &[("class", "threaded-comment")])?;
            self.xhtml.start_element("p")?;
            self.xhtml.characters(&self.text)?;
            self.xhtml.end_element("p")?;
            self.xhtml.end_element("div")?;
            self.text.clear();
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_text {
            self.text.push_str(text);
        }
        Ok(())
    }
}

/// `DrawingShapeHandler`: the text of each shape's text body as a paragraph, and the links of
/// shapes whose relationship the drawings of the sheets so far named.
pub(crate) struct DrawingShapes<'x, 'h, 'l> {
    xhtml: &'x mut Xhtml<'h>,
    hyperlinks: &'l HashMap<String, String>,
    in_body: bool,
    in_text: bool,
    text: String,
    shape_text: String,
}

impl<'x, 'h, 'l> DrawingShapes<'x, 'h, 'l> {
    pub(crate) fn new(xhtml: &'x mut Xhtml<'h>, hyperlinks: &'l HashMap<String, String>) -> Self {
        Self {
            xhtml,
            hyperlinks,
            in_body: false,
            in_text: false,
            text: String::new(),
            shape_text: String::new(),
        }
    }
}

impl SaxHandler for DrawingShapes<'_, '_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match local_name {
            "txBody" => {
                self.in_body = true;
                self.shape_text.clear();
            }
            "t" if self.in_body => {
                self.in_text = true;
                self.text.clear();
            }
            "hlinkClick" | "hlinkHover" => {
                let id = attribute(attributes, RELATIONSHIPS_NS, "id")
                    .or_else(|| attribute_named(attributes, "r:id"));
                if let Some(url) = id.and_then(|id| self.hyperlinks.get(id)) {
                    self.xhtml.start_element_with("a", &[("href", url)])?;
                    self.xhtml.characters(url)?;
                    self.xhtml.end_element("a")?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if local_name == "t" && self.in_text {
            self.in_text = false;
            self.shape_text.push_str(&self.text);
        } else if local_name == "p" && self.in_body && !self.shape_text.is_empty() {
            self.shape_text.push('\n');
        } else if local_name == "txBody" {
            self.in_body = false;
            let text = trim(&self.shape_text);
            if !text.is_empty() {
                self.xhtml.element("p", text)?;
            }
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_text {
            self.text.push_str(text);
        }
        Ok(())
    }
}

/// `ExternalLinkHandler`: links to the workbooks, OLE links and DDE links an external link
/// part names.
pub(crate) struct ExternalLinks<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    pub(crate) dde: bool,
}

impl<'x, 'h> ExternalLinks<'x, 'h> {
    pub(crate) fn new(xhtml: &'x mut Xhtml<'h>) -> Self {
        Self { xhtml, dde: false }
    }
}

impl SaxHandler for ExternalLinks<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match local_name {
            "file" => {
                if let Some(href) = attribute_named(attributes, "href") {
                    external_reference(self.xhtml, "externalWorkbook", href)?;
                }
            }
            "oleLink" => {
                if let Some(id) = attribute(attributes, RELATIONSHIPS_NS, "id") {
                    external_reference(self.xhtml, "oleLink", &format!("relationship:{id}"))?;
                }
            }
            "ddeLink" => {
                self.dde = true;
                let service = attribute_named(attributes, "ddeService");
                let topic = attribute_named(attributes, "ddeTopic");
                if service.is_some() || topic.is_some() {
                    let reference = format!("{}|{}", service.unwrap_or(""), topic.unwrap_or(""));
                    external_reference(self.xhtml, "ddeLink", &reference)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    ignore_rest!();
}

/// `ConnectionsHandler`: the data connections, web queries and their sources.
pub(crate) struct Connections<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    pub(crate) connection: bool,
    pub(crate) web_query: bool,
}

impl<'x, 'h> Connections<'x, 'h> {
    pub(crate) fn new(xhtml: &'x mut Xhtml<'h>) -> Self {
        Self {
            xhtml,
            connection: false,
            web_query: false,
        }
    }
}

impl SaxHandler for Connections<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let mut link = |class: &str, name: &str| -> Result<(), Failure> {
            if let Some(value) = attribute_named(attributes, name) {
                external_reference(self.xhtml, class, value)?;
            }
            Ok(())
        };
        match local_name {
            "connection" => self.connection = true,
            "dbPr" => link("dbConnection", "connection")?,
            "webPr" => {
                self.web_query = true;
                link("webQuery", "url")?;
            }
            "olapPr" => link("olapConnection", "connection")?,
            "textPr" => link("textFileImport", "sourceFile")?,
            _ => {}
        }
        Ok(())
    }

    ignore_rest!();
}

/// `PivotCacheHandler`: whether a pivot cache draws on data outside the workbook.
#[derive(Debug, Default)]
pub(crate) struct PivotCache {
    pub(crate) external: bool,
}

impl SaxHandler for PivotCache {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match local_name {
            "cacheSource" => {
                if matches!(
                    attribute_named(attributes, "type"),
                    Some("external" | "consolidation")
                ) {
                    self.external = true;
                }
            }
            "worksheetSource" => {
                if attribute(attributes, RELATIONSHIPS_NS, "id").is_some() {
                    self.external = true;
                }
            }
            "consolidation" | "rangeSets" => self.external = true,
            _ => {}
        }
        Ok(())
    }

    ignore_rest!();
}
