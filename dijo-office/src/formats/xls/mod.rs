//! Excel 97–2003 workbooks (XLS, BIFF8) as Tika's `OfficeParser` reads them with its Excel
//! extractor, `ExcelExtractor`: the summary information as metadata, then the workbook stream's
//! records through POI's event model into Tika's `TikaHSSFListener`, a page per sheet with its
//! cells in a table and the text found outside the cells after it, the pictures the drawing
//! groups store as embedded documents, and the hidden sheets, rows, columns, protection and
//! comment authors as metadata, then the documents embedded in `MBD` storages.
//!
//! Numbers are written with one formatter, in the `en_US` locale, with dates as their cells
//! hold them in the workbook's date system; see [`crate::formats::data_format`]. Tika writes
//! dates and the general format with its `TikaExcelDataFormatter` and other formats with POI's
//! own `DataFormatter`, dates in Java's default time zone and the 1900 date system (#92, #107).
//! Where `DataFormatter` writes a number otherwise than Excel displays it, such as the literal
//! texts of a date format, `?` digits or the zero section of an accounting format, the
//! formatter writes it as Excel does (#186).
//!
//! The documents embedded in `MBD` storages follow, handed on as `handleEmbeddedOfficeDoc` hands
//! them. A workbook before BIFF8, whose stream is named "Book", is read as POI's
//! `OldExcelExtractor` reads it for Tika's `OldExcelParser`: a paragraph per line of its text.
//!
//! An encrypted workbook is decrypted with the password, or failed as Tika fails it where
//! the password does not open it. One POI or Tika's listener throws on fails as Tika
//! fails it. The macros follow when asked for.

mod factory;
mod listener;
mod old;
mod records;
mod stream;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::BufWriter;

use factory::{write_drawing_group, Event, Records};
use listener::{is_listened, write_outside, Found, Interrupted, Listener, Nowhere, Pass, Settings};
use stream::{Mark, RecordStream};

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::lines;
use crate::formats::compound::detected_otherwise;
use crate::formats::embedded::Wrapping;
use crate::formats::escher::{self, Escher};
use crate::formats::metadata::write_sheet_numbers;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::office;
use crate::formats::ole2::vba::Macros;
use crate::formats::ole2::{office_document, Handed, Summaries};
use crate::formats::poi::{written, Halt, Thrown, Unread, ILLEGAL_ARGUMENT, OFFICE_PARSER};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{
    unless_different, Decline, Difference, Failure, Footprint, Parser, Prepared, Unported,
    PASSWORD_USED,
};
use crate::input::{self, Pages, Paging, Spool};
use crate::ole2::{Content, EntryId, EntryKind, Ole2, Unreadable};

const MEDIA_TYPE: &str = "application/vnd.ms-excel";
/// `POIFSDocumentType.XLR`: a Works 7.0 spreadsheet, which `OfficeParser` reads as a workbook.
const WORKS_SPREADSHEET: &str = "application/x-tika-msworks-spreadsheet";
/// The types `OfficeParser` reads with its Excel extractor.
const MEDIA_TYPES: [&str; 2] = [MEDIA_TYPE, WORKS_SPREADSHEET];
/// `InternalWorkbook.WORKBOOK_DIR_ENTRY_NAMES`, in the order POI looks for them.
const WORKBOOK_ENTRY_NAMES: [&str; 4] = ["Workbook", "WORKBOOK", "BOOK", "WorkBook"];
/// Tika's parser of the workbooks of Excel 2 to 4 stored bare.
const OLD_EXCEL_PARSER: &str = "org.apache.tika.parser.microsoft.OldExcelParser";
/// The stream `OldExcelExtractor` reads of a compound document without a "Book" stream.
const OLD_WORKBOOK_ENTRY: &str = "Workbook";
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

pub struct XlsParser;

/// Tika's `OldExcelParser` of the workbooks of Excel 2 to 4 stored bare, outside a compound
/// document: the text of `OldExcelExtractor`, a paragraph per line.
pub struct OldExcelParser;

impl Parser for OldExcelParser {
    fn name(&self) -> &'static str {
        "dijo::OldExcelParser"
    }
    fn tika_name(&self) -> &'static str {
        OLD_EXCEL_PARSER
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

    /// It reads the workbook's records in place, twice: once as far as deciding whether it
    /// extracts the workbook, then as it writes it, the text of each record written as it
    /// comes, where `OldExcelExtractor.getText` gives all of it together.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Fails workbooks POI throws on, encrypted ones among them, which POI refuses. A compound
    /// document is read as `OldExcelExtractor` opens one: its "Book" stream, else its
    /// "Workbook" stream.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let declined = |unread: Unread| unread.declined_by(OLD_EXCEL_PARSER);
        if !crate::ole2::is_ole(input) {
            plan_old(input, OLD_EXCEL_PARSER)?;
            return Ok(Box::new(move |xhtml| write_old(input, xhtml)));
        }
        let stream = old_book(input).map_err(declined)?;
        plan_old(&stream, OLD_EXCEL_PARSER)?;
        Ok(Box::new(move |xhtml| write_old(&stream, xhtml)))
    }
}

/// `OldExcelExtractor.open(directory)` of a compound document: its "Book" stream, else its
/// "Workbook" stream, each looked for ignoring case, in place where it lies in one run.
fn old_book(input: &[u8]) -> Result<Cow<'_, [u8]>, Unread> {
    let file = Ole2::open_stream(input).map_err(|Unreadable(reason)| {
        Thrown::new(format!(
            "POIFSFileSystem cannot open the compound document: {reason}"
        ))
    })?;
    let root = file.root();
    let stream = |name: &str| {
        file.child_ignoring_case(root, name)
            .filter(|&entry| file.kind(entry) == EntryKind::Document)
    };
    let book = match stream(OLD_BOOK_ENTRY) {
        Some(book) => book,
        None => match file.child_ignoring_case(root, OLD_WORKBOOK_ENTRY) {
            Some(entry) if file.kind(entry) == EntryKind::Document => entry,
            Some(_) => {
                return Err(Thrown::runtime(
                    ILLEGAL_ARGUMENT,
                    "Did not have an Excel 5/95 Book stream: the Workbook entry, a directory",
                )
                .into())
            }
            None => {
                return Err(
                    Thrown::new("the compound document has no Book or Workbook stream").into(),
                )
            }
        },
    };
    Ok(file
        .content(book)
        .map_err(|Unreadable(reason)| {
            Thrown::new(format!("cannot read the old workbook stream: {reason}"))
        })?
        .bytes())
}

impl Parser for XlsParser {
    fn name(&self) -> &'static str {
        "dijo::OfficeParser"
    }
    fn tika_name(&self) -> &'static str {
        OFFICE_PARSER
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    /// It reads the workbook stream in place, twice: once to plan it, holding an index of the
    /// shared strings and of the drawing groups, the hyperlinks of the sheets and what the
    /// metadata records; once to write it, a sheet whose cells come in order as they come, and
    /// the pictures and embedded documents one at a time, each spooled. What still grows is
    /// Tika's own: the text outside the cells of a sheet, until the sheet ends; and the cells of
    /// a sheet that come out of order, which Tika's listener sorts. A workbook before BIFF8 is
    /// read as [`OldExcelParser`] reads one.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Fails encrypted workbooks the password does not open and workbooks POI or Tika throws on,
    /// and writes the macros after the workbook when asked to. A compound document typed a workbook that
    /// POI's container detection names a kind of no content, such as an MS Graph chart, is read
    /// as `OfficeParser` reads it once it has detected that kind: its summary information only.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        if let Some(parser) = detected_otherwise(input, &MEDIA_TYPES) {
            return parser.prepare_paged(input, pages, metadata, configs);
        }
        let mut workbook = Workbook::read(input, pages, configs)?;
        let difference = workbook.difference.take();
        let macros = workbook.macros.take();
        Macros::followed_by(
            unless_different(Box::new(move |xhtml| workbook.write(xhtml)), difference),
            macros,
        )
    }
}

/// Counts what `records` have read since `read`, so that the pages of a mapped workbook are
/// released as they are read on.
fn count_read(paging: &Paging, records: &Records, read: &mut usize) {
    let at = records.stream().position();
    paging.read(at.saturating_sub(*read));
    *read = at;
}

/// How the parser declines a workbook POI throws on for `reason`.
pub(super) fn poi_throws(reason: &str) -> Decline<'static> {
    Unread::from(Thrown::new(reason)).declined()
}

/// A workbook read: the summary information, the type `OfficeParser` sets, its content, and
/// where the native reading of it differs from Tika's, if it does.
struct Workbook<'a> {
    summaries: Summaries,
    media_type: &'static str,
    difference: Option<Difference>,
    /// The macros, where they are asked for.
    macros: Option<Macros>,
    content: WorkbookContent<'a>,
}

/// What `ExcelExtractor` makes of a workbook.
enum WorkbookContent<'a> {
    /// A BIFF8 workbook read through Tika's listener.
    Biff8(Box<Biff8<'a>>),
    /// The workbook stream of a workbook before BIFF8, which `OldExcelExtractor.getText`
    /// reads, in place where it lies in one run.
    Old(Cow<'a, [u8]>),
}

/// A BIFF8 workbook planned through Tika's listener, to be written through it, with the
/// documents its `MBD` storages embed.
struct Biff8<'a> {
    file: Ole2<'a>,
    /// The workbook stream.
    stream: Content<'a>,
    configs: &'a ParserConfigs,
    pages: Pages<'a>,
    /// Whether a password was given, which `ExcelExtractor` asks the `PasswordProvider` for
    /// before it reads the workbook, marking the metadata.
    asks_password: bool,
    planned: Planned,
    /// The storages documents are embedded in.
    storages: Vec<EntryId>,
}

/// What planning a workbook found: the metadata of its sheets and how each is written, its
/// drawing groups, by where the records of each start, and the sheets whose shapes show each
/// stored picture, by its number.
struct Planned {
    found: Found,
    drawing_groups: Vec<Vec<Mark>>,
    sheets_of_pictures: HashMap<i32, BTreeSet<usize>>,
}

/// A picture of the drawing groups, as `findPictures` hands it to `handleEmbeddedResource`.
struct Picture<'d> {
    data: &'d [u8],
    media_type: &'static str,
    /// The sheets whose shapes show it, numbered from one.
    sheets: BTreeSet<usize>,
}

impl<'a> Workbook<'a> {
    fn read(
        input: &'a [u8],
        pages: Pages<'a>,
        configs: &'a ParserConfigs,
    ) -> Result<Self, Decline<'static>> {
        let (file, media_type) =
            office::open_as(input, &MEDIA_TYPES, "workbook").map_err(Unread::declined)?;
        let root = file.root();
        let summaries = Summaries::read(&file, root)?;
        let Some(name) = WORKBOOK_ENTRY_NAMES
            .into_iter()
            .find(|&name| file.child(root, name).is_some())
        else {
            if file.child(root, OLD_BOOK_ENTRY).is_none() {
                return Err(poi_throws("the compound document has no workbook entry"));
            }
            let book = file
                .child_ignoring_case(root, OLD_BOOK_ENTRY)
                .filter(|&entry| file.kind(entry) == EntryKind::Document)
                .ok_or_else(|| poi_throws("the old workbook entry is no stream"))?;
            let stream = file
                .content(book)
                .map_err(|Unreadable(reason)| {
                    poi_throws(&format!("cannot read the old workbook stream: {reason}"))
                })?
                .bytes();
            plan_old(&stream, OFFICE_PARSER)?;
            return Ok(Workbook {
                summaries,
                media_type,
                difference: None,
                macros: Macros::of(&file, configs),
                content: WorkbookContent::Old(stream),
            });
        };
        let entry = file
            .child_ignoring_case(root, name)
            .expect("the workbook entry the root has");
        if file.kind(entry) != EntryKind::Document {
            return Err(poi_throws("the workbook entry is a directory"));
        }
        let stream = file.content(entry).map_err(|Unreadable(reason)| {
            poi_throws(&format!("cannot read the workbook stream: {reason}"))
        })?;
        let planned = plan(stream.clone(), pages, configs)?;
        let mut storages = Vec::new();
        let mut difference = None;
        for child in file.children(root) {
            if file.name(child).starts_with(EMBEDDED_STORAGE_PREFIX)
                && file.kind(child) != EntryKind::Document
            {
                let handed = office_document(&file, child, TikaMetadata::new(), None, configs)?;
                difference = difference.or_else(|| handed.as_ref().and_then(Handed::difference));
                storages.push(child);
            }
        }
        Ok(Workbook {
            summaries,
            media_type,
            difference,
            macros: Macros::of(&file, configs),
            content: WorkbookContent::Biff8(Box::new(Biff8 {
                file,
                stream,
                configs,
                pages,
                asks_password: configs.password.is_some(),
                planned,
                storages,
            })),
        })
    }

    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        self.summaries.write(xhtml.metadata_mut());
        xhtml.metadata_mut().set("Content-Type", self.media_type);
        match self.content {
            WorkbookContent::Biff8(workbook) => workbook.write(xhtml),
            WorkbookContent::Old(stream) => write_old(&stream, xhtml),
        }
    }
}

/// `OldExcelExtractor.getText` of an old workbook stream read as far as its end, its text let
/// go, as reading it decides whether it is extracted.
fn plan_old(stream: &[u8], tika_name: &'static str) -> Result<(), Decline<'static>> {
    let declined = |unread: Unread| unread.declined_by(tika_name);
    let mut texts = old::Texts::new(stream).map_err(declined)?;
    while texts.next().map_err(declined)?.is_some() {}
    Ok(())
}

/// `OldExcelParser.parse`: each line of the text in a paragraph, the text of each record read
/// and written as it comes. A record's text ends with the line feed `getText` ends its line
/// with, so that its lines are those of the whole text.
fn write_old(stream: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
    let unplanned_old = |unread: Unread| unplanned(&format!("{unread:?}"));
    let mut texts = old::Texts::new(stream).map_err(unplanned_old)?;
    while let Some(mut text) = texts.next().map_err(unplanned_old)? {
        text.push('\n');
        for line in lines(&text) {
            xhtml.start_element("p")?;
            xhtml.characters(line)?;
            xhtml.end_element("p")?;
        }
    }
    Ok(())
}

/// A failure of the write of a workbook its plan read without one, which reads it alike.
fn unplanned(reason: &str) -> Failure {
    Failure::Malformed(format!(
        "the workbook failed when written, where its plan read it: {reason}"
    ))
}

impl Biff8<'_> {
    /// Writes the sheets as the listener does while it reads the records, then the documents of
    /// the `MBD` storages. Where the output stops taking text inside a sheet, the rest of the
    /// sheets is left out, but the pictures and the metadata of the sheets are not. Tika's
    /// listener stores the failure and reads no more records: the text outside the sheet's
    /// cells is written again with its text dropped, and the metadata of the sheets is not set
    /// (#107).
    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        if self.asks_password {
            xhtml.metadata_mut().set(PASSWORD_USED, "true");
        }
        let thrown = |thrown: Thrown| unplanned(&thrown.to_string());
        let mut records = Records::new(self.stream.clone()).map_err(thrown)?;
        records
            .decrypt(self.configs)
            .map_err(|_| unplanned("its encryption no longer opens"))?;
        let settings = settings(self.configs);
        let planned = &self.planned;
        let mut listener = Listener::new(settings, Pass::Write(&planned.found.plans));
        let mut stopped = None;
        let paging = Paging::new(self.pages);
        let mut read = 0;
        while let Some(event) = records.next().map_err(thrown)? {
            count_read(&paging, &records, &mut read);
            let processed = match event {
                Event::Record { sid, record } if is_listened(sid, &settings) => {
                    listener.process(sid, &record, records.stream(), xhtml)
                }
                Event::Record { .. } => Ok(()),
                Event::DrawingGroup(index) => {
                    listener.drawing_group(index);
                    Ok(())
                }
            };
            match processed {
                Ok(()) => {}
                Err(Interrupted::Stopped(Stop::ReaderGone)) => return Err(Stop::ReaderGone.into()),
                Err(Interrupted::Stopped(stop)) => {
                    stopped = Some(stop);
                    break;
                }
                Err(Interrupted::Thrown(thrown)) => return Err(unplanned(&thrown.to_string())),
                Err(Interrupted::Unported(Unported(reason))) => return Err(unplanned(&reason)),
            }
        }
        if stopped.is_none() {
            let (_, outside) = listener.finish().map_err(thrown)?;
            write_outside(&outside, xhtml)?;
        }
        let mut stored = 0;
        for &index in &planned.found.drawing_groups {
            let group = written(spool_drawing_group(
                records.stream(),
                &planned.drawing_groups[index],
            ))?;
            let escher = escher::decode(&group).map_err(thrown)?;
            find_pictures(
                &escher,
                &planned.sheets_of_pictures,
                &mut stored,
                &mut |picture| {
                    let mut metadata = TikaMetadata::new();
                    if !picture.sheets.is_empty() {
                        write_sheet_numbers(&picture.sheets, &mut metadata);
                    }
                    metadata.set("Content-Type", picture.media_type);
                    xhtml.parse_embedded(picture.data, &metadata, Wrapping::PackageEntry)
                },
            )?;
        }
        write_metadata(&planned.found, xhtml.metadata_mut());
        if let Some(stop) = stopped {
            return Err(stop.into());
        }
        for storage in self.storages {
            let handed =
                office_document(&self.file, storage, TikaMetadata::new(), None, self.configs)
                    .map_err(|Unported(reason)| unplanned(&reason))?;
            if let Some(handed) = handed {
                handed.write(xhtml, Wrapping::PackageEntry)?;
            }
        }
        Ok(())
    }
}

/// `updateMetadata`.
fn write_metadata(found: &Found, metadata: &mut TikaMetadata) {
    if found.protected_sheet {
        metadata.set(excel::PROTECTED_WORKSHEET, "true");
    }
    if found.hidden_column {
        metadata.set(excel::HAS_HIDDEN_COLUMNS, "true");
    }
    if found.hidden_row {
        metadata.set(excel::HAS_HIDDEN_ROWS, "true");
    }
    if !found.comment_authors.is_empty() {
        for author in found.comment_authors.values() {
            metadata.add(excel::COMMENT_PERSONS, author.clone());
        }
        metadata.set(excel::HAS_COMMENTS, "true");
    }
    if !found.hidden_sheets.is_empty() {
        for name in &found.hidden_sheets {
            metadata.add(excel::HIDDEN_SHEET_NAMES, name.clone());
        }
        metadata.set(excel::HAS_HIDDEN_SHEETS, "true");
    }
    if !found.very_hidden_sheets.is_empty() {
        for name in &found.very_hidden_sheets {
            metadata.add(excel::VERY_HIDDEN_SHEET_NAMES, name.clone());
        }
        metadata.set(excel::HAS_VERY_HIDDEN_SHEETS, "true");
    }
}

/// What of the workbook Tika's Office parser settings have the listener write.
fn settings(configs: &ParserConfigs) -> Settings {
    let config = &configs.office;
    Settings {
        headers_and_footers: config.includes_headers_and_footers(),
        shape_based_content: config.includes_shape_based_content(),
        phonetic_runs: config.concatenates_phonetic_runs(),
        missing_rows: config.includes_missing_rows(),
    }
}

/// `TikaHSSFListener.processFile` as far as deciding whether the workbook is read: the
/// records, decrypted where the workbook is encrypted, through the listener, which plans how
/// each sheet is written and finds what the workbook's metadata records, and the pictures of
/// the drawing groups.
fn plan(
    stream: Content<'_>,
    pages: Pages<'_>,
    configs: &ParserConfigs,
) -> Result<Planned, Decline<'static>> {
    let settings = settings(configs);
    let mut records = Records::new(stream)?;
    records.decrypt(configs)?;
    let mut listener = Listener::new(settings, Pass::Plan);
    let paging = Paging::new(pages);
    let mut read = 0;
    while let Some(event) = records.next()? {
        count_read(&paging, &records, &mut read);
        match event {
            Event::Record { sid, record } => {
                if is_listened(sid, &settings) {
                    match listener.process(sid, &record, records.stream(), &mut Nowhere) {
                        Ok(()) => {}
                        Err(Interrupted::Thrown(thrown)) => return Err(thrown.into()),
                        Err(Interrupted::Unported(unported)) => return Err(unported.into()),
                        Err(Interrupted::Stopped(stop)) => {
                            unreachable!("planning writes nowhere, which stops nothing: {stop:?}")
                        }
                    }
                }
            }
            Event::DrawingGroup(index) => listener.drawing_group(index),
        }
    }
    let (found, _) = listener.finish()?;
    let sheets_of_pictures = sheets_of_pictures(&found.sheet_drawings);
    let stream = records.stream().clone();
    let drawing_groups = records.into_drawing_groups();
    for &index in &found.drawing_groups {
        let group =
            spool_drawing_group(&stream, &drawing_groups[index]).map_err(|halt| match halt {
                Halt::Declined(declined) => declined,
                Halt::Written(Failure::Extraction(error)) => Decline::Fails(error),
                Halt::Written(failure) => {
                    unreachable!("spooling a drawing group fails only to write: {failure:?}")
                }
            })?;
        escher::validate(&group)?;
    }
    Ok(Planned {
        found,
        drawing_groups,
        sheets_of_pictures,
    })
}

/// The data of a drawing group whose records start at `starts`, read from `stream` and spooled
/// as an embedded document is: in memory up to what is read whole, past it to a file.
fn spool_drawing_group(
    stream: &RecordStream<'_>,
    starts: &[Mark],
) -> Result<input::Input<'static>, Halt<Failure>> {
    let failed = |error: std::io::Error| {
        Halt::Written(Failure::Extraction(crate::Error::IoError(format!(
            "Could not spool a drawing group of the workbook: {error}"
        ))))
    };
    let mut spool = BufWriter::new(Spool::new(None));
    write_drawing_group(stream, starts, &mut spool)?;
    spool
        .into_inner()
        .map_err(|error| failed(error.into_error()))?
        .finish()
        .map_err(failed)
}

/// `buildPicToSheetsMap`: the sheets, numbered from one, whose shapes show each stored picture,
/// by its number.
fn sheets_of_pictures(drawings: &BTreeMap<i32, Vec<u8>>) -> HashMap<i32, BTreeSet<usize>> {
    /// `buildPicToSheetsMap`'s reading of a sheet's drawing data: the records up to the first
    /// POI fails on, or whose length is not positive.
    fn sheet_records(data: &[u8]) -> Vec<Escher<'_>> {
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
        if let escher::Content::Options(options) = &record.content {
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

/// `findPictures`: each BSE record counted, and the picture of each that holds one handed to
/// `each`.
fn find_pictures<'d, E>(
    records: &'d [Escher<'_>],
    sheets: &HashMap<i32, BTreeSet<usize>>,
    stored: &mut i32,
    each: &mut impl FnMut(Picture<'d>) -> Result<(), E>,
) -> Result<(), E> {
    for record in records {
        if let escher::Content::Stored(bse) = &record.content {
            *stored += 1;
            if let Some(blip) = &bse.blip {
                each(Picture {
                    data: escher::picture_data(&blip.data),
                    media_type: escher::media_type(blip.record_id),
                    sheets: sheets.get(stored).cloned().unwrap_or_default(),
                })?;
            }
        }
        find_pictures(&record.children, sheets, stored, each)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
