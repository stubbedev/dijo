//! An EPUB content document as Tika's `EpubContentParser` hands it on: read as XML, behind an
//! `EmbeddedContentHandler`, the `EpubNormalizingHandler`, which drops the prefixes of attribute
//! names and keeps the elements it has open, and a `BodyContentHandler`, which keeps what lies
//! inside the XHTML `html/body`, into the EPUB's own `XHTMLContentHandler`. The namespace
//! prefixes the document maps reach the output wherever they are mapped. An element outside
//! XHTML in the body, such as an iBooks `svg`, is written by its local name in its own
//! namespace: neither indented nor followed by a line break.

use crate::formats::xhtml::{ExpandedName, Stop, Xhtml, XHTML_NAMESPACE};
use crate::formats::xml::{Attribute, SaxHandler, StartTag};
use crate::formats::Failure;

/// Where an open element lies for the `BodyContentHandler`'s `/xhtml:html/xhtml:body/descendant::node()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The XHTML `html` root.
    Html,
    /// An XHTML `body` of that root.
    Body,
    /// Below such a body: written.
    Inside,
    /// Anywhere else: dropped, with all below it.
    Outside,
}

/// Whether an element of `namespace` is XHTML: in its namespace, or in none, as a content
/// document without the XHTML namespace has it. Tika's `BodyContentHandler` matches the XHTML
/// namespace only, so such a document yields no text (#89).
fn is_xhtml(namespace: &str) -> bool {
    namespace == XHTML_NAMESPACE || namespace.is_empty()
}

impl Place {
    /// Where an element of `namespace` and `local_name` opened inside `parent` lies.
    fn of(parent: Option<Place>, namespace: &str, local_name: &str) -> Self {
        match parent {
            None if is_xhtml(namespace) && local_name == "html" => Self::Html,
            Some(Self::Html) if is_xhtml(namespace) && local_name == "body" => Self::Body,
            Some(Self::Body | Self::Inside) => Self::Inside,
            None | Some(Self::Html | Self::Outside) => Self::Outside,
        }
    }

    /// Whether text directly in an element lying here is written.
    fn writes_text(self) -> bool {
        matches!(self, Self::Body | Self::Inside)
    }
}

/// The prefix and namespace of an `xmlns` attribute, as SAX's `startPrefixMapping` gives them.
fn prefix_mapping(declaration: &Attribute) -> (&str, &str) {
    let prefix = if declaration.qualified_name == "xmlns" {
        ""
    } else {
        declaration.local_name.as_str()
    };
    (prefix, declaration.value.as_str())
}

/// The namespace of the `xml` prefix.
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// The attributes as the `EpubNormalizingHandler` passes them on, by local name: where one has a
/// prefix, only the first of each local name. Attributes of the `xml` prefix keep it, so
/// `xml:lang` and `lang` are both kept: Tika collapses them into the first (#89).
fn normalized(attributes: &[Attribute]) -> Vec<(&str, &str)> {
    let mut kept: Vec<(&str, &str)> = Vec::with_capacity(attributes.len());
    for attribute in attributes {
        let name = if attribute.namespace == XML_NAMESPACE {
            attribute.qualified_name.as_str()
        } else {
            attribute.local_name.as_str()
        };
        if !kept.iter().any(|(kept, _)| *kept == name) {
            kept.push((name, &attribute.value));
        }
    }
    kept
}

/// An element the content document has open: where it lies, and its name as it is written,
/// in no namespace as in XHTML.
struct Open {
    place: Place,
    name: ExpandedName,
}

impl Open {
    fn new(place: Place, namespace: &str, local_name: &str) -> Self {
        let namespace = if is_xhtml(namespace) {
            XHTML_NAMESPACE
        } else {
            namespace
        };
        Self {
            place,
            name: ExpandedName::new(namespace, local_name),
        }
    }
}

/// One content document written into the EPUB's XHTML.
pub(super) struct ContentDocument<'a, 'x> {
    xhtml: &'a mut Xhtml<'x>,
    open: Vec<Open>,
}

impl<'a, 'x> ContentDocument<'a, 'x> {
    pub(super) fn new(xhtml: &'a mut Xhtml<'x>) -> Self {
        Self {
            xhtml,
            open: Vec::new(),
        }
    }

    /// `XHTMLBalancingHandler.drainOpenElements`: ends the elements a failed parse left open,
    /// writing the ends of those inside the body.
    pub(super) fn drain_open_elements(&mut self) -> Result<(), Stop> {
        while let Some(open) = self.open.pop() {
            self.end(&open)?;
        }
        Ok(())
    }

    /// Ends `open`, written if it lies inside the body.
    fn end(&mut self, open: &Open) -> Result<(), Stop> {
        if open.place == Place::Inside {
            self.xhtml
                .end_element_in(open.name.namespace(), open.name.name())?;
        }
        Ok(())
    }
}

impl SaxHandler for ContentDocument<'_, '_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let place = Place::of(
            self.open.last().map(|open| open.place),
            namespace,
            local_name,
        );
        let open = Open::new(place, namespace, local_name);
        if place == Place::Inside {
            self.xhtml.start_element_in(
                open.name.namespace(),
                local_name,
                &normalized(attributes),
            )?;
        }
        self.open.push(open);
        Ok(())
    }

    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        for declaration in tag.declarations {
            let (prefix, uri) = prefix_mapping(declaration);
            self.xhtml.start_prefix_mapping(prefix, uri)?;
        }
        self.start_element(tag.namespace, tag.local_name, tag.attributes)
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        let open = self
            .open
            .pop()
            .expect("the XML reader ends only the elements it started");
        self.end(&open)?;
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self
            .open
            .last()
            .is_some_and(|open| open.place.writes_text())
        {
            self.xhtml.characters(text)?;
        }
        Ok(())
    }
}
