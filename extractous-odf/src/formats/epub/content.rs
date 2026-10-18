//! An EPUB content document as Tika's `EpubContentParser` hands it on: read as XML, behind an
//! `EmbeddedContentHandler`, the `EpubNormalizingHandler`, which drops the prefixes of attribute
//! names and keeps the elements it has open, and a `BodyContentHandler`, which keeps what lies
//! inside the XHTML `html/body`, into the EPUB's own `XHTMLContentHandler`. The namespace
//! prefixes the document maps reach the output wherever they are mapped.

use crate::formats::xhtml::{Stop, Xhtml, XHTML_NAMESPACE};
use crate::formats::xml::{self, Attribute, SaxHandler, StartTag};
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

/// One content document written into the EPUB's XHTML.
pub(super) struct ContentDocument<'a, 'x> {
    xhtml: &'a mut Xhtml<'x>,
    open: Vec<(Place, String)>,
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
        while let Some((place, name)) = self.open.pop() {
            if place == Place::Inside {
                self.xhtml.end_element(&name)?;
            }
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
            self.open.last().map(|(place, _)| *place),
            namespace,
            local_name,
        );
        if place == Place::Inside && !is_xhtml(namespace) {
            return Err(Failure::Malformed(format!(
                "The content document has the element {local_name} in {namespace:?}, outside \
                 XHTML, which only Tika's path writes"
            )));
        }
        self.open.push((place, local_name.to_owned()));
        if place != Place::Inside {
            return Ok(());
        }
        self.xhtml
            .start_element_with(local_name, &normalized(attributes))?;
        Ok(())
    }

    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        for declaration in tag.declarations {
            let (prefix, uri) = prefix_mapping(declaration);
            self.xhtml.start_prefix_mapping(prefix, uri)?;
        }
        self.start_element(tag.namespace, tag.local_name, tag.attributes)
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        let (place, _) = self
            .open
            .pop()
            .expect("the XML reader ends only the elements it started");
        if place == Place::Inside {
            self.xhtml.end_element(local_name)?;
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self
            .open
            .last()
            .is_some_and(|(place, _)| place.writes_text())
        {
            self.xhtml.characters(text)?;
        }
        Ok(())
    }
}

/// Whether a content document may have an element outside XHTML in its body: whether it maps a
/// prefix an element uses or declares a default namespace more than once. A document in an
/// encoding that does not spell markup in ASCII may.
fn may_hold_foreign_elements(input: &[u8]) -> bool {
    let ascii_markup = !matches!(
        input,
        [0xfe, 0xff, ..] | [0xff, 0xfe, ..] | [0, ..] | [_, 0, ..]
    );
    if !ascii_markup {
        return true;
    }
    let default_declarations = input
        .windows(6)
        .filter(|window| *window == b"xmlns=")
        .count();
    let prefixed_element = input.windows(2).enumerate().any(|(index, window)| {
        window[0] == b'<' && window[1] != b'/' && window[1] != b'!' && window[1] != b'?' && {
            let name = &input[index + 1..];
            let end = name
                .iter()
                .position(|&byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/'))
                .unwrap_or(name.len());
            name[..end].contains(&b':')
        }
    });
    default_declarations > 1 || prefixed_element
}

/// Whether the body of the content document `input` holds an element outside XHTML, before
/// anything Java's parser would stop at, which this port does not write.
pub(super) fn holds_foreign_elements(input: &[u8], what: &str) -> bool {
    struct Finder {
        open: Vec<Place>,
        found: bool,
    }

    impl SaxHandler for Finder {
        fn start_element(
            &mut self,
            namespace: &str,
            local_name: &str,
            _attributes: &[Attribute],
        ) -> Result<(), Failure> {
            let place = Place::of(self.open.last().copied(), namespace, local_name);
            self.open.push(place);
            if place == Place::Inside && !is_xhtml(namespace) {
                self.found = true;
                return Err(Failure::Malformed(format!("<{local_name}> found")));
            }
            Ok(())
        }

        fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
            self.open.pop();
            Ok(())
        }

        fn characters(&mut self, _text: &str) -> Result<(), Failure> {
            Ok(())
        }
    }

    if !may_hold_foreign_elements(input) {
        return false;
    }
    let mut finder = Finder {
        open: Vec::new(),
        found: false,
    };
    let stopped = xml::parse_sax(input, what, &mut finder).is_err();
    stopped && finder.found
}
