//! Excel 97–2003 workbooks (XLS, BIFF8) as Tika's `OfficeParser` reads them with its Excel
//! extractor, `ExcelExtractor`: the summary information as metadata, then the workbook stream's
//! records through POI's event model into Tika's `TikaHSSFListener`, a page per sheet with its
//! cells in a table and the text found outside the cells after it, the pictures the drawing
//! groups store as embedded documents, and the hidden sheets, rows, columns, protection and
//! comment authors as metadata.
//!
//! Numbers are written as Tika's listener writes them: dates and the general format with
//! Tika's `TikaExcelDataFormatter`, other formats with POI's own `DataFormatter`, both in the
//! `en_US` locale with dates in Java's default time zone and the 1900 date system; see
//! [`crate::formats::data_format`].
//!
//! The documents embedded in `MBD` storages follow, handed on as `handleEmbeddedOfficeDoc` hands
//! them. A workbook before BIFF8, whose stream is named "Book", is read as POI's
//! `OldExcelExtractor` reads it for Tika's `OldExcelParser`: a paragraph per line of its text.
//!
//! An encrypted workbook is decrypted with the password, or failed as Tika's path fails it where
//! the password does not open it. One POI throws on and macros extracted when asked to are left
//! to Tika: see [`XlsParser::defers`].

mod factory;
mod listener;
mod old;
mod records;
mod stream;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use factory::{Event, Records};
use listener::{is_listened, render, Cell, Extracted, Listener, Out, Page, Settings};

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::lines;
use crate::formats::embedded::Wrapping;
use crate::formats::escher::{self, Content, Escher};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{office_document, Handed, Summaries};
use crate::formats::poi::Thrown;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::xlsx::write_sheet_numbers;
use crate::formats::{Declined, Failure, Parser, Prepared, Unported, PASSWORD_USED};
use crate::java::time::TimeZone;
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::{EntryKind, Ole2, Unreadable};

const MEDIA_TYPE: &str = "application/vnd.ms-excel";
/// `InternalWorkbook.WORKBOOK_DIR_ENTRY_NAMES`, in the order POI looks for them.
const WORKBOOK_ENTRY_NAMES: [&str; 4] = ["Workbook", "WORKBOOK", "BOOK", "WorkBook"];
/// The entry of a workbook before BIFF8, which Tika reads with its `OldExcelExtractor`.
const OLD_BOOK_ENTRY: &str = "Book";
/// The start of the names of the storages documents are embedded in.
const EMBEDDED_STORAGE_PREFIX: &str = "MBD";

/// Metadata names the Excel extractor sets.
mod excel {
    pub(super) const PROTECTED_WORKSHEET: &str = "msoffice:excel:protected-worksheet";
    pub(super) const HAS_HIDDEN_COLUMNS: &str = "msoffice:excel:has-hidden-cols";
    pub(super) const HAS_HIDDEN_ROWS: &str = "msoffice:excel:has-hidden-rows";
    pub(super) use crate::formats::metadata::office::{COMMENT_PERSONS, HAS_COMMENTS};
    pub(super) const HIDDEN_SHEET_NAMES: &str = "msoffice:excel:hidden-sheet-names";
    pub(super) const HAS_HIDDEN_SHEETS: &str = "msoffice:excel:has-hidden-sheets";
    pub(super) const VERY_HIDDEN_SHEET_NAMES: &str = "msoffice:excel:very-hidden-sheet-names";
    pub(super) const HAS_VERY_HIDDEN_SHEETS: &str = "msoffice:excel:has-very-hidden-sheets";
}

pub(crate) struct XlsParser;

/// Tika's `OldExcelParser` of the workbooks of Excel 2 to 4 stored bare, outside a compound
/// document: the text of `OldExcelExtractor`, a paragraph per line.
pub(crate) struct OldExcelParser;

impl Parser for OldExcelParser {
    fn name(&self) -> &'static str {
        "extractous::OldExcelParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OldExcelParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.ms-excel.sheet.4",
            "application/vnd.ms-excel.workspace.4",
            "application/vnd.ms-excel.sheet.3",
            "application/vnd.ms-excel.workspace.3",
            "application/vnd.ms-excel.sheet.2",
        ]
    }

    /// Defers workbooks POI throws on and encrypted ones, which POI refuses.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        if crate::ole2::is_ole(input) {
            return Err(Unported(
                "a compound document typed as a bare old workbook, which POI opens as one"
                    .to_owned(),
            )
            .into());
        }
        let text = old::text(input)?;
        Ok(Box::new(move |xhtml| {
            write_old(&text, xhtml).map_err(Failure::from)
        }))
    }
}

impl Parser for XlsParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Fails encrypted workbooks the password does not open, and defers workbooks POI or Tika
    /// throws on, macros extracted when asked to, and any workbook when Java's default time
    /// zone, which dates are written in, cannot be read.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let zone = TimeZone::java_default()
            .map_err(|reason| format!("Java's default time zone cannot be read: {reason}"))?;
        let workbook = Workbook::read(input, configs, zone)?;
        Ok(Box::new(move |xhtml| workbook.write(xhtml)))
    }
}

/// A workbook read: the summary information and its content.
struct Workbook {
    summaries: Summaries,
    content: WorkbookContent,
}

/// What `ExcelExtractor` makes of a workbook.
enum WorkbookContent {
    /// A BIFF8 workbook read through Tika's listener.
    Biff8(Box<Biff8>),
    /// The text of a workbook before BIFF8, as `OldExcelExtractor.getText` gives it.
    Old(String),
}

/// A BIFF8 workbook read through Tika's listener, with the documents its `MBD` storages embed.
struct Biff8 {
    /// Whether a password was given, which `ExcelExtractor` asks the `PasswordProvider` for
    /// before it reads the workbook, marking the metadata.
    asks_password: bool,
    read: Extracted,
    pictures: Vec<Picture>,
    storages: Vec<Handed>,
}

/// A picture of the drawing groups, as `findPictures` hands it to `handleEmbeddedResource`.
struct Picture {
    data: Vec<u8>,
    media_type: &'static str,
    /// The sheets whose shapes show it, numbered from one.
    sheets: BTreeSet<usize>,
}

impl Workbook {
    fn read(
        input: &[u8],
        configs: &ParserConfigs,
        zone: &'static TimeZone,
    ) -> Result<Self, Declined> {
        if configs.office.extracts_macros() {
            return Err(
                Unported("macros are extracted, which this port does not do".to_owned()).into(),
            );
        }
        let file = Ole2::open(input).map_err(|Unreadable(reason)| {
            Unported(format!("POI cannot open the compound document: {reason}"))
        })?;
        if let Err(Unreadable(reason)) = Ole2::open_stream(input) {
            return Err(Unported(format!(
                "POI reads the compound document from a stream differently: {reason}"
            ))
            .into());
        }
        let root = file.root();
        if ole_directory_type(&file, root) != MediaType::named(MEDIA_TYPE) {
            return Err(Unported(
                "the compound document is no workbook to POI's container detection".to_owned(),
            )
            .into());
        }
        let summaries = Summaries::read(&file, root)?;
        let Some(name) = WORKBOOK_ENTRY_NAMES
            .into_iter()
            .find(|&name| file.child(root, name).is_some())
        else {
            if file.child(root, OLD_BOOK_ENTRY).is_none() {
                return Err(Unported(
                    "the compound document has no workbook entry, which Tika fails on".to_owned(),
                )
                .into());
            }
            let book = file
                .child_ignoring_case(root, OLD_BOOK_ENTRY)
                .filter(|&entry| file.kind(entry) == EntryKind::Document)
                .ok_or_else(|| {
                    Unported("the old workbook entry is no stream, which POI throws on".to_owned())
                })?;
            let stream = file.read(book).map_err(|Unreadable(reason)| {
                Unported(format!("POI cannot read the old workbook stream: {reason}"))
            })?;
            return Ok(Workbook {
                summaries,
                content: WorkbookContent::Old(old::text(&stream)?),
            });
        };
        let entry = file
            .child_ignoring_case(root, name)
            .expect("the workbook entry the root has");
        if file.kind(entry) != EntryKind::Document {
            return Err(Unported("the workbook entry is a directory".to_owned()).into());
        }
        let stream = file.read(entry).map_err(|Unreadable(reason)| {
            Unported(format!("POI cannot read the workbook stream: {reason}"))
        })?;
        let (read, pictures) = read_records(&stream, configs, zone)?;
        let mut storages = Vec::new();
        for child in file.children(root) {
            if file.name(child).starts_with(EMBEDDED_STORAGE_PREFIX)
                && file.kind(child) != EntryKind::Document
            {
                storages.extend(office_document(
                    &file,
                    child,
                    TikaMetadata::new(),
                    None,
                    configs,
                )?);
            }
        }
        Ok(Workbook {
            summaries,
            content: WorkbookContent::Biff8(Box::new(Biff8 {
                asks_password: configs.password.is_some(),
                read,
                pictures,
                storages,
            })),
        })
    }

    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        self.summaries.write(xhtml.metadata_mut());
        xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
        match self.content {
            WorkbookContent::Biff8(workbook) => workbook.write(xhtml),
            WorkbookContent::Old(text) => write_old(&text, xhtml).map_err(Failure::from),
        }
    }
}

/// `OldExcelParser.parse`: each line of the text in a paragraph.
fn write_old(text: &str, xhtml: &mut Xhtml) -> Result<(), Stop> {
    for line in lines(text) {
        xhtml.start_element("p")?;
        xhtml.characters(line)?;
        xhtml.end_element("p")?;
    }
    Ok(())
}

impl Biff8 {
    /// Writes the sheets as the listener does while it reads the records, then the documents of
    /// the `MBD` storages. Where the output stops taking text inside a sheet, Tika's listener
    /// stores the failure and reads no more records: the sheet is left unclosed, the text
    /// outside its cells is written again with its text dropped, and the metadata of the sheets
    /// is not set.
    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        if self.asks_password {
            xhtml.metadata_mut().set(PASSWORD_USED, "true");
        }
        let mut stopped = None;
        let mut outside = &self.read.outside;
        for page in &self.read.pages {
            if let Err(stop) = write_page(page, xhtml) {
                if stop == Stop::ReaderGone {
                    return Err(stop.into());
                }
                stopped = Some(stop);
                outside = &page.outside;
                break;
            }
        }
        write_outside(outside, xhtml)?;
        for picture in &self.pictures {
            let mut metadata = TikaMetadata::new();
            if !picture.sheets.is_empty() {
                write_sheet_numbers(&picture.sheets, &mut metadata);
            }
            metadata.set("Content-Type", picture.media_type);
            xhtml.parse_embedded(&picture.data, &metadata, Wrapping::PackageEntry)?;
        }
        if let Some(stop) = stopped {
            return Err(stop.into());
        }
        self.write_metadata(xhtml.metadata_mut());
        for storage in self.storages {
            storage.write(xhtml, Wrapping::PackageEntry)?;
        }
        Ok(())
    }

    /// `updateMetadata`.
    fn write_metadata(&self, metadata: &mut TikaMetadata) {
        let read = &self.read;
        if read.protected_sheet {
            metadata.set(excel::PROTECTED_WORKSHEET, "true");
        }
        if read.hidden_column {
            metadata.set(excel::HAS_HIDDEN_COLUMNS, "true");
        }
        if read.hidden_row {
            metadata.set(excel::HAS_HIDDEN_ROWS, "true");
        }
        if !read.comment_authors.is_empty() {
            for author in read.comment_authors.values() {
                metadata.add(excel::COMMENT_PERSONS, author.clone());
            }
            metadata.set(excel::HAS_COMMENTS, "true");
        }
        if !read.hidden_sheets.is_empty() {
            for name in &read.hidden_sheets {
                metadata.add(excel::HIDDEN_SHEET_NAMES, name.clone());
            }
            metadata.set(excel::HAS_HIDDEN_SHEETS, "true");
        }
        if !read.very_hidden_sheets.is_empty() {
            for name in &read.very_hidden_sheets {
                metadata.add(excel::VERY_HIDDEN_SHEET_NAMES, name.clone());
            }
            metadata.set(excel::HAS_VERY_HIDDEN_SHEETS, "true");
        }
    }
}

/// `processSheet`: the page, then the text found outside the cells, each in a div.
fn write_page(page: &Page, xhtml: &mut Xhtml) -> Result<(), Stop> {
    write_out(&page.table, xhtml)?;
    write_outside(&page.outside, xhtml)?;
    xhtml.end_element("div")
}

/// `processExtraText`.
fn write_outside(cells: &[Cell], xhtml: &mut Xhtml) -> Result<(), Stop> {
    let mut out = Vec::new();
    for cell in cells {
        out.push(Out::StartClass("div", "outside"));
        render(cell, &mut out);
        out.push(Out::End("div"));
    }
    write_out(&out, xhtml)
}

fn write_out(out: &[Out], xhtml: &mut Xhtml) -> Result<(), Stop> {
    for event in out {
        match event {
            Out::Start(name) => xhtml.start_element(name)?,
            Out::StartClass(name, class) => xhtml.start_element_with(name, &[("class", class)])?,
            Out::StartLink(address) => xhtml.start_element_with("a", &[("href", address)])?,
            Out::End(name) => xhtml.end_element(name)?,
            Out::Text(text) => xhtml.characters(text)?,
            Out::Element(name, text) => xhtml.element(name, text)?,
        }
    }
    Ok(())
}

/// `TikaHSSFListener.processFile`: the records, decrypted where the workbook is encrypted,
/// through the listener, the text left outside the sheets, and the pictures of the drawing
/// groups.
fn read_records(
    stream: &[u8],
    configs: &ParserConfigs,
    zone: &'static TimeZone,
) -> Result<(Extracted, Vec<Picture>), Declined> {
    let config = &configs.office;
    let throws = |Thrown(reason)| Unported(format!("POI or Tika throws on the workbook: {reason}"));
    let settings = Settings {
        headers_and_footers: config.includes_headers_and_footers(),
        shape_based_content: config.includes_shape_based_content(),
        phonetic_runs: config.concatenates_phonetic_runs(),
        missing_rows: config.includes_missing_rows(),
    };
    let mut records = Records::new(stream).map_err(throws)?;
    records.decrypt(configs)?;
    let mut listener = Listener::new(settings, zone);
    while let Some(event) = records.next().map_err(throws)? {
        match event {
            Event::Record { sid, record } => {
                if is_listened(sid, &settings) {
                    listener.process(sid, &record).map_err(throws)?;
                }
            }
            Event::DrawingGroup(index) => listener.drawing_group(index),
        }
    }
    let read = listener.finish().map_err(throws)?;
    let sheets_of_pictures = sheets_of_pictures(&read.sheet_drawings);
    let mut pictures = Vec::new();
    let mut stored = 0;
    for &index in &read.drawing_groups {
        let records = escher::decode(&records.drawing_groups[index]).map_err(throws)?;
        find_pictures(&records, &sheets_of_pictures, &mut stored, &mut pictures);
    }
    Ok((read, pictures))
}

/// `buildPicToSheetsMap`: the sheets, numbered from one, whose shapes show each stored picture,
/// by its number.
fn sheets_of_pictures(drawings: &BTreeMap<i32, Vec<u8>>) -> HashMap<i32, BTreeSet<usize>> {
    /// `buildPicToSheetsMap`'s reading of a sheet's drawing data: the records up to the first
    /// POI fails on, or whose length is not positive.
    fn sheet_records(data: &[u8]) -> Vec<Escher> {
        let mut records = Vec::new();
        let mut position = 0usize;
        while (position as i64) < data.len() as i64 - 8 {
            let Ok((record, length)) = escher::record_at(data, position) else {
                break;
            };
            if length <= 0 {
                break;
            }
            records.push(record);
            position += length as usize;
        }
        records
    }

    fn collect(record: &Escher, sheet: usize, map: &mut HashMap<i32, BTreeSet<usize>>) {
        if let Content::Options(options) = &record.content {
            for picture in options.pictures() {
                if picture > 0 {
                    map.entry(picture).or_default().insert(sheet);
                }
            }
        }
        for child in &record.children {
            collect(child, sheet, map);
        }
    }
    let mut map = HashMap::new();
    for (&index, data) in drawings {
        let sheet = index as usize + 1;
        for record in sheet_records(data) {
            collect(&record, sheet, &mut map);
        }
    }
    map
}

/// `findPictures`: each BSE record counted, and the picture of each that holds one.
fn find_pictures(
    records: &[Escher],
    sheets: &HashMap<i32, BTreeSet<usize>>,
    stored: &mut i32,
    pictures: &mut Vec<Picture>,
) {
    for record in records {
        if let Content::Stored(bse) = &record.content {
            *stored += 1;
            if let Some(blip) = &bse.blip {
                pictures.push(Picture {
                    data: escher::picture_data(&blip.data).to_vec(),
                    media_type: escher::media_type(blip.record_id),
                    sheets: sheets.get(stored).cloned().unwrap_or_default(),
                });
            }
        }
        find_pictures(&record.children, sheets, stored, pictures);
    }
}

#[cfg(test)]
mod tests;
