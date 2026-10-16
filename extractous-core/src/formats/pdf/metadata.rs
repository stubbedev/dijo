//! The metadata Tika's `PDFParser` takes from a document before writing its text: the offsets
//! of its `%%EOF` markers (`StartXRefScanner`), its header version, the access permissions,
//! the document information dictionary and the XMP packet (`PDMetadataExtractor`), in the order
//! Tika sets them.

use lopdf::Object;

use super::content::parse_literal;
use super::cos::{name_string, text_string, Pdf};
use super::jfloat;
use super::recording::Recording;
use super::{date, Unported};
use crate::formats::xmp::{
    self, Property, DCTERMS_CREATED, DCTERMS_MODIFIED, DC_CREATOR, DC_SUBJECT, DC_TITLE,
};
use crate::java::is_blank;
use crate::java::time::format_instant;

const STARTXREF: &[u8] = b"startxref";
/// `StartXRefScanner.MAX_LENGTH_LONG`.
const MAX_LENGTH_LONG: usize = 19;

/// `StartXRefOffset`: a `startxref` found, and where its `%%EOF` marker ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StartXRef {
    pub(super) startxref: i64,
    pub(super) end_eof_offset: usize,
}

fn is_whitespace(c: Option<u8>) -> bool {
    matches!(c, Some(0 | 9 | 12 | 10 | 13 | 32))
}

/// `StartXRefScanner.scan`: every `startxref` keyword with the number after it and the end of
/// the `%%EOF` marker that follows, if any.
pub(super) fn scan_startxrefs(input: &[u8]) -> Vec<StartXRef> {
    let mut offsets = Vec::new();
    let mut position = 0;
    while position + STARTXREF.len() <= input.len() {
        let Some(found) = input[position..]
            .windows(STARTXREF.len())
            .position(|window| window == STARTXREF)
        else {
            break;
        };
        let start = position + found;
        let mut cursor = start + STARTXREF.len();
        if let Some(entry) = read_startxref(input, &mut cursor) {
            offsets.push(entry);
        }
        position = cursor.max(start + STARTXREF.len());
    }
    offsets
}

/// `readStartXRef`: the number after `startxref` and the `%%EOF` after it; `None` where
/// reading the number fails, the cursor left where the failure left it.
fn read_startxref(input: &[u8], cursor: &mut usize) -> Option<StartXRef> {
    let byte = |at: usize| input.get(at).copied();
    loop {
        let c = byte(*cursor);
        if c == Some(b'%') {
            *cursor += 1;
            while !matches!(byte(*cursor), None | Some(b'\n' | b'\r')) {
                *cursor += 1;
            }
            if byte(*cursor).is_some() {
                *cursor += 1;
            }
        } else if is_whitespace(c) {
            *cursor += 1;
        } else {
            break;
        }
    }
    let digits_start = *cursor;
    while let Some(b'0'..=b'9') = byte(*cursor) {
        *cursor += 1;
        if *cursor - digits_start > MAX_LENGTH_LONG {
            return None;
        }
    }
    byte(*cursor)?;
    let digits = std::str::from_utf8(&input[digits_start..*cursor]).unwrap_or_default();
    let startxref = digits.parse::<i64>().ok()?;
    while is_whitespace(byte(*cursor)) {
        *cursor += 1;
    }
    let marker = b"%%EOF";
    if input[*cursor..].starts_with(marker) {
        *cursor += marker.len();
        if matches!(byte(*cursor), Some(b'\n' | b'\r')) {
            *cursor += 1;
        }
    }
    Some(StartXRef {
        startxref,
        end_eof_offset: *cursor,
    })
}

/// `COSParser.parseHeader`: the version of the `%PDF-` header, as PDFBox reads it.
pub(super) fn header_version(input: &[u8]) -> Result<f32, Unported> {
    const MARKER: &str = "%PDF-";
    let mut lines = LineReader { input, position: 0 };
    let mut header = lines.next_line()?;
    if !header.contains(MARKER) {
        header = lines.next_line()?;
        while !header.contains(MARKER) {
            if header.chars().next().is_some_and(crate::java::is_digit) {
                break;
            }
            header = lines.next_line()?;
        }
    }
    let Some(start) = header.find(MARKER) else {
        return Err(Unported(
            "a document without a %PDF- header, which PDFBox fails on".to_owned(),
        ));
    };
    let header: Vec<char> = header[start..].chars().collect();
    let version: Vec<char> = header[MARKER.len()..].to_vec();
    let exact = version.len() == 3 && version[0].is_ascii_digit() && version[2].is_ascii_digit();
    let version: String = if exact {
        version.iter().collect()
    } else if version.len() < 3 {
        "1.4".to_owned()
    } else {
        version[..3].iter().collect()
    };
    let parts = java_split_count(&format!("{MARKER}{version}"));
    let parsed = if parts == 2 {
        java_parse_float(&version)
    } else {
        None
    };
    Ok(match parsed {
        Some(version) if version >= 0.0 => version,
        _ => 1.7,
    })
}

/// The number of parts `String.split("-")` gives: trailing empty strings removed.
fn java_split_count(text: &str) -> usize {
    let mut parts: Vec<&str> = text.split('-').collect();
    while parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    parts.len()
}

/// `Float.parseFloat` of a short decimal: trimmed, with an optional type suffix, digits with
/// at most one dot.
fn java_parse_float(text: &str) -> Option<f32> {
    let text = crate::java::trim(text);
    let text = text.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(text);
    let decimal = !text.is_empty()
        && text != "."
        && text.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && text.bytes().filter(|&b| b == b'.').count() <= 1;
    decimal.then(|| text.parse::<f32>().ok()).flatten()
}

struct LineReader<'a> {
    input: &'a [u8],
    position: usize,
}

impl LineReader<'_> {
    /// `BaseParser.readLine`.
    fn next_line(&mut self) -> Result<String, Unported> {
        if self.position >= self.input.len() {
            return Err(Unported(
                "a document without a %PDF- header, which PDFBox fails on".to_owned(),
            ));
        }
        let mut line = String::new();
        while let Some(&c) = self.input.get(self.position) {
            self.position += 1;
            if c == b'\n' || c == b'\r' {
                if c == b'\r' && self.input.get(self.position) == Some(&b'\n') {
                    self.position += 1;
                }
                return Ok(line);
            }
            line.push(char::from(c));
        }
        Ok(line)
    }
}

/// `PDMetadataExtractor.addMetadata(Metadata, Property, String)`: a decoded, non-blank value,
/// added once, and only as a further value of a property that takes several.
fn add_metadata(
    metadata: &mut Recording,
    property: &Property,
    value: Option<&str>,
) -> Result<(), Unported> {
    let Some(value) = value.filter(|value| !is_blank(value)) else {
        return Ok(());
    };
    let decoded = xmp_decode(value);
    if is_blank(&decoded) {
        return Ok(());
    }
    if property.is_multi() || metadata.get(property.name).is_none() {
        if metadata.values(property.name).contains(&decoded) {
            return Ok(());
        }
        xmp::add(metadata, property, &decoded)?;
    }
    Ok(())
}

/// `PDMetadataExtractor.decode`: a value written as a PDF literal string with a byte order
/// mark, decoded as PDFBox decodes text strings.
fn xmp_decode(value: &str) -> String {
    let encoded = ["\\376\\377", "\\377\\376", "\\357\\273\\277"];
    if value.encode_utf16().count() < 8 || !encoded.iter().any(|bom| value.starts_with(bom)) {
        return value.to_owned();
    }
    let latin1: Vec<u8> = std::iter::once(b'(')
        .chain(
            value
                .chars()
                .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')),
        )
        .chain(std::iter::once(b')'))
        .collect();
    match parse_literal(&latin1) {
        Some(bytes) => text_string(&bytes),
        None => value.to_owned(),
    }
}

/// `PDMetadataExtractor.addMetadata(Metadata, Property, Calendar)`.
fn add_date(metadata: &mut Recording, name: &str, millis: Option<i64>) {
    if let Some(millis) = millis {
        metadata.set(name, format_instant(millis));
    }
}

/// `PDDocumentInformation`'s date of `key`: a text string read by `DateConverter`.
fn info_date(pdf: &Pdf, info: &lopdf::Dictionary, key: &[u8]) -> Result<Option<i64>, Unported> {
    match pdf.get(info, key) {
        Some(Object::String(bytes, _)) => date::to_calendar(&text_string(bytes)),
        _ => Ok(None),
    }
}

/// `PDFParser.extractMetadata` from the catalog's language on: the XMP packet, the page count,
/// the document information and the version.
pub(super) fn document(
    pdf: &Pdf,
    metadata: &mut Recording,
    version: f32,
    acro_form_fields: bool,
) -> Result<(), Unported> {
    let catalog = pdf.catalog().ok_or_else(|| {
        Unported("a document without a catalog, which PDFBox fails on".to_owned())
    })?;
    if let Some(language) = pdf.text(catalog, b"Lang") {
        metadata.set("dc:language", language);
    }
    if acro_form_fields {
        metadata.set("pdf:has-acro-form-fields", "true");
    }
    match pdf.stream(catalog, b"Metadata") {
        None => metadata.set("pdf:has-xmp", "false"),
        Some(stream) => {
            metadata.set("pdf:has-xmp", "true");
            let packet = pdf.decode(stream).map_err(|unported| {
                Unported(format!(
                    "an XMP stream PDFBox fails to read, whose exception Tika records: {}",
                    unported.0
                ))
            })?;
            xmp::extract(&packet, metadata, xmp_decode).map_err(|unported| Unported(unported.0))?;
            if let Some(conformance) = metadata.get("pdfaid:conformance").map(str::to_owned) {
                let part = metadata
                    .get("pdfaid:part")
                    .map_or_else(|| "UNKNOWN".to_owned(), str::to_owned);
                metadata.set(
                    "pdfa:pdf-version",
                    format!("A-{part}{}", conformance.to_lowercase()),
                );
            }
        }
    }
    metadata.set("xmpTPg:NPages", pdf.number_of_pages()?.to_string());
    let empty = lopdf::Dictionary::new();
    let info = pdf.dict(&pdf.doc.trailer, b"Info").unwrap_or(&empty);
    let title = pdf.text(info, b"Title");
    let author = pdf.text(info, b"Author");
    let creator = pdf.text(info, b"Creator");
    let keywords = pdf.text(info, b"Keywords");
    let producer = pdf.text(info, b"Producer");
    let subject = pdf.text(info, b"Subject");
    let trapped = pdf.name_as_string(info, b"Trapped");
    let text = xmp::text;
    add_metadata(metadata, &text("pdf:docinfo:title"), title.as_deref())?;
    add_metadata(metadata, &text("pdf:docinfo:creator"), author.as_deref())?;
    if metadata.get("dc:creator").is_none() {
        add_metadata(metadata, &DC_CREATOR, author.as_deref())?;
    }
    if metadata.get("dc:title").is_none() {
        add_metadata(metadata, &DC_TITLE, title.as_deref())?;
    }
    add_metadata(
        metadata,
        &text("pdf:docinfo:creator-tool"),
        creator.as_deref(),
    )?;
    add_metadata(metadata, &text("xmp:CreatorTool"), creator.as_deref())?;
    add_metadata(metadata, &text("pdf:docinfo:keywords"), keywords.as_deref())?;
    add_metadata(metadata, &text("pdf:docinfo:producer"), producer.as_deref())?;
    add_metadata(metadata, &text("pdf:producer"), producer.as_deref())?;
    add_metadata(metadata, &text("pdf:docinfo:subject"), subject.as_deref())?;
    add_metadata(metadata, &DC_SUBJECT, keywords.as_deref())?;
    add_metadata(metadata, &DC_SUBJECT, subject.as_deref())?;
    add_metadata(metadata, &text("pdf:docinfo:trapped"), trapped.as_deref())?;
    add_metadata(metadata, &text("pdf:trapped"), trapped.as_deref())?;
    let created = info_date(pdf, info, b"CreationDate")?;
    add_date(metadata, "pdf:docinfo:created", created);
    add_date(metadata, DCTERMS_CREATED.name, created);
    let modified = info_date(pdf, info, b"ModDate")?;
    add_date(metadata, DCTERMS_MODIFIED.name, modified);
    add_date(metadata, "pdf:docinfo:modified", modified);
    const HANDLED: [&str; 9] = [
        "Author",
        "Creator",
        "CreationDate",
        "ModDate",
        "Keywords",
        "Producer",
        "Subject",
        "Title",
        "Trapped",
    ];
    for (key, _) in info.iter() {
        let name = name_string(key);
        if HANDLED.contains(&name.as_str()) {
            continue;
        }
        match pdf.get(info, key) {
            Some(Object::Array(items)) => {
                for item in items {
                    custom(metadata, &name, item)?;
                }
            }
            Some(value) => custom(metadata, &name, value)?,
            None => {}
        }
    }
    let version = jfloat::float_to_string(version);
    metadata.set("pdf:pdf-version", version.clone());
    add_format(metadata, format!("application/pdf; version={version}"));
    if let Some(extensions) = pdf.get(catalog, b"Extensions") {
        let Some(extensions) = super::cos::as_dict(extensions) else {
            return Err(Unported(
                "an /Extensions entry that is not a dictionary, which Tika fails on".to_owned(),
            ));
        };
        for (name, _) in extensions.iter() {
            if name == b"ADBE" {
                let Some(adobe) = pdf.get(extensions, name) else {
                    continue;
                };
                let Some(adobe) = super::cos::as_dict(adobe) else {
                    return Err(Unported(
                        "an Adobe extension that is not a dictionary, which Tika fails on"
                            .to_owned(),
                    ));
                };
                let base = pdf
                    .name_as_string(adobe, b"BaseVersion")
                    .unwrap_or_else(|| "null".to_owned());
                let level = pdf.int(adobe, b"ExtensionLevel", -1);
                if level != -1 {
                    let extension = format!("{base} Adobe Extension Level {level}");
                    metadata.set("pdf:pdf-extension-version", extension.clone());
                    add_format(
                        metadata,
                        format!("application/pdf; version=\"{extension}\""),
                    );
                }
            } else {
                metadata.set("pdf:found-non-adobe-extension-name", name_string(name));
            }
        }
    }
    Ok(())
}

/// `PDFParser.addFormat`: another value of `dc:format`, set as an array.
fn add_format(metadata: &mut Recording, value: String) {
    let mut values = metadata.values("dc:format").to_vec();
    values.push(value);
    metadata.reset_values("dc:format", values);
}

/// `PDMetadataExtractor.addMetadata(Metadata, KeyPrefix, String, COSBase)`: a custom entry of
/// the document information dictionary, an array's items as they are held, unresolved.
fn custom(metadata: &mut Recording, name: &str, value: &Object) -> Result<(), Unported> {
    match value {
        Object::Array(items) => {
            for item in items {
                custom(metadata, name, item)?;
            }
        }
        Object::String(bytes, _) => custom_text(metadata, name, &text_string(bytes)),
        Object::Dictionary(_) | Object::Stream(_) => {}
        Object::Null => custom_text(metadata, name, "COSNull{}"),
        Object::Name(value) => {
            custom_text(
                metadata,
                name,
                &format!("COSName{{{}}}", name_string(value)),
            );
        }
        Object::Integer(value) => custom_text(metadata, name, &format!("COSInt{{{value}}}")),
        Object::Boolean(value) => custom_text(metadata, name, &value.to_string()),
        Object::Real(_) | Object::Reference(_) => {
            return Err(Unported(
                "a custom document information value Tika writes as PDFBox prints it".to_owned(),
            ))
        }
    }
    Ok(())
}

fn custom_text(metadata: &mut Recording, name: &str, value: &str) {
    if is_blank(name) || is_blank(value) {
        return;
    }
    let decoded = xmp_decode(value);
    if is_blank(&decoded) || name.encode_utf16().count() > 1024 {
        return;
    }
    let key = format!("pdf:docinfo:custom:{name}");
    if metadata.values(&key).contains(&decoded) {
        return;
    }
    metadata.add(&key, decoded);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_pdf_encoded_xmp_values() {
        assert_eq!(xmp_decode("\\376\\377\\000A\\000B"), "AB");
        assert_eq!(xmp_decode("plain text"), "plain text");
    }

    #[test]
    fn scans_startxrefs_as_tika_does() {
        let input = b"%PDF-1.4\n...startxref\n123\n%%EOF\n...startxref 456 %%EOF\r\n";
        let offsets = scan_startxrefs(input);
        assert_eq!(offsets.len(), 2);
        assert_eq!(offsets[0].startxref, 123);
        assert_eq!(
            &input[..offsets[0].end_eof_offset],
            b"%PDF-1.4\n...startxref\n123\n%%EOF\n"
        );
        assert_eq!(offsets[1].startxref, 456);
        assert_eq!(offsets[1].end_eof_offset, input.len() - 1);
        assert!(scan_startxrefs(b"startxref 12").is_empty());
    }

    #[test]
    fn reads_header_versions_as_pdfbox_does() {
        assert_eq!(header_version(b"%PDF-1.7\n%\xe2\xe3").unwrap(), 1.7);
        assert_eq!(header_version(b"%PDF-1.4 junk\n").unwrap(), 1.4);
        assert_eq!(header_version(b"%PDF-\n").unwrap(), 1.4);
        assert_eq!(header_version(b"%PDF-1.x\n").unwrap(), 1.7);
    }
}
