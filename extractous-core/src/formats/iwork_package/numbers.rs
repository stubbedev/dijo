//! Tika's `NumbersContentHandler`: each sheet a `div`, its tables' names and cells row by row,
//! the values of pop-up menu cells looked up by their choices, chart names, the text of its
//! text boxes, and the document's metadata and sheet names.

use std::collections::HashMap;

use super::{cell, integer, value, ContentHandler};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::StartTag;
use crate::formats::Failure;

const SHEET_NAMES: &str = "numbers:sheet-names";
const PAGE_COUNT: &str = "meta:page-count";
/// The prefix `NUMBERS_METADATA` gives metadata it maps to no property of its own.
const METADATA_PREFIX: &str = "numbers:";

pub(super) struct Numbers {
    in_sheet: bool,
    parse_text: bool,
    in_metadata: bool,
    /// The local and qualified names of the element of metadata being read.
    metadata_element: Option<(String, String)>,
    in_table: bool,
    sheets: i32,
    columns: i32,
    column: i32,
    menu_items: HashMap<String, Option<String>>,
    menu_item: Option<String>,
}

impl Default for Numbers {
    fn default() -> Self {
        Self {
            in_sheet: false,
            parse_text: false,
            in_metadata: false,
            metadata_element: None,
            in_table: false,
            sheets: 0,
            columns: -1,
            column: 0,
            menu_items: HashMap::new(),
            menu_item: None,
        }
    }
}

impl Numbers {
    /// A cell of the open table, in a new row once the last one has its columns.
    fn table_cell(&mut self, xhtml: &mut Xhtml, value: Option<&str>) -> Result<(), Failure> {
        if self.column >= self.columns {
            self.column = 0;
            xhtml.end_element("tr")?;
            xhtml.start_element("tr")?;
        }
        cell(xhtml, value)?;
        self.column += 1;
        Ok(())
    }
}

/// `addMetadata`: the authors, title and comment as Tika's properties, the rest under the
/// `numbers:` prefix.
fn add_metadata(xhtml: &mut Xhtml, local_name: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    let metadata = xhtml.metadata_mut();
    match local_name {
        "authors" => metadata.add("dc:creator", value),
        "title" => metadata.add("dc:title", value),
        "comment" => metadata.add("w:Comments", value),
        _ => metadata.add_prefixed(METADATA_PREFIX, local_name, value),
    }
}

impl ContentHandler for Numbers {
    fn start(&mut self, xhtml: &mut Xhtml, tag: &StartTag<'_>) -> Result<(), Failure> {
        let name = tag.qualified_name;
        if name == "ls:workspace" {
            self.in_sheet = true;
            self.sheets += 1;
            xhtml.start_element("div")?;
            if let Some(sheet) = value(tag, "ls:workspace-name") {
                xhtml.metadata_mut().add(SHEET_NAMES, sheet);
            }
        }
        if name == "sf:text" {
            xhtml.start_element("p")?;
        }
        if name == "sf:p" {
            self.parse_text = true;
        }
        if name == "sf:metadata" {
            self.in_metadata = true;
            return Ok(());
        }
        if self.in_metadata && self.metadata_element.is_none() {
            self.metadata_element = Some((tag.local_name.to_owned(), name.to_owned()));
        }
        if self.in_metadata && name == "sf:string" {
            if let Some((local_name, _)) = &self.metadata_element {
                let local_name = local_name.clone();
                add_metadata(xhtml, &local_name, value(tag, "sfa:string"));
            }
        }
        if !self.in_sheet {
            return Ok(());
        }
        if name == "sf:tabular-model" {
            xhtml.start_element("div")?;
            if let Some(table) = value(tag, "sf:name") {
                xhtml.characters(table)?;
            }
            xhtml.end_element("div")?;
            self.in_table = true;
            xhtml.start_element("table")?;
            xhtml.start_element("tr")?;
            self.column = 0;
        }
        if name == "sf:menu-choices" {
            self.menu_items = HashMap::new();
        }
        if self.in_table && name == "sf:grid" {
            self.columns = integer(tag, "sf:numcols")?;
        }
        if name == "sf:t" {
            self.menu_item = value(tag, "sfa:ID").map(str::to_owned);
        }
        if name == "sf:ct" {
            if let Some(item) = &self.menu_item {
                self.menu_items
                    .insert(item.clone(), value(tag, "sfa:s").map(str::to_owned));
            }
            if self.in_table {
                self.table_cell(xhtml, value(tag, "sfa:s"))?;
            }
        }
        if self.in_table && (name == "sf:n" || name == "sf:rn") {
            self.table_cell(xhtml, value(tag, "sf:v"))?;
        }
        if self.in_table && name == "sf:proxied-cell-ref" {
            let choice = value(tag, "sfa:IDREF")
                .and_then(|id| self.menu_items.get(id))
                .cloned()
                .flatten();
            self.table_cell(xhtml, choice.as_deref())?;
        }
        if name == "sf:chart-name" {
            xhtml.start_element_with("div", &[("class", "chart")])?;
            xhtml.start_element("h1")?;
            if let Some(chart) = value(tag, "sfa:string") {
                xhtml.characters(chart)?;
            }
            xhtml.end_element("h1")?;
            xhtml.end_element("div")?;
        }
        Ok(())
    }

    fn end(&mut self, xhtml: &mut Xhtml, _: &str, name: &str) -> Result<(), Failure> {
        if name == "ls:workspace" {
            self.in_sheet = false;
            xhtml.end_element("div")?;
        }
        if name == "sf:text" {
            xhtml.end_element("p")?;
        }
        if name == "sf:p" {
            self.parse_text = false;
        }
        if name == "sf:metadata" {
            self.in_metadata = false;
        }
        if self.in_metadata
            && self
                .metadata_element
                .as_ref()
                .is_some_and(|(_, qualified)| qualified == name)
        {
            self.metadata_element = None;
        }
        if !self.in_sheet {
            return Ok(());
        }
        if name == "sf:tabular-model" {
            self.in_table = false;
            xhtml.end_element("tr")?;
            xhtml.end_element("table")?;
        }
        if name == "sf:t" {
            self.menu_item = None;
        }
        Ok(())
    }

    fn characters(&mut self, xhtml: &mut Xhtml, text: &str) -> Result<(), Failure> {
        if self.parse_text && !text.is_empty() {
            xhtml.characters_chunk(text)?;
        }
        Ok(())
    }

    fn end_document(&mut self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        xhtml
            .metadata_mut()
            .set(PAGE_COUNT, self.sheets.to_string());
        Ok(())
    }
}
