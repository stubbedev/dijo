//! Excel binary workbooks (XLSB) as Tika's `OOXMLParser` reads them with its
//! `XSSFBExcelExtractorDecorator` on POI's binary readers: the package's metadata and the path
//! the workbook was saved at, then each sheet as a table under its name, its comments after the
//! table, its headers and footers, the text of its drawings and its links, then the pictures and
//! attachments it embeds and the thumbnail. Cells are read from POI's binary records
//! (`XSSFBSheetHandler`), numbers written as POI's `DataFormatter` writes them.

use std::collections::{BTreeMap, BTreeSet};

use super::cell::CellAddress;
use super::header_footer;
use super::sheet::{SheetOutput, SheetSettings};
use super::{excel, refuse_strict, sheet_kind, Sheet, Workbook};
use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::data_format::{builtin_format, error_text, DataFormatter, FormatError};
use crate::formats::metadata::office::ORIGINAL_RESOURCE_NAME;
use crate::formats::metadata::write_sheet_numbers;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::extractor::{self, kind, name};
use crate::formats::ooxml::{Package, Properties, Relationship, Unported, HYPERLINK_RELATIONSHIP};
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Paging};
use crate::java::is_blank;
use crate::text::charset::Charset;

/// The content type of a binary workbook part, `XSSFRelation.XLSB_BINARY_WORKBOOK`'s.
const BINARY_WORKBOOK_TYPE: &str = "application/vnd.ms-excel.sheet.binary.macroEnabled.main";
/// `XSSFBRelation.STYLES_BINARY`'s content type.
const STYLES_TYPE: &str = "application/vnd.ms-excel.styles";
/// `XSSFBRelation.SHARED_STRINGS_BINARY`'s content type.
const SHARED_STRINGS_TYPE: &str = "application/vnd.ms-excel.sharedStrings";
/// `XSSFBParser.MAX_RECORD_LENGTH`.
const MAX_RECORD_LENGTH: u64 = 1_000_000;
/// The highest row `XSSFBSheetHandler` reads.
const MAX_ROW: i64 = 0x0010_0000;
/// The length of a cell's header: its column and style.
const CELL_HEADER: usize = 8;
/// The byte of flags a `RichStr` starts with, before its text.
const RICH_STRING_FLAGS: usize = 1;
/// The relationship types of `XSSFBReader.WORKSHEET_RELS`.
const SHEET_KINDS: [&str; 5] = [
    sheet_kind::WORKSHEET,
    sheet_kind::CHARTSHEET,
    sheet_kind::MACRO_SHEET,
    "http://schemas.microsoft.com/office/2006/relationships/xlIntlMacrosheet",
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/dialogsheet",
];

/// `XSSFBRecordType`, the records the readers handle, by id.
mod record {
    pub(super) const ROW_HEADER: u32 = 0;
    pub(super) const CELL_BLANK: u32 = 1;
    pub(super) const CELL_RK: u32 = 2;
    pub(super) const CELL_ERROR: u32 = 3;
    pub(super) const CELL_BOOL: u32 = 4;
    pub(super) const CELL_REAL: u32 = 5;
    pub(super) const CELL_ST: u32 = 6;
    pub(super) const CELL_ISST: u32 = 7;
    pub(super) const CELL_RICH_STRING: u32 = 62;
    pub(super) const FORMULA_STRING: u32 = 8;
    pub(super) const FORMULA_NUMBER: u32 = 9;
    pub(super) const FORMULA_BOOL: u32 = 10;
    pub(super) const FORMULA_ERROR: u32 = 11;
    pub(super) const SST_ITEM: u32 = 19;
    pub(super) const FORMAT: u32 = 44;
    pub(super) const XF: u32 = 47;
    pub(super) const END_SHEET_DATA: u32 = 146;
    pub(super) const BUNDLE_SHEET: u32 = 156;
    pub(super) const BEGIN_HEADER_FOOTER: u32 = 479;
    pub(super) const BEGIN_FORMATS: u32 = 615;
    pub(super) const END_FORMATS: u32 = 616;
    pub(super) const BEGIN_CELL_XFS: u32 = 617;
    pub(super) const END_CELL_XFS: u32 = 618;
    pub(super) const COMMENT_AUTHOR: u32 = 632;
    pub(super) const BEGIN_COMMENT: u32 = 635;
    pub(super) const END_COMMENT: u32 = 636;
    pub(super) const COMMENT_TEXT: u32 = 637;
    pub(super) const ABSOLUTE_PATH: u32 = 2071;
}

/// Tika's `OOXMLParser` of binary workbooks.
pub struct XlsbParser;

impl Parser for XlsbParser {
    fn name(&self) -> &'static str {
        "extractous::OOXMLParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/vnd.ms-excel.sheet.binary.macroenabled.12"]
    }

    /// The sheets are read a record at a time as they are written; the styles, the shared
    /// strings and a sheet's comments, which cells refer to, are read whole first, as POI does.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    /// Defers workbooks with OLE objects Tika opens, macros it extracts when asked to, a strict
    /// or other main document, and packages POI does not open as this port does.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let package = Package::open(input, pages)?;
        refuse_strict(&package, &[BINARY_WORKBOOK_TYPE])?;
        let workbook = open(package, &configs.office).map_err(|Unported(reason)| reason)?;
        Ok(Box::new(move |xhtml| workbook.write(xhtml)))
    }
}

/// A binary workbook as far as this port reads it before writing: its package and properties,
/// and its sheets, unless reading them fails as Tika's does.
struct Binary<'a> {
    workbook: Workbook<'a>,
    properties: Properties,
    sheets: Result<(), Failure>,
}

fn open<'a>(package: Package<'a>, config: &'a OfficeParserConfig) -> Result<Binary<'a>, Unported> {
    let Some(core) =
        Package::relationships_of(&package.relationships, kind::OFFICE_DOCUMENT).next()
    else {
        return Err(Unported(
            "the package has no transitional main document, which POI's XSSFBReader refuses"
                .to_owned(),
        ));
    };
    let workbook = package.package_part(core).ok_or_else(|| {
        Unported("the package's main document is missing, which Tika fails on".to_owned())
    })?;
    let content_type = &package.parts[workbook].content_type;
    if content_type != BINARY_WORKBOOK_TYPE {
        return Err(Unported(format!(
            "the package's main part has the content type {content_type:?}, which Tika does \
             not read as a binary workbook"
        )));
    }
    let properties = Properties::read(&package)?;
    let mut binary = Binary {
        workbook: Workbook {
            package,
            config,
            workbook,
            sheets: Vec::new(),
            styles: None,
            shared_strings: None,
        },
        properties,
        sheets: Ok(()),
    };
    match sheet_refs(&binary.workbook) {
        Ok(sheets) => binary.workbook.sheets = sheets,
        Err(failure) => binary.sheets = Err(failure),
    }
    let workbook = &binary.workbook;
    for source in workbook.main_parts() {
        for relationship in workbook.relationships(source) {
            extractor::check_embedded(&workbook.package, config, source, relationship)?;
        }
    }
    for sheet in &workbook.sheets {
        for kind in [sheet_kind::DRAWING, sheet_kind::VML_DRAWING] {
            for drawing in workbook.internal_related(sheet.part, kind) {
                let images =
                    Package::relationships_of(workbook.relationships(drawing), kind::IMAGE);
                for image in images.filter(|image| image.is_internal()) {
                    if image.part.is_none() {
                        return Err(Unported(format!(
                            "{} draws the missing {}, which Tika records failing on",
                            workbook.package.name(drawing),
                            image.target_uri()
                        )));
                    }
                }
            }
        }
    }
    Ok(binary)
}

/// `SheetIterator`'s sheets: those the workbook's records name with a relationship to a sheet
/// part, in their order; one whose part is missing fails, as `next()` throws.
fn sheet_refs(workbook: &Workbook) -> Result<Vec<Sheet>, Failure> {
    let relationships = workbook.relationships(workbook.workbook);
    let mut parts: BTreeMap<&str, Option<usize>> = BTreeMap::new();
    for relationship in relationships {
        if SHEET_KINDS.contains(&relationship.kind.as_str()) {
            parts.insert(&relationship.id, sheet_part(relationship)?);
        }
    }
    let bytes = workbook.package.read(workbook.workbook)?;
    let mut sheets = Vec::new();
    parse_records(&bytes, &|_| true, |id, data| {
        if id == record::BUNDLE_SHEET {
            if let Some(sheet) = bundle_sheet(data)? {
                sheets.push(sheet);
            }
        }
        Ok(())
    })?;
    sheets
        .into_iter()
        .map(|(id, name)| match parts.get(id.as_str()) {
            Some(Some(part)) => Ok(Sheet { name, part: *part }),
            _ => Err(Failure::Malformed(format!(
                "POIXMLException: Failed to find sheet package for sheetId={id}"
            ))),
        })
        .collect()
}

/// The part a sheet relationship leads to, as `getPart(createPartName(getTargetURI()))` finds
/// it; an external one, whose URI is no part name, fails.
fn sheet_part(relationship: &Relationship) -> Result<Option<usize>, Failure> {
    if !relationship.is_internal() {
        return Err(Failure::Malformed(format!(
            "InvalidFormatException: the sheet relationship {} leads out of the package",
            relationship.id
        )));
    }
    Ok(relationship.part)
}

/// `SheetRefLoader.addWorksheet`: the relationship id and name of a `BrtBundleSh` record, unless
/// its id is blank, read as the current format, else as Excel's beta format where that reads
/// the whole record.
fn bundle_sheet(data: &[u8]) -> Result<Option<(String, String)>, Failure> {
    match bundle_sheet_at(data, 4) {
        Ok((sheet, _)) => Ok(sheet),
        Err(Thrown::Parse(reason)) => match bundle_sheet_at(data, 8) {
            Ok((sheet, end)) if end == data.len() => Ok(sheet),
            Ok(_) => Err(Failure::Malformed(reason)),
            Err(thrown) => Err(thrown.into()),
        },
        Err(thrown) => Err(thrown.into()),
    }
}

/// The sheet of a `BrtBundleSh` record whose tab id is at `offset`, and where its name ends.
fn bundle_sheet_at(
    data: &[u8],
    offset: usize,
) -> Result<(Option<(String, String)>, usize), Thrown> {
    let tab = u32_at(data, offset)?;
    if !(1..=0xFFFF).contains(&tab) {
        return Err(Thrown::Parse(format!(
            "XSSFBParseException: table id out of range: {tab}"
        )));
    }
    let (id, read) = wide_string(data, offset + 4)?;
    let (name, name_read) = wide_string(data, offset + 4 + read)?;
    let end = offset + 4 + read + name_read;
    Ok(((!is_blank(&id)).then_some((id, name)), end))
}

/// What POI throws reading a record: its own parse exception, which `addWorksheet` retries in
/// the beta format, or any other.
#[derive(Debug)]
enum Thrown {
    Parse(String),
    Other(String),
}

impl From<Thrown> for Failure {
    fn from(thrown: Thrown) -> Self {
        match thrown {
            Thrown::Parse(reason) | Thrown::Other(reason) => Failure::Malformed(reason),
        }
    }
}

/// `LittleEndian.getUInt`.
fn u32_at(data: &[u8], at: usize) -> Result<u32, Thrown> {
    data.get(at..at + 4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
        .ok_or_else(|| {
            Thrown::Other(format!(
                "ArrayIndexOutOfBoundsException: Index {} out of bounds for length {}",
                at + 3,
                data.len()
            ))
        })
}

/// `XSSFBUtils.readXLWideString`: a string of UTF-16 units after its 32-bit count, and the
/// bytes it takes.
fn wide_string(data: &[u8], offset: usize) -> Result<(String, usize), Thrown> {
    let count = u32_at(data, offset)?;
    let length = (count as i32).wrapping_mul(2);
    let start = offset + 4;
    if i64::from(start as i32) + i64::from(length) > data.len() as i64 {
        return Err(Thrown::Parse(
            "XSSFBParseException: trying to read beyond data length".to_owned(),
        ));
    }
    let length = usize::try_from(length).map_err(|_| {
        Thrown::Other(format!(
            "StringIndexOutOfBoundsException: offset {start}, count {length}, length {}",
            data.len()
        ))
    })?;
    let text = Charset::named("UTF-16LE")
        .decode(&data[start..start + length])
        .expect("the JDK decoders decode UTF-16LE");
    Ok((text, length + 4))
}

/// `XSSFBUtils.readXLNullableWideString`: none where the count says null, which takes the count's
/// four bytes. POI takes no bytes for it, so the strings after it read it again (#127).
fn nullable_wide_string(data: &[u8], offset: usize) -> Result<(String, usize), Thrown> {
    if u32_at(data, offset)? == u32::MAX {
        return Ok((String::new(), 4));
    }
    wide_string(data, offset)
}

/// `XSSFBParser.parse`: each record whose id `wanted` takes handed to `handle`, the others
/// skipped. A record longer than POI allocates fails, as one cut short does. The pages of a part
/// spooled to a mapped file are released as its records are read.
fn parse_records(
    data: &Input<'_>,
    wanted: &dyn Fn(u32) -> bool,
    mut handle: impl FnMut(u32, &[u8]) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let short = || Failure::Malformed("IllegalStateException: Unexpected end-of-file".to_owned());
    let paging = Paging::new(data.pages());
    let mut at = 0;
    while let Some(&first) = data.get(at) {
        let start = at;
        at += 1;
        let id = if first & 0x80 != 0 {
            let second = *data.get(at).ok_or_else(short)?;
            at += 1;
            (u32::from(second & 0x7F) << 7) + u32::from(first & 0x7F)
        } else {
            u32::from(first)
        };
        let mut length = 0u64;
        for shift in 0..4 {
            let byte = *data.get(at).ok_or_else(short)?;
            at += 1;
            length += u64::from(byte & 0x7F) << (shift * 7);
            if byte & 0x80 == 0 {
                break;
            }
        }
        let wanted = wanted(id);
        if wanted && length > MAX_RECORD_LENGTH {
            return Err(Failure::Malformed(format!(
                "RecordFormatException: Tried to allocate an array of length {length}, but the \
                 maximum length for this record type is {MAX_RECORD_LENGTH}"
            )));
        }
        let end = at as u64 + length;
        if end > data.len() as u64 {
            return Err(if wanted {
                short()
            } else {
                Failure::Malformed(format!(
                    "XSSFBParseException: End of file reached before expected. Tried to skip \
                     {length}"
                ))
            });
        }
        let end = end as usize;
        if wanted {
            handle(id, &data[at..end])?;
        }
        paging.read(end - start);
        at = end;
    }
    Ok(())
}

/// `XSSFBStylesTable`: the number format of each cell style.
#[derive(Debug, Default)]
struct Styles {
    formats: BTreeMap<i16, String>,
    style_formats: Vec<i16>,
}

impl Styles {
    fn read(data: &Input<'_>) -> Result<Self, Failure> {
        let mut styles = Styles::default();
        let (mut in_cell_formats, mut in_formats) = (false, false);
        parse_records(data, &|_| true, |id, data| {
            match id {
                record::BEGIN_CELL_XFS => in_cell_formats = true,
                record::END_CELL_XFS => in_cell_formats = false,
                record::XF if in_cell_formats => styles.style_formats.push(i16_at(data, 2)?),
                record::BEGIN_FORMATS => in_formats = true,
                record::END_FORMATS => in_formats = false,
                record::FORMAT if in_formats => {
                    let format = i16_at(data, 0)?;
                    let (text, _) = wide_string(data, 2)?;
                    styles.formats.insert(format, text);
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(styles)
    }

    /// `getNumberFormatIndex` and `getNumberFormatString` of a cell's style, the general format
    /// where POI knows none; a style it lacks fails.
    fn number_format(&self, style: usize) -> Result<(i16, String), Failure> {
        let index = *self.style_formats.get(style).ok_or_else(|| {
            Failure::Malformed(format!(
                "IndexOutOfBoundsException: Index {style} out of bounds for length {}",
                self.style_formats.len()
            ))
        })?;
        let format = self
            .formats
            .get(&index)
            .cloned()
            .or_else(|| builtin_format(i32::from(index)).map(str::to_owned));
        Ok(match format {
            Some(format) => (index, format),
            None => (0, "General".to_owned()),
        })
    }
}

/// The 16-bit number at `at`: a number format's id, which `BrtFmt` and `BrtXf` hold in two
/// bytes. POI reads the low byte alone (#127).
fn i16_at(data: &[u8], at: usize) -> Result<i16, Failure> {
    data.get(at..at + 2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .ok_or_else(|| out_of_bounds(at + 1, data))
}

fn out_of_bounds(at: usize, data: &[u8]) -> Failure {
    Failure::Malformed(format!(
        "ArrayIndexOutOfBoundsException: Index {at} out of bounds for length {}",
        data.len()
    ))
}

/// `TikaXSSFBSharedStringsTable`'s strings.
fn shared_strings(data: &Input<'_>) -> Result<Vec<String>, Failure> {
    let mut strings = Vec::new();
    parse_records(data, &|_| true, |id, data| {
        if id == record::SST_ITEM {
            strings.push(wide_string(data, RICH_STRING_FLAGS)?.0);
        }
        Ok(())
    })?;
    Ok(strings)
}

/// A comment of `TikaXSSFBCommentsTable`.
#[derive(Debug)]
struct Comment {
    author: String,
    text: Option<String>,
}

/// `TikaXSSFBCommentsTable`: the comments by cell.
fn comments(data: &Input<'_>) -> Result<BTreeMap<(i32, i32), Comment>, Failure> {
    let mut comments = BTreeMap::new();
    let mut authors: Vec<String> = Vec::new();
    let (mut author, mut row, mut column) = (-1i32, -1i32, -1i32);
    let mut text: Option<String> = None;
    parse_records(data, &|_| true, |id, data| {
        match id {
            record::BEGIN_COMMENT => {
                author = u32_at(data, 0)? as i32;
                row = u32_at(data, 4)? as i32;
                column = u32_at(data, 12)? as i32;
            }
            record::COMMENT_TEXT => text = Some(wide_string(data, RICH_STRING_FLAGS)?.0),
            record::END_COMMENT => {
                let name = usize::try_from(author)
                    .ok()
                    .and_then(|index| authors.get(index))
                    .cloned()
                    .unwrap_or_default();
                comments.insert(
                    (row, column),
                    Comment {
                        author: name,
                        text: text.take(),
                    },
                );
                (author, row, column) = (-1, -1, -1);
            }
            record::COMMENT_AUTHOR => authors.push(wide_string(data, 0)?.0),
            _ => {}
        }
        Ok(())
    })?;
    Ok(comments)
}

impl Binary<'_> {
    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let Binary {
            workbook,
            properties,
            sheets,
        } = self;
        extractor::write_package_metadata(&workbook.package, xhtml.metadata_mut());
        properties.write(xhtml.metadata_mut());
        xhtml
            .metadata_mut()
            .set(excel::PROTECTED_WORKSHEET, "false");
        write_sheets(&workbook, sheets, xhtml)?;
        let sheets_of_pictures = workbook.sheets_of_pictures();
        let sheets: BTreeSet<usize> = workbook.sheets.iter().map(|sheet| sheet.part).collect();
        let written = |part: usize, relationship: &Relationship| {
            sheets.contains(&part) && relationship.kind == HYPERLINK_RELATIONSHIP
        };
        extractor::write_embedded_parts_but(
            &workbook.package,
            &workbook.main_parts(),
            &written,
            &extractor::DescribedParts::new(),
            xhtml,
            &|part, metadata| {
                if let Some(sheets) = sheets_of_pictures.get(&part) {
                    write_sheet_numbers(sheets, metadata);
                }
            },
        )?;
        extractor::write_thumbnails(&workbook.package, xhtml)?;
        Ok(())
    }
}

/// `buildXHTML`: the path the workbook was saved at, then each sheet, unless reading the
/// `sheets` failed, which fails it where Tika's sheet iterator throws.
fn write_sheets(
    workbook: &Workbook,
    sheets: Result<(), Failure>,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let package = &workbook.package;
    let bytes = package.read(workbook.workbook)?;
    let mut path = None;
    parse_records(&bytes, &|id| id == record::ABSOLUTE_PATH, |_, data| {
        path = Some(wide_string(data, 0)?.0);
        Ok(())
    })?;
    if let Some(path) = path {
        xhtml.metadata_mut().set(ORIGINAL_RESOURCE_NAME, path);
    }
    let styles = match package.parts_by_content_type(STYLES_TYPE).first() {
        Some(&part) => Some(Styles::read(&package.read(part)?)?),
        None => None,
    };
    sheets?;
    let strings = match package.parts_by_content_type(SHARED_STRINGS_TYPE).first() {
        Some(&part) => shared_strings(&package.read(part)?)?,
        None => Vec::new(),
    };
    let mut formatter = DataFormatter::new(false);
    let settings = SheetSettings {
        headers_and_footers: workbook.config.includes_headers_and_footers(),
        missing_rows: workbook.config.includes_missing_rows(),
    };
    for sheet in &workbook.sheets {
        let comments = match workbook.comments_part(sheet.part) {
            Some(part) => Some(comments(&package.read(part)?)?),
            None => None,
        };
        if comments
            .as_ref()
            .is_some_and(|comments| !comments.is_empty())
        {
            xhtml.metadata_mut().set(name::HAS_COMMENTS, "true");
        }
        xhtml.start_element("div")?;
        xhtml.element("h1", &sheet.name)?;
        xhtml.start_element("table")?;
        xhtml.start_element("tbody")?;
        let mut output = SheetOutput::new(settings);
        let mut reader = SheetReader {
            styles: styles.as_ref(),
            strings: &strings,
            formatter: &mut formatter,
            output: &mut output,
            xhtml: &mut *xhtml,
            row: 0,
            last_started: -1,
            last_ended: -1,
        };
        let bytes = package.read(sheet.part)?;
        parse_records(&bytes, &|_| true, |id, data| reader.record(id, data))?;
        xhtml.end_element("tbody")?;
        xhtml.end_element("table")?;
        for comment in comments.iter().flat_map(BTreeMap::values) {
            xhtml.start_element_with("p", &[("class", "cell-comment")])?;
            if !comment.author.is_empty() {
                xhtml.characters(&format!("{}: ", comment.author))?;
            }
            if let Some(text) = &comment.text {
                xhtml.characters(text)?;
            }
            xhtml.end_element("p")?;
        }
        for text in output.headers.iter().chain(&output.footers) {
            xhtml.element("p", text)?;
        }
        workbook.write_drawings(sheet.part, xhtml)?;
        workbook.write_links(sheet.part, xhtml)?;
        xhtml.end_element("div")?;
    }
    Ok(())
}

/// `XSSFBSheetHandler`, without comments, as Tika gives it none, writing to `SheetTextAsHTML`.
struct SheetReader<'r, 'x, 'h> {
    styles: Option<&'r Styles>,
    strings: &'r [String],
    formatter: &'r mut DataFormatter,
    output: &'r mut SheetOutput,
    xhtml: &'x mut Xhtml<'h>,
    row: i32,
    last_started: i32,
    last_ended: i32,
}

/// `XSSFBUtils.castToInt`: a value strictly between `Integer.MIN_VALUE` and `MAX_VALUE`.
fn cast_to_int(value: u32) -> Result<i32, Failure> {
    i32::try_from(value)
        .ok()
        .filter(|&value| value != i32::MAX)
        .ok_or_else(|| {
            Failure::Malformed(format!(
                "POIXMLException: val ({value}) can't be cast to int"
            ))
        })
}

impl SheetReader<'_, '_, '_> {
    fn record(&mut self, id: u32, data: &[u8]) -> Result<(), Failure> {
        match id {
            record::ROW_HEADER => {
                let row = cast_to_int(u32_at(data, 0)?)?;
                if i64::from(row) > MAX_ROW {
                    return Err(Failure::Malformed(format!(
                        "XSSFBParseException: Row number beyond allowable range: {row}"
                    )));
                }
                self.row = row;
                self.start_row(row)?;
            }
            record::CELL_ISST => {
                let column = self.column(data)?;
                let index = cast_to_int(u32_at(data, CELL_HEADER)?)?;
                let text = usize::try_from(index)
                    .ok()
                    .and_then(|index| self.strings.get(index))
                    .ok_or_else(|| {
                        Failure::Malformed(format!(
                            "IndexOutOfBoundsException: Index {index} out of bounds for length {}",
                            self.strings.len()
                        ))
                    })?
                    .clone();
                self.cell(column, &text)?;
            }
            record::CELL_ST | record::FORMULA_STRING => {
                let column = self.column(data)?;
                let (text, _) = wide_string(data, CELL_HEADER)?;
                self.cell(column, &text)?;
            }
            record::CELL_RICH_STRING => {
                let column = self.column(data)?;
                let (text, _) = wide_string(data, CELL_HEADER + RICH_STRING_FLAGS)?;
                self.cell(column, &text)?;
            }
            record::CELL_RK => {
                let column = self.column(data)?;
                let value = rk_number(data, CELL_HEADER)?;
                self.number(column, data, value)?;
            }
            record::CELL_REAL | record::FORMULA_NUMBER => {
                let column = self.column(data)?;
                let bytes = data
                    .get(CELL_HEADER..CELL_HEADER + 8)
                    .ok_or_else(|| out_of_bounds(CELL_HEADER + 7, data))?;
                let value = f64::from_le_bytes(bytes.try_into().expect("eight bytes"));
                self.number(column, data, value)?;
            }
            record::CELL_BOOL | record::FORMULA_BOOL => {
                let column = self.column(data)?;
                let value = *data
                    .get(CELL_HEADER)
                    .ok_or_else(|| out_of_bounds(CELL_HEADER, data))?;
                self.cell(column, if value == 1 { "TRUE" } else { "FALSE" })?;
            }
            record::CELL_ERROR | record::FORMULA_ERROR => {
                let column = self.column(data)?;
                let code = *data
                    .get(CELL_HEADER)
                    .ok_or_else(|| out_of_bounds(CELL_HEADER, data))?;
                if let Some(text) = error_text(code) {
                    self.cell(column, text)?;
                }
            }
            record::CELL_BLANK => {
                self.column(data)?;
            }
            record::END_SHEET_DATA => self.end_row(self.last_started)?,
            record::BEGIN_HEADER_FOOTER => self.headers_and_footers(data)?,
            _ => {}
        }
        Ok(())
    }

    /// `XSSFBCellHeader.parse`'s column, which reads the style too.
    fn column(&self, data: &[u8]) -> Result<i32, Failure> {
        let column = cast_to_int(u32_at(data, 0)?)?;
        data.get(4..7).ok_or_else(|| out_of_bounds(6, data))?;
        Ok(column)
    }

    fn start_row(&mut self, row: i32) -> Result<(), Failure> {
        if row == self.last_started {
            return Ok(());
        }
        if self.last_started != self.last_ended {
            self.end_row(self.last_started)?;
        }
        self.output.start_row(row, self.xhtml)?;
        self.last_started = row;
        Ok(())
    }

    fn end_row(&mut self, row: i32) -> Result<(), Failure> {
        if self.last_ended == row {
            return Ok(());
        }
        self.xhtml.end_element("tr")?;
        self.last_ended = row;
        Ok(())
    }

    fn cell(&mut self, column: i32, text: &str) -> Result<(), Failure> {
        let reference = CellAddress {
            row: self.row,
            column,
        }
        .format();
        self.output
            .cell(Some(&reference), Some(text), None, self.xhtml)
    }

    /// A number written in its cell's style's number format.
    fn number(&mut self, column: i32, data: &[u8], value: f64) -> Result<(), Failure> {
        let style = usize::from(data[4]) | usize::from(data[5]) << 8 | usize::from(data[6]) << 16;
        let styles = self.styles.ok_or_else(|| {
            Failure::Malformed("NullPointerException: the workbook has no styles".to_owned())
        })?;
        let (index, format) = styles.number_format(style)?;
        let text = match self.formatter.format_raw(value, i32::from(index), &format) {
            Ok(text) => text,
            Err(FormatError::Thrown(reason)) => return Err(Failure::Malformed(reason)),
            Err(FormatError::Unported(what)) => {
                return Err(Failure::Malformed(format!(
                    "a number in {what}, which this port does not write"
                )))
            }
        };
        self.cell(column, &text)
    }

    /// `handleHeaderFooter`: each of the six headers and footers with text, as its sections
    /// joined by spaces.
    fn headers_and_footers(&mut self, data: &[u8]) -> Result<(), Failure> {
        let mut offset = 2usize;
        for header in [true, false, true, false, true, false] {
            if offset + 4 >= data.len() {
                continue;
            }
            let (raw, read) = nullable_wide_string(data, offset)?;
            offset += read;
            let text = header_footer::sections(&raw)
                .into_iter()
                .filter(|section| !section.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !is_blank(&text) {
                self.output.header_footer(text, header);
            }
        }
        Ok(())
    }
}

/// `rkNumber`: an RK number, a float's top 30 bits or a 30-bit integer, hundredths where the
/// lowest bit says so.
fn rk_number(data: &[u8], offset: usize) -> Result<f64, Failure> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| out_of_bounds(offset + 3, data))?;
    let first = bytes[0];
    let hundredths = first & 1 == 1;
    let floating = first >> 1 & 1 == 0;
    let mut raw = [0u8; 8];
    raw[4] = first & !3;
    raw[5..8].copy_from_slice(&bytes[1..4]);
    let value = if floating {
        f64::from_le_bytes(raw)
    } else {
        f64::from(i32::from_le_bytes(raw[4..8].try_into().expect("four bytes")) >> 2)
    };
    Ok(if hundredths { value / 100.0 } else { value })
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn reads_rk_numbers_as_poi_does() {
        let integer = |value: i32, hundredths: bool| {
            let raw = (value << 2) | 2 | i32::from(hundredths);
            raw.to_le_bytes()
        };
        assert_eq!(rk_number(&integer(42, false), 0).unwrap(), 42.0);
        assert_eq!(rk_number(&integer(-7, true), 0).unwrap(), -0.07);
        let float = (1.5f64.to_bits() >> 32) as u32;
        assert_eq!(rk_number(&float.to_le_bytes(), 0).unwrap(), 1.5);
    }

    #[test]
    fn reads_records_of_long_ids_and_lengths() {
        let data = [0x9C, 0x01, 0x82, 0x01, 0xAA, 0xBB];
        let mut seen = Vec::new();
        assert!(
            parse_records(&Input::Borrowed(&data), &|_| true, |id, data| {
                seen.push((id, data.len()));
                Ok(())
            })
            .is_err()
        );
        let mut data = vec![0x9C, 0x01, 0x82, 0x00];
        data.extend_from_slice(&[0; 2]);
        parse_records(&Input::Borrowed(&data), &|_| true, |id, data| {
            seen.push((id, data.len()));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [(156, 2)]);
    }

    #[test]
    fn reads_wide_strings_and_fails_beyond_the_record() {
        let mut data = 2u32.to_le_bytes().to_vec();
        data.extend_from_slice(b"H\0i\0");
        assert_eq!(wide_string(&data, 0).unwrap(), ("Hi".to_owned(), 8));
        assert!(wide_string(&data[..6], 0).is_err());
        assert_eq!(
            nullable_wide_string(&u32::MAX.to_le_bytes(), 0).unwrap(),
            (String::new(), 4)
        );
    }
}
