//! Tika's `KeynoteContentHandler`: each slide and master slide a `div`, its title an `h1`, its
//! notes and body paragraphs, its tables' cells row by row, the text of its text bodies, and
//! the slides' size, count, title and authors as metadata.

use super::{cell, integer, value, ContentHandler};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::StartTag;
use crate::formats::Failure;

const WIDTH: &str = "keynote:slides-width";
const HEIGHT: &str = "keynote:slides-height";
const SLIDE_COUNT: &str = "meta:slide-count";

#[derive(Default)]
pub(super) struct Keynote {
    in_slide: bool,
    in_theme: bool,
    /// Whether a table with an ID is open (`tableId != null`).
    in_table: bool,
    columns: Option<i32>,
    column: Option<i32>,
    in_metadata: bool,
    in_title: bool,
    in_authors: bool,
    in_text: bool,
    slides: i32,
}

impl Keynote {
    /// `parseTableData`: a cell, in a row started at the first column and ended after the
    /// last.
    fn table_data(&mut self, xhtml: &mut Xhtml, value: Option<&str>) -> Result<(), Failure> {
        let column = self.column.ok_or_else(|| {
            Failure::Malformed(
                "NullPointerException: a Keynote table cell before the table's columns".to_owned(),
            )
        })?;
        if column == 0 {
            xhtml.start_element("tr")?;
        }
        cell(xhtml, value)?;
        let column = column + 1;
        if Some(column) == self.columns {
            xhtml.end_element("tr")?;
            self.column = Some(0);
        } else {
            self.column = Some(column);
        }
        Ok(())
    }
}

impl ContentHandler for Keynote {
    fn start(&mut self, xhtml: &mut Xhtml, tag: &StartTag<'_>) -> Result<(), Failure> {
        let name = tag.qualified_name;
        match name {
            "key:theme" => self.in_theme = true,
            "key:slide" => {
                self.in_slide = true;
                self.slides += 1;
                xhtml.start_element("div")?;
            }
            "key:master-slide" => {
                self.in_slide = true;
                xhtml.start_element("div")?;
            }
            "key:title-placeholder" if self.in_slide => xhtml.start_element("h1")?,
            "sf:sticky-note" | "key:notes" if self.in_slide => xhtml.start_element("p")?,
            "key:body-placeholder" if self.in_slide => xhtml.start_element("p")?,
            "key:size" if !self.in_theme => {
                let metadata = xhtml.metadata_mut();
                for (key, attribute) in [(WIDTH, "sfa:w"), (HEIGHT, "sfa:h")] {
                    match value(tag, attribute) {
                        Some(size) => metadata.set(key, size),
                        None => metadata.remove(key),
                    }
                }
            }
            "sf:text-body" => self.in_text = true,
            "key:metadata" => self.in_metadata = true,
            "key:title" if self.in_metadata => self.in_title = true,
            "key:authors" if self.in_metadata => self.in_authors = true,
            "key:string" if self.in_title => match value(tag, "sfa:string") {
                Some(title) => xhtml.metadata_mut().set("dc:title", title),
                None => xhtml.metadata_mut().remove("dc:title"),
            },
            "key:string" if self.in_authors => {
                if let Some(author) = value(tag, "sfa:string") {
                    xhtml.metadata_mut().add("dc:creator", author);
                }
            }
            "sf:tabular-model" if self.in_slide => {
                self.in_table = value(tag, "sfa:ID").is_some();
                xhtml.start_element("table")?;
            }
            "sf:columns" if self.in_table => {
                self.columns = Some(integer(tag, "sf:count")?);
                self.column = Some(0);
            }
            "sf:ct" if self.in_table => self.table_data(xhtml, value(tag, "sfa:s"))?,
            "sf:n" if self.in_table => self.table_data(xhtml, value(tag, "sf:v"))?,
            "sf:p" => xhtml.start_element("p")?,
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, xhtml: &mut Xhtml, _: &str, name: &str) -> Result<(), Failure> {
        match name {
            "key:theme" => self.in_theme = false,
            "key:slide" | "key:master-slide" => {
                self.in_slide = false;
                xhtml.end_element("div")?;
            }
            "key:title-placeholder" if self.in_slide => xhtml.end_element("h1")?,
            "sf:sticky-note" | "key:notes" | "key:body-placeholder" if self.in_slide => {
                xhtml.end_element("p")?
            }
            "sf:text-body" => self.in_text = false,
            "key:metadata" => self.in_metadata = false,
            "key:title" if self.in_metadata => self.in_title = false,
            "key:authors" if self.in_metadata => self.in_authors = false,
            "sf:tabular-model" if self.in_slide => {
                if self.column.is_some_and(|column| column != 0) {
                    xhtml.end_element("tr")?;
                }
                xhtml.end_element("table")?;
                self.in_table = false;
                self.columns = None;
                self.column = None;
            }
            "sf:p" => xhtml.end_element("p")?,
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, xhtml: &mut Xhtml, text: &str) -> Result<(), Failure> {
        if self.in_text && self.in_slide && !text.is_empty() {
            xhtml.characters_chunk(text)?;
        }
        Ok(())
    }

    fn end_document(&mut self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        xhtml
            .metadata_mut()
            .set(SLIDE_COUNT, self.slides.to_string());
        Ok(())
    }
}
