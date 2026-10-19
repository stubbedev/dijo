//! Keynote, Numbers and Pages documents of iWork '08 and '09, as Tika's `IWorkPackageParser`
//! reads them: the package read front to back by its local headers, as commons-compress's
//! `ZipArchiveInputStream` reads it, and each content entry whose root element names the
//! document read as XML by the content handler of its kind ([`keynote`], [`numbers`],
//! [`pages`]). The document starts with the first such entry, which gives it its type, where
//! Tika's parser gives it the type of the last (#132): a package without one writes no
//! document. The package's preview, a thumbnail, is never parsed.
//!
//! Each content entry is spooled as it is read: in prepare, to find its kind, and again as it is
//! written, the stream read front to back once more up to it, one entry at a time.

mod keynote;
mod numbers;
mod pages;
#[cfg(test)]
mod tests;

use crate::compress::zip::stream::{self, ZipStream};
use crate::compress::zip::Decoded;
use crate::compress::Undecoded;
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler, StartTag};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared, Unported};
use crate::input::{Input, Pages};
use crate::mime::{IworkDocument, IWORK_CONTENT_ENTRIES};

/// `MARK_LIMIT`: the bytes of a content entry its root element is looked for in.
const MARK_LIMIT: usize = 1096;
/// The type `IWORKDocumentType.ENCRYPTED` names a content entry the stream refuses to read.
const PROTECTED: &str = "application/x-tika-iworks-protected";

/// What a content entry is, as `detectType` finds.
enum Entry {
    /// A document of this kind, its XML.
    Document(IworkDocument, Content),
    /// One whose data the stream refuses, as it refuses an encrypted entry.
    Encrypted,
}

/// A content entry read ahead: the how manyth entry of a content entry's name it is in the
/// stream, and its XML where it spooled to a file, which holds no memory.
struct Content {
    at: usize,
    kept: Option<Input<'static>>,
}

/// The package as `parse` reads it: its content entries of a known kind, in order, and the
/// failure that stops the stream, if one does.
struct Package {
    entries: Vec<Entry>,
    failure: Option<String>,
}

/// The XML of the content entry `zip` is in, whose data has the pages `pages`, spooled: all of
/// it, or why it is read otherwise.
fn spool<'a>(
    zip: &ZipStream<'a>,
    pages: Pages<'a>,
) -> Result<Result<Input<'a>, Unread>, stream::Failure> {
    Ok(match zip.spool(pages)? {
        Ok(Decoded::Complete(content)) => Ok(content),
        Ok(Decoded::Truncated(_) | Decoded::Oversized(_)) => Err(Unread::CutShort),
        Err(undecoded) => Err(Unread::Unspooled(undecoded)),
    })
}

/// Why the XML of a content entry is not read whole.
enum Unread {
    /// The archive cuts it short, or it decodes past the limit.
    CutShort,
    /// Spooling it fails.
    Unspooled(Undecoded),
}

/// Reads the package's entries front to back, as `parse` does, from `input`, whose pages are
/// `pages`, finding the kind of each content entry.
fn read(input: &[u8], pages: Pages<'_>) -> Result<Package, Decline<'static>> {
    let mut zip = ZipStream::new(input, false);
    let mut entries = Vec::new();
    let mut at = 0;
    let failed = |failure: stream::Failure| {
        format!("the iWork package's ZIP stream could not be read past an entry: {failure:?}")
    };
    loop {
        let entry = match zip.next_entry() {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(failure) => {
                return Ok(Package {
                    entries,
                    failure: Some(failed(failure)),
                })
            }
        };
        if !IWORK_CONTENT_ENTRIES.contains(&entry.name.as_str()) {
            continue;
        }
        at += 1;
        let content = match spool(&zip, pages) {
            Ok(Ok(content)) => content,
            Ok(Err(Unread::CutShort)) => {
                return Err(Unported::new(
                    "an iWork content entry the archive cuts short or that decodes past the \
                     limit, which Xerces reads up to where the stream fails",
                )
                .into())
            }
            Ok(Err(Unread::Unspooled(undecoded))) => {
                return Err(Decline::Fails(Error::IoError(format!(
                    "reading the iWork content entry {} fails: {undecoded}",
                    entry.name
                ))))
            }
            Err(stream::Failure::Unsupported | stream::Failure::DataDescriptor) => {
                entries.push(Entry::Encrypted);
                continue;
            }
            Err(failure) => {
                return Ok(Package {
                    entries,
                    failure: Some(failed(failure)),
                })
            }
        };
        let head = &content[..content.len().min(MARK_LIMIT)];
        if let Some(kind) = IworkDocument::of(head) {
            if xml::has_document_type(&content) {
                return Err(Unported::new(
                    "a document type declaration in an iWork content entry, under which Xerces \
                     may report whitespace as ignorable",
                )
                .into());
            }
            let kept = match content {
                Input::Mapped(map) => Some(Input::Mapped(map)),
                _ => None,
            };
            entries.push(Entry::Document(kind, Content { at, kept }));
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
fn parse(
    xhtml: &mut Xhtml,
    content: &Input<'_>,
    handler: impl ContentHandler,
) -> Result<(), Failure> {
    let mut driven = Driven { xhtml, handler };
    xml::parse_part(content, "the iWork content entry", &mut driven)?;
    driven.handler.end_document(driven.xhtml)
}

/// Reads the stream of `input`, whose pages are `pages`, front to back again, handing `each` the
/// XML of each content entry of `entries`, which [`read`] found in it, with its kind.
fn each_document(
    input: &[u8],
    pages: Pages<'_>,
    entries: Vec<Entry>,
    mut each: impl FnMut(IworkDocument, &Input<'_>) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let mut zip = ZipStream::new(input, false);
    let mut at = 0;
    let again = || Failure::Malformed("the iWork package reads otherwise again".to_owned());
    for (kind, content) in entries.into_iter().filter_map(|entry| match entry {
        Entry::Document(kind, content) => Some((kind, content)),
        Entry::Encrypted => None,
    }) {
        while at < content.at {
            let entry = zip.next_entry().ok().flatten().ok_or_else(again)?;
            if IWORK_CONTENT_ENTRIES.contains(&entry.name.as_str()) {
                at += 1;
            }
        }
        let xml = match content.kept {
            Some(kept) => kept,
            None => match spool(&zip, pages).map_err(|_| again())? {
                Ok(xml) => xml,
                Err(Unread::CutShort) => return Err(again()),
                Err(Unread::Unspooled(undecoded)) => {
                    return Err(Failure::Extraction(Error::IoError(format!(
                        "reading an iWork content entry again fails: {undecoded}"
                    ))))
                }
            },
        };
        each(kind, &xml)?;
    }
    Ok(())
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
        "dijo::IWorkPackageParser"
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

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        _: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let Package { entries, failure } = read(input, pages)?;
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
            each_document(input, pages, entries, |kind, content| match kind {
                IworkDocument::Keynote => parse(xhtml, content, keynote::Keynote::default()),
                IworkDocument::Numbers => parse(xhtml, content, numbers::Numbers::default()),
                IworkDocument::Pages => parse(xhtml, content, pages::Pages::default()),
            })?;
            match failure {
                Some(failure) => Err(Failure::Malformed(failure)),
                None => Ok(()),
            }
        }))
    }
}
