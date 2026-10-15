//! A document's interactive form as Tika reads it: the fields PDFBox's `PDFieldFactory` makes
//! of the AcroForm's field dictionaries, written by `extractAcroForm` as nested ordered lists of
//! names and values, and the signatures `PDFParser.extractSignatures` reports.

use std::collections::HashSet;

use lopdf::{Dictionary, Object};

use super::cos::{as_dict, name_string, text_string, Identity, Pdf};
use super::date::{self, Zoned};
use super::Unported;
use crate::formats::handlers::is_blank;
use crate::formats::java_time::format_instant;
use crate::formats::metadata::TikaMetadata;

/// `AbstractPDF2XHTML.MAX_RECURSION_DEPTH`.
const MAX_RECURSION_DEPTH: usize = 100;
/// How deep the port follows a field tree for its signatures, where PDFBox recurses without
/// bound.
const MAX_TREE_DEPTH: usize = 512;
/// `PDButton.FLAG_RADIO`.
const FLAG_RADIO: i32 = 1 << 15;
/// `PDButton.FLAG_PUSHBUTTON`.
const FLAG_PUSHBUTTON: i32 = 1 << 16;
/// `TikaCoreProperties.HAS_SIGNATURE`.
const HAS_SIGNATURE: &str = "tk:has-signature";

/// The kind of field `PDFieldFactory.createField` makes of a dictionary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    NonTerminal,
    Text,
    /// A check box or a radio button.
    Button,
    PushButton,
    Choice,
    Signature,
}

/// A field, with the field dictionaries above it, nearest first, that its inheritable
/// attributes come from before the form's.
#[derive(Debug)]
pub(super) struct Field<'p> {
    dict: &'p Dictionary,
    identity: Identity,
    kind: Kind,
    parents: Vec<&'p Dictionary>,
}

/// One line of the form's list: a field's name and value, or a signature's data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Line {
    Field {
        alternate: Option<String>,
        text: String,
    },
    Signature {
        alternate: Option<String>,
        data: Vec<(&'static str, String)>,
    },
    StartList,
    EndList,
}

/// The form's fields, `PDAcroForm.getFields`.
fn fields<'p>(pdf: &'p Pdf, form: &'p Dictionary) -> Result<Vec<Field<'p>>, Unported> {
    let mut fields = Vec::new();
    for item in pdf.array(form, b"Fields").unwrap_or(&[]) {
        let Some((identity, object)) = pdf.resolve_identified(item) else {
            continue;
        };
        let Some(dict) = as_dict(object) else {
            continue;
        };
        if let Some(field) = create(pdf, dict, identity, Vec::new())? {
            fields.push(field);
        }
    }
    Ok(fields)
}

/// `PDFieldFactory.createField`.
fn create<'p>(
    pdf: &'p Pdf,
    dict: &'p Dictionary,
    identity: Identity,
    parents: Vec<&'p Dictionary>,
) -> Result<Option<Field<'p>>, Unported> {
    let field = |kind| {
        Ok(Some(Field {
            dict,
            identity,
            kind,
            parents,
        }))
    };
    if dict.has(b"Kids") {
        let named_kid = pdf.array(dict, b"Kids").is_some_and(|kids| {
            kids.iter().any(|kid| {
                pdf.resolve(kid)
                    .and_then(as_dict)
                    .is_some_and(|kid| pdf.text(kid, b"T").is_some())
            })
        });
        if named_kid {
            return field(Kind::NonTerminal);
        }
    }
    let kind = match field_type(pdf, dict).as_deref() {
        Some("Ch") => Kind::Choice,
        Some("Tx") => Kind::Text,
        Some("Sig") => Kind::Signature,
        Some("Btn") => {
            let flags = pdf.int(dict, b"Ff", 0);
            if flags & FLAG_RADIO != 0 || flags & FLAG_PUSHBUTTON == 0 {
                Kind::Button
            } else {
                Kind::PushButton
            }
        }
        _ => return Ok(None),
    };
    field(kind)
}

/// `PDFieldFactory.findFieldType`: the field's type, or the nearest parent's, none where the
/// parents come back round to a dictionary already asked.
fn field_type(pdf: &Pdf, dict: &Dictionary) -> Option<String> {
    let mut seen = HashSet::new();
    let mut current = dict;
    while seen.insert(std::ptr::from_ref(current)) {
        if let Some(kind) = pdf.name_as_string(current, b"FT") {
            return Some(kind);
        }
        current = match pdf.get(current, b"Parent") {
            Some(parent) => as_dict(parent)?,
            None => pdf.dict(current, b"P")?,
        };
    }
    None
}

impl<'p> Field<'p> {
    /// `PDNonTerminalField.getChildren`.
    fn children(&self, pdf: &'p Pdf) -> Result<Vec<Field<'p>>, Unported> {
        let mut children = Vec::new();
        for kid in pdf.array(self.dict, b"Kids").unwrap_or(&[]) {
            let Some((identity, object)) = pdf.resolve_identified(kid) else {
                continue;
            };
            let Some(dict) = as_dict(object) else {
                continue;
            };
            if identity == self.identity {
                continue;
            }
            let mut parents = Vec::with_capacity(self.parents.len() + 1);
            parents.push(self.dict);
            parents.extend_from_slice(&self.parents);
            if let Some(child) = create(pdf, dict, identity, parents)? {
                children.push(child);
            }
        }
        Ok(children)
    }

    /// `getInheritableAttribute`: the field's own value of `key`, else its parents', else the
    /// form's.
    fn inherited(&self, pdf: &'p Pdf, form: &'p Dictionary, key: &[u8]) -> Option<&'p Object> {
        std::iter::once(self.dict)
            .chain(self.parents.iter().copied())
            .find(|dict| dict.has(key))
            .map_or_else(|| pdf.get(form, key), |dict| pdf.get(dict, key))
    }

    /// `getValueAsString`.
    fn value(&self, pdf: &'p Pdf, form: &'p Dictionary) -> Result<String, Unported> {
        let unported_list = || {
            Unported(
                "a field value list of what is not a string, which PDFBox fails to cast".to_owned(),
            )
        };
        let strings = |items: &[Object]| -> Result<Vec<String>, Unported> {
            items
                .iter()
                .map(|item| match item {
                    Object::String(bytes, _) => Ok(text_string(bytes)),
                    _ => Err(unported_list()),
                })
                .collect()
        };
        Ok(match self.kind {
            Kind::Text => match self.inherited(pdf, form, b"V") {
                Some(Object::String(bytes, _)) => text_string(bytes),
                Some(Object::Stream(stream)) => text_string(&pdf.decode(stream)?),
                _ => String::new(),
            },
            Kind::Button => match self.inherited(pdf, form, b"V") {
                Some(Object::Name(name)) => {
                    let state = name_string(name);
                    let exports = match self.inherited(pdf, form, b"Opt") {
                        Some(Object::String(bytes, _)) => {
                            let value = text_string(bytes);
                            if value.is_empty() {
                                Vec::new()
                            } else {
                                vec![value]
                            }
                        }
                        Some(Object::Array(items)) => strings(items)?,
                        _ => Vec::new(),
                    };
                    match crate::formats::java::parse_int(&state) {
                        Some(index) if !exports.is_empty() => usize::try_from(index)
                            .ok()
                            .and_then(|index| exports.get(index))
                            .cloned()
                            .unwrap_or(state),
                        _ => state,
                    }
                }
                _ => "Off".to_owned(),
            },
            Kind::PushButton => String::new(),
            Kind::Choice => {
                let values = match pdf.get(self.dict, b"V") {
                    Some(Object::String(bytes, _)) => vec![text_string(bytes)],
                    Some(Object::Array(items)) => strings(items)?,
                    _ => Vec::new(),
                };
                format!("[{}]", values.join(", "))
            }
            Kind::NonTerminal => match pdf.get(self.dict, b"V") {
                None => String::new(),
                Some(value) => java_to_string(value)?,
            },
            Kind::Signature => unreachable!("signatures are written apart"),
        })
    }

    /// The signature dictionary of a signature field, `PDSignatureField.getValue`.
    fn signature(&self, pdf: &'p Pdf) -> Option<&'p Dictionary> {
        pdf.dict(self.dict, b"V")
    }
}

/// `COSBase.toString` of a value a non-terminal field holds.
fn java_to_string(value: &Object) -> Result<String, Unported> {
    Ok(match value {
        Object::String(bytes, _) => format!("COSString{{{}}}", text_string(bytes)),
        Object::Name(name) => format!("COSName{{{}}}", name_string(name)),
        Object::Integer(value) => format!("COSInt{{{value}}}"),
        Object::Boolean(value) => value.to_string(),
        _ => {
            return Err(Unported(
                "a non-terminal field value Tika writes as PDFBox prints it".to_owned(),
            ))
        }
    })
}

/// The data of a signature: its name, contact, location, reason and date, by key.
fn signature_data(
    pdf: &Pdf,
    signature: &Dictionary,
) -> Result<Vec<(&'static str, String)>, Unported> {
    let mut data = Vec::new();
    if let Some(value) = pdf.text(signature, b"ContactInfo") {
        data.push(("contactInfo", value));
    }
    if let Some(date) = signature_date(pdf, signature)? {
        data.push(("date", date_with_offset(date)?));
    }
    for (key, name) in [
        ("location", b"Location" as &[u8]),
        ("name", b"Name"),
        ("reason", b"Reason"),
    ] {
        if let Some(value) = pdf.text(signature, name) {
            data.push((key, value));
        }
    }
    Ok(data
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .collect())
}

/// `PDSignature.getSignDate`.
fn signature_date(pdf: &Pdf, signature: &Dictionary) -> Result<Option<Zoned>, Unported> {
    match pdf.text(signature, b"M") {
        Some(text) => date::to_zoned(&text),
        None => Ok(None),
    }
}

/// `SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ssZ")` in the date's own zone.
fn date_with_offset(date: Zoned) -> Result<String, Unported> {
    let local = format_instant(date.millis + date.offset);
    if local.starts_with('-') || local.starts_with("0000") {
        return Err(Unported(
            "a signature date before the common era, which SimpleDateFormat writes by era"
                .to_owned(),
        ));
    }
    let wall = local.trim_start_matches('+').trim_end_matches('Z');
    let minutes = date.offset / 60_000;
    let sign = if minutes < 0 { '-' } else { '+' };
    let minutes = minutes.abs();
    Ok(format!(
        "{wall}{sign}{:02}{:02}",
        minutes / 60,
        minutes % 60
    ))
}

/// Whether the form has fields, `PDAcroForm.getFields` not empty.
pub(super) fn has_fields(pdf: &Pdf) -> Result<bool, Unported> {
    let Some(form) = pdf
        .catalog()
        .and_then(|catalog| pdf.dict(catalog, b"AcroForm"))
    else {
        return Ok(false);
    };
    Ok(!fields(pdf, form)?.is_empty())
}

/// `extractAcroForm` without XFA: the lines of the form's list, if the document has a form.
pub(super) fn lines(pdf: &Pdf, metadata: &mut TikaMetadata) -> Result<Option<Vec<Line>>, Unported> {
    let Some(form) = pdf
        .catalog()
        .and_then(|catalog| pdf.dict(catalog, b"AcroForm"))
    else {
        return Ok(None);
    };
    let mut lines = Vec::new();
    for field in fields(pdf, form)? {
        field_lines(pdf, form, &field, 0, &mut lines, metadata)?;
    }
    Ok(Some(lines))
}

/// `processAcroField`.
fn field_lines<'p>(
    pdf: &'p Pdf,
    form: &'p Dictionary,
    field: &Field<'p>,
    depth: usize,
    lines: &mut Vec<Line>,
    metadata: &mut TikaMetadata,
) -> Result<(), Unported> {
    if depth >= MAX_RECURSION_DEPTH {
        return Ok(());
    }
    let partial = pdf.text(field.dict, b"T");
    let alternate = pdf.text(field.dict, b"TU");
    let mut text = partial.map(|name| format!("{name}: ")).unwrap_or_default();
    if field.kind == Kind::Signature {
        if let Some(signature) = field.signature(pdf) {
            metadata.set(HAS_SIGNATURE, "true");
            lines.push(Line::Signature {
                alternate,
                data: signature_data(pdf, signature)?,
            });
        }
        return Ok(());
    }
    let value = field.value(pdf, form)?;
    if value != "null" {
        text.push_str(&value);
    }
    if alternate.is_some() || !text.is_empty() {
        lines.push(Line::Field { alternate, text });
    }
    if field.kind == Kind::NonTerminal {
        lines.push(Line::StartList);
        for child in field.children(pdf)? {
            field_lines(pdf, form, &child, depth + 1, lines, metadata)?;
        }
        lines.push(Line::EndList);
    }
    Ok(())
}

/// `PDFieldTree`'s `enqueueKids`: a field and, depth first, the fields below it not met yet.
fn enqueue<'p>(
    pdf: &'p Pdf,
    field: Field<'p>,
    seen: &mut HashSet<Identity>,
    tree: &mut Vec<Field<'p>>,
    depth: usize,
) -> Result<(), Unported> {
    if depth > MAX_TREE_DEPTH {
        return Err(Unported(format!(
            "a field tree deeper than the {MAX_TREE_DEPTH} levels the port follows"
        )));
    }
    seen.insert(field.identity);
    let children = if field.kind == Kind::NonTerminal {
        field.children(pdf)?
    } else {
        Vec::new()
    };
    tree.push(field);
    for child in children {
        if !seen.contains(&child.identity) {
            enqueue(pdf, child, seen, tree, depth + 1)?;
        }
    }
    Ok(())
}

/// `PDFParser.extractSignatures`: the signature fields of the whole field tree and what their
/// signatures say.
pub(super) fn signatures(pdf: &Pdf, metadata: &mut TikaMetadata) -> Result<(), Unported> {
    let Some(form) = pdf
        .catalog()
        .and_then(|catalog| pdf.dict(catalog, b"AcroForm"))
    else {
        return Ok(());
    };
    let mut seen = HashSet::new();
    let mut tree: Vec<Field> = Vec::new();
    for field in fields(pdf, form)? {
        enqueue(pdf, field, &mut seen, &mut tree, 0)?;
    }
    let signature_fields: Vec<&Field> = tree
        .iter()
        .filter(|field| field.kind == Kind::Signature)
        .collect();
    if signature_fields.is_empty() {
        return Ok(());
    }
    metadata.set("pdf:has-signature-fields", "true");
    let mut signed = false;
    for field in signature_fields {
        let Some(signature) = field.signature(pdf) else {
            continue;
        };
        add_not_blank(metadata, "tk:signature:name", pdf.text(signature, b"Name"));
        if let Some(date) = signature_date(pdf, signature)? {
            metadata.add("tk:signature:date", format_instant(date.millis));
        }
        add_not_blank(
            metadata,
            "tk:signature:contact-info",
            pdf.text(signature, b"ContactInfo"),
        );
        add_not_blank(
            metadata,
            "tk:signature:filter",
            pdf.name_as_string(signature, b"Filter"),
        );
        add_not_blank(
            metadata,
            "tk:signature:location",
            pdf.text(signature, b"Location"),
        );
        add_not_blank(
            metadata,
            "tk:signature:reason",
            pdf.text(signature, b"Reason"),
        );
        signed = true;
    }
    if signed {
        metadata.set(HAS_SIGNATURE, "true");
    }
    Ok(())
}

/// `PDMetadataExtractor.addNotNull`: the value, unless it is missing or blank.
fn add_not_blank(metadata: &mut TikaMetadata, name: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !is_blank(value)) {
        metadata.add(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(text: &str) -> Result<String, Unported> {
        date_with_offset(date::to_zoned(text)?.unwrap())
    }

    #[test]
    fn writes_signature_dates_in_their_own_zone() {
        assert_eq!(
            written("D:20240102030405-05'00'").unwrap(),
            "2024-01-02T03:04:05-0500"
        );
        assert_eq!(
            written("D:20240102030405Z").unwrap(),
            "2024-01-02T03:04:05+0000"
        );
        assert_eq!(written("D:2024").unwrap(), "2024-01-01T00:00:00+0000");
    }
}
