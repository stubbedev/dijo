//! Tika's `PagesContentHandler`: each page a `div`, headed and footed by the default header
//! and footer with the page number in its format, its paragraphs, footnotes, annotations and
//! the tables it refers to, and the document's metadata.
//!
//! Where Tika's handler misreads the document, this one reads it as it is written (#132): a
//! table's rows have as many cells as its grid has columns, each cell of its data source one,
//! numbers and dates included; a header or footer keeps all of its text; a page group, which
//! holds the objects placed on a page, is numbered as that page; and tables and annotations
//! are found by their IDs only.

use std::collections::HashMap;

use super::{integer, value, ContentHandler};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::StartTag;
use crate::formats::Failure;

const PAGE_COUNT: &str = "meta:page-count";
/// The prefix `PAGES` gives metadata it maps to no property of its own.
const METADATA_PREFIX: &str = "pages:";
const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
/// `AutoPageNumberUtils.MAX`.
const LETTERS: i32 = 26;

/// `DocumentPart`: the part of the document being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Metadata,
    ParsableText,
    Headers,
    HeaderOdd,
    HeaderEven,
    HeaderFirst,
    Footers,
    FooterOdd,
    FooterEven,
    FooterFirst,
    Footnotes,
    Annotations,
}

/// `HeaderFooter`: the default headers or footers, and how their page numbers are written.
#[derive(Default)]
struct HeaderFooter {
    odd: Option<String>,
    even: Option<String>,
    first: Option<String>,
    auto_page_number: bool,
    page_number_format: Option<String>,
}

impl HeaderFooter {
    /// `identifyPart` of a header's or footer's name.
    fn part(name: Option<&str>) -> Option<Part> {
        match name? {
            "SFWPDefaultOddHeaderIdentifier" => Some(Part::HeaderOdd),
            "SFWPDefaultEvenHeaderIdentifier" => Some(Part::HeaderEven),
            "SFWPDefaultFirstHeaderIdentifier" => Some(Part::HeaderFirst),
            "SFWPDefaultOddFooterIdentifier" => Some(Part::FooterOdd),
            "SFWPDefaultEvenFooterIdentifier" => Some(Part::FooterEven),
            "SFWPDefaultFirstFooterIdentifier" => Some(Part::FooterFirst),
            _ => None,
        }
    }

    /// `output`: the header or footer of page `page` in a `div` of class `header`, its page
    /// number after a tab.
    fn output(&self, xhtml: &mut Xhtml, page: i32) -> Result<(), Failure> {
        let text = if page == 1 && self.first.is_some() {
            &self.first
        } else if page % 2 == 0 && self.even.is_some() {
            &self.even
        } else {
            &self.odd
        };
        let Some(text) = text else {
            return Ok(());
        };
        xhtml.start_element_with("div", &[("class", "header")])?;
        xhtml.characters(text)?;
        if self.auto_page_number {
            let number = match self.page_number_format.as_deref() {
                None => Some(page.to_string()),
                Some("upper-roman") => Some(roman(page)?),
                Some("lower-roman") => Some(roman(page)?.to_lowercase()),
                Some("upper-alpha") => Some(alphabetic(page)),
                Some("lower-alpha") => Some(alphabetic(page).to_lowercase()),
                Some(_) => None,
            };
            if let Some(number) = number {
                xhtml.characters(&format!("\t{number}"))?;
            }
        }
        xhtml.end_element("div")?;
        Ok(())
    }
}

/// `AutoPageNumberUtils.asAlphaNumeric`: the page's letter, repeated once more for each time
/// the alphabet comes round.
fn alphabetic(page: i32) -> String {
    let mut index = page % LETTERS;
    let mut ratio = page / LETTERS;
    if index == 0 {
        ratio -= 1;
        index = LETTERS;
    }
    let at = usize::try_from(index - 1).expect("a page number is not negative");
    ALPHABET[at..=at].repeat(usize::try_from(ratio + 1).unwrap_or(0))
}

/// `AutoPageNumberUtils.asRomanNumerals`, which throws past 1 to 3999.
fn roman(page: i32) -> Result<String, Failure> {
    if !(1..=3999).contains(&page) {
        return Err(Failure::Malformed(format!(
            "NumberFormatException: Roman numerals are 1-3999 ({page})"
        )));
    }
    const NUMERALS: [(i32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut rest = page;
    let mut numerals = String::new();
    for (value, numeral) in NUMERALS {
        while rest >= value {
            numerals.push_str(numeral);
            rest -= value;
        }
    }
    Ok(numerals)
}

/// `Annotations`: the text of each annotation by its target, and the annotation being read,
/// with its target if it has one.
#[derive(Default)]
struct Annotations {
    texts: HashMap<String, String>,
    current: Option<(Option<String>, String)>,
}

/// A table being read: its ID, its columns, its rows, and where in its data source the reading
/// is.
#[derive(Default)]
struct Table {
    id: Option<String>,
    columns: Option<usize>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    /// Inside the data source, how many elements are open in it: one for a cell being read.
    datasource: Option<usize>,
    /// The value of the cell being read.
    cell: String,
}

impl Table {
    /// The start of an element of the table.
    fn start(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        let name = tag.qualified_name;
        match self.datasource {
            Some(0) => {
                self.cell = ["sf:v", "sf:cell-date"]
                    .iter()
                    .find_map(|attribute| value(tag, attribute))
                    .unwrap_or_default()
                    .to_owned();
            }
            Some(_) if name == "sf:ct" => {
                if let Some(text) = value(tag, "sfa:s") {
                    self.cell = text.to_owned();
                }
            }
            Some(_) => {}
            None if name == "sf:datasource" => {
                self.datasource = Some(0);
                return Ok(());
            }
            None => {
                if name == "sf:grid" {
                    let columns = integer(tag, "sf:numcols")?;
                    self.columns = Some(
                        usize::try_from(columns)
                            .ok()
                            .filter(|&n| n > 0)
                            .ok_or_else(|| {
                                Failure::Malformed(format!(
                                    "A Pages table's grid has {columns} columns"
                                ))
                            })?,
                    );
                }
                return Ok(());
            }
        }
        self.datasource = self.datasource.map(|open| open + 1);
        Ok(())
    }

    /// The end of an element of the table.
    fn end(&mut self) -> Result<(), Failure> {
        match self.datasource {
            None => {}
            Some(0) => self.datasource = None,
            Some(open) => {
                self.datasource = Some(open - 1);
                if open == 1 {
                    let columns = self.columns.ok_or_else(|| {
                        Failure::Malformed("A Pages table has cells before its grid".to_owned())
                    })?;
                    self.row.push(std::mem::take(&mut self.cell));
                    if self.row.len() == columns {
                        self.rows.push(std::mem::take(&mut self.row));
                    }
                }
            }
        }
        Ok(())
    }

    /// The rows read, the last one even where it is cut short.
    fn into_rows(mut self) -> Vec<Vec<String>> {
        if !self.row.is_empty() {
            self.rows.push(self.row);
        }
        self.rows
    }
}

/// A Java `NullPointerException` the handler throws on a document whose parts come out of
/// order, which fails the parse.
fn null(what: &str) -> Failure {
    Failure::Malformed(format!(
        "NullPointerException: the Pages document has {what}"
    ))
}

#[derive(Default)]
pub(super) struct Pages {
    part: Option<Part>,
    ghost_text: bool,
    parse_property: bool,
    /// The pages the text has started.
    pages: i32,
    /// The highest page a page group is placed on.
    last_group_page: i32,
    /// The number of the page being written, from when the first page or page group opens.
    page: Option<i32>,
    headers: Option<HeaderFooter>,
    footers: Option<HeaderFooter>,
    /// The footnotes by their marks, and the mark last seen.
    footnotes: Option<(HashMap<String, String>, Option<String>)>,
    annotations: Option<Annotations>,
    /// The tables by their IDs.
    tables: HashMap<String, Vec<Vec<String>>>,
    table: Option<Table>,
    /// The local and qualified names of the element of metadata being read.
    metadata_element: Option<(String, String)>,
}

impl Pages {
    fn header(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        match (&self.headers, self.page) {
            (Some(headers), Some(page)) => headers.output(xhtml, page),
            _ => Ok(()),
        }
    }

    fn footer(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        match (&self.footers, self.page) {
            (Some(footers), Some(page)) => footers.output(xhtml, page),
            _ => Ok(()),
        }
    }

    fn any_page(&self) -> bool {
        self.page.is_some()
    }

    /// `writeMetaData`: the value of a primitive element of metadata, as Tika's property for its
    /// element or under the `pages:` prefix by its local name.
    fn write_metadata(&self, xhtml: &mut Xhtml, value: &str) {
        let Some((local_name, qualified_name)) = &self.metadata_element else {
            return;
        };
        let metadata = xhtml.metadata_mut();
        match qualified_name.as_str() {
            "sf:authors" => metadata.set("dc:creator", value),
            "sf:title" => metadata.set("dc:title", value),
            "sl:SLCreationDateProperty" => metadata.set("dcterms:created", value),
            "sl:SLLastModifiedDateProperty" => metadata.set("dcterms:modified", value),
            "sl:language" => metadata.set("dc:language", value),
            _ => metadata.add_prefixed(METADATA_PREFIX, local_name, value),
        }
    }

    /// `outputTable` of the table `id` refers to, if it was read: each cell a `td`, an empty one
    /// too.
    fn output_table(&self, xhtml: &mut Xhtml, id: Option<&str>) -> Result<(), Failure> {
        let Some(rows) = id.and_then(|id| self.tables.get(id)) else {
            return Ok(());
        };
        xhtml.start_element("table")?;
        for row in rows {
            xhtml.start_element("tr")?;
            for value in row {
                xhtml.start_element("td")?;
                xhtml.characters(value)?;
                xhtml.end_element("td")?;
            }
            xhtml.end_element("tr")?;
        }
        xhtml.end_element("table")?;
        Ok(())
    }
}

impl ContentHandler for Pages {
    fn start(&mut self, xhtml: &mut Xhtml, tag: &StartTag<'_>) -> Result<(), Failure> {
        let name = tag.qualified_name;
        if self.parse_property {
            let primitive = match name {
                "sl:string" | "sf:string" => value(tag, "sfa:string"),
                "sl:number" => value(tag, "sfa:number"),
                "sl:date" => value(tag, "sf:val"),
                _ => None,
            };
            if let Some(primitive) = primitive {
                self.write_metadata(xhtml, primitive);
            }
        }
        match name {
            "sl:publication-info" | "sf:metadata" => self.part = Some(Part::Metadata),
            "sf:page-start" | "sl:page-group" => {
                let reopen = self.part == Some(Part::ParsableText);
                if reopen {
                    xhtml.end_element("p")?;
                }
                if self.any_page() {
                    self.footer(xhtml)?;
                    xhtml.end_element("div")?;
                }
                xhtml.start_element("div")?;
                let page = if name == "sl:page-group" {
                    let page = integer(tag, "sl:page")?;
                    self.last_group_page = self.last_group_page.max(page);
                    page
                } else {
                    self.pages += 1;
                    self.pages
                };
                self.page = Some(page);
                self.header(xhtml)?;
                if reopen {
                    xhtml.start_element("p")?;
                }
            }
            "sf:p" => {
                if self.any_page() {
                    self.part = Some(Part::ParsableText);
                    xhtml.start_element("p")?;
                }
            }
            "sf:attachment" => {
                if value(tag, "sf:kind") == Some("tabular-attachment") {
                    self.table = Some(Table {
                        id: value(tag, "sfa:ID").map(str::to_owned),
                        ..Table::default()
                    });
                    return Ok(());
                }
            }
            "sf:attachment-ref" => self.output_table(xhtml, value(tag, "sfa:IDREF"))?,
            "sf:headers" => {
                self.headers = Some(HeaderFooter::default());
                self.part = Some(Part::Headers);
            }
            "sf:footers" => {
                self.footers = Some(HeaderFooter::default());
                self.part = Some(Part::Footers);
            }
            "sf:header" => {
                if self.headers.is_none() {
                    return Err(null("a header outside the headers"));
                }
                self.part = HeaderFooter::part(value(tag, "sf:name"));
            }
            "sf:footer" => {
                if self.footers.is_none() {
                    return Err(null("a footer outside the footers"));
                }
                self.part = HeaderFooter::part(value(tag, "sf:name"));
            }
            "sf:page-number" => {
                let format = value(tag, "sf:format").map(str::to_owned);
                let in_footer = matches!(
                    self.part,
                    Some(Part::FooterOdd | Part::FooterFirst | Part::FooterEven)
                );
                let numbered = if in_footer {
                    Some(self.footers_mut()?)
                } else {
                    self.headers.as_mut()
                };
                if let Some(numbered) = numbered {
                    numbered.auto_page_number = true;
                    numbered.page_number_format = format;
                }
                xhtml.characters(&self.pages.to_string())?;
            }
            "sf:footnotes" => {
                self.footnotes = Some((HashMap::new(), None));
                self.part = Some(Part::Footnotes);
            }
            "sf:footnote-mark" => {
                let (_, last) = self
                    .footnotes
                    .as_mut()
                    .ok_or_else(|| null("a footnote mark outside the footnotes"))?;
                *last = value(tag, "sf:mark").map(str::to_owned);
            }
            "sf:footnote" if self.part == Some(Part::ParsableText) => {
                let text = self.footnotes.as_ref().and_then(|(footnotes, _)| {
                    value(tag, "sf:autonumber").and_then(|mark| footnotes.get(mark))
                });
                if let Some(text) = text {
                    xhtml.start_element_with("div", &[("style", "footnote")])?;
                    xhtml.characters("Footnote:")?;
                    xhtml.characters(text)?;
                    xhtml.end_element("div")?;
                }
            }
            "sf:annotations" => {
                self.annotations = Some(Annotations::default());
                self.part = Some(Part::Annotations);
            }
            "sf:annotation" if self.part == Some(Part::Annotations) => {
                if let Some(annotations) = &mut self.annotations {
                    annotations.current =
                        Some((value(tag, "sf:target").map(str::to_owned), String::new()));
                }
            }
            "sf:annotation-field" if self.part == Some(Part::ParsableText) => {
                xhtml.start_element_with("div", &[("style", "annotated")])?;
                let annotations = self
                    .annotations
                    .as_ref()
                    .ok_or_else(|| null("an annotated field without annotations"))?;
                let text = value(tag, "sfa:ID").and_then(|id| annotations.texts.get(id));
                if let Some(text) = text {
                    xhtml.start_element_with("div", &[("style", "annotation")])?;
                    xhtml.characters(text)?;
                    xhtml.end_element("div")?;
                }
            }
            "sf:ghost-text" => self.ghost_text = true,
            _ => {}
        }
        if let Some(table) = &mut self.table {
            table.start(tag)?;
        }
        if self.part == Some(Part::Metadata) {
            self.metadata_element = Some((tag.local_name.to_owned(), name.to_owned()));
            self.parse_property = true;
        }
        Ok(())
    }

    fn end(&mut self, xhtml: &mut Xhtml, local_name: &str, name: &str) -> Result<(), Failure> {
        if self
            .metadata_element
            .as_ref()
            .is_some_and(|(local, _)| local == local_name)
        {
            self.metadata_element = None;
            self.parse_property = false;
        }
        if let Some(table) = &mut self.table {
            if name != "sf:attachment" {
                table.end()?;
            }
        }
        match name {
            "sl:publication-info" | "sf:metadata" => self.part = None,
            "sf:p" if self.any_page() => {
                self.part = None;
                xhtml.end_element("p")?;
            }
            "sf:attachment" => {
                if let Some(table) = self.table.take() {
                    if let Some(id) = table.id.clone() {
                        self.tables.insert(id, table.into_rows());
                    }
                }
            }
            "sf:header" | "sf:footer" => self.part = None,
            "sf:annotation" if self.part == Some(Part::Annotations) => {
                let annotations = self
                    .annotations
                    .as_mut()
                    .ok_or_else(|| null("an annotation outside the annotations"))?;
                let (target, text) = annotations
                    .current
                    .as_ref()
                    .ok_or_else(|| null("an annotation ended twice"))?;
                if !text.is_empty() {
                    if let Some(target) = target {
                        annotations.texts.insert(target.clone(), text.clone());
                    }
                    annotations.current = None;
                }
            }
            "sf:annotation-field" if self.part == Some(Part::ParsableText) => {
                xhtml.end_element("div")?
            }
            "sf:ghost-text" => self.ghost_text = false,
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, xhtml: &mut Xhtml, text: &str) -> Result<(), Failure> {
        if text.is_empty() {
            return Ok(());
        }
        let Some(part) = self.part else {
            return Ok(());
        };
        let append =
            |piece: &mut Option<String>| piece.get_or_insert_with(String::new).push_str(text);
        match part {
            Part::ParsableText if !self.ghost_text => xhtml.characters_chunk(text)?,
            Part::HeaderFirst => append(&mut self.headers_mut()?.first),
            Part::HeaderEven => append(&mut self.headers_mut()?.even),
            Part::HeaderOdd => append(&mut self.headers_mut()?.odd),
            Part::FooterFirst => append(&mut self.footers_mut()?.first),
            Part::FooterEven => append(&mut self.footers_mut()?.even),
            Part::FooterOdd => append(&mut self.footers_mut()?.odd),
            Part::Footnotes => {
                if let Some((footnotes, Some(mark))) = &mut self.footnotes {
                    footnotes.entry(mark.clone()).or_default().push_str(text);
                }
            }
            Part::Annotations => {
                if let Some(Annotations {
                    current: Some((_, current)),
                    ..
                }) = &mut self.annotations
                {
                    current.push_str(text);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end_document(&mut self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        xhtml
            .metadata_mut()
            .set(PAGE_COUNT, self.pages.max(self.last_group_page).to_string());
        if self.any_page() {
            self.footer(xhtml)?;
            xhtml.end_element("div")?;
        }
        Ok(())
    }
}

impl Pages {
    fn headers_mut(&mut self) -> Result<&mut HeaderFooter, Failure> {
        self.headers
            .as_mut()
            .ok_or_else(|| null("header text outside the headers"))
    }

    fn footers_mut(&mut self) -> Result<&mut HeaderFooter, Failure> {
        self.footers
            .as_mut()
            .ok_or_else(|| null("footer text outside the footers"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_pages_as_tikas_auto_page_number_utils_does() {
        assert_eq!(alphabetic(1), "A");
        assert_eq!(alphabetic(26), "Z");
        assert_eq!(alphabetic(27), "AA");
        assert_eq!(alphabetic(53), "AAA");
        assert_eq!(alphabetic(0), "");
        assert_eq!(roman(1994).unwrap(), "MCMXCIV");
        assert!(roman(0).is_err());
        assert!(roman(4000).is_err());
    }
}
