//! The workbook parts Tika reads with its own SAX shims rather than POI's tables: the number
//! formats of the cell styles (`XSSFStylesShim`), the shared strings (`XSSFSharedStringsShim`)
//! and a sheet's comments (`XSSFCommentsShim`).

use std::collections::HashMap;

use crate::formats::xml::attribute_named;

use super::cell::CellAddress;
use super::SPREADSHEETML_NS;
use crate::formats::xml::{Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java::parse_int;

/// `XSSFStylesShim`: the number format of each cell style.
#[derive(Debug, Default)]
pub(crate) struct Styles {
    formats: HashMap<i16, String>,
    cell_formats: Vec<i16>,
    in_cell_formats: bool,
    in_formats: bool,
}

/// `Short.parseShort`.
fn parse_java_short(text: &str) -> Option<i16> {
    parse_int(text).and_then(|value| i16::try_from(value).ok())
}

impl Styles {
    pub(crate) fn len(&self) -> usize {
        self.cell_formats.len()
    }

    /// `getFormatIndex`: -1 for a style it does not have.
    pub(crate) fn format_index(&self, style: i32) -> i16 {
        usize::try_from(style)
            .ok()
            .and_then(|style| self.cell_formats.get(style))
            .copied()
            .unwrap_or(-1)
    }

    /// `getFormatString`: the style's own format, else the built-in one of its index.
    pub(crate) fn format_string(&self, style: i32) -> Option<String> {
        let index = self.format_index(style);
        if index == -1 {
            return None;
        }
        self.formats.get(&index).cloned().or_else(|| {
            crate::formats::data_format::builtin_format(i32::from(index)).map(str::to_owned)
        })
    }
}

impl SaxHandler for Styles {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if namespace != SPREADSHEETML_NS {
            return Ok(());
        }
        let value = |name: &str| attribute_named(attributes, name);
        match local_name {
            "numFmts" => self.in_formats = true,
            "numFmt" if self.in_formats => {
                if let (Some(id), Some(code)) = (value("numFmtId"), value("formatCode")) {
                    if let Some(id) = parse_java_short(id) {
                        self.formats.insert(id, code.to_owned());
                    }
                }
            }
            "cellXfs" => self.in_cell_formats = true,
            "xf" if self.in_cell_formats => {
                let id = value("numFmtId").and_then(parse_java_short).unwrap_or(0);
                self.cell_formats.push(id);
            }
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if namespace != SPREADSHEETML_NS {
            return Ok(());
        }
        match local_name {
            "numFmts" => self.in_formats = false,
            "cellXfs" => self.in_cell_formats = false,
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// `XSSFSharedStringsShim`: each `si`'s runs, with its phonetic runs if asked for, as one string.
#[derive(Debug)]
pub(crate) struct SharedStrings {
    pub(crate) strings: Vec<String>,
    phonetic: bool,
    current: Option<String>,
    in_text: bool,
    in_phonetic: bool,
}

impl SharedStrings {
    pub(crate) fn new(phonetic: bool) -> Self {
        Self {
            strings: Vec::new(),
            phonetic,
            current: None,
            in_text: false,
            in_phonetic: false,
        }
    }
}

impl SaxHandler for SharedStrings {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if namespace != SPREADSHEETML_NS {
            return Ok(());
        }
        match local_name {
            "sst" => self.current = Some(String::new()),
            "si" => {
                if let Some(current) = &mut self.current {
                    current.clear();
                }
            }
            "t" => self.in_text = true,
            "rPh" => {
                self.in_phonetic = true;
                if let Some(current) = &mut self.current {
                    if self.phonetic && !current.is_empty() {
                        current.push(' ');
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if namespace != SPREADSHEETML_NS {
            return Ok(());
        }
        match local_name {
            "si" => {
                if let Some(current) = &self.current {
                    self.strings.push(current.clone());
                }
            }
            "t" => self.in_text = false,
            "rPh" => self.in_phonetic = false,
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if !self.in_text || (self.in_phonetic && !self.phonetic) {
            return Ok(());
        }
        if let Some(current) = &mut self.current {
            current.push_str(text);
        }
        Ok(())
    }
}

/// A comment: its author and its text runs joined by spaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Comment {
    pub(crate) author: String,
    pub(crate) text: String,
}

/// `XSSFCommentsShim`: comments by cell, in the order their cells were first commented on.
#[derive(Debug, Default)]
pub(crate) struct Comments {
    order: Vec<CellAddress>,
    by_cell: HashMap<CellAddress, Comment>,
    authors: Vec<String>,
    buffer: String,
    text: String,
    in_author: bool,
    in_text: bool,
    in_run_text: bool,
    reference: Option<String>,
    author: i32,
}

impl Comments {
    pub(crate) fn find(&self, address: CellAddress) -> Option<&Comment> {
        self.by_cell.get(&address)
    }

    /// The commented cells, in the order they were first commented on.
    pub(crate) fn cells(&self) -> impl Iterator<Item = CellAddress> + '_ {
        self.order.iter().copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }
}

impl SaxHandler for Comments {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let value = |name: &str| attribute_named(attributes, name);
        match local_name {
            "author" => {
                self.in_author = true;
                self.buffer.clear();
            }
            "comment" => {
                self.reference = value("ref").map(str::to_owned);
                self.author = match value("authorId") {
                    Some(id) => parse_int(id).ok_or_else(|| {
                        Failure::Malformed(format!("The comment author ID {id:?} is not an int"))
                    })?,
                    None => -1,
                };
                self.text.clear();
            }
            "text" => self.in_text = true,
            "t" if self.in_text => {
                self.in_run_text = true;
                self.buffer.clear();
            }
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        match local_name {
            "author" => {
                self.in_author = false;
                self.authors.push(self.buffer.clone());
            }
            "t" if self.in_run_text => {
                self.in_run_text = false;
                if !self.text.is_empty() {
                    self.text.push(' ');
                }
                self.text.push_str(&self.buffer);
            }
            "text" => self.in_text = false,
            "comment" => {
                if let Some(reference) = self.reference.take() {
                    let author = usize::try_from(self.author)
                        .ok()
                        .and_then(|author| self.authors.get(author))
                        .cloned()
                        .unwrap_or_default();
                    let address = CellAddress::parse(&reference)?;
                    let comment = Comment {
                        author,
                        text: self.text.clone(),
                    };
                    if self.by_cell.insert(address, comment).is_none() {
                        self.order.push(address);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_author || self.in_run_text {
            self.buffer.push_str(text);
        }
        Ok(())
    }
}
