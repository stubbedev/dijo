//! Excel workbooks (XLSX, XLSM, XLTX, XLTM, XLAM) as Tika's `OOXMLParser` reads them with its
//! event-based Excel extractor, `XSSFExcelExtractorDecorator`: the package's metadata, then each
//! sheet in the workbook's order as a table under its name, with its comments, threaded
//! comments, headers and footers, the text of its drawn shapes, diagrams and charts and its
//! links; then the workbook's hidden sheets, the people of its comments and its links to
//! outside data, the pictures and attachments it embeds, links to what lies outside the
//! package, and the thumbnail.
//!
//! Numbers are written as POI's `DataFormatter` writes them in the `en_US` locale, with Tika's
//! general format and dates in Java's default time zone: see [`format`].
//!
//! A workbook embedding an OLE object, which Tika opens as a compound file, is left to Tika, as
//! is a package POI would read in a degraded way or refuse, one with a part whose XML Tika
//! records failing on, and one using what this port does not read: see
//! [`Workbook::prepare`].

mod cell;
pub(crate) mod format;
mod handlers;
mod header_footer;
mod parts;
mod sheet;

use std::collections::{BTreeSet, HashMap};

use handlers::{
    Connections, DrawingShapes, ExternalLinks, Persons, PivotCache, SheetRefs, ThreadedComments,
    WorkbookMetadata,
};
use parts::{Comments, SharedStrings, Styles};
use sheet::{SheetOutput, SheetReader, SheetSettings};

use crate::config::{OfficeParserConfig, ParserConfigs};
use crate::formats::docx;
use crate::formats::java_time::TimeZone;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ooxml::extractor::{self, external_reference, kind, name, Ignore};
use crate::formats::ooxml::{
    self, Package, Properties, Relationship, Unported, HYPERLINK_RELATIONSHIP,
};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, SaxHandler};
use crate::formats::{Declined, Failure, Parser, Prepared};

/// The SpreadsheetML namespace.
pub(crate) const SPREADSHEETML_NS: &str =
    "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

/// The content types of a workbook part, as `OOXMLExtractorFactory`'s `XSSF_RELATIONS` list them.
const WORKBOOK_TYPES: [&str; 5] = [
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
    "application/vnd.ms-excel.sheet.macroEnabled.main+xml",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml",
    "application/vnd.ms-excel.template.macroEnabled.main+xml",
    "application/vnd.ms-excel.addin.macroEnabled.main+xml",
];
/// The relationships of a drawing whose parts Tika reads with its Word and PowerPoint text
/// handler, in its order, with the class of the div they are written in.
const TEXT_PART_KINDS: [(&str, &str); 2] =
    [(kind::DIAGRAM_DATA, "diagram-data"), (kind::CHART, "chart")];
/// The content type of Power Query's data.
const POWER_QUERY_TYPE: &str = "application/vnd.ms-excel.customDataProperties+xml";

/// Relationship types of workbooks, as POI's `XSSFRelation` and Tika name them.
mod sheet_kind {
    pub(super) const WORKSHEET: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";
    pub(super) const CHARTSHEET: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet";
    pub(super) const MACRO_SHEET: &str =
        "http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet";
    pub(super) const SHARED_STRINGS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings";
    pub(super) const DRAWING: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing";
    pub(super) const VML_DRAWING: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing";
    pub(super) const EXTERNAL_LINK: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink";
    pub(super) const CONNECTIONS: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/connections";
    pub(super) const QUERY_TABLE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/queryTable";
    pub(super) const PIVOT_CACHE_DEFINITION: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition";
    pub(super) const PERSON: &str =
        "http://schemas.microsoft.com/office/2017/10/relationships/person";
    pub(super) const THREADED_COMMENT: &str =
        "http://schemas.microsoft.com/office/2017/10/relationships/threadedComment";
}

/// Metadata names the Excel extractor sets.
mod excel {
    pub(super) const PROTECTED_WORKSHEET: &str = "msoffice:excel:protected-worksheet";
    pub(super) const HAS_HIDDEN_COLUMNS: &str = "msoffice:excel:has-hidden-cols";
    pub(super) const HAS_HIDDEN_ROWS: &str = "msoffice:excel:has-hidden-rows";
    pub(super) const HAS_EXTERNAL_LINKS: &str = "msoffice:excel:has-external-links";
    pub(super) const HAS_DDE_LINKS: &str = "msoffice:excel:has-dde-links";
    pub(super) const HAS_DATA_CONNECTIONS: &str = "msoffice:excel:has-data-connections";
    pub(super) const HAS_WEB_QUERIES: &str = "msoffice:excel:has-web-queries";
    pub(super) const HAS_EXTERNAL_PIVOT_DATA: &str = "msoffice:excel:has-external-pivot-data";
    pub(super) const HAS_POWER_QUERY: &str = "msoffice:excel:has-power-query";
    pub(super) const SHEET_NUMBER: &str = "msoffice:excel:sheet-number";
    pub(super) const SHEET_NUMBERS: &str = "msoffice:excel:sheet-numbers";
}

pub(crate) struct XlsxParser;

impl Parser for XlsxParser {
    fn name(&self) -> &'static str {
        "extractous::OOXMLParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.ms-excel.sheet.macroenabled.12",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.template",
            "application/vnd.ms-excel.template.macroenabled.12",
            "application/vnd.ms-excel.addin.macroenabled.12",
        ]
    }

    /// Defers workbooks with OLE objects Tika opens, macros it extracts when asked to, parts
    /// whose XML Tika records failing on, packages POI does not open as this port does, and any
    /// workbook when Java's default time zone, which dates are written in, cannot be read.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let zone = TimeZone::java_default()
            .map_err(|reason| format!("Java's default time zone cannot be read: {reason}"))?;
        let read = Workbook::open(input, &configs.office)
            .and_then(Workbook::prepare)
            .map_err(|Unported(reason)| reason)?;
        Ok(Box::new(move |xhtml| read.write(zone, xhtml)))
    }
}

/// A sheet of the workbook: its name and its part.
struct Sheet {
    name: String,
    part: usize,
}

/// An Excel package being read.
struct Workbook<'a> {
    package: Package<'a>,
    config: &'a OfficeParserConfig,
    /// The workbook part.
    workbook: usize,
    /// The sheets in the workbook's order, as `XSSFReader.SheetIterator` gives them.
    sheets: Vec<Sheet>,
    styles: Option<usize>,
    shared_strings: Option<usize>,
}

impl<'a> Workbook<'a> {
    fn open(input: &'a [u8], config: &'a OfficeParserConfig) -> Result<Self, Unported> {
        let package = Package::open(input)?;
        let Some(core) =
            Package::relationships_of(&package.relationships, kind::OFFICE_DOCUMENT).next()
        else {
            return Err(Unported(
                "the package has no transitional main document, which POI's XSSFReader \
                 refuses"
                    .to_owned(),
            ));
        };
        let workbook = package.package_part(core).ok_or_else(|| {
            Unported("the package's main document is missing, which Tika fails on".to_owned())
        })?;
        let content_type = &package.parts[workbook].content_type;
        if !WORKBOOK_TYPES.contains(&content_type.as_str()) {
            return Err(Unported(format!(
                "the package's main part has the content type {content_type:?}, which Tika \
                 does not read as an Excel workbook"
            )));
        }
        let refs = read_checked(&package, workbook, SheetRefs::default())?;
        let relationships = &package.parts[workbook].relationships;
        let mut sheet_parts: HashMap<&str, Option<usize>> = HashMap::new();
        for relationship in relationships {
            if [
                sheet_kind::WORKSHEET,
                sheet_kind::CHARTSHEET,
                sheet_kind::MACRO_SHEET,
            ]
            .contains(&relationship.kind.as_str())
            {
                sheet_parts.insert(&relationship.id, internal_part(relationship)?);
            }
        }
        let sheets = refs
            .sheets
            .into_iter()
            .map(|sheet| match sheet_parts.get(sheet.id.as_str()) {
                Some(Some(part)) => Ok(Sheet {
                    name: sheet.name,
                    part: *part,
                }),
                _ => Err(Unported(format!(
                    "the sheet {:?} has no part, which Tika records failing on",
                    sheet.name
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let styles = first_part(relationships, kind::STYLES)?;
        let shared_strings = first_part(relationships, sheet_kind::SHARED_STRINGS)?;
        Ok(Self {
            package,
            config,
            workbook,
            sheets,
            styles,
            shared_strings,
        })
    }

    fn relationships(&self, part: usize) -> &[Relationship] {
        &self.package.parts[part].relationships
    }

    /// The internal parts a part's relationships of `kind` lead to, in order, as
    /// `getPart(createPartName(getTargetURI()))` finds them, skipping missing ones.
    fn internal_related(&self, part: usize, kind: &str) -> Vec<usize> {
        Package::relationships_of(self.relationships(part), kind)
            .filter(|relationship| relationship.is_internal())
            .filter_map(|relationship| relationship.part)
            .collect()
    }

    /// The comments part of a sheet, as `parseSheetComments` finds it: the first comments
    /// relationship's part.
    fn comments_part(&self, sheet: usize) -> Option<usize> {
        Package::relationships_of(self.relationships(sheet), kind::COMMENTS)
            .next()
            .and_then(|relationship| relationship.part)
    }

    /// The parts that might embed documents, as `getMainDocumentParts` lists them: each sheet,
    /// its drawings and its VML drawings, then the main documents.
    fn main_parts(&self) -> Vec<usize> {
        let mut parts = Vec::new();
        for sheet in &self.sheets {
            parts.push(sheet.part);
            parts.extend(self.internal_related(sheet.part, sheet_kind::DRAWING));
            parts.extend(self.internal_related(sheet.part, sheet_kind::VML_DRAWING));
        }
        parts.extend(extractor::core_document_parts(&self.package));
        parts
    }

    /// `recordImagesOnSheet` for each sheet's drawings and VML drawings: the sheets, numbered
    /// from 1, each picture is drawn on.
    fn sheets_of_pictures(&self) -> HashMap<usize, BTreeSet<usize>> {
        let mut sheets: HashMap<usize, BTreeSet<usize>> = HashMap::new();
        for (index, sheet) in self.sheets.iter().enumerate() {
            for kind in [sheet_kind::DRAWING, sheet_kind::VML_DRAWING] {
                for drawing in self.internal_related(sheet.part, kind) {
                    for image in self.internal_related(drawing, kind::IMAGE) {
                        sheets.entry(image).or_default().insert(index + 1);
                    }
                }
            }
        }
        sheets
    }

    /// The XML parts the parse writes as it reads them, or reads for what it writes as it goes,
    /// which only their check reads ahead.
    fn streamed_parts(&self) -> Vec<usize> {
        let mut parts: Vec<usize> = Vec::new();
        for sheet in &self.sheets {
            parts.push(sheet.part);
            parts.extend(Package::related(
                self.relationships(sheet.part),
                sheet_kind::THREADED_COMMENT,
            ));
            if self.config.includes_shape_based_content() {
                for drawing in self.internal_related(sheet.part, sheet_kind::DRAWING) {
                    parts.push(drawing);
                    for kind in TEXT_PART_KINDS.map(|(kind, _)| kind) {
                        parts.extend(Package::related(self.relationships(drawing), kind));
                    }
                }
            }
            parts.extend(Package::related(
                self.relationships(sheet.part),
                sheet_kind::QUERY_TABLE,
            ));
        }
        let workbook = self.relationships(self.workbook);
        for kind in [
            sheet_kind::EXTERNAL_LINK,
            sheet_kind::CONNECTIONS,
            sheet_kind::PIVOT_CACHE_DEFINITION,
        ] {
            parts.extend(
                Package::relationships_of(workbook, kind)
                    .filter(|relationship| relationship.is_internal())
                    .filter_map(|relationship| relationship.part),
            );
        }
        parts
    }

    /// Finds what this port leaves to Tika in a package it could open (embedded documents and
    /// charts, macros to extract, parts whose XML Tika fails on, and properties it does not
    /// read), and reads what Tika reads whole before it writes from it: the properties, styles,
    /// shared strings, each sheet's comments, the workbook's metadata and its people.
    fn prepare(self) -> Result<Read<'a>, Unported> {
        for source in self.main_parts() {
            for relationship in self.relationships(source) {
                extractor::check_embedded(&self.package, self.config, source, relationship, false)?;
            }
        }
        for sheet in &self.sheets {
            for kind in [sheet_kind::DRAWING, sheet_kind::VML_DRAWING] {
                for drawing in self.internal_related(sheet.part, kind) {
                    let images =
                        Package::relationships_of(self.relationships(drawing), kind::IMAGE);
                    for image in images.filter(|image| image.is_internal()) {
                        if image.part.is_none() {
                            return Err(Unported(format!(
                                "{} draws the missing {}, which Tika records failing on",
                                self.package.name(drawing),
                                image.target_uri()
                            )));
                        }
                    }
                }
            }
        }
        if self.config.includes_shape_based_content() {
            for sheet in &self.sheets {
                for drawing in self.internal_related(sheet.part, sheet_kind::DRAWING) {
                    for kind in TEXT_PART_KINDS.map(|(kind, _)| kind) {
                        let mut relationships =
                            Package::relationships_of(self.relationships(drawing), kind);
                        if let Some(external) = relationships.find(|r| !r.is_internal()) {
                            return Err(Unported(format!(
                                "{} draws {} outside the package, which Tika records failing on",
                                self.package.name(drawing),
                                external.target_uri()
                            )));
                        }
                    }
                }
            }
        }
        let styles = match self.styles {
            Some(part) => Some(read_checked(&self.package, part, Styles::default())?),
            None => None,
        };
        let strings = match self.shared_strings {
            Some(part) => Some(read_checked(
                &self.package,
                part,
                SharedStrings::new(self.config.concatenates_phonetic_runs()),
            )?),
            None => None,
        };
        let mut comments = Vec::with_capacity(self.sheets.len());
        for sheet in &self.sheets {
            comments.push(match self.comments_part(sheet.part) {
                Some(part) => Some(read_checked(&self.package, part, Comments::default())?),
                None => None,
            });
        }
        for part in self.streamed_parts() {
            read_checked(&self.package, part, Ignore)?;
        }
        let mut persons = Vec::new();
        for part in Package::related(self.relationships(self.workbook), sheet_kind::PERSON) {
            persons.push(read_checked(&self.package, part, Persons::default())?);
        }
        let metadata = read_checked(&self.package, self.workbook, WorkbookMetadata::default())?;
        if metadata.hides_an_unnamed_sheet() {
            return Err(Unported("a hidden sheet has no name".to_owned()));
        }
        let properties = Properties::read(&self.package)?;
        Ok(Read {
            workbook: self,
            properties,
            styles,
            strings,
            comments,
            metadata,
            persons,
        })
    }

    fn write(
        &self,
        read: &Read,
        zone: &'static TimeZone,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        extractor::write_package_metadata(&self.package, xhtml.metadata_mut());
        read.properties.write(xhtml.metadata_mut());
        xhtml
            .metadata_mut()
            .set(excel::PROTECTED_WORKSHEET, "false");
        self.write_sheets(read, zone, xhtml)?;
        read.metadata.write(xhtml.metadata_mut());
        for persons in &read.persons {
            persons.write(xhtml.metadata_mut());
        }
        self.write_external_data(xhtml)?;
        let sheets_of_pictures = self.sheets_of_pictures();
        extractor::write_embedded_parts(
            &self.package,
            &self.main_parts(),
            xhtml,
            &|part, metadata| {
                if let Some(sheets) = sheets_of_pictures.get(&part) {
                    write_sheet_numbers(sheets, metadata);
                }
            },
        )?;
        extractor::write_thumbnails(&self.package, xhtml)?;
        Ok(())
    }

    /// Reads a part checked well-formed into `handler`.
    fn read<H: SaxHandler>(&self, part: usize, mut handler: H) -> Result<H, Failure> {
        let bytes = self.package.read(part)?;
        xml::parse_sax(&bytes, self.package.name(part), &mut handler)?;
        Ok(handler)
    }

    /// The sheets, each in a `sheet` div, as `buildXHTML` writes them.
    fn write_sheets(
        &self,
        read: &Read,
        zone: &'static TimeZone,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let mut formatter = format::DataFormatter::new(zone);
        let settings = SheetSettings {
            headers_and_footers: self.config.includes_headers_and_footers(),
            missing_rows: self.config.includes_missing_rows(),
        };
        let mut drawing_hyperlinks: HashMap<String, String> = HashMap::new();
        for (sheet, comments) in self.sheets.iter().zip(&read.comments) {
            for drawing in self.internal_related(sheet.part, sheet_kind::DRAWING) {
                for relationship in
                    Package::relationships_of(self.relationships(drawing), HYPERLINK_RELATIONSHIP)
                {
                    drawing_hyperlinks.insert(
                        relationship.id.clone(),
                        relationship.target_uri().to_owned(),
                    );
                }
            }
            if comments.as_ref().is_some_and(|comments| comments.len() > 0) {
                xhtml.metadata_mut().set(name::HAS_COMMENTS, "true");
            }
            xhtml.start_element_with("div", &[("class", "sheet")])?;
            xhtml.element("h1", &sheet.name)?;
            xhtml.start_element("table")?;
            xhtml.start_element("tbody")?;
            let mut output = SheetOutput::new(settings);
            let mut reader = SheetReader::new(
                read.styles.as_ref(),
                comments.as_ref(),
                read.strings.as_ref(),
                &mut formatter,
                &mut output,
                xhtml,
            );
            let bytes = self.package.read(sheet.part)?;
            xml::parse_sax(&bytes, self.package.name(sheet.part), &mut reader)?;
            let captured = reader.captured;
            let metadata = xhtml.metadata_mut();
            if captured.protection {
                metadata.set(excel::PROTECTED_WORKSHEET, "true");
            }
            if captured.hidden_column {
                metadata.set(excel::HAS_HIDDEN_COLUMNS, "true");
            }
            if captured.hidden_row {
                metadata.set(excel::HAS_HIDDEN_ROWS, "true");
            }
            for part in
                Package::related(self.relationships(sheet.part), sheet_kind::THREADED_COMMENT)
            {
                self.read(part, ThreadedComments::new(xhtml))?;
            }
            xhtml.end_element("tbody")?;
            xhtml.end_element("table")?;
            for text in output.headers.iter().chain(&output.footers) {
                let content = header_footer::extract(text);
                if !content.is_empty() {
                    xhtml.element("p", &content)?;
                }
            }
            if self.config.includes_shape_based_content() {
                for drawing in self.internal_related(sheet.part, sheet_kind::DRAWING) {
                    self.read(drawing, DrawingShapes::new(xhtml, &drawing_hyperlinks))?;
                    self.write_text_parts(drawing, xhtml)?;
                }
            }
            let mut first = true;
            for relationship in
                Package::relationships_of(self.relationships(sheet.part), HYPERLINK_RELATIONSHIP)
            {
                if !first {
                    xhtml.characters(" ")?;
                }
                first = false;
                let url = relationship.target_uri();
                xhtml.start_element_with("a", &[("href", url)])?;
                xhtml.characters(url)?;
                xhtml.end_element("a")?;
            }
            xhtml.end_element("div")?;
        }
        Ok(())
    }

    /// `handleGeneralTextContainingPart` for a drawing's diagrams, then its charts, each kind's
    /// parts read by one of Tika's Word and PowerPoint text handlers.
    fn write_text_parts(&self, drawing: usize, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let no_links = HashMap::new();
        for (kind, class) in TEXT_PART_KINDS {
            docx::write_general_text_parts(&self.package, drawing, kind, class, &no_links, xhtml)?;
        }
        Ok(())
    }

    /// `extractExternalDataSources`: links to outside workbooks, connections and queries, and
    /// what the workbook draws on outside itself.
    fn write_external_data(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let relationships = self.relationships(self.workbook);
        let mut links =
            Package::relationships_of(relationships, sheet_kind::EXTERNAL_LINK).peekable();
        if links.peek().is_some() {
            xhtml.metadata_mut().set(excel::HAS_EXTERNAL_LINKS, "true");
        }
        for relationship in links {
            if !relationship.is_internal() {
                external_reference(xhtml, "externalLink", relationship.target_uri())?;
                continue;
            }
            if let Some(part) = relationship.part {
                let handler = self.read(part, ExternalLinks::new(xhtml))?;
                if handler.dde {
                    xhtml.metadata_mut().set(excel::HAS_DDE_LINKS, "true");
                }
            }
        }
        for part in self.internal_targets(relationships, sheet_kind::CONNECTIONS) {
            let handler = self.read(part, Connections::new(xhtml))?;
            let (connection, web_query) = (handler.connection, handler.web_query);
            if connection {
                xhtml
                    .metadata_mut()
                    .set(excel::HAS_DATA_CONNECTIONS, "true");
            }
            if web_query {
                xhtml.metadata_mut().set(excel::HAS_WEB_QUERIES, "true");
            }
        }
        for part in self.internal_targets(relationships, sheet_kind::PIVOT_CACHE_DEFINITION) {
            if self.read(part, PivotCache::default())?.external {
                xhtml
                    .metadata_mut()
                    .set(excel::HAS_EXTERNAL_PIVOT_DATA, "true");
            }
        }
        let parts = &self.package.parts;
        if parts
            .iter()
            .any(|part| part.content_type == POWER_QUERY_TYPE)
            || parts
                .iter()
                .any(|part| part.name.contains("/customData/") || part.name.contains("/dataMashup"))
        {
            xhtml.metadata_mut().set(excel::HAS_POWER_QUERY, "true");
        }
        Ok(())
    }

    /// The parts `getRelatedPart` finds for relationships of `kind`, skipping those it throws
    /// for: external ones and those whose part is missing.
    fn internal_targets(&self, relationships: &[Relationship], kind: &str) -> Vec<usize> {
        Package::relationships_of(relationships, kind)
            .filter(|relationship| relationship.is_internal())
            .filter_map(|relationship| relationship.part)
            .collect()
    }
}

/// `PageAnchoring.applySheetMetadata`: the sheets an embedded picture is drawn on, and the
/// sheet where it is one.
fn write_sheet_numbers(sheets: &BTreeSet<usize>, metadata: &mut TikaMetadata) {
    for sheet in sheets {
        metadata.add(excel::SHEET_NUMBERS, sheet.to_string());
    }
    if let [sheet] = sheets.iter().collect::<Vec<_>>()[..] {
        metadata.set(excel::SHEET_NUMBER, sheet.to_string());
    }
}

/// The part an internal relationship leads to, as `getPart(createPartName(getTargetURI()))`
/// finds it, `None` where the package lacks it; an external one, whose URI is no part name,
/// is [`Unported`], as POI throws.
fn internal_part(relationship: &Relationship) -> Result<Option<usize>, Unported> {
    if !relationship.is_internal() {
        return Err(Unported(format!(
            "the relationship {} leads out of the package to {}, which POI fails on",
            relationship.id,
            relationship.target_uri()
        )));
    }
    Ok(relationship.part)
}

/// The part of the first of `relationships` of `kind`, as `XSSFRelation.getContents` reads it;
/// one that leads nowhere, which Tika records failing on, is [`Unported`].
fn first_part(relationships: &[Relationship], kind: &str) -> Result<Option<usize>, Unported> {
    let Some(relationship) = Package::relationships_of(relationships, kind).next() else {
        return Ok(None);
    };
    match internal_part(relationship)? {
        Some(part) => Ok(Some(part)),
        None => Err(Unported(format!(
            "the {kind} part {} is missing, which Tika records failing on",
            relationship.target_uri()
        ))),
    }
}

/// A workbook checked and read as far as Tika reads parts whole: what writing it takes.
struct Read<'a> {
    workbook: Workbook<'a>,
    properties: Properties,
    styles: Option<Styles>,
    strings: Option<SharedStrings>,
    /// The comments of each sheet, in the workbook's order.
    comments: Vec<Option<Comments>>,
    metadata: WorkbookMetadata,
    persons: Vec<Persons>,
}

impl Read<'_> {
    fn write(&self, zone: &'static TimeZone, xhtml: &mut Xhtml) -> Result<(), Failure> {
        self.workbook.write(self, zone, xhtml)
    }
}

/// Reads `part` into `handler`; XML Tika fails or records failing on is [`Unported`].
fn read_checked<H: SaxHandler>(package: &Package, part: usize, handler: H) -> Result<H, Unported> {
    ooxml::read_quietly(package, part, handler)?.ok_or_else(|| {
        Unported(format!(
            "Tika fails or records a failure on {}",
            package.name(part)
        ))
    })
}

#[cfg(test)]
mod tests;
