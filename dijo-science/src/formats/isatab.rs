//! ISA-Tab files, tab-separated text in Commons CSV's TDF format. A study file is read as Tika's
//! `ISArchiveParser` reads it: where the file lies in a directory with exactly one investigation
//! file, the investigation's fields and the study's own as metadata under `isatab:`, a heading
//! of the investigation, then the study's table, then the table of each of its assays, read from
//! the same directory. Tika without its scientific module leaves them empty
//! (#172). Investigation and assay files, which Tika reads with no parser even with the
//! module, are read on their own: an investigation's fields as metadata and its records as a
//! table, an assay's table as `ISArchiveParser` writes it.
//!
//! A heading whose identifier the investigation does not give is written without it, where
//! `ISArchiveParser` writes `null`. Each study is given its own fields alone, and a study field
//! with no value is read as empty, where `ISArchiveParser` carries an earlier study's fields over
//! into the study read and fails on a field with no value (#183).

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::auto_detect_reader::{self, owned_chars, AutoDetectReader, OwnedChars};
use crate::formats::embedded::RESOURCE_NAME;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{
    java_file_name, unless_different, Decline, Failure, Footprint, InputFile, Parser, Prepared,
};
use crate::input::Pages;
use crate::java;
use dijo_html::formats::text::csv::{Records, TabDelimited};

const STUDY: &str = "application/x-isatab";
const INVESTIGATION: &str = "application/x-isatab-investigation";
const ASSAY: &str = "application/x-isatab-assay";
/// `ISATabUtils.ISATAB`, the prefix of the fields' names.
const ISATAB: &str = "isatab:";

/// `ISATabUtils.sections`: the sections of an investigation whose fields are metadata.
const SECTIONS: [&str; 4] = [
    "ONTOLOGY SOURCE REFERENCE",
    "INVESTIGATION",
    "INVESTIGATION PUBLICATIONS",
    "INVESTIGATION CONTACTS",
];
const STUDY_SECTION: &str = "STUDY";
const STUDY_FILE_NAME: &str = "Study File Name";
const INVESTIGATION_IDENTIFIER: &str = "isatab:Investigation Identifier";
const STUDY_IDENTIFIER: &str = "isatab:Study Identifier";
const STUDY_ASSAY_FILE_NAME: &str = "isatab:Study Assay File Name";

/// Tika's `ISArchiveParser`.
pub struct IsaTabParser;

impl Parser for IsaTabParser {
    fn name(&self) -> &'static str {
        "dijo::ISArchiveParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.isatab.ISArchiveParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[STUDY, INVESTIGATION, ASSAY]
    }

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

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let content_type = metadata.get("Content-Type").map(str::to_owned);
        let kind = match content_type.as_deref() {
            Some(STUDY) => Kind::Study,
            Some(INVESTIGATION) => Kind::Investigation,
            Some(ASSAY) => Kind::Assay,
            other => {
                return Err(Decline::Fails(Error::ParseError(format!(
                    "an ISA-Tab file of the content type {other:?}"
                ))))
            }
        };
        let difference = AutoDetectReader::difference(input, content_type.as_deref());
        unless_different(
            Box::new(move |xhtml| {
                let file = File {
                    input,
                    pages,
                    content_type,
                };
                match kind {
                    Kind::Study => study(file, xhtml),
                    Kind::Investigation => investigation(file, xhtml),
                    Kind::Assay => {
                        let records = file.records(xhtml)?;
                        table(records, "the ISA-Tab assay", xhtml)
                    }
                }
            }),
            difference,
        )
    }
}

/// Which of the files of an ISA-Tab archive a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Study,
    Investigation,
    Assay,
}

/// The document's bytes, their pages, and its content type.
struct File<'a> {
    input: &'a [u8],
    pages: Pages<'a>,
    content_type: Option<String>,
}

/// The records of a file, read as they are decoded.
type FileRecords<'a> = Records<
    std::iter::FlatMap<auto_detect_reader::Text<'a>, OwnedChars, fn(String) -> OwnedChars>,
    TabDelimited,
>;

impl<'a> File<'a> {
    /// The records of the document, read through an `AutoDetectReader`, which sets its metadata.
    fn records(self, xhtml: &mut Xhtml) -> Result<FileRecords<'a>, Failure> {
        let (detection, text) =
            AutoDetectReader::stream(self.input, self.content_type.as_deref(), self.pages);
        auto_detect_reader::set_metadata(&detection, xhtml.metadata_mut());
        let chars: fn(String) -> OwnedChars = owned_chars;
        Ok(Records::new(text.flat_map(chars), TabDelimited))
    }
}

/// `ISArchiveParser.parse` of a study file.
fn study(file: File, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let path = match xhtml.context().input_file() {
        InputFile::Original(path) => Some(PathBuf::from(path)),
        InputFile::Spooled => None,
    };
    let study_name = xhtml
        .metadata_mut()
        .get(RESOURCE_NAME)
        .map(str::to_owned)
        .or_else(|| {
            path.as_deref()
                .and_then(Path::to_str)
                .map(|path| java_file_name(path).to_owned())
        });
    let directory = path
        .as_deref()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    if let Some(directory) = &directory {
        if let [investigation] = investigations(directory)?.as_slice() {
            let bytes = read(&directory.join(investigation), "investigation")?;
            let reader = AutoDetectReader::read(&bytes, file.content_type.as_deref());
            auto_detect_reader::set_metadata(&reader.detection, xhtml.metadata_mut());
            let mut fields = Fields::new(study_name.as_deref());
            let chars = reader.text.chars();
            for record in Records::new(chars, TabDelimited) {
                let record = record.map_err(|reason| malformed("investigation", reason))?;
                if fields.read(&record, xhtml.metadata_mut()) == Read::Ended {
                    break;
                }
            }
            heading(xhtml, "h1", "INVESTIGATION", INVESTIGATION_IDENTIFIER)?;
        }
    }
    heading(xhtml, "h2", "STUDY", STUDY_IDENTIFIER)?;
    let content_type = file.content_type.clone();
    let records = file.records(xhtml)?;
    table(records, "the ISA-Tab study", xhtml)?;
    let assays = xhtml.metadata_mut().values(STUDY_ASSAY_FILE_NAME).to_vec();
    let Some(directory) = directory else {
        return Ok(());
    };
    for assay in assays {
        xhtml.start_element("div")?;
        xhtml.element("h3", &format!("ASSAY {assay}"))?;
        let bytes = read(&within(&directory, &assay)?, "assay")?;
        let reader = AutoDetectReader::read(&bytes, content_type.as_deref());
        auto_detect_reader::set_metadata(&reader.detection, xhtml.metadata_mut());
        table(
            Records::new(reader.text.chars(), TabDelimited),
            "an ISA-Tab assay",
            xhtml,
        )?;
        xhtml.end_element("div")?;
    }
    Ok(())
}

/// An investigation file read on its own: its fields as metadata, as `ISATabUtils` reads those
/// of the investigation sections, and every record as a row of a table.
fn investigation(file: File, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut fields = Fields::new(None);
    let records = file.records(xhtml)?;
    xhtml.start_element("table")?;
    xhtml.start_element("tbody")?;
    for record in records {
        let record = record.map_err(|reason| malformed("investigation", reason))?;
        fields.read(&record, xhtml.metadata_mut());
        row(xhtml, &record)?;
    }
    xhtml.end_element("tbody")?;
    xhtml.end_element("table")?;
    Ok(())
}

/// A heading of the text and the value of the metadata `name`, where it has one.
fn heading(xhtml: &mut Xhtml, element: &str, text: &str, name: &str) -> Result<(), Failure> {
    let written = match xhtml.metadata_mut().get(name) {
        Some(identifier) => format!("{text} {identifier}"),
        None => text.to_owned(),
    };
    xhtml.element(element, &written)?;
    Ok(())
}

/// `ISATabUtils.parseStudy` and `parseAssay`: a table whose head holds the first record's
/// values, and whose body holds a row of each other record.
fn table<I, E>(mut records: I, what: &str, xhtml: &mut Xhtml) -> Result<(), Failure>
where
    I: Iterator<Item = Result<Vec<String>, E>>,
    E: std::fmt::Display,
{
    let failed = |reason: E| Failure::Malformed(format!("{what}: {reason}"));
    xhtml.start_element("table")?;
    xhtml.start_element("thead")?;
    if let Some(head) = records.next() {
        for value in head.map_err(failed)? {
            xhtml.start_element("th")?;
            xhtml.characters(&value)?;
            xhtml.end_element("th")?;
        }
    }
    xhtml.end_element("thead")?;
    xhtml.start_element("tbody")?;
    for record in records {
        row(xhtml, &record.map_err(failed)?)?;
    }
    xhtml.end_element("tbody")?;
    xhtml.end_element("table")?;
    Ok(())
}

fn row(xhtml: &mut Xhtml, record: &[String]) -> Result<(), Failure> {
    xhtml.start_element("tr")?;
    for value in record {
        xhtml.start_element("td")?;
        xhtml.characters(value)?;
        xhtml.end_element("td")?;
    }
    xhtml.end_element("tr")?;
    Ok(())
}

fn malformed(what: &str, reason: String) -> Failure {
    Failure::Malformed(format!("the ISA-Tab {what}: {reason}"))
}

/// The names of the investigation files of `directory`, `i_*.txt`, in order.
fn investigations(directory: &Path) -> Result<Vec<String>, Failure> {
    let entries = fs::read_dir(directory).map_err(|error| {
        Failure::Extraction(Error::IoError(format!(
            "listing the ISA-Tab directory {}: {error}",
            directory.display()
        )))
    })?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Failure::Extraction(Error::IoError(format!(
                "listing the ISA-Tab directory {}: {error}",
                directory.display()
            )))
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let investigation = name
            .strip_prefix("i_")
            .and_then(|rest| rest.strip_suffix(".txt"))
            .is_some_and(|between| !between.is_empty());
        if investigation {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// The file `name` names in `directory`, which must not lead out of it, as
/// `FilenameUtils.resolveWithin` resolves it.
fn within(directory: &Path, name: &str) -> Result<PathBuf, Failure> {
    let relative = Path::new(name);
    let inside = !name.is_empty()
        && relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
    if !inside {
        return Err(Failure::Extraction(Error::IoError(format!(
            "the ISA-Tab assay {name:?} lies outside {}",
            directory.display()
        ))));
    }
    Ok(directory.join(relative))
}

fn read(path: &Path, what: &str) -> Result<Vec<u8>, Failure> {
    fs::read(path).map_err(|error| {
        Failure::Extraction(Error::IoError(format!(
            "reading the ISA-Tab {what} {}: {error}",
            path.display()
        )))
    })
}

/// Whether reading an investigation's fields goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    Continues,
    /// The fields of the study read are all read: `extractMetadata` breaks off.
    Ended,
}

/// `ISATabUtils.extractMetadata`: the fields of an investigation's sections, and of the study of
/// the file named `study`, if any, as metadata.
struct Fields<'s> {
    study: Option<&'s str>,
    investigation_section: bool,
    study_section: bool,
    study_target: bool,
    /// The fields of the study section read, the last value of each name.
    pending: Vec<(String, String)>,
}

impl<'s> Fields<'s> {
    fn new(study: Option<&'s str>) -> Self {
        Self {
            study,
            investigation_section: false,
            study_section: false,
            study_target: false,
            pending: Vec::new(),
        }
    }

    fn read(&mut self, record: &[String], metadata: &mut TikaMetadata) -> Read {
        let Some(field) = record.first() else {
            return Read::Continues;
        };
        if java::to_upper_case(field) == *field && record.len() == 1 {
            self.investigation_section = SECTIONS.contains(&field.as_str());
            self.study_section = self.study.is_some() && field == STUDY_SECTION;
            if self.study_section {
                self.pending.clear();
            }
            return Read::Continues;
        }
        if self.investigation_section {
            add(field, record, metadata);
        } else if self.study_section {
            if self.study_target {
                return Read::Ended;
            }
            let value = record.get(1).map_or("", String::as_str);
            match self.pending.iter_mut().find(|(name, _)| name == field) {
                Some((_, pending)) => value.clone_into(pending),
                None => self.pending.push((field.clone(), value.to_owned())),
            }
            self.study_target = field == STUDY_FILE_NAME && Some(value) == self.study;
            if self.study_target {
                for (name, value) in &self.pending {
                    metadata.add_prefixed(ISATAB, name, value.as_str());
                }
                self.study_section = false;
            }
        } else if self.study_target {
            add(field, record, metadata);
        }
        Read::Continues
    }
}

/// `ISATabUtils.addMetadata`: each value after the field's name.
fn add(field: &str, record: &[String], metadata: &mut TikaMetadata) {
    for value in record.iter().skip(1) {
        metadata.add_prefixed(ISATAB, field, value.as_str());
    }
}

#[cfg(test)]
mod tests;
