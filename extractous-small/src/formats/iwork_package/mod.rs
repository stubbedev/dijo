//! Keynote, Numbers and Pages documents of iWork '08 and '09, as Tika's `IWorkPackageParser`
//! reads them: the package read front to back by its local headers, as commons-compress's
//! `ZipArchiveInputStream` reads it, and each content entry whose root element names the
//! document read as XML by the content handler of its kind ([`keynote`], [`numbers`],
//! [`pages`]). The document starts with the first such entry, which gives it its type, where
//! Tika's parser gives it the type of the last (#132): a package without one writes no
//! document. The package's preview, a thumbnail, is never parsed.

mod keynote;
mod numbers;
mod pages;
#[cfg(test)]
mod tests;

use crate::compress::zip::stream::{self, ZipStream};
use crate::compress::zip::Content;
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler, StartTag};
use crate::formats::{Decline, Failure, Parser, Prepared, Unported};
use crate::mime::{IworkDocument, IWORK_CONTENT_ENTRIES};

/// `MARK_LIMIT`: the bytes of a content entry its root element is looked for in.
const MARK_LIMIT: usize = 1096;
/// The type `IWORKDocumentType.ENCRYPTED` names a content entry the stream refuses to read.
const PROTECTED: &str = "application/x-tika-iworks-protected";

/// What a content entry is, as `detectType` finds.
enum Entry {
    /// A document of this kind, its XML.
    Document(IworkDocument, Vec<u8>),
    /// One whose data the stream refuses, as it refuses an encrypted entry.
    Encrypted,
}

/// The package as `parse` reads it: its content entries of a known kind, in order, and the
/// failure that stops the stream, if one does.
struct Package {
    entries: Vec<Entry>,
    failure: Option<String>,
}

/// Reads the package's entries front to back, as `parse` does, keeping the content entries.
fn read(input: &[u8]) -> Result<Package, Unported> {
    let mut zip = ZipStream::new(input, false);
    let mut entries = Vec::new();
    let failed = |failure: stream::Failure| -> Result<String, Unported> {
        match failure {
            stream::Failure::Method(method) => Err(Unported::new(format!(
                "an iWork package entry compressed with method {method}, which the port does \
                 not decompress"
            ))),
            other => Ok(format!(
                "the iWork package's ZIP stream could not be read past an entry: {other:?}"
            )),
        }
    };
    loop {
        let entry = match zip.next_entry() {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(failure) => {
                return Ok(Package {
                    entries,
                    failure: Some(failed(failure)?),
                })
            }
        };
        if !IWORK_CONTENT_ENTRIES.contains(&entry.name.as_str()) {
            continue;
        }
        let content = match zip.content() {
            Ok(Content::Complete(content)) => content,
            Ok(Content::Truncated(_) | Content::Oversized(_)) => {
                return Err(Unported::new(
                    "an iWork content entry the archive cuts short, which Xerces reads up to \
                     where the stream fails",
                ))
            }
            Err(stream::Failure::Unsupported | stream::Failure::DataDescriptor) => {
                entries.push(Entry::Encrypted);
                continue;
            }
            Err(failure) => {
                return Ok(Package {
                    entries,
                    failure: Some(failed(failure)?),
                })
            }
        };
        let head = &content[..content.len().min(MARK_LIMIT)];
        if let Some(kind) = IworkDocument::of(head) {
            if xml::has_document_type(&content) {
                return Err(Unported::new(
                    "a document type declaration in an iWork content entry, under which Xerces \
                     may report whitespace as ignorable",
                ));
            }
            entries.push(Entry::Document(kind, content.into_owned()));
        }
    }
    Ok(Package {
        entries,
        failure: None,
    })
}

/// What a content handler does with the events of a content entry's XML, writing to the
/// document's XHTML.
trait ContentHandler {
    fn start(&mut self, xhtml: &mut Xhtml, tag: &StartTag<'_>) -> Result<(), Failure>;
    fn end(
        &mut self,
        xhtml: &mut Xhtml,
        local_name: &str,
        qualified_name: &str,
    ) -> Result<(), Failure>;
    fn characters(&mut self, xhtml: &mut Xhtml, text: &str) -> Result<(), Failure>;
    fn end_document(&mut self, xhtml: &mut Xhtml) -> Result<(), Failure>;
}

/// A content handler and the XHTML it writes to, as the SAX parser drives them.
struct Driven<'x, 'h, H> {
    xhtml: &'x mut Xhtml<'h>,
    handler: H,
}

impl<H: ContentHandler> SaxHandler for Driven<'_, '_, H> {
    fn start_element(&mut self, _: &str, _: &str, _: &[Attribute]) -> Result<(), Failure> {
        unreachable!("the content handlers take start tags")
    }

    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        self.handler.start(self.xhtml, tag)
    }

    fn end_element(&mut self, _: &str, _: &str) -> Result<(), Failure> {
        unreachable!("the content handlers take end tags")
    }

    fn end_tag(&mut self, _: &str, local_name: &str, qualified_name: &str) -> Result<(), Failure> {
        self.handler.end(self.xhtml, local_name, qualified_name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.handler.characters(self.xhtml, text)
    }
}

/// `XMLReaderUtils.parseSAX` of a content entry's `content` into `handler`, which is told the
/// document ended once the XML has been read whole.
fn parse(xhtml: &mut Xhtml, content: &[u8], handler: impl ContentHandler) -> Result<(), Failure> {
    let mut driven = Driven { xhtml, handler };
    xml::parse_sax(content, "the iWork content entry", &mut driven)?;
    driven.handler.end_document(driven.xhtml)
}

/// The value of the attribute written `name`, as SAX's `getValue(qName)`.
fn value<'a>(tag: &'a StartTag<'_>, name: &str) -> Option<&'a str> {
    xml::attribute_named(tag.attributes, name)
}

/// `Integer.parseInt` of an attribute, which fails the parse where it is no number.
fn integer(tag: &StartTag<'_>, name: &str) -> Result<i32, Failure> {
    let text = value(tag, name);
    text.and_then(crate::java::parse_int).ok_or_else(|| {
        Failure::Malformed(format!(
            "NumberFormatException: the {name} of {} is no number: {text:?}",
            tag.qualified_name
        ))
    })
}

/// Writes a `td` holding `value`, if it has one, as `XHTMLContentHandler.element` does.
fn cell(xhtml: &mut Xhtml, value: Option<&str>) -> Result<(), Failure> {
    if let Some(value) = value {
        xhtml.element("td", value)?;
    }
    Ok(())
}

/// Tika's `IWorkPackageParser`.
pub struct IworkPackageParser;

impl Parser for IworkPackageParser {
    fn name(&self) -> &'static str {
        "extractous::IWorkPackageParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.iwork.IWorkPackageParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.apple.iwork",
            "application/vnd.apple.keynote",
            "application/vnd.apple.numbers",
            "application/vnd.apple.pages",
        ]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let Package { entries, failure } = read(input)?;
        if entries.is_empty() {
            return Err(match failure {
                Some(failure) => Decline::Fails(Error::ParseError(failure)),
                None => Decline::Unwritten(metadata.clone()),
            });
        }
        let media_type = match &entries[0] {
            Entry::Encrypted => PROTECTED,
            Entry::Document(kind, _) => kind.media_type(),
        };
        Ok(Box::new(move |xhtml| {
            xhtml.metadata_mut().set("Content-Type", media_type);
            for entry in entries {
                match entry {
                    Entry::Encrypted => {}
                    Entry::Document(kind, content) => match kind {
                        IworkDocument::Keynote => {
                            parse(xhtml, &content, keynote::Keynote::default())?
                        }
                        IworkDocument::Numbers => {
                            parse(xhtml, &content, numbers::Numbers::default())?
                        }
                        IworkDocument::Pages => parse(xhtml, &content, pages::Pages::default())?,
                    },
                }
            }
            match failure {
                Some(failure) => Err(Failure::Malformed(failure)),
                None => Ok(()),
            }
        }))
    }
}
