//! The package of an EPUB as Tika's `EpubParser` reads it: the root package document
//! `META-INF/container.xml` names, its Dublin Core metadata through Tika's `OPFParser` and
//! `DcXMLParser`, and its manifest and spine through the `ContentOrderScraper`.

use crate::formats::element_metadata::ElementHandlers;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java::{self, JavaHashMap};

const RENDITION_LAYOUT: &str = "epub:rendition:layout";
const VERSION: &str = "epub:version";
const PRE_PAGINATED: &str = "pre-paginated";

/// `XMLReaderUtils.getAttrValue`: the value of the first attribute named `local_name`, in any
/// namespace.
pub(super) fn attribute_value<'a>(
    attributes: &'a [Attribute],
    local_name: &str,
) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.local_name == local_name)
        .map(|attribute| attribute.value.as_str())
}

/// Tika's `RootFinder`: the `full-path` of the last `rootfile` in `container.xml`, which a
/// `rootfile` without one leaves unset.
pub(super) fn root(container: &[u8]) -> Result<Option<String>, Failure> {
    struct RootFinder(Option<String>);

    impl SaxHandler for RootFinder {
        fn start_element(
            &mut self,
            _namespace: &str,
            local_name: &str,
            attributes: &[Attribute],
        ) -> Result<(), Failure> {
            if java::equals_ignore_case(local_name, "rootfile") {
                self.0 = attribute_value(attributes, "full-path").map(str::to_owned);
            }
            Ok(())
        }

        fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
            Ok(())
        }

        fn characters(&mut self, _text: &str) -> Result<(), Failure> {
            Ok(())
        }
    }

    let mut finder = RootFinder(None);
    xml::parse_sax(container, "META-INF/container.xml", &mut finder)?;
    Ok(finder.0)
}

/// Tika's `OPFHandler`: the EPUB version, and whether the layout is pre-paginated.
#[derive(Default)]
struct Rendition {
    in_layout: bool,
    layout: String,
}

impl Rendition {
    fn start(&mut self, local_name: &str, attributes: &[Attribute], metadata: &mut TikaMetadata) {
        match local_name {
            "itemref" => {
                if attribute_value(attributes, "properties")
                    .is_some_and(|properties| properties.contains("rendition:layout-pre-paginated"))
                {
                    metadata.set(RENDITION_LAYOUT, PRE_PAGINATED);
                }
            }
            "meta" => {
                if attribute_value(attributes, "property") == Some("rendition:layout") {
                    self.in_layout = true;
                }
            }
            "package" => {
                if let Some(version) = attribute_value(attributes, "version")
                    .filter(|version| !crate::java::is_blank(version))
                {
                    metadata.set(VERSION, version);
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, local_name: &str, metadata: &mut TikaMetadata) {
        if self.in_layout && local_name == "meta" {
            if self.layout == PRE_PAGINATED {
                metadata.set(RENDITION_LAYOUT, PRE_PAGINATED);
            }
            self.in_layout = false;
            self.layout.clear();
        }
    }

    fn characters(&mut self, text: &str) {
        if self.in_layout {
            self.layout.push_str(text);
        }
    }
}

/// The handlers Tika's `DcXMLParser` tees an XML document's events to, and `OPFParser`'s
/// `OPFHandler` after them for a package document.
struct MetadataHandlers<'m> {
    metadata: &'m mut TikaMetadata,
    dublin_core: ElementHandlers,
    rendition: Option<Rendition>,
}

impl SaxHandler for MetadataHandlers<'_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.dublin_core.start(namespace, local_name);
        if let Some(rendition) = &mut self.rendition {
            rendition.start(local_name, attributes, self.metadata);
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.dublin_core.end(namespace, local_name, self.metadata);
        if let Some(rendition) = &mut self.rendition {
            rendition.end(local_name, self.metadata);
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.dublin_core.characters(text);
        if let Some(rendition) = &mut self.rendition {
            rendition.characters(text);
        }
        Ok(())
    }
}

/// Which of Tika's XML metadata parsers reads a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetadataParser {
    /// `DcXMLParser`, for `metadata.xml`.
    DublinCore,
    /// `OPFParser`, for the package document: the layout is reflowable unless it says otherwise.
    Package,
}

/// Reads the metadata of the XML document `input`, which `what` names, into `metadata`, as
/// Tika's `parser` does, failing as its `XMLParser` fails.
pub(super) fn read_metadata(
    input: &[u8],
    what: &str,
    parser: MetadataParser,
    metadata: &mut TikaMetadata,
) -> Result<(), Failure> {
    if parser == MetadataParser::Package {
        metadata.set(RENDITION_LAYOUT, "reflowable");
    }
    let mut handler = MetadataHandlers {
        metadata,
        dublin_core: ElementHandlers::dublin_core(),
        rendition: (parser == MetadataParser::Package).then(Rendition::default),
    };
    xml::parse_sax(input, what, &mut handler).map_err(|failure| match failure {
        Failure::Malformed(reason) => Failure::Malformed(format!("XML parse error: {reason}")),
        other => other,
    })
}

/// A manifest item: where it is, relative to the package document, and its media type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Item {
    pub(super) href: String,
    pub(super) media_type: Option<String>,
}

/// What Tika's `ContentOrderScraper` reads from the package document.
#[derive(Debug, Default)]
pub(super) struct Contents {
    /// The manifest items by id, in Java's `HashMap` order.
    pub(super) items: JavaHashMap<Item>,
    /// The ids the spine lists, in its order.
    pub(super) spine: Vec<String>,
    cover_image: Option<String>,
    cover_meta: Option<String>,
    in_manifest: bool,
    in_spine: bool,
}

impl Contents {
    /// The manifest id of the cover image: the EPUB 3 `cover-image` item, or else the item the
    /// EPUB 2 `cover` meta names.
    pub(super) fn cover(&self) -> Option<&str> {
        self.cover_image.as_deref().or(self.cover_meta.as_deref())
    }

    /// Reads the manifest and spine of the package document `input`.
    pub(super) fn read(input: &[u8], what: &str) -> Result<Self, Failure> {
        let mut contents = Self::default();
        xml::parse_sax(input, what, &mut contents)?;
        Ok(contents)
    }
}

impl SaxHandler for Contents {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if java::equals_ignore_case(local_name, "manifest") {
            self.in_manifest = true;
        } else if java::equals_ignore_case(local_name, "spine") {
            self.in_spine = true;
        }
        if self.in_manifest {
            if java::equals_ignore_case(local_name, "item") {
                let id = attribute_value(attributes, "id");
                let href = attribute_value(attributes, "href");
                if let (Some(id), Some(href)) = (id, href) {
                    let href = java::url_decode(href).ok_or_else(|| {
                        Failure::Malformed(format!(
                            "The manifest item {id} has the href {href:?}, which Java's \
                             URLDecoder rejects"
                        ))
                    })?;
                    let media_type = attribute_value(attributes, "media-type").map(str::to_owned);
                    self.items.insert(id, Item { href, media_type });
                    let cover_image =
                        attribute_value(attributes, "properties").is_some_and(|properties| {
                            java::trim(properties)
                                .split(is_regex_whitespace)
                                .any(|property| property == "cover-image")
                        });
                    if self.cover_image.is_none() && cover_image {
                        self.cover_image = Some(id.to_owned());
                    }
                }
            }
        } else if java::equals_ignore_case(local_name, "meta")
            && self.cover_meta.is_none()
            && attribute_value(attributes, "name") == Some("cover")
        {
            if let Some(content) = attribute_value(attributes, "content")
                .filter(|content| !crate::java::is_blank(content))
            {
                self.cover_meta = Some(java::trim(content).to_owned());
            }
        }
        if self.in_spine && java::equals_ignore_case(local_name, "itemRef") {
            if let Some(id) = attribute_value(attributes, "idref") {
                self.spine.push(id.to_owned());
            }
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if java::equals_ignore_case(local_name, "manifest") {
            self.in_manifest = false;
        } else if java::equals_ignore_case(local_name, "spine") {
            self.in_spine = false;
        }
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// Whether a Java regular expression's `\s` matches `character`.
fn is_regex_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// Whether `encryption.xml` names an encrypted item, as Tika's `EncryptionHandler` collects them.
pub(super) fn names_encrypted_items(encryption: &[u8]) -> Result<bool, Failure> {
    struct CipherReferences(bool);

    impl SaxHandler for CipherReferences {
        fn start_element(
            &mut self,
            _namespace: &str,
            local_name: &str,
            _attributes: &[Attribute],
        ) -> Result<(), Failure> {
            self.0 |= local_name == "CipherReference";
            Ok(())
        }

        fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
            Ok(())
        }

        fn characters(&mut self, _text: &str) -> Result<(), Failure> {
            Ok(())
        }
    }

    let mut references = CipherReferences(false);
    xml::parse_sax(encryption, "META-INF/encryption.xml", &mut references)?;
    Ok(references.0)
}
