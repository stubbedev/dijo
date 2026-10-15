//! The metadata of a package as Tika's `SAXBasedMetadataExtractor` takes it from the core
//! properties POI reads, `docProps/app.xml` and `docProps/custom.xml`, named and formatted as
//! Tika names and formats them.

use std::collections::HashSet;

use super::package::refuse_document_type;
use super::package::{Package, Unported, CORE_PROPERTIES_CONTENT_TYPE};
use super::read_quietly;
use crate::formats::java::{self, parse_long};
use crate::formats::metadata::{TikaMetadata, USER_DEFINED_PREFIX};
use crate::formats::ole2::add_multi;
use crate::formats::xml::{
    self, attribute, attribute_named, Attribute, SaxHandler, StartTag, XMLNS,
};
use crate::formats::Failure;
use crate::unicode::decimal_digit;

const CORE_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const DC_NAMESPACE: &str = "http://purl.org/dc/elements/1.1/";
const DCTERMS_NAMESPACE: &str = "http://purl.org/dc/terms/";
const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const MARKUP_COMPATIBILITY_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/markup-compatibility/2006";
const VT_NAMESPACE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes";
const EXTENDED_PROPERTIES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties";
const CUSTOM_PROPERTIES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties";
/// `SAXBasedMetadataExtractor.MAX_TEXT_BUFFER_LENGTH`, in UTF-16 code units.
const MAX_TEXT_LENGTH: usize = 64 * 1024;
/// `SAXBasedMetadataExtractor.MAX_DECIMAL_LENGTH`.
const MAX_DECIMAL_LENGTH: usize = 256;
/// The largest number of digits this port writes a decimal custom property out with.
const MAX_PLAIN_DECIMAL_DIGITS: i64 = 100_000;

/// The core properties POI reads, in the order Tika takes them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct CoreProperties {
    category: Option<String>,
    content_status: Option<String>,
    created: Option<Instant>,
    creator: Option<String>,
    description: Option<String>,
    identifier: Option<String>,
    keywords: Option<String>,
    language: Option<String>,
    last_modified_by: Option<String>,
    last_printed: Option<Instant>,
    modified: Option<Instant>,
    revision: Option<String>,
    subject: Option<String>,
    title: Option<String>,
    version: Option<String>,
}

/// Seconds since the epoch, as a `java.util.Date` Tika formats truncated to seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Instant(i64);

impl Instant {
    /// `DateUtils.formatDate`: ISO 8601 in UTC, to the second.
    fn format(self) -> String {
        let days = self.0.div_euclid(86_400);
        let seconds = self.0.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    }
}

/// The metadata of the package, written to `metadata` as `SAXBasedMetadataExtractor.extract`
/// writes it.
pub(crate) fn extract(package: &Package, metadata: &mut TikaMetadata) -> Result<(), Unported> {
    if let Some(core) = core_properties(package)? {
        core.write(metadata);
    }
    if let Some(part) = package_part(package, EXTENDED_PROPERTIES) {
        if let Some(extended) = read_quietly(package, part, ExtendedProperties::default())? {
            extended.write(metadata);
        }
    }
    if let Some(part) = package_part(package, CUSTOM_PROPERTIES) {
        if let Some(custom) = read_quietly(package, part, CustomProperties::default())? {
            custom.write(metadata)?;
        }
    }
    Ok(())
}

fn unreadable(package: &Package, part: usize) -> Unported {
    Unported(format!("the part {} cannot be read", package.name(part)))
}

fn package_part(package: &Package, kind: &str) -> Option<usize> {
    Package::relationships_of(&package.relationships, kind)
        .next()
        .and_then(|relationship| package.package_part(relationship))
}

/// The core properties of the first part of their content type POI reads, as `getParts`
/// unmarshals them; a part whose XML is malformed is passed over, one breaking a rule POI
/// enforces fails the package.
fn core_properties(package: &Package) -> Result<Option<CoreProperties>, Unported> {
    for part in package.parts_by_content_type(CORE_PROPERTIES_CONTENT_TYPE) {
        let bytes = package.read(part).map_err(|_| unreadable(package, part))?;
        let mut reader = CoreReader::default();
        if xml::parse_sax(&bytes, package.name(part), &mut reader).is_err() {
            continue;
        }
        if let Some(violation) = reader.violation {
            return Err(Unported(format!(
                "POI refuses the core properties of {}: {violation}",
                package.name(part)
            )));
        }
        return reader.properties().map(Some);
    }
    Ok(None)
}

impl CoreProperties {
    fn write(&self, metadata: &mut TikaMetadata) {
        set(metadata, "cp:category", self.category.as_deref());
        set(metadata, "cp:contentStatus", self.content_status.as_deref());
        set_date(metadata, "dcterms:created", self.created);
        if let Some(creator) = &self.creator {
            add_multi(metadata, "dc:creator", creator);
        }
        set(metadata, "dc:description", self.description.as_deref());
        set(metadata, "dc:identifier", self.identifier.as_deref());
        if let Some(subject) = &self.subject {
            metadata.add("dc:subject", subject.clone());
        }
        if let Some(keywords) = &self.keywords {
            metadata.add("meta:keyword", keywords.clone());
            metadata.add("dc:subject", keywords.clone());
        }
        set(metadata, "dc:language", self.language.as_deref());
        set(
            metadata,
            "meta:last-author",
            self.last_modified_by.as_deref(),
        );
        set_date(metadata, "meta:print-date", self.last_printed);
        set_date(metadata, "dcterms:modified", self.modified);
        set(metadata, "cp:revision", self.revision.as_deref());
        set(metadata, "dc:title", self.title.as_deref());
        set(metadata, "cp:version", self.version.as_deref());
    }
}

fn set(metadata: &mut TikaMetadata, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        metadata.set(name, value);
    }
}

fn set_date(metadata: &mut TikaMetadata, name: &str, value: Option<Instant>) {
    if let Some(value) = value {
        metadata.set(name, value.format());
    }
}

const CORE_FIELDS: [(&str, &str); 16] = [
    (CORE_NAMESPACE, "category"),
    (CORE_NAMESPACE, "contentStatus"),
    (CORE_NAMESPACE, "contentType"),
    (DCTERMS_NAMESPACE, "created"),
    (DC_NAMESPACE, "creator"),
    (DC_NAMESPACE, "description"),
    (DC_NAMESPACE, "identifier"),
    (CORE_NAMESPACE, "keywords"),
    (DC_NAMESPACE, "language"),
    (CORE_NAMESPACE, "lastModifiedBy"),
    (CORE_NAMESPACE, "lastPrinted"),
    (DCTERMS_NAMESPACE, "modified"),
    (CORE_NAMESPACE, "revision"),
    (DC_NAMESPACE, "subject"),
    (DC_NAMESPACE, "title"),
    (CORE_NAMESPACE, "version"),
];

/// Reads the core properties as POI's `PackagePropertiesUnmarshaller`: the text of the first
/// element of each field below the root, and the OPC rules it checks every element against.
#[derive(Default)]
struct CoreReader {
    depth: usize,
    texts: [Option<String>; 16],
    /// The fields being read, with the depth of their element.
    open: Vec<(usize, usize)>,
    violation: Option<String>,
}

impl CoreReader {
    fn text(&self, local: &str) -> Option<&str> {
        CORE_FIELDS
            .iter()
            .position(|(_, field)| *field == local)
            .and_then(|index| self.texts[index].as_deref())
            .filter(|text| !text.is_empty())
    }

    fn properties(&self) -> Result<CoreProperties, Unported> {
        let string = |local| self.text(local).map(str::to_owned);
        let date = |local| self.text(local).map(parse_core_date).transpose();
        Ok(CoreProperties {
            category: string("category"),
            content_status: string("contentStatus"),
            created: date("created")?,
            creator: string("creator"),
            description: string("description"),
            identifier: string("identifier"),
            keywords: string("keywords"),
            language: string("language"),
            last_modified_by: string("lastModifiedBy"),
            last_printed: date("lastPrinted")?,
            modified: date("modified")?,
            revision: string("revision"),
            subject: string("subject"),
            title: string("title"),
            version: string("version"),
        })
    }

    /// `checkElementForOPCCompliance` on one element.
    fn check(&mut self, tag: &StartTag<'_>) {
        if self.violation.is_some() {
            return;
        }
        let first = tag
            .attributes
            .iter()
            .chain(tag.declarations)
            .min_by(|a, b| {
                a.qualified_name
                    .encode_utf16()
                    .cmp(b.qualified_name.encode_utf16())
            });
        if let Some(first) = first {
            if first.namespace == XMLNS && first.value == MARKUP_COMPATIBILITY_NAMESPACE {
                self.violation = Some("it uses the Markup Compatibility namespace [M4.2]".into());
                return;
            }
        }
        let name = tag.qualified_name;
        if tag.namespace == DCTERMS_NAMESPACE {
            if tag.local_name != "created" && tag.local_name != "modified" {
                self.violation = Some(format!("it refines Dublin Core with {name} [M4.3]"));
                return;
            }
            let prefix = name.split_once(':').map_or("null", |(prefix, _)| prefix);
            if attribute(tag.attributes, XSI_NAMESPACE, "type") != Some(&format!("{prefix}:W3CDTF"))
            {
                self.violation = Some(format!("{name} is not typed as a W3CDTF date"));
                return;
            }
        }
        if attribute(tag.attributes, XML_NAMESPACE, "lang").is_some() {
            self.violation = Some(format!("{name} has the attribute xml:lang [M4.4]"));
        }
    }
}

impl SaxHandler for CoreReader {
    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        self.check(tag);
        self.start_element(tag.namespace, tag.local_name, tag.attributes)
    }

    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if self.depth > 0 {
            if let Some(index) = CORE_FIELDS
                .iter()
                .position(|&(uri, local)| uri == namespace && local == local_name)
            {
                if self.texts[index].is_none() {
                    self.texts[index] = Some(String::new());
                    self.open.push((index, self.depth));
                }
            }
        }
        self.depth += 1;
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        self.depth -= 1;
        if self
            .open
            .last()
            .is_some_and(|&(_, depth)| depth == self.depth)
        {
            self.open.pop();
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        for &(index, _) in &self.open {
            if let Some(field) = &mut self.texts[index] {
                field.push_str(text);
            }
        }
        Ok(())
    }

    fn document_type(&mut self) -> Result<(), Failure> {
        refuse_document_type()
    }
}

/// `PackagePropertiesPart.parseDateValue` for the shapes this port reads: a date with or without
/// a time to the second, a fraction of a second, and `Z` or an offset. POI fails on a date it
/// cannot read; one of another shape is [`Unported`].
fn parse_core_date(text: &str) -> Result<Instant, Unported> {
    let unported = || Unported(format!("the core properties have the date {text:?}"));
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let field = bytes.get(range)?;
        field.iter().all(u8::is_ascii_digit).then(|| {
            field
                .iter()
                .fold(0, |value, &d| value * 10 + i64::from(d - b'0'))
        })
    };
    let at = |index: usize, expected: u8| bytes.get(index) == Some(&expected);
    let (year, month, day) = match (digits(0..4), digits(5..7), digits(8..10)) {
        (Some(year), Some(month), Some(day)) if at(4, b'-') && at(7, b'-') => (year, month, day),
        _ => return Err(unported()),
    };
    if !(1583..=9999).contains(&year) {
        return Err(unported());
    }
    let date = days_from_civil_lenient(year, month, day) * 86_400;
    if bytes.len() == 10 || (bytes.len() == 11 && at(10, b'Z')) {
        return Ok(Instant(date));
    }
    let (Some(hour), Some(minute), Some(second)) = (digits(11..13), digits(14..16), digits(17..19))
    else {
        return Err(unported());
    };
    if !(at(10, b'T') && at(13, b':') && at(16, b':')) {
        return Err(unported());
    }
    let mut rest = &text[19..];
    let mut milliseconds = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let length = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if length == 0 || length > 9 {
            return Err(unported());
        }
        milliseconds = fraction[..length].parse::<i64>().map_err(|_| unported())?;
        rest = &fraction[length..];
    }
    let offset = match rest {
        "" | "Z" => 0,
        _ => parse_offset(rest).ok_or_else(unported)?,
    };
    let seconds = date + hour * 3600 + minute * 60 + second - offset;
    Ok(Instant(seconds + milliseconds.div_euclid(1000)))
}

/// An offset `+HH:MM` or `+HHMM`, in seconds.
fn parse_offset(text: &str) -> Option<i64> {
    let sign = match text.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = text[1..].replacen(':', "", 1);
    if digits.len() != 4 || !digits.bytes().all(|d| d.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Days since 1970-01-01 of a Gregorian date, a month or day out of range carried over as a
/// lenient `GregorianCalendar` carries it.
fn days_from_civil_lenient(year: i64, month: i64, day: i64) -> i64 {
    let year = year + (month - 1).div_euclid(12);
    let month = (month - 1).rem_euclid(12) + 1;
    days_from_civil(year, month, 1) + day - 1
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date of a day since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Appends to a property's text up to `MAX_TEXT_BUFFER_LENGTH` UTF-16 code units.
fn append_capped(buffer: &mut String, length: &mut usize, text: &str) {
    for character in text.chars() {
        if *length >= MAX_TEXT_LENGTH {
            return;
        }
        buffer.push(character);
        *length += character.len_utf16();
    }
}

/// `ExtendedPropertiesHandler`: `docProps/app.xml`.
#[derive(Default)]
struct ExtendedProperties {
    application: Option<String>,
    app_version: Option<String>,
    company: Option<String>,
    manager: Option<String>,
    notes: Option<String>,
    presentation_format: Option<String>,
    template: Option<String>,
    total_time: i32,
    doc_security: i32,
    pages: i32,
    slides: i32,
    paragraphs: i32,
    lines: i32,
    words: i32,
    characters: i32,
    characters_with_spaces: i32,
    current: Option<String>,
    text: String,
    text_length: usize,
}

impl SaxHandler for ExtendedProperties {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.current = Some(local_name.to_owned());
        self.text.clear();
        self.text_length = 0;
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if self.current.as_deref() != Some(local_name) {
            return Ok(());
        }
        let value = java::trim(&self.text).to_owned();
        self.current = None;
        if value.is_empty() {
            return Ok(());
        }
        let number = || safe_parse_int(&value);
        match local_name {
            "Application" => self.application = Some(value),
            "AppVersion" => self.app_version = Some(value),
            "Company" => self.company = Some(value),
            "Manager" => self.manager = Some(value),
            "Notes" => self.notes = Some(value),
            "PresentationFormat" => self.presentation_format = Some(value),
            "Template" => self.template = Some(value),
            "TotalTime" => self.total_time = number(),
            "DocSecurity" => self.doc_security = number(),
            "Pages" => self.pages = number(),
            "Slides" => self.slides = number(),
            "Paragraphs" => self.paragraphs = number(),
            "Lines" => self.lines = number(),
            "Words" => self.words = number(),
            "Characters" => self.characters = number(),
            "CharactersWithSpaces" => self.characters_with_spaces = number(),
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        append_capped(&mut self.text, &mut self.text_length, text);
        Ok(())
    }
}

/// `safeParseInt`: a `long` within `0..=Integer.MAX_VALUE`, else 0.
fn safe_parse_int(value: &str) -> i32 {
    parse_long(value)
        .and_then(|number| i32::try_from(number).ok())
        .filter(|number| *number >= 0)
        .unwrap_or(0)
}

impl ExtendedProperties {
    fn write(&self, metadata: &mut TikaMetadata) {
        let prefix = |name: &str| format!("extended-properties:{name}");
        set(
            metadata,
            &prefix("Application"),
            self.application.as_deref(),
        );
        set(metadata, &prefix("AppVersion"), self.app_version.as_deref());
        set(metadata, "dc:publisher", self.company.as_deref());
        set(metadata, &prefix("Company"), self.company.as_deref());
        if let Some(manager) = &self.manager {
            add_multi(metadata, &prefix("Manager"), manager);
        }
        set(metadata, &prefix("Notes"), self.notes.as_deref());
        set(
            metadata,
            &prefix("PresentationFormat"),
            self.presentation_format.as_deref(),
        );
        set(metadata, &prefix("Template"), self.template.as_deref());
        set_positive(metadata, &prefix("TotalTime"), self.total_time);
        set_positive(metadata, &prefix("DocSecurity"), self.doc_security);
        metadata.set(
            &prefix("doc-security-string"),
            doc_security_string(self.doc_security),
        );
        if self.pages > 0 {
            metadata.set("xmpTPg:NPages", self.pages.to_string());
        } else if self.slides > 0 {
            metadata.set("xmpTPg:NPages", self.slides.to_string());
        }
        set_positive(metadata, "meta:page-count", self.pages);
        set_positive(metadata, "meta:slide-count", self.slides);
        set_positive(metadata, "meta:paragraph-count", self.paragraphs);
        set_positive(metadata, "meta:line-count", self.lines);
        set_positive(metadata, "meta:word-count", self.words);
        set_positive(metadata, "meta:character-count", self.characters);
        set_positive(
            metadata,
            "meta:character-count-with-spaces",
            self.characters_with_spaces,
        );
    }
}

fn set_positive(metadata: &mut TikaMetadata, name: &str, value: i32) {
    if value > 0 {
        metadata.set(name, value.to_string());
    }
}

fn doc_security_string(flag: i32) -> &'static str {
    match flag {
        0 => "None",
        1 => "PasswordProtected",
        2 => "ReadOnlyRecommended",
        4 => "ReadOnlyEnforced",
        8 => "LockedForAnnotations",
        _ => "Unknown",
    }
}

/// `CustomPropertiesHandler`: `docProps/custom.xml`.
#[derive(Default)]
struct CustomProperties {
    /// The values by name, in the order of a `LinkedHashMap`.
    values: Vec<(String, String)>,
    dates: HashSet<String>,
    current_name: Option<String>,
    current_type: Option<String>,
    text: String,
    text_length: usize,
    /// A decimal this port does not write out.
    unported: Option<String>,
}

impl CustomProperties {
    fn put(&mut self, name: String, value: String) {
        match self.values.iter_mut().find(|(known, _)| *known == name) {
            Some((_, known)) => *known = value,
            None => self.values.push((name, value)),
        }
    }

    fn write(&self, metadata: &mut TikaMetadata) -> Result<(), Unported> {
        if let Some(unported) = &self.unported {
            return Err(Unported(unported.clone()));
        }
        for (name, value) in &self.values {
            let value = if self.dates.contains(name) {
                parse_offset_date_time(value).map_or_else(|| value.clone(), Instant::format)
            } else {
                value.clone()
            };
            metadata.add_prefixed(USER_DEFINED_PREFIX, name, value);
        }
        Ok(())
    }
}

impl SaxHandler for CustomProperties {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name == "property" {
            self.current_name = attribute_named(attributes, "name").map(str::to_owned);
            self.current_type = None;
        } else if namespace == VT_NAMESPACE
            && self.current_name.is_some()
            && self.current_type.is_none()
        {
            self.current_type = Some(local_name.to_owned());
            self.text.clear();
            self.text_length = 0;
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        let typed = namespace == VT_NAMESPACE && self.current_type.as_deref() == Some(local_name);
        if let (true, Some(property)) = (typed, self.current_name.clone()) {
            let raw = self.text.clone();
            let trimmed = java::trim(&raw).to_owned();
            match local_name {
                "lpwstr" | "lpstr" | "bstr" => self.put(property, raw),
                "filetime" | "date" => {
                    self.dates.insert(property.clone());
                    self.put(property, trimmed);
                }
                "bool" => match trimmed.as_str() {
                    "1" | "true" => self.put(property, "true".to_owned()),
                    "0" | "false" => self.put(property, "false".to_owned()),
                    _ => {}
                },
                "i1" | "i2" | "i4" | "int" | "ui1" | "ui2" | "i8" | "ui4" | "ui8" | "uint"
                | "r4" | "r8" => self.put(property, trimmed),
                "decimal" if trimmed.encode_utf16().count() <= MAX_DECIMAL_LENGTH => {
                    match plain_decimal(&trimmed) {
                        Ok(Some(plain)) => self.put(property, plain),
                        Ok(None) => {}
                        Err(reason) => self.unported = Some(reason),
                    }
                }
                _ => {}
            }
            self.current_type = None;
        } else if local_name == "property" {
            self.current_name = None;
            self.current_type = None;
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        append_capped(&mut self.text, &mut self.text_length, text);
        Ok(())
    }
}

/// `new BigDecimal(text).toPlainString()`, `None` where `BigDecimal` rejects the text.
fn plain_decimal(text: &str) -> Result<Option<String>, String> {
    let (negative, rest) = match text.chars().next() {
        Some('-') => (true, &text[1..]),
        Some('+') => (false, &text[1..]),
        _ => (false, text),
    };
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(index) => (&rest[..index], Some(&rest[index + 1..])),
        None => (rest, None),
    };
    let (integer, fraction) = match mantissa.find('.') {
        Some(index) => (&mantissa[..index], &mantissa[index + 1..]),
        None => (mantissa, ""),
    };
    let digits: Option<String> = integer
        .chars()
        .chain(fraction.chars())
        .map(|c| decimal_digit(c).and_then(|d| char::from_digit(d, 10)))
        .collect();
    let Some(digits) = digits else {
        return Ok(None);
    };
    if digits.is_empty() {
        return Ok(None);
    }
    let exponent = match exponent {
        None => 0,
        Some(exponent) => match parse_long(exponent).filter(|e| i32::try_from(*e).is_ok()) {
            Some(exponent) => exponent,
            None => return Ok(None),
        },
    };
    let fraction_length = i64::try_from(fraction.chars().count()).map_err(|e| e.to_string())?;
    let scale = fraction_length - exponent;
    if i32::try_from(scale).is_err() {
        return Ok(None);
    }
    if scale.abs() > MAX_PLAIN_DECIMAL_DIGITS {
        return Err(format!(
            "the custom property decimal {text:?} is written out with {} digits",
            scale.abs()
        ));
    }
    let unscaled = digits.trim_start_matches('0');
    let zero = unscaled.is_empty();
    let unscaled = if zero { "0" } else { unscaled };
    let sign = if negative && !zero { "-" } else { "" };
    let plain = match usize::try_from(scale) {
        Ok(0) => unscaled.to_owned(),
        Err(_) if zero => "0".to_owned(),
        Err(_) => format!("{unscaled}{}", "0".repeat(scale.unsigned_abs() as usize)),
        Ok(scale) if unscaled.len() > scale => {
            let point = unscaled.len() - scale;
            format!("{}.{}", &unscaled[..point], &unscaled[point..])
        }
        Ok(scale) => format!("0.{}{unscaled}", "0".repeat(scale - unscaled.len())),
    };
    Ok(Some(format!("{sign}{plain}")))
}

/// `OffsetDateTime.parse(text).toInstant()` to the second: an ISO date, a time with optional
/// seconds and fraction, and `Z` or an offset, in any case.
fn parse_offset_date_time(text: &str) -> Option<Instant> {
    let upper = text.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let field = bytes.get(range)?;
        field.iter().all(u8::is_ascii_digit).then(|| {
            field
                .iter()
                .fold(0, |value, &d| value * 10 + i64::from(d - b'0'))
        })
    };
    let at = |index: usize, expected: u8| bytes.get(index) == Some(&expected);
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute) = (number(11..13)?, number(14..16)?);
    if !(at(4, b'-') && at(7, b'-') && at(10, b'T') && at(13, b':')) {
        return None;
    }
    let mut position = 16;
    let mut second = 0;
    if at(16, b':') {
        second = number(17..19)?;
        position = 19;
        if at(19, b'.') {
            let length = bytes[20..]
                .iter()
                .take_while(|d| d.is_ascii_digit())
                .count();
            if length > 9 {
                return None;
            }
            position = 20 + length;
        }
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&month)
        || day < 1
        || day > month_days[usize::try_from(month - 1).ok()?]
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let offset = match &upper[position..] {
        "Z" => 0,
        zone => {
            let sign = match zone.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let parts: Vec<&str> = zone[1..].split(':').collect();
            if parts.len() < 2 || parts.len() > 3 || parts.iter().any(|part| part.len() != 2) {
                return None;
            }
            let field = |part: &str| -> Option<i64> {
                part.bytes()
                    .all(|d| d.is_ascii_digit())
                    .then(|| part.parse().ok())
                    .flatten()
            };
            let (hours, minutes) = (field(parts[0])?, field(parts[1])?);
            let seconds = parts.get(2).map_or(Some(0), |part| field(part))?;
            if hours > 18 || minutes > 59 || seconds > 59 {
                return None;
            }
            sign * (hours * 3600 + minutes * 60 + seconds)
        }
    };
    let days = days_from_civil(year, month, day);
    Some(Instant(
        days * 86_400 + hour * 3600 + minute * 60 + second - offset,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_dates_as_tika_does() {
        assert_eq!(Instant(0).format(), "1970-01-01T00:00:00Z");
        assert_eq!(
            parse_core_date("2011-07-29T16:52:00Z").unwrap().format(),
            "2011-07-29T16:52:00Z"
        );
        assert_eq!(
            parse_core_date("2011-07-29T16:52:00.5+02:00")
                .unwrap()
                .format(),
            "2011-07-29T14:52:00Z"
        );
        assert_eq!(
            parse_core_date("2011-07-29").unwrap().format(),
            "2011-07-29T00:00:00Z"
        );
        assert_eq!(
            parse_core_date("2011-13-32T25:00:00Z").unwrap().format(),
            "2012-02-02T01:00:00Z"
        );
        assert!(parse_core_date("29/07/2011").is_err());
        assert!(parse_core_date("1500-01-01T00:00:00Z").is_err());
    }

    #[test]
    fn parses_offset_date_times_as_java_does() {
        let format = |text| parse_offset_date_time(text).map(Instant::format);
        assert_eq!(
            format("2010-12-30T23:00:00Z").as_deref(),
            Some("2010-12-30T23:00:00Z")
        );
        assert_eq!(
            format("2010-12-31t01:00:00.123+02:00").as_deref(),
            Some("2010-12-30T23:00:00Z")
        );
        assert_eq!(
            format("2010-12-30T23:00Z").as_deref(),
            Some("2010-12-30T23:00:00Z")
        );
        assert_eq!(format("2010-02-30T23:00:00Z"), None);
        assert_eq!(format("2010-12-30T23:00:00"), None);
        assert_eq!(format("2010-12-30"), None);
    }

    #[test]
    fn writes_decimals_as_big_decimal_does() {
        let plain = |text| plain_decimal(text).unwrap();
        assert_eq!(plain("1.50").as_deref(), Some("1.50"));
        assert_eq!(plain("1E+3").as_deref(), Some("1000"));
        assert_eq!(plain("1.5E-3").as_deref(), Some("0.0015"));
        assert_eq!(plain("-0").as_deref(), Some("0"));
        assert_eq!(plain("-0.00").as_deref(), Some("0.00"));
        assert_eq!(plain("+007").as_deref(), Some("7"));
        assert_eq!(plain("-12.3").as_deref(), Some("-12.3"));
        assert_eq!(plain("0E+3").as_deref(), Some("0"));
        assert_eq!(plain("abc"), None);
        assert_eq!(plain("."), None);
        assert_eq!(plain("1e"), None);
    }

    #[test]
    fn splits_multiple_values_as_tika_does() {
        let mut metadata = TikaMetadata::new();
        add_multi(&mut metadata, "dc:creator", "Anna; Bo;Anna;;");
        assert_eq!(metadata.values("dc:creator"), ["Anna", " Bo"]);
    }
}
