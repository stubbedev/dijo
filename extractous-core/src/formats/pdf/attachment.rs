//! The files a document carries, as Tika's `AbstractPDF2XHTML` finds them: the file
//! specifications annotations hold, those among the document's objects and those its tree of
//! embedded files names. A file a string names is written as a link; each file a dictionary
//! embeds is written as a `div` naming it, then parsed as an embedded document.

use std::collections::HashSet;

use lopdf::xref::XrefEntry;
use lopdf::{Dictionary, Object, ObjectId, StringFormat};

use super::cos::{as_dict, text_string, Pdf};
use super::encoding::java_bucket;
use super::{Unported, MAX_RECURSION_DEPTH};
use crate::formats::embedded::{ResourceType, RESOURCE_NAME};
use crate::formats::handlers::is_blank;
use crate::formats::java;
use crate::formats::metadata::TikaMetadata;

/// The type of a file specification dictionary.
const FILESPEC: &[u8] = b"Filespec";
/// The keys of a complex file specification's file names, each with the key of its embedded
/// file, in the order `extractMultiOSPDEmbeddedFiles` extracts them.
const PLATFORMS: [(&[u8], &[u8]); 4] = [
    (b"F", b"F"),
    (b"Mac", b"Mac"),
    (b"DOS", b"DOS"),
    (b"Unix", b"Unix"),
];

/// The attributes of an element, in the order SAX's `AttributesImpl` holds them.
pub(super) type Attributes = Vec<(String, String)>;

/// A file specification, as `PDFileSpecification.createFS` makes one.
#[derive(Debug, Clone, Copy)]
pub(super) enum FileSpec<'p> {
    /// `PDSimpleFileSpecification`: a file a string names.
    Simple { bytes: &'p [u8], hex: bool },
    /// `PDComplexFileSpecification`.
    Complex(&'p Dictionary),
}

/// What Tika's set of extracted file specifications tells them apart by: a `COSDictionary`
/// by identity, a `COSString` by its text and form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum SpecKey {
    Dictionary(usize),
    String { text: String, hex: bool },
}

impl FileSpec<'_> {
    pub(super) fn key(&self) -> SpecKey {
        match *self {
            Self::Simple { bytes, hex } => SpecKey::String {
                text: text_string(bytes),
                hex,
            },
            Self::Complex(dict) => SpecKey::Dictionary(identity(dict)),
        }
    }
}

/// A dictionary's identity, which Java's for the `COSDictionary` PDFBox reads it into
/// follows: one object read once.
fn identity(dict: &Dictionary) -> usize {
    std::ptr::from_ref(dict) as usize
}

/// `PDFileSpecification.createFS` of an annotation's file specification.
pub(super) fn file_spec(object: Option<&Object>) -> Result<Option<FileSpec<'_>>, Unported> {
    match object {
        None => Ok(None),
        Some(Object::String(bytes, format)) => Ok(Some(FileSpec::Simple {
            bytes,
            hex: *format == StringFormat::Hexadecimal,
        })),
        Some(other) => match as_dict(other) {
            Some(dict) => Ok(Some(FileSpec::Complex(dict))),
            None => Err(Unported(
                "a file specification neither a string nor a dictionary, which PDFBox fails on"
                    .to_owned(),
            )),
        },
    }
}

/// What Tika writes of a file specification.
#[derive(Debug)]
pub(super) enum Written {
    /// An empty `div` naming a file outside the document.
    Link(Attributes),
    /// An empty `div` naming a file the document embeds, then the file parsed as an embedded
    /// document.
    Embedded {
        attributes: Attributes,
        file: EmbeddedFile,
    },
}

/// A file the document embeds, and the metadata Tika parses it with.
#[derive(Debug)]
pub(super) struct EmbeddedFile {
    pub(super) metadata: TikaMetadata,
    pub(super) data: Vec<u8>,
}

/// `AbstractPDF2XHTML.setOrReplaceAttribute`.
fn set_or_replace(attributes: &mut Attributes, name: &str, value: &str) {
    match attributes.iter_mut().find(|(key, _)| key == name) {
        Some((_, current)) => value.clone_into(current),
        None => attributes.push((name.to_owned(), value.to_owned())),
    }
}

/// `AbstractPDF2XHTML.processDoc`, apart from its check for specifications already extracted:
/// a link for a simple specification, and each embedded file of a complex one.
pub(super) fn written(
    pdf: &Pdf,
    name: &str,
    annotation_type: &str,
    spec: FileSpec,
    mut attributes: Attributes,
) -> Result<Vec<Written>, Unported> {
    let dict = match spec {
        FileSpec::Simple { bytes, .. } => {
            set_or_replace(&mut attributes, "class", "linked");
            set_or_replace(&mut attributes, "id", &text_string(bytes));
            return Ok(vec![Written::Link(attributes)]);
        }
        FileSpec::Complex(dict) => dict,
    };
    if !attributes.iter().any(|(key, _)| key == "source") {
        attributes.push(("source".to_owned(), "attachment".to_owned()));
    }
    let mut written = Vec::new();
    let files = pdf.dict(dict, b"EF");
    for (name_key, file_key) in PLATFORMS {
        let Some(stream) = files.and_then(|files| match pdf.get(files, file_key) {
            Some(Object::Stream(stream)) => Some(stream),
            _ => None,
        }) else {
            continue;
        };
        let file_name = [pdf.text(dict, name_key), pdf.text(dict, b"UF")]
            .into_iter()
            .flatten()
            .find(|file_name| !java::trim(file_name).is_empty())
            .unwrap_or_else(|| name.to_owned());
        let mut metadata = TikaMetadata::new();
        metadata.set(RESOURCE_NAME, file_name.clone());
        let size = pdf
            .dict(&stream.dict, b"Params")
            .map_or(-1, |params| pdf.int(params, b"Size", -1));
        if size > -1 {
            metadata.set("Content-Length", size.to_string());
        }
        ResourceType::Attachment.set(&mut metadata);
        metadata.set("tk:orig-resource-name", file_name.clone());
        let described = [
            (
                "pdf:embedded-file-annotation-type",
                Some(annotation_type.to_owned()),
            ),
            (
                "pdf:embedded-file-subtype",
                pdf.name_as_string(&stream.dict, b"Subtype"),
            ),
            ("pdf:embedded-file-description", pdf.text(dict, b"Desc")),
            (
                "pdf:associated-file-relationship",
                pdf.name_as_string(dict, b"AFRelationship"),
            ),
        ];
        for (key, value) in described {
            if let Some(value) = value.filter(|value| !is_blank(value)) {
                metadata.set(key, value);
            }
        }
        let data = pdf.decode(stream)?;
        set_or_replace(&mut attributes, "class", "embedded");
        set_or_replace(&mut attributes, "id", &file_name);
        written.push(Written::Embedded {
            attributes: attributes.clone(),
            file: EmbeddedFile { metadata, data },
        });
    }
    Ok(written)
}

/// `PDFDOMUtil.findType` for file specifications, as `processPageAnnotation` looks for them
/// in an annotation: the dictionaries of type `Filespec` within it, itself included, not
/// looking into pages and parents, nor below a specification.
pub(super) fn find_file_specs<'p>(pdf: &'p Pdf, annotation: &'p Dictionary) -> Vec<&'p Dictionary> {
    let mut found = Vec::new();
    find(
        pdf,
        Node::Dictionary(annotation),
        0,
        &mut HashSet::new(),
        &mut found,
    );
    found
}

/// What `PDFDOMUtil.find` is at: a reference, or a value it leads to.
#[derive(Clone, Copy)]
enum Node<'p> {
    Reference(ObjectId),
    Dictionary(&'p Dictionary),
    Array(&'p [Object]),
    Other,
}

impl<'p> Node<'p> {
    fn of(object: &'p Object) -> Self {
        match object {
            Object::Reference(id) => Self::Reference(*id),
            Object::Array(items) => Self::Array(items),
            other => as_dict(other).map_or(Self::Other, Self::Dictionary),
        }
    }

    /// What Java's `HashSet<COSBase>` tells nodes apart by: a reference by the object it
    /// names, a dictionary or an array by identity. Other values are never descended into.
    fn seen_key(self) -> Option<(u8, usize)> {
        match self {
            Self::Reference((number, generation)) => {
                Some((0, (number as usize) << 16 | usize::from(generation)))
            }
            Self::Dictionary(dict) => Some((1, identity(dict))),
            Self::Array(items) => Some((2, items.as_ptr() as usize)),
            Self::Other => None,
        }
    }
}

fn find<'p>(
    pdf: &'p Pdf,
    node: Node<'p>,
    depth: usize,
    seen: &mut HashSet<(u8, usize)>,
    found: &mut Vec<&'p Dictionary>,
) {
    let Some(key) = node.seen_key() else {
        return;
    };
    if seen.contains(&key) || depth >= MAX_RECURSION_DEPTH {
        return;
    }
    seen.insert(key);
    match node {
        Node::Reference(id) => {
            if let Some(target) = pdf.indirect(id) {
                find(pdf, Node::of(target), depth + 1, seen, found);
            }
        }
        Node::Dictionary(dict) => match pdf.name(dict, b"Type") {
            Some(FILESPEC) => found.push(dict),
            Some(b"P" | b"Page" | b"Parent") => {}
            _ => {
                for (_, value) in dict.iter() {
                    find(pdf, Node::of(value), depth + 1, seen, found);
                }
            }
        },
        Node::Array(items) => {
            for item in items {
                find(pdf, Node::of(item), depth + 1, seen, found);
            }
        }
        Node::Other => {}
    }
}

/// The file specifications `extractEmbeddedDocuments` processes, each with the name it is
/// processed under: those among the document's objects, then those its tree of embedded
/// files names that are not among them.
pub(super) fn document_specs(pdf: &Pdf) -> Result<Vec<(String, &Dictionary)>, Unported> {
    let mut specs: Vec<(String, &Dictionary)> = file_spec_objects(pdf)?
        .into_iter()
        .map(|dict| (String::new(), dict))
        .collect();
    let mut seen: HashSet<usize> = specs.iter().map(|(_, dict)| identity(dict)).collect();
    let Some(tree) = pdf
        .catalog()
        .and_then(|catalog| pdf.dict(catalog, b"Names"))
        .and_then(|names| pdf.dict(names, b"EmbeddedFiles"))
    else {
        return Ok(specs);
    };
    let mut named = Vec::new();
    name_tree(pdf, Some(tree), 0, &mut named)?;
    for (name, spec) in named {
        let spec = spec.ok_or_else(|| {
            Unported(
                "a tree of embedded files naming null, whose specification PDFBox fails on"
                    .to_owned(),
            )
        })?;
        if seen.insert(identity(spec)) {
            specs.push((name, spec));
        }
    }
    Ok(specs)
}

/// `COSDocument.getObjectsByType(COSName.FILESPEC)`: the file specifications among the
/// objects the cross-reference table lists, in the order PDFBox's `HashMap` of their keys
/// holds them.
///
/// The objects `lopdf` could not read are left out where none of them can be a file
/// specification, the document's bytes not spelling `Filespec` and none of them held in an
/// object stream; otherwise the first is needed, and what PDFBox recovers of the table, and
/// of them, is not known.
fn file_spec_objects(pdf: &Pdf) -> Result<Vec<&Dictionary>, Unported> {
    let mut keys: Vec<ObjectId> = Vec::with_capacity(pdf.doc.reference_table.entries.len());
    let mut compressed_lost = false;
    for (&number, entry) in &pdf.doc.reference_table.entries {
        match *entry {
            XrefEntry::Normal { generation, .. } if generation > 0 => {
                return Err(Unported(format!(
                    "object {number} of generation {generation}, whose earlier generations \
                     PDFBox may still list among the document's objects"
                )));
            }
            XrefEntry::Normal { generation, .. } => keys.push((number, generation)),
            XrefEntry::Compressed { .. } => {
                compressed_lost |= pdf.is_lost(number);
                keys.push((number, 0));
            }
            XrefEntry::Free | XrefEntry::UnusableFree => {}
        }
    }
    let capacity = java_presized_capacity(keys.len());
    let mut found: Vec<(usize, &Dictionary)> = Vec::new();
    let mut lost: Vec<ObjectId> = Vec::new();
    for key in keys {
        if pdf.is_lost(key.0) {
            lost.push(key);
            continue;
        }
        let Some(dict) = pdf.indirect(key).and_then(as_dict) else {
            continue;
        };
        if pdf.name(dict, b"Type") == Some(FILESPEC) {
            found.push((java_bucket(object_key_hash(key), capacity), dict));
        }
    }
    if !found.is_empty() || pdf.spells_filespec || compressed_lost {
        if let Some(&key) = lost.first() {
            pdf.indirect(key);
        }
    }
    found.sort_by_key(|&(bucket, _)| bucket);
    if found.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(Unported(
            "file specifications PDFBox's table of objects holds in one bucket, in an order \
             the port does not follow"
                .to_owned(),
        ));
    }
    Ok(found.into_iter().map(|(_, dict)| dict).collect())
}

/// `COSObjectKey.hashCode`.
fn object_key_hash((number, generation): ObjectId) -> i32 {
    let value = i64::from(number) << 16 | i64::from(generation);
    (value ^ (value >> 32)) as i32
}

/// The table size of a Java `HashMap` that `putAll` of `size` entries sizes from empty:
/// `tableSizeFor` of what the entries need at the default load factor.
fn java_presized_capacity(size: usize) -> usize {
    ((size as f64 / 0.75).ceil() as usize).next_power_of_two()
}

/// `extractFilesfromEFTree`: the names and specifications of a node of the tree of embedded
/// files and of the nodes below it, depth first.
fn name_tree<'p>(
    pdf: &'p Pdf,
    node: Option<&'p Dictionary>,
    depth: usize,
    named: &mut Vec<(String, Option<&'p Dictionary>)>,
) -> Result<(), Unported> {
    if depth > MAX_RECURSION_DEPTH {
        return Err(Unported(
            "a tree of embedded files deeper than Tika follows, which it records as an exception"
                .to_owned(),
        ));
    }
    let Some(node) = node else {
        return Ok(());
    };
    if let Some(names) = node_names(pdf, node) {
        named.extend(names);
    }
    for kid in pdf.array(node, b"Kids").unwrap_or(&[]) {
        let kid = pdf.resolve(kid).and_then(as_dict);
        name_tree(pdf, kid, depth + 1, named)?;
    }
    Ok(())
}

/// `PDNameTreeNode.getNames` of the tree of embedded files: a later value of a name replaces
/// the earlier where it stands, and a node naming with what is not a string, or naming what
/// is neither a dictionary nor null, has no names Tika reads.
fn node_names<'p>(
    pdf: &'p Pdf,
    node: &'p Dictionary,
) -> Option<Vec<(String, Option<&'p Dictionary>)>> {
    let items = pdf.array(node, b"Names")?;
    let mut names: Vec<(String, Option<&'p Dictionary>)> = Vec::with_capacity(items.len() / 2);
    for [key, value] in items.as_chunks::<2>().0 {
        let Some(Object::String(bytes, _)) = pdf.resolve(key) else {
            return None;
        };
        let value = match pdf.resolve(value) {
            None => None,
            Some(value) => Some(as_dict(value)?),
        };
        let name = text_string(bytes);
        match names.iter_mut().find(|(existing, _)| *existing == name) {
            Some((_, current)) => *current = value,
            None => names.push((name, value)),
        }
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_object_keys_as_pdfbox_does() {
        assert_eq!(object_key_hash((1, 0)), 1 << 16);
        assert_eq!(object_key_hash((70_000, 2)), 292_552_707);
    }

    #[test]
    fn sizes_tables_as_java_putall_does() {
        assert_eq!(java_presized_capacity(1), 2);
        assert_eq!(java_presized_capacity(12), 16);
        assert_eq!(java_presized_capacity(13), 32);
    }
}
