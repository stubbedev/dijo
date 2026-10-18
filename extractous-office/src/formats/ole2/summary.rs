//! Tika's `SummaryExtractor`: the summary information and document summary information streams
//! of a compound document's directory into metadata, with the custom properties under
//! `custom:`. A stream POI cannot read is passed over; one it fails on partway keeps what was
//! set before the failure, as the getters POI throws from are called in Tika's order.
//!
//! Where Tika slips, this port does not (#106): the edit time is a duration in whole minutes, as
//! Office Open XML's `TotalTime` is, and only where the document records one; unsigned 32-bit
//! counts and custom numbers stay unsigned; and a value POI keeps as bytes is passed over
//! rather than written as a number or as text.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::codepage;
use super::property_set::{self, Fault, PropertySet, Section, Value, FILETIME_EPOCH_MILLIS};
use crate::formats::metadata::{TikaMetadata, USER_DEFINED_PREFIX};
use crate::formats::Unported;
use crate::java;
use crate::java::dtoa::{java_double_to_string, java_float_to_string};
use crate::java::is_blank;
use crate::java::time::format_instant;
use crate::ole2::{EntryId, EntryKind, Ole2};

const SUMMARY_INFORMATION: &str = "\u{5}SUMMARYINFORMATION";
const DOCUMENT_SUMMARY_INFORMATION: &str = "\u{5}DOCUMENTSUMMARYINFORMATION";

/// Property ids of the summary information, as POI's `PropertyIDMap` names them.
mod pid {
    pub(super) const TITLE: u32 = 2;
    pub(super) const SUBJECT: u32 = 3;
    pub(super) const AUTHOR: u32 = 4;
    pub(super) const KEYWORDS: u32 = 5;
    pub(super) const COMMENTS: u32 = 6;
    pub(super) const TEMPLATE: u32 = 7;
    pub(super) const LAST_AUTHOR: u32 = 8;
    pub(super) const REVISION: u32 = 9;
    pub(super) const EDIT_TIME: u32 = 10;
    pub(super) const LAST_PRINTED: u32 = 11;
    pub(super) const CREATED: u32 = 12;
    pub(super) const LAST_SAVED: u32 = 13;
    pub(super) const PAGE_COUNT: u32 = 14;
    pub(super) const WORD_COUNT: u32 = 15;
    pub(super) const CHARACTER_COUNT: u32 = 16;
    pub(super) const THUMBNAIL: u32 = 17;
    pub(super) const APPLICATION: u32 = 18;
    pub(super) const SECURITY: u32 = 19;
    pub(super) const CATEGORY: u32 = 2;
    pub(super) const SLIDE_COUNT: u32 = 7;
    pub(super) const MANAGER: u32 = 14;
    pub(super) const COMPANY: u32 = 15;
    pub(super) const CODEPAGE: u32 = 1;
}

/// The property sets of a directory's summary information streams, read and checked to be
/// ported, which [`Summaries::write`] writes as `SummaryExtractor.parseSummaries` does.
#[derive(Debug, Clone)]
pub struct Summaries(Vec<PropertySet>);

impl Summaries {
    /// `SummaryExtractor.parseSummaries(directory)`'s reading: the property sets POI reads, or
    /// why Tika's path should read them.
    pub fn read(file: &Ole2<'_>, directory: EntryId) -> Result<Summaries, Unported> {
        let mut sets = Vec::new();
        for name in [SUMMARY_INFORMATION, DOCUMENT_SUMMARY_INFORMATION] {
            let Some(entry) = file
                .children(directory)
                .find(|&child| file.name(child).to_uppercase() == name)
            else {
                continue;
            };
            if file.kind(entry) != EntryKind::Document {
                continue;
            }
            let Ok(stream) = file.content(entry) else {
                continue;
            };
            if let Some(set) = property_set::read(&stream.bytes())? {
                sets.push(set);
            }
        }
        let summaries = Summaries(sets);
        summaries.write_checked(&mut TikaMetadata::new())?;
        Ok(summaries)
    }

    /// Writes the properties into `metadata`, as `parseSummaries` does.
    pub fn write(&self, metadata: &mut TikaMetadata) {
        self.write_checked(metadata)
            .expect("summaries read are ported");
    }

    fn write_checked(&self, metadata: &mut TikaMetadata) -> Result<(), Unported> {
        for set in &self.0 {
            let parsed = if set.is_summary_information() {
                parse_summary_information(set.first_section(), metadata)
            } else if set.is_document_summary_information() {
                parse_document_summary_information(set, metadata)
            } else {
                Ok(())
            };
            if let Err(Fault::Unported(reason)) = parsed {
                return Err(Unported(reason));
            }
        }
        Ok(())
    }
}

/// `SummaryExtractor.addMulti`: the values separated by semicolons, each not blank and not
/// present yet.
pub fn add_multi(metadata: &mut TikaMetadata, name: &str, value: &str) {
    if is_blank(value) {
        return;
    }
    let mut seen: HashSet<String> = metadata.values(name).iter().cloned().collect();
    for part in java::split(value, ';') {
        if !seen.contains(part) && !is_blank(part) {
            metadata.add(name, part);
            seen.insert(part.to_owned());
        }
    }
}

fn set(metadata: &mut TikaMetadata, name: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !is_blank(value)) {
        metadata.set(name, value);
    }
}

fn set_date(metadata: &mut TikaMetadata, name: &str, millis: Option<i64>) {
    if let Some(millis) = millis {
        metadata.set(name, format_instant(millis));
    }
}

fn set_positive(metadata: &mut TikaMetadata, name: &str, value: i64) {
    if value > 0 {
        metadata.set(name, value.to_string());
    }
}

/// `parse(SummaryInformation)`.
fn parse_summary_information(section: &Section, metadata: &mut TikaMetadata) -> Result<(), Fault> {
    match section.get(pid::THUMBNAIL) {
        None | Some(Value::Bytes(_)) => {}
        Some(_) => return Err(Fault::Runtime),
    }
    set(metadata, "dc:title", string(section, pid::TITLE)?);
    if let Some(author) = string(section, pid::AUTHOR)? {
        add_multi(metadata, "dc:creator", &author);
    }
    if let Some(keywords) = string(section, pid::KEYWORDS)?.filter(|k| !is_blank(k)) {
        metadata.add("meta:keyword", keywords.clone());
        metadata.add("dc:subject", keywords);
    }
    if let Some(subject) = string(section, pid::SUBJECT)?.filter(|s| !is_blank(s)) {
        metadata.add("dc:subject", subject);
    }
    set(
        metadata,
        "meta:last-author",
        string(section, pid::LAST_AUTHOR)?,
    );
    set(metadata, "w:Comments", string(section, pid::COMMENTS)?);
    set(
        metadata,
        "extended-properties:Template",
        string(section, pid::TEMPLATE)?,
    );
    set(
        metadata,
        "extended-properties:Application",
        string(section, pid::APPLICATION)?,
    );
    set(metadata, "cp:revision", string(section, pid::REVISION)?);
    set_date(metadata, "dcterms:created", date(section, pid::CREATED)?);
    set_date(
        metadata,
        "dcterms:modified",
        date(section, pid::LAST_SAVED)?,
    );
    set_date(
        metadata,
        "meta:print-date",
        date(section, pid::LAST_PRINTED)?,
    );
    if let Some(minutes) = edit_minutes(section)? {
        metadata.set("extended-properties:TotalTime", minutes.to_string());
    }
    set_positive(
        metadata,
        "extended-properties:DocSecurity",
        int(section, pid::SECURITY)?,
    );
    set_positive(metadata, "meta:word-count", int(section, pid::WORD_COUNT)?);
    set_positive(
        metadata,
        "meta:character-count",
        int(section, pid::CHARACTER_COUNT)?,
    );
    let pages = int(section, pid::PAGE_COUNT)?;
    set_positive(metadata, "meta:page-count", pages);
    set_positive(metadata, "xmpTPg:NPages", pages);
    Ok(())
}

/// `parse(DocumentSummaryInformation)`.
fn parse_document_summary_information(
    set_: &PropertySet,
    metadata: &mut TikaMetadata,
) -> Result<(), Fault> {
    let section = set_.first_section();
    set(
        metadata,
        "extended-properties:Company",
        string(section, pid::COMPANY)?,
    );
    if let Some(manager) = string(section, pid::MANAGER)? {
        add_multi(metadata, "extended-properties:Manager", &manager);
    }
    let language = custom_properties(set_)?.and_then(|custom| match custom.get("Language") {
        Some(Value::String(language)) => Some(language.clone()),
        _ => None,
    });
    set(metadata, "dc:language", language);
    set(metadata, "cp:category", string(section, pid::CATEGORY)?);
    let slides = int(section, pid::SLIDE_COUNT)?;
    set_positive(metadata, "meta:slide-count", slides);
    set_positive(metadata, "xmpTPg:NPages", slides);
    if let Some(custom) = custom_properties(set_)? {
        custom.write(metadata)?;
    }
    Ok(())
}

/// The edit time, a file time counting 100-nanosecond units from zero, in whole minutes, as
/// Office Open XML's `TotalTime` gives it. Tika writes the file time itself, cut to whole
/// milliseconds, and `0` where the document records none (#106).
fn edit_minutes(section: &Section) -> Result<Option<i64>, Fault> {
    Ok(date(section, pid::EDIT_TIME)?.map(|millis| (millis + FILETIME_EPOCH_MILLIS) / 60_000))
}

/// `PropertySet.getPropertyStringValue`: text as it is, and any other value as Java writes it.
/// A value POI keeps as bytes, of a type it does not convert, is passed over; Tika writes it
/// by its length as a number or as text in code page 1252 (#106).
fn string(section: &Section, id: u32) -> Result<Option<String>, Fault> {
    let Some(value) = section.get(id) else {
        return Ok(None);
    };
    Ok(Some(match value {
        Value::String(text) => text.clone(),
        Value::Bytes(_) => return Ok(None),
        Value::Byte(byte) => byte.to_string(),
        Value::Integer(int) => int.to_string(),
        Value::Long(long) => long.to_string(),
        Value::BigInteger(big) => big.to_string(),
        Value::Boolean(boolean) => boolean.to_string(),
        Value::Double(double) => java_double_to_string(*double),
        Value::Float(float) => java_float_to_string(*float),
        Value::Date(_) => {
            return Err(Fault::Unported(format!(
                "property {id} is a date, which Java writes with Date.toString in the machine's \
                 time zone"
            )))
        }
    }))
}

/// A date property, as the `(Date)` cast of POI's getters takes it.
fn date(section: &Section, id: u32) -> Result<Option<i64>, Fault> {
    match section.get(id) {
        None => Ok(None),
        Some(Value::Date(millis)) => Ok(Some(*millis)),
        Some(_) => Err(Fault::Runtime),
    }
}

/// `Section.getPropertyIntValue`: zero where the property is missing, and a failure where it is
/// no `Integer` or `Long`. An unsigned 32-bit count stays unsigned, where POI cuts it to an
/// `int` (#106).
fn int(section: &Section, id: u32) -> Result<i64, Fault> {
    match section.get(id) {
        None => Ok(0),
        Some(Value::Integer(int)) => Ok(i64::from(*int)),
        Some(Value::Long(long)) => Ok(*long),
        Some(_) => Err(Fault::Runtime),
    }
}

/// POI's `CustomProperties`: the names of the second section's properties, by the id the
/// dictionary gives them, each name once.
struct CustomProperties<'a> {
    names: BTreeMap<u32, String>,
    ids: HashMap<String, u32>,
    values: HashMap<u32, Option<&'a Value>>,
}

/// `DocumentSummaryInformation.getCustomProperties`: `None` without a second section.
fn custom_properties(set: &PropertySet) -> Result<Option<CustomProperties<'_>>, Fault> {
    let Some(section) = set.sections.get(1) else {
        return Ok(None);
    };
    let mut custom = CustomProperties {
        names: BTreeMap::new(),
        ids: HashMap::new(),
        values: HashMap::new(),
    };
    let mut codepage = -1;
    for (id, value) in section.properties() {
        if id == pid::CODEPAGE {
            if let Some(Value::Integer(page)) = value {
                codepage = *page;
            }
            continue;
        }
        let Some(dictionary) = section.dictionary() else {
            continue;
        };
        let Some(name) = dictionary.get(&id) else {
            continue;
        };
        codepage = checked_code_page(codepage)?;
        if let Some(old) = custom.ids.remove(name) {
            custom.names.remove(&old);
            custom.values.remove(&old);
        }
        custom.names.insert(id, name.clone());
        custom.ids.insert(name.clone(), id);
        custom.values.insert(id, value);
    }
    Ok(Some(custom))
}

/// `CustomProperties.checkCodePage`: Unicode where POI refuses the code page; a failure where
/// the JDK has no charset of the name it gives. A charset that cannot encode the name also
/// turns to Unicode, which changes nothing this port reads.
fn checked_code_page(page: i32) -> Result<i32, Fault> {
    let page = if page == -1 { codepage::DEFAULT } else { page };
    if page == codepage::UNICODE {
        return Ok(page);
    }
    match codepage::charset(page) {
        Ok(_) => Ok(page),
        Err(Fault::Encoding) if codepage::encoding(page).is_none() => Ok(codepage::UNICODE),
        Err(_) => Err(Fault::Runtime),
    }
}

impl CustomProperties<'_> {
    /// `get(name)`.
    fn get(&self, name: &str) -> Option<&Value> {
        self.ids
            .get(name)
            .and_then(|id| self.values.get(id))
            .copied()
            .flatten()
    }

    /// `parse(CustomProperties)`: each value Tika takes under `custom:` and its name, in the
    /// order of the ids.
    fn write(&self, metadata: &mut TikaMetadata) -> Result<(), Fault> {
        for (id, name) in &self.names {
            let Some(value) = self.values.get(id).copied().flatten() else {
                continue;
            };
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Date(millis) => format_instant(*millis),
                Value::Boolean(boolean) => boolean.to_string(),
                // Tika cuts it to an `int` (#106).
                Value::Long(long) => long.to_string(),
                Value::Double(double) => java_double_to_string(*double),
                Value::Integer(int) => int.to_string(),
                _ => continue,
            };
            metadata.add_prefixed(USER_DEFINED_PREFIX, name, text);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::ole2::property_set::tests::{
        codepage, dictionary, lpstr, lpwstr, property_set, sections, typed,
    };
    use crate::formats::ole2::property_set::{
        DOCUMENT_SUMMARY_INFORMATION as DSI, SUMMARY_INFORMATION as SI, USER_PROPERTIES,
    };

    fn summary(stream: &[u8]) -> TikaMetadata {
        let set = property_set::read(stream).unwrap().unwrap();
        let mut metadata = TikaMetadata::new();
        let result = if set.is_summary_information() {
            parse_summary_information(set.first_section(), &mut metadata)
        } else {
            parse_document_summary_information(&set, &mut metadata)
        };
        assert!(!matches!(result, Err(Fault::Unported(_))));
        metadata
    }

    const FILETIME_2020: i64 = 132_223_104_000_000_000;

    #[test]
    fn maps_the_summary_information_as_tika_does() {
        let metadata = summary(&property_set(
            SI,
            &[
                (1, codepage(1252)),
                (2, lpstr("Report")),
                (4, lpstr("Ann;Bo;Ann")),
                (5, lpstr("k1")),
                (3, lpstr("Subject")),
                (10, typed(64, &1_210_000_000i64.to_le_bytes())),
                (12, typed(64, &FILETIME_2020.to_le_bytes())),
                (14, typed(3, &3i32.to_le_bytes())),
                (15, typed(3, &0i32.to_le_bytes())),
                (18, lpwstr("Microsoft Excel")),
            ],
        ));
        assert_eq!(metadata.values("dc:title"), ["Report"]);
        assert_eq!(metadata.values("dc:creator"), ["Ann", "Bo"]);
        assert_eq!(metadata.values("meta:keyword"), ["k1"]);
        assert_eq!(metadata.values("dc:subject"), ["k1", "Subject"]);
        assert_eq!(
            metadata.values("extended-properties:Application"),
            ["Microsoft Excel"]
        );
        assert_eq!(metadata.values("dcterms:created"), ["2020-01-01T00:00:00Z"]);
        assert_eq!(metadata.values("extended-properties:TotalTime"), ["2"]);
        assert_eq!(metadata.values("meta:page-count"), ["3"]);
        assert_eq!(metadata.values("xmpTPg:NPages"), ["3"]);
        assert!(metadata.values("meta:word-count").is_empty());
    }

    #[test]
    fn keeps_what_was_set_before_a_getter_throws() {
        let metadata = summary(&property_set(
            SI,
            &[
                (2, lpstr("Kept")),
                (12, lpstr("no date")),
                (14, typed(3, &[1, 0, 0, 0])),
            ],
        ));
        assert_eq!(metadata.values("dc:title"), ["Kept"]);
        assert!(metadata.values("extended-properties:TotalTime").is_empty());
        assert!(metadata.values("meta:page-count").is_empty());
    }

    #[test]
    fn passes_over_bytes_and_writes_numbers() {
        let metadata = summary(&property_set(
            SI,
            &[
                (2, typed(10, &7u32.to_le_bytes())),
                (6, typed(16, &[0xFF])),
                (15, typed(19, &u32::MAX.to_le_bytes())),
            ],
        ));
        assert!(metadata.values("dc:title").is_empty());
        assert_eq!(metadata.values("w:Comments"), ["-1"]);
        assert_eq!(metadata.values("meta:word-count"), ["4294967295"]);
    }

    #[test]
    fn writes_no_edit_time_where_there_is_none() {
        let metadata = summary(&property_set(SI, &[(2, lpstr("Untimed"))]));
        assert!(metadata.values("extended-properties:TotalTime").is_empty());
    }

    #[test]
    fn maps_custom_properties_as_tika_does() {
        let metadata = summary(&sections(&[
            (DSI, vec![(15, lpstr("Acme")), (14, lpstr("Boss;Boss2"))]),
            (
                USER_PROPERTIES,
                vec![
                    (1, codepage(1252)),
                    (
                        0,
                        dictionary(&[(5, "Language"), (3, "Due"), (4, "Big"), (6, "Rate")]),
                    ),
                    (5, lpstr("da-DK")),
                    (3, typed(64, &FILETIME_2020.to_le_bytes())),
                    (4, typed(19, &u32::MAX.to_le_bytes())),
                    (6, typed(5, &0.5f64.to_le_bytes())),
                ],
            ),
        ]));
        assert_eq!(metadata.values("extended-properties:Company"), ["Acme"]);
        assert_eq!(
            metadata.values("extended-properties:Manager"),
            ["Boss", "Boss2"]
        );
        assert_eq!(metadata.values("dc:language"), ["da-DK"]);
        assert_eq!(metadata.values("custom:Due"), ["2020-01-01T00:00:00Z"]);
        assert_eq!(metadata.values("custom:Big"), ["4294967295"]);
        assert_eq!(metadata.values("custom:Rate"), ["0.5"]);
        assert_eq!(metadata.values("custom:Language"), ["da-DK"]);
    }

    #[test]
    fn reads_a_document_summary_by_its_format_id() {
        let metadata = summary(&property_set(DSI, &[(2, lpstr("Cat"))]));
        assert_eq!(metadata.values("cp:category"), ["Cat"]);
    }

    /// What `SummaryReference.java` wrote for each document, by its key.
    #[cfg(feature = "tika")]
    fn reference() -> Vec<(String, Vec<String>)> {
        use std::io::Read;
        const SUMMARIES: &[u8] = include_bytes!("../../../tests/fixtures/ole2/summaries.tsv.gz");
        let mut text = String::new();
        flate2::read::GzDecoder::new(SUMMARIES)
            .read_to_string(&mut text)
            .unwrap();
        let mut documents: Vec<(String, Vec<String>)> = Vec::new();
        for line in text.lines() {
            match line.strip_prefix('#') {
                Some(key) => documents.push((key.to_owned(), Vec::new())),
                None => documents.last_mut().unwrap().1.push(line.to_owned()),
            }
        }
        documents
    }

    #[cfg(feature = "tika")]
    fn escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('\t', "\\t")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    }

    /// The lines `SummaryReference.java` writes for a document, or why the port leaves it to
    /// Tika.
    #[cfg(feature = "tika")]
    fn lines(data: &[u8]) -> Result<Vec<String>, Unported> {
        let Ok(file) = Ole2::open(data) else {
            return Ok(vec!["!unreadable".to_owned()]);
        };
        let mut metadata = TikaMetadata::new();
        Summaries::read(&file, file.root())?.write(&mut metadata);
        let mut names: Vec<&str> = metadata.names().collect();
        names.sort_unstable();
        Ok(names
            .iter()
            .flat_map(|name| {
                metadata
                    .values(name)
                    .iter()
                    .map(move |value| format!("{}\t{}", escape(name), escape(value)))
            })
            .collect())
    }

    #[cfg(feature = "tika")]
    const TOTAL_TIME: &str = "extended-properties:TotalTime\t";

    /// Tika's lines with the edit time as this port writes it, where the port's agrees (#106):
    /// the raw file time in whole minutes, and no line for Tika's `0` where the port has none.
    #[cfg(feature = "tika")]
    fn with_edit_minutes(expected: &[String], actual: &[String]) -> Vec<String> {
        let edit_time = |lines: &[String]| {
            lines
                .iter()
                .find_map(|line| line.strip_prefix(TOTAL_TIME))
                .map(str::to_owned)
        };
        let Some(raw) = edit_time(expected) else {
            return expected.to_vec();
        };
        let file_time: i64 = raw.parse().unwrap();
        let minutes = (file_time != 0).then(|| (file_time / 600_000_000).to_string());
        if edit_time(actual) != minutes {
            return expected.to_vec();
        }
        expected
            .iter()
            .filter_map(|line| match (line.strip_prefix(TOTAL_TIME), &minutes) {
                (None, _) => Some(line.clone()),
                (Some(_), Some(minutes)) => Some(format!("{TOTAL_TIME}{minutes}")),
                (Some(_), None) => None,
            })
            .collect()
    }

    /// Compares the documents under `root`, named `name` in the reference.
    #[cfg(feature = "tika")]
    fn assert_same_as_tika(name: &str, root: &std::path::Path) {
        let mut compared = 0;
        let mut unported = Vec::new();
        let mut different = Vec::new();
        for (key, expected) in reference() {
            let Some(path) = key.strip_prefix(&format!("{name}/")) else {
                continue;
            };
            let data = std::fs::read(root.join(path)).unwrap();
            match lines(&data) {
                Ok(actual) if actual == with_edit_minutes(&expected, &actual) => compared += 1,
                Ok(actual) => {
                    different.push(format!("{key}\n  tika {expected:?}\n  port {actual:?}"))
                }
                Err(Unported(reason)) => unported.push(format!("{key}: {reason}")),
            }
        }
        println!("{compared} the same, unported: {unported:#?}");
        assert!(different.is_empty(), "{}", different.join("\n"));
        assert!(unported.is_empty(), "{unported:#?}");
        assert!(compared > 0);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn reads_the_test_documents_as_tika_does() {
        assert_same_as_tika("documents", std::path::Path::new("../test_files/documents"));
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
    fn reads_tikas_test_documents_as_tika_does() {
        assert_same_as_tika("corpus", &crate::formats::parity::conformance_corpus());
    }

    #[test]
    fn splits_multiple_values_as_tika_does() {
        let mut metadata = TikaMetadata::new();
        add_multi(&mut metadata, "dc:creator", "Anna; Bo;Anna;;");
        assert_eq!(metadata.values("dc:creator"), ["Anna", " Bo"]);
    }
}
