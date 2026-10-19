//! Word 2003 XML (WordprocessingML) and Excel 2003 XML (SpreadsheetML), as Tika's
//! `WordMLParser` ([`word`]) and `SpreadsheetMLParser` ([`spreadsheet`]) read them on their
//! `AbstractXML2003Parser`: the document properties as metadata, teed with the format's own
//! handler, which writes the body to the XHTML. A document declaring a document type is left to
//! Tika, as the whitespace Xerces may report as ignorable under it is dropped by these handlers
//! and kept by the metadata ones.

mod spreadsheet;
mod word;

pub use spreadsheet::SpreadsheetMlParser;
pub use word::WordMlParser;

use crate::formats::element_metadata::{ElementHandlers, Values};
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Prepared};
use crate::input::Pages;

/// `MS_OFFICE_PROPERTIES_URN`, the namespace of the document properties.
const OFFICE_URN: &str = "urn:schemas-microsoft-com:office:office";

/// The document properties `AbstractXML2003Parser` reads, as `(element, name, values)`, in the
/// order of its handlers.
const PROPERTIES: [(&str, &str, Values); 15] = [
    ("Title", "dc:title", Values::Bag),
    ("Author", "dc:creator", Values::Bag),
    ("LastAuthor", "meta:last-author", Values::Single),
    ("Revision", "cp:revision", Values::Single),
    ("TotalTime", "extended-properties:TotalTime", Values::Single),
    ("Created", "dcterms:created", Values::Single),
    ("LastSaved", "meta:save-date", Values::Single),
    ("Pages", "meta:page-count", Values::Single),
    ("Words", "meta:word-count", Values::Single),
    ("Characters", "meta:character-count", Values::Single),
    (
        "CharactersWithSpaces",
        "meta:character-count-with-spaces",
        Values::Single,
    ),
    ("Company", "extended-properties:Company", Values::Single),
    ("Lines", "meta:line-count", Values::Single),
    ("Paragraphs", "meta:paragraph-count", Values::Single),
    ("Version", "cp:version", Values::Single),
];

/// What a format's handler does with the events of the document, beside the property handlers.
trait Body {
    fn start(
        &mut self,
        xhtml: &mut Xhtml,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure>;
    fn end(&mut self, xhtml: &mut Xhtml, namespace: &str, local_name: &str) -> Result<(), Failure>;
    fn characters(&mut self, xhtml: &mut Xhtml, text: &str) -> Result<(), Failure>;
}

/// The `TeeContentHandler` the document's events go to: the property handlers, then the body's.
struct Tee<'x, 'h, B> {
    xhtml: &'x mut Xhtml<'h>,
    properties: ElementHandlers,
    body: B,
}

impl<B: Body> SaxHandler for Tee<'_, '_, B> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.properties.start(namespace, local_name);
        self.body
            .start(self.xhtml, namespace, local_name, attributes)
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        self.properties
            .end(namespace, local_name, self.xhtml.metadata_mut());
        self.body.end(self.xhtml, namespace, local_name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.properties.characters(text);
        self.body.characters(self.xhtml, text)
    }
}

/// `AbstractXML2003Parser.parse`: the content type set, then the document read into `body`, its
/// `pages` released as they are read. A document Xerces fails on fails the parse, after what
/// was written before.
fn prepare<'a, B: Body + 'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    media_type: &'static str,
    body: B,
) -> Result<Prepared<'a>, Decline<'a>> {
    if xml::has_document_type(input) {
        return Err(Decline::Deferred(
            "a document type declaration, under which Xerces may report whitespace as \
             ignorable, which the property handlers keep and the body's drop"
                .to_owned(),
        ));
    }
    Ok(Box::new(move |xhtml| {
        xhtml.metadata_mut().set("Content-Type", media_type);
        let mut tee = Tee {
            xhtml: &mut *xhtml,
            properties: ElementHandlers::new(OFFICE_URN, &PROPERTIES),
            body,
        };
        let parsed = xml::parse_paged(input, pages, "the Office 2003 XML document", &mut tee);
        xhtml.end_document()?;
        parsed.map_err(|failure| match failure {
            Failure::Malformed(reason) => Failure::Malformed(format!("XML parse error: {reason}")),
            other => other,
        })
    }))
}
