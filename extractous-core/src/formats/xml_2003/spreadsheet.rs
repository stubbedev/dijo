//! Tika's `SpreadsheetMLParser`: each worksheet a `div` named by it, each table a table, each
//! row a row and each cell a cell holding its data trimmed, linked where the cell has a link,
//! as its `ExcelMLHandler` writes them.

use super::Body;
use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{attribute, Attribute};
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java;

const MEDIA_TYPE: &str = "application/vnd.ms-spreadsheetml";
/// `MS_SPREADSHEET_URN`, the namespace of the workbook's elements.
const SPREADSHEET_URN: &str = "urn:schemas-microsoft-com:office:spreadsheet";

/// Tika's `SpreadsheetMLParser`.
pub(crate) struct SpreadsheetMlParser;

impl Parser for SpreadsheetMlParser {
    fn name(&self) -> &'static str {
        "extractous::SpreadsheetMLParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.xml.SpreadsheetMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        super::prepare(input, MEDIA_TYPE, Excel::default())
    }
}

/// The state of `ExcelMLHandler`.
#[derive(Debug, Default)]
struct Excel {
    data: String,
    href: Option<String>,
    in_data: bool,
    in_body: bool,
}

impl Body for Excel {
    fn start(
        &mut self,
        xhtml: &mut Xhtml,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if namespace != SPREADSHEET_URN {
            return Ok(());
        }
        match java::to_lower_case(local_name).as_str() {
            "body" => self.in_body = true,
            "table" => {
                xhtml.start_element("table")?;
                xhtml.start_element("tbody")?;
            }
            "worksheet" => match attribute(attributes, SPREADSHEET_URN, "Name") {
                Some(name) => xhtml.start_element_with("div", &[("name", name)])?,
                None => xhtml.start_element("div")?,
            },
            "row" => xhtml.start_element("tr")?,
            "cell" => {
                self.href = attribute(attributes, SPREADSHEET_URN, "HRef").map(str::to_owned);
                xhtml.start_element("td")?;
            }
            "data" => self.in_data = true,
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, xhtml: &mut Xhtml, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if namespace != SPREADSHEET_URN {
            return Ok(());
        }
        match java::to_lower_case(local_name).as_str() {
            "table" => {
                xhtml.end_element("tbody")?;
                xhtml.end_element("table")?;
            }
            "worksheet" => xhtml.end_element("div")?,
            "row" => xhtml.end_element("tr")?,
            "cell" => xhtml.end_element("td")?,
            "data" => {
                let href = self.href.take();
                if let Some(href) = &href {
                    xhtml.start_element_with("a", &[("href", href)])?;
                }
                xhtml.characters_chunk(java::trim(&self.data))?;
                if href.is_some() {
                    xhtml.end_element("a")?;
                }
                self.data.clear();
                self.in_data = false;
            }
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, xhtml: &mut Xhtml, text: &str) -> Result<(), Failure> {
        if self.in_data {
            self.data.push_str(text);
        } else if self.in_body {
            xhtml.characters_chunk(text)?;
        }
        Ok(())
    }
}
