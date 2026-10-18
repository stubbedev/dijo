//! The files a document carries, as Tika's `AbstractPDF2XHTML` finds them: the file
//! specifications annotations hold, those among the document's objects and those its tree of
//! embedded files names. A file a string names is written as a link; each file a dictionary
//! embeds is written as a `div` naming it, then parsed as an embedded document.

use std::collections::HashSet;

use lopdf::{Dictionary, Object, ObjectId, StringFormat};

use super::cos::{as_dict, text_string, Identity, Pdf};
use super::{Unported, MAX_RECURSION_DEPTH};
use crate::formats::embedded::{ResourceType, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::Exception;
use crate::java;
use crate::java::is_blank;

/// The type of a file specification dictionary.
const FILESPEC: &[u8] = b"Filespec";
/// The keys of a complex file specification's embedded files, the one it is extracted from
/// first: the file, its Unicode-named copy, then the platform copies PDF 2.0 deprecates.
const FILE_KEYS: [&[u8]; 5] = [b"F", b"UF", b"DOS", b"Mac", b"Unix"];
/// The keys of a complex file specification's file names, the one it is named by first.
const NAME_KEYS: [&[u8]; 5] = [b"UF", b"F", b"DOS", b"Mac", b"Unix"];

/// The attributes of an element, in the order SAX's `AttributesImpl` holds them.
pub(super) type Attributes = Vec<(String, String)>;

/// A file specification, as `PDFileSpecification.createFS` makes one.
#[derive(Debug, Clone, Copy)]
pub(super) enum FileSpec<'p> {
    /// `PDSimpleFileSpecification`: a file a string names.
    Simple { bytes: &'p [u8], hex: bool },
    /// `PDComplexFileSpecification`, of the dictionary of `identity`.
    Complex {
        identity: Identity,
        dict: &'p Dictionary,
    },
}

/// What Tika's set of extracted file specifications tells them apart by: a `COSDictionary`
/// by identity, which Java's for the `COSDictionary` PDFBox reads it into follows, a
/// `COSString` by its text and form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum SpecKey {
    Dictionary(Identity),
    String { text: String, hex: bool },
}

impl SpecKey {
    /// Whether this is a dictionary held directly in another object, told apart by where it is
    /// held.
    pub(super) fn is_direct(&self) -> bool {
        matches!(self, Self::Dictionary(identity) if identity.is_direct())
    }
}

impl FileSpec<'_> {
    pub(super) fn key(&self) -> SpecKey {
        match *self {
            Self::Simple { bytes, hex } => SpecKey::String {
                text: text_string(bytes),
                hex,
            },
            Self::Complex { identity, .. } => SpecKey::Dictionary(identity),
        }
    }
}

/// `PDFileSpecification.createFS` of an annotation's file specification, of its identity.
pub(super) fn file_spec(
    object: Option<(Identity, &Object)>,
) -> Result<Option<FileSpec<'_>>, Unported> {
    match object {
        None => Ok(None),
        Some((_, Object::String(bytes, format))) => Ok(Some(FileSpec::Simple {
            bytes,
            hex: *format == StringFormat::Hexadecimal,
        })),
        Some((identity, other)) => match as_dict(other) {
            Some(dict) => Ok(Some(FileSpec::Complex { identity, dict })),
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
/// a link for a simple specification, and the file a complex one embeds.
///
/// Tika extracts the Mac, DOS and Unix copies of a file as files of their own, each named by
/// its platform's name before `UF`; the port extracts the file once, from the first copy of
/// [`FILE_KEYS`], named by the first name of [`NAME_KEYS`] that is not blank, else by the name
/// it is listed under (#109).
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
        FileSpec::Complex { dict, .. } => dict,
    };
    if !attributes.iter().any(|(key, _)| key == "source") {
        attributes.push(("source".to_owned(), "attachment".to_owned()));
    }
    let files = pdf.dict(dict, b"EF");
    let Some(stream) = FILE_KEYS.into_iter().find_map(|key| {
        files.and_then(|files| match pdf.get(files, key) {
            Some(Object::Stream(stream)) => Some(stream),
            _ => None,
        })
    }) else {
        return Ok(Vec::new());
    };
    let file_name = NAME_KEYS
        .into_iter()
        .filter_map(|key| pdf.text(dict, key))
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
    Ok(vec![Written::Embedded {
        attributes,
        file: EmbeddedFile { metadata, data },
    }])
}

/// `PDFDOMUtil.findType` for file specifications, as `processPageAnnotation` looks for them
/// in an annotation of `identity`: the dictionaries of type `Filespec` within it, itself
/// included, not looking into pages and parents, nor below a specification, each with its
/// identity.
pub(super) fn find_file_specs<'p>(
    pdf: &'p Pdf,
    identity: Identity,
    annotation: &'p Dictionary,
) -> Vec<(Identity, &'p Dictionary)> {
    let mut found = Vec::new();
    find(
        pdf,
        Node::Dictionary(identity, annotation),
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
    Dictionary(Identity, &'p Dictionary),
    Array(&'p [Object]),
    Other,
}

impl<'p> Node<'p> {
    /// The node `object` is, of the identity `identity`.
    fn of(identity: Identity, object: &'p Object) -> Self {
        match object {
            Object::Reference(id) => Self::Reference(*id),
            Object::Array(items) => Self::Array(items),
            other => as_dict(other).map_or(Self::Other, |dict| Self::Dictionary(identity, dict)),
        }
    }

    /// The node a value held directly is.
    fn direct(object: &'p Object) -> Self {
        Self::of(Identity::of_direct(object), object)
    }

    /// What Java's `HashSet<COSBase>` tells nodes apart by: a reference by the object it
    /// names, a dictionary or an array by identity. Other values are never descended into.
    fn seen_key(self) -> Option<(u8, usize)> {
        match self {
            Self::Reference((number, generation)) => {
                Some((0, (number as usize) << 16 | usize::from(generation)))
            }
            Self::Dictionary(_, dict) => Some((1, std::ptr::from_ref(dict) as usize)),
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
    found: &mut Vec<(Identity, &'p Dictionary)>,
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
                let target = Node::of(Identity::Indirect(id), target);
                find(pdf, target, depth + 1, seen, found);
            }
        }
        Node::Dictionary(identity, dict) => match pdf.name(dict, b"Type") {
            Some(FILESPEC) => found.push((identity, dict)),
            Some(b"P" | b"Page" | b"Parent") => {}
            _ => {
                for (_, value) in dict.iter() {
                    find(pdf, Node::direct(value), depth + 1, seen, found);
                }
            }
        },
        Node::Array(items) => {
            for item in items {
                find(pdf, Node::direct(item), depth + 1, seen, found);
            }
        }
        Node::Other => {}
    }
}

/// The file specifications `extractEmbeddedDocuments` processes, each with the name it is
/// processed under: those among the document's objects, then those its tree of embedded
/// files names that are not among them, and the `IOException` it throws where that tree is
/// deeper than Tika follows, before it processes the specifications the tree names.
pub(super) struct DocumentSpecs<'p> {
    pub(super) specs: Vec<(String, FileSpec<'p>)>,
    pub(super) thrown: Option<Exception>,
}

pub(super) fn document_specs<'p>(pdf: &'p Pdf) -> DocumentSpecs<'p> {
    let listed = file_spec_objects(pdf);
    let mut seen: HashSet<Identity> = listed.iter().map(|&(identity, _)| identity).collect();
    let mut specs: Vec<(String, FileSpec)> = listed
        .into_iter()
        .map(|(identity, dict)| (String::new(), FileSpec::Complex { identity, dict }))
        .collect();
    let Some(tree) = pdf
        .catalog()
        .and_then(|catalog| pdf.dict(catalog, b"Names"))
        .and_then(|names| pdf.dict(names, b"EmbeddedFiles"))
    else {
        return DocumentSpecs {
            specs,
            thrown: None,
        };
    };
    let mut named = Vec::new();
    if let Err(thrown) = name_tree(pdf, Some(tree), 0, &mut named) {
        return DocumentSpecs {
            specs,
            thrown: Some(thrown),
        };
    }
    for (name, identity, dict) in named {
        if seen.insert(identity) {
            specs.push((name, FileSpec::Complex { identity, dict }));
        }
    }
    DocumentSpecs {
        specs,
        thrown: None,
    }
}

/// `COSDocument.getObjectsByType(COSName.FILESPEC)`: the file specifications among the
/// objects the cross-reference table lists, by object number and generation. Tika takes them
/// in the order PDFBox's `HashMap` of their keys holds them, which follows hash buckets, not
/// the document (#109). The table keeps an earlier generation of an object an update replaces,
/// and so does PDFBox's.
///
/// PDFBox reads every object for it; here each is read in a scope of its own, and only the
/// file specifications are held for the document. A document whose bytes do not spell
/// `Filespec` and whose table puts no object in an object stream holds none, and its objects
/// are left unread.
fn file_spec_objects<'p>(pdf: &'p Pdf) -> Vec<(Identity, &'p Dictionary)> {
    let listed = pdf.listed();
    if !pdf.spells_filespec && !listed.iter().any(|&id| pdf.is_compressed(id)) {
        return Vec::new();
    }
    let mut scope = pdf.scoped();
    let specs: Vec<ObjectId> = listed
        .into_iter()
        .filter(|&id| {
            scope.trim();
            scope
                .indirect(id)
                .and_then(as_dict)
                .is_some_and(|dict| scope.name(dict, b"Type") == Some(FILESPEC))
        })
        .collect();
    drop(scope);
    specs
        .into_iter()
        .map(|id| {
            let dict = pdf
                .indirect(id)
                .and_then(as_dict)
                .expect("a file specification reads again as it read");
            (Identity::Indirect(id), dict)
        })
        .collect()
}

/// A name of the tree of embedded files, and the specification it names, of its identity.
type Named<'p> = (String, Identity, &'p Dictionary);

/// `extractFilesfromEFTree`: the names and specifications of a node of the tree of embedded
/// files and of the nodes below it, depth first.
fn name_tree<'p>(
    pdf: &'p Pdf,
    node: Option<&'p Dictionary>,
    depth: usize,
    named: &mut Vec<Named<'p>>,
) -> Result<(), Exception> {
    if depth > MAX_RECURSION_DEPTH {
        return Err(Exception::new(
            "java.io.IOException",
            "Hit max recursion depth",
        ));
    }
    let Some(node) = node else {
        return Ok(());
    };
    named.extend(node_names(pdf, node));
    for kid in pdf.array(node, b"Kids").unwrap_or(&[]) {
        let kid = pdf.resolve(kid).and_then(as_dict);
        name_tree(pdf, kid, depth + 1, named)?;
    }
    Ok(())
}

/// The names and specifications of a node of the tree of embedded files, each pair as it
/// stands. PDFBox's `PDNameTreeNode.getNames` keeps a repeated name where it first stands with
/// its last value, and gives no names for a node with one entry naming with what is not a
/// string, or naming what is not a dictionary (#109); such an entry is skipped alone here.
fn node_names<'p>(pdf: &'p Pdf, node: &'p Dictionary) -> Vec<Named<'p>> {
    let Some(items) = pdf.array(node, b"Names") else {
        return Vec::new();
    };
    items
        .as_chunks::<2>()
        .0
        .iter()
        .filter_map(|[key, value]| match pdf.resolve(key) {
            Some(Object::String(bytes, _)) => {
                let (identity, spec) = pdf.resolve_identified(value)?;
                Some((text_string(bytes), identity, as_dict(spec)?))
            }
            _ => None,
        })
        .collect()
}
