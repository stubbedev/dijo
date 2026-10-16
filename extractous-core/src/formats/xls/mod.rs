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
//! An encrypted workbook is decrypted with the password, or failed as Tika's path fails it where
//! the password does not open it. One in a format before BIFF8, one POI throws on, one with
//! documents embedded in `MBD` storages, and macros extracted when asked to, are left to Tika:
//! see [`XlsParser::defers`].

mod factory;
mod listener;
mod records;
mod stream;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use factory::{Event, Records};
use listener::{is_listened, render, Cell, Extracted, Listener, Out, Page, Settings};

use crate::config::ParserConfigs;
use crate::formats::embedded::Wrapping;
use crate::formats::escher::{self, Content, Escher};
use crate::formats::java_time::TimeZone;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::Summaries;
use crate::formats::poi::Thrown;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::xlsx::write_sheet_numbers;
use crate::formats::{Declined, Failure, Parser, Prepared, Unported, PASSWORD_USED};
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::{EntryKind, Ole2, Unreadable};

const MEDIA_TYPE: &str = "application/vnd.ms-excel";
/// `InternalWorkbook.WORKBOOK_DIR_ENTRY_NAMES`, in the order POI looks for them.
const WORKBOOK_ENTRY_NAMES: [&str; 4] = ["Workbook", "WORKBOOK", "BOOK", "WorkBook"];
/// The entry of a workbook before BIFF8, which Tika reads with its `OldExcelExtractor`.
const OLD_BOOK_ENTRY: &str = "Book";

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

impl Parser for XlsParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Fails encrypted workbooks the password does not open, and defers workbooks POI or Tika
    /// throws on, ones before BIFF8, ones with embedded documents, macros extracted when asked
    /// to, and any workbook when Java's default time zone, which dates are written in, cannot be
    /// read.
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

/// A workbook read through Tika's listener.
struct Workbook {
    summaries: Summaries,
    /// Whether a password was given, which `ExcelExtractor` asks the `PasswordProvider` for
    /// before it reads the workbook, marking the metadata.
    asks_password: bool,
    read: Extracted,
    pictures: Vec<Picture>,
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
        let Some(name) = WORKBOOK_ENTRY_NAMES
            .into_iter()
            .find(|&name| file.child(root, name).is_some())
        else {
            return Err(Unported(if file.child(root, OLD_BOOK_ENTRY).is_some() {
                "the workbook is from before BIFF8, which Tika reads with its OldExcelExtractor"
                    .to_owned()
            } else {
                "the compound document has no workbook entry, which Tika fails on".to_owned()
            })
            .into());
        };
        let entry = file
            .child_ignoring_case(root, name)
            .expect("the workbook entry the root has");
        if file.kind(entry) != EntryKind::Document {
            return Err(Unported("the workbook entry is a directory".to_owned()).into());
        }
        if file.children(root).any(|child| {
            file.name(child).starts_with("MBD") && file.kind(child) != EntryKind::Document
        }) {
            return Err(Unported(
                "the workbook embeds documents, which this port does not parse".to_owned(),
            )
            .into());
        }
        let stream = file.read(entry).map_err(|Unreadable(reason)| {
            Unported(format!("POI cannot read the workbook stream: {reason}"))
        })?;
        let (read, pictures) = read_records(&stream, configs, zone)?;
        Ok(Workbook {
            summaries: Summaries::read(&file, root)?,
            asks_password: configs.password.is_some(),
            read,
            pictures,
        })
    }

    /// Writes the sheets as the listener does while it reads the records. Where the output
    /// stops taking text inside a sheet, Tika's listener stores the failure and reads no more
    /// records: the sheet is left unclosed, the text outside its cells is written again with
    /// its text dropped, and the metadata of the sheets is not set.
    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        self.summaries.write(xhtml.metadata_mut());
        xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
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
