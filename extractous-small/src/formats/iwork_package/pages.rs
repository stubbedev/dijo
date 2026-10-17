//! Tika's `PagesContentHandler`: each page a `div`, headed and footed by the default header
//! and footer with the page number in its format, its paragraphs, footnotes, annotations and
//! the tables it refers to, three cells to a row, and the document's metadata.

use std::collections::HashMap;

use super::{cell, integer, value, ContentHandler};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::StartTag;
use crate::formats::Failure;

const PAGE_COUNT: &str = "meta:page-count";
/// The prefix `PAGES` gives metadata it maps to no property of its own.
const METADATA_PREFIX: &str = "pages:";
/// The cells `parseTableData` puts in a row, whatever the table's columns.
const ROW_CELLS: usize = 3;
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

/// `Annotations`: the text of each annotation by its target, which may be missing, as a Java
/// `HashMap` keys one by `null`.
#[derive(Default)]
struct Annotations {
    texts: HashMap<Option<String>, String>,
    current: Option<(Option<String>, String)>,
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
    pages: i32,
    page_groups: i32,
    headers: Option<HeaderFooter>,
    footers: Option<HeaderFooter>,
    /// The footnotes by their marks, and the mark last seen.
    footnotes: Option<(HashMap<String, String>, Option<String>)>,
    annotations: Option<Annotations>,
    /// The tables by their IDs, which may be missing, as a Java `HashMap` keys one by `null`.
    tables: HashMap<Option<String>, Vec<Vec<Option<String>>>>,
    /// The ID of the table being read (`activeTableId`), which a table without one has not.
    table: Option<String>,
    row: Vec<Option<String>>,
    /// The local and qualified names of the element of metadata being read.
    metadata_element: Option<(String, String)>,
}

impl Pages {
    fn header(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        match &self.headers {
            Some(headers) => headers.output(xhtml, self.pages),
            None => Ok(()),
        }
    }

    fn footer(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        match &self.footers {
            Some(footers) => footers.output(xhtml, self.pages),
            None => Ok(()),
        }
    }

    fn any_page(&self) -> bool {
        self.pages + self.page_groups > 0
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

    /// `parseTableData`: the columns of a grid, which go unused, and each `sf:ct` value a cell.
    fn table_data(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        match tag.qualified_name {
            "sf:grid" => {
                integer(tag, "sf:numcols")?;
            }
            "sf:ct" => {
                self.row.push(value(tag, "sfa:s").map(str::to_owned));
                if self.row.len() >= ROW_CELLS {
                    let row = std::mem::take(&mut self.row);
                    self.tables
                        .get_mut(&self.table)
                        .ok_or_else(|| null("table data of a table it lost"))?
                        .push(row);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `outputTable` of the table `id` refers to, if it was read.
    fn output_table(&self, xhtml: &mut Xhtml, id: Option<&str>) -> Result<(), Failure> {
        let Some(rows) = self.tables.get(&id.map(str::to_owned)) else {
            return Ok(());
        };
        xhtml.start_element("table")?;
        for row in rows {
            xhtml.start_element("tr")?;
            for value in row {
                cell(xhtml, value.as_deref())?;
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
                if name == "sl:page-group" {
                    self.page_groups += 1;
                } else {
                    self.pages += 1;
                }
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
                    let id = value(tag, "sfa:ID").map(str::to_owned);
                    self.tables.insert(id.clone(), Vec::new());
                    self.table = id;
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
                let text = annotations
                    .texts
                    .get(&value(tag, "sfa:ID").map(str::to_owned));
                if let Some(text) = text {
                    xhtml.start_element_with("div", &[("style", "annotation")])?;
                    xhtml.characters(text)?;
                    xhtml.end_element("div")?;
                }
            }
            "sf:ghost-text" => self.ghost_text = true,
            _ => {}
        }
        if self.table.is_some() {
            self.table_data(tag)?;
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
        match name {
            "sl:publication-info" | "sf:metadata" => self.part = None,
            "sf:p" if self.any_page() => {
                self.part = None;
                xhtml.end_element("p")?;
            }
            "sf:attachment" => self.table = None,
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
                    annotations.texts.insert(target.clone(), text.clone());
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
        let owned = || Some(text.to_owned());
        match part {
            Part::ParsableText if !self.ghost_text => xhtml.characters_chunk(text)?,
            Part::HeaderFirst => self.headers_mut()?.first = owned(),
            Part::HeaderEven => self.headers_mut()?.even = owned(),
            Part::HeaderOdd => self.headers_mut()?.odd = owned(),
            Part::FooterFirst => self.footers_mut()?.first = owned(),
            Part::FooterEven => self.footers_mut()?.even = owned(),
            Part::FooterOdd => self.footers_mut()?.odd = owned(),
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
        xhtml.metadata_mut().set(PAGE_COUNT, self.pages.to_string());
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
