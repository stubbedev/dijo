//! Documents embedded in one a native parser reads, parsed as Tika's
//! `ParsingEmbeddedDocumentExtractor` parses them under our Java `ContentEmbeddedDocumentExtractor`:
//! every one but a thumbnail, by the extraction's own parser and settings, OCR included, its body
//! written into the embedding document, in a `package-entry` div headed by its name when the
//! parser asks for one. Until every format has a native parser, Tika parses them, through
//! `TikaNativeMain.parseEmbeddedBytes`, and its events are replayed here. An extraction that
//! spools its embedded documents copies each to a file of its own instead, as our Java
//! `EmbeddedSpooler` does.

use std::cell::RefCell;
use std::fs::File;
use std::io::Write as _;

use crate::config::ParserConfigs;
use crate::embedded::{EmbeddedSpool, SpooledEmbedded};
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::Failure;
use crate::java::LINE_SEPARATOR;

/// The class of the div an embedded document is written in.
pub(crate) const PACKAGE_ENTRY: &str = "package-entry";
pub(crate) const RESOURCE_NAME: &str = "tk:resource-name";
pub(crate) const INTERNAL_PATH: &str = "tk:internal-path";
pub(crate) const RELATIONSHIP_ID: &str = "tk:embedded-relationship-id";
const RESOURCE_TYPE: &str = "tk:embedded-resource-type";
const PARSED_BY_FULL_SET: &str = "tk:parsed-by-full-set";
const EMBEDDED_EXCEPTION: &str = "tk:exception:embedded-exception";
const EMBEDDED_STREAM_EXCEPTION: &str = "tk:exception:embedded-stream-exception";
/// `TikaCoreProperties.TIKA_META_EXCEPTION_WARNING`.
pub(crate) const EXCEPTION_WARNING: &str = "tk:exception:warn";

/// What the parses of one extraction share, as Tika's `ParseContext` does: the parser settings,
/// and whether its embedded documents are parsed or spooled.
#[derive(Debug)]
pub(crate) struct ParseContext<'c> {
    configs: &'c ParserConfigs,
    spooler: Option<RefCell<Spooler>>,
}

impl<'c> ParseContext<'c> {
    /// An extraction that parses its embedded documents under `configs`.
    pub(crate) fn new(configs: &'c ParserConfigs) -> Self {
        Self {
            configs,
            spooler: None,
        }
    }

    /// An extraction that copies its embedded documents to `spool`.
    pub(crate) fn spooling(configs: &'c ParserConfigs, spool: &EmbeddedSpool) -> Self {
        Self {
            configs,
            spooler: Some(RefCell::new(Spooler {
                spool: spool.clone(),
                budget: spool.byte_budget,
                spooled: Vec::new(),
            })),
        }
    }

    pub(crate) fn configs(&self) -> &'c ParserConfigs {
        self.configs
    }

    /// The embedded documents copied to the spool, in order.
    pub(crate) fn into_spooled(self) -> Vec<SpooledEmbedded> {
        self.spooler
            .map_or_else(Vec::new, |spooler| spooler.into_inner().spooled)
    }
}

/// Our Java `EmbeddedSpooler`: each document embedded directly in the extracted one copied to
/// a file of the spool named by its number, the extraction's byte budget shared among them.
#[derive(Debug)]
struct Spooler {
    spool: EmbeddedSpool,
    budget: u64,
    spooled: Vec<SpooledEmbedded>,
}

impl Spooler {
    /// `parseEmbedded`: copies `data` to the next file, as much of it as the budget leaves,
    /// named by the document's resource name or its number.
    fn spool(&mut self, data: &[u8], metadata: &TikaMetadata) -> Result<(), Failure> {
        let index = self.spooled.len();
        let path = self.spool.dir.join(index.to_string());
        let kept = data
            .len()
            .min(usize::try_from(self.budget).unwrap_or(usize::MAX));
        let written = File::create_new(&path).and_then(|mut file| file.write_all(&data[..kept]));
        written.map_err(|e| {
            Failure::Extraction(Error::IoError(format!(
                "Could not spool an embedded document to {}: {e}",
                path.display()
            )))
        })?;
        let size = kept as u64;
        self.budget -= size;
        self.spooled.push(SpooledEmbedded {
            name: metadata
                .get(RESOURCE_NAME)
                .map_or_else(|| format!("embedded-{index}"), str::to_owned),
            content_type: metadata.get("Content-Type").map(str::to_owned),
            path,
            size,
            truncated: kept < data.len(),
        });
        Ok(())
    }
}

/// What the embedded documents of a document are parsed with, and what they leave for its
/// metadata.
#[derive(Debug)]
pub(crate) struct Embedding<'c> {
    context: &'c ParseContext<'c>,
    record: EmbeddedRecord,
}

impl<'c> Embedding<'c> {
    pub(crate) fn new(context: &'c ParseContext<'c>) -> Self {
        Self {
            context,
            record: EmbeddedRecord::default(),
        }
    }

    pub(crate) fn context(&self) -> &'c ParseContext<'c> {
        self.context
    }

    /// Copies an embedded document to the spool, when the extraction spools them: whether it
    /// did.
    pub(crate) fn spool(&self, data: &[u8], metadata: &TikaMetadata) -> Result<bool, Failure> {
        let Some(spooler) = &self.context.spooler else {
            return Ok(false);
        };
        spooler.borrow_mut().spool(data, metadata)?;
        Ok(true)
    }

    /// Takes in the parsers an embedded document's parse used, as Tika's shared `ParseRecord`
    /// records each when it starts, before any of the document's content is written.
    pub(crate) fn absorb_parsers(&mut self, metadata: &crate::Metadata) {
        for parser in metadata.get(PARSED_BY_FULL_SET).into_iter().flatten() {
            self.record_parser(parser);
        }
    }

    /// Records `parser`, which a document parsed into this one's handler starts with, as
    /// Tika's shared `ParseRecord` records each parser `CompositeParser` dispatches to.
    pub(crate) fn record_parser(&mut self, parser: &str) {
        if !self
            .record
            .parsers
            .iter()
            .any(|recorded| recorded == parser)
        {
            self.record.parsers.push(parser.to_owned());
        }
    }

    /// Takes in the failures an embedded document's parse recorded: those of the documents
    /// embedded in it, then its own `recorded` failure, in the order Tika's shared `ParseRecord`
    /// records them, once the content has been written.
    pub(crate) fn absorb_failures(&mut self, metadata: &crate::Metadata, recorded: Option<String>) {
        self.record.exceptions.extend(
            metadata
                .get(EMBEDDED_EXCEPTION)
                .into_iter()
                .flatten()
                .cloned(),
        );
        self.record.exceptions.extend(recorded);
    }

    pub(crate) fn into_record(self) -> EmbeddedRecord {
        self.record
    }
}

/// What a document's embedded documents leave for its metadata: the parsers that read them and
/// the failures Tika records.
#[derive(Debug, Default)]
pub(crate) struct EmbeddedRecord {
    parsers: Vec<String>,
    exceptions: Vec<String>,
}

impl EmbeddedRecord {
    /// Adds to `metadata` what Tika's `CompositeParser` adds once the document's parser, the
    /// one it `dispatched` to, has returned: that parser and every parser a document embedded
    /// in it was dispatched to (not the extractors a parser records in `tk:parsed-by` itself),
    /// and the failures of its embedded documents.
    pub(crate) fn finish(self, metadata: &mut TikaMetadata, dispatched: &str) {
        let mut parsers = vec![dispatched.to_owned()];
        for parser in self.parsers {
            if !parsers.contains(&parser) {
                parsers.push(parser);
            }
        }
        metadata.set_values(PARSED_BY_FULL_SET, parsers);
        for exception in self.exceptions {
            metadata.add(EMBEDDED_EXCEPTION, exception);
        }
    }
}

/// Whether the embedded document is written in a `package-entry` div headed by its name, as
/// `parseEmbedded`'s `outputHtml` asks, or bare; and whether the parser hands the extractor
/// its `XHTMLContentHandler` or, for `UnderlyingPackageEntry`, the content handler it was
/// given, which the div and the document's events go to without the XHTML's line breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wrapping {
    PackageEntry,
    UnderlyingPackageEntry,
    Bare,
}

/// What an embedded document is to the document embedding it, as Tika's
/// `TikaCoreProperties.EmbeddedResourceType` names it in `tk:embedded-resource-type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceType {
    Attachment,
    Inline,
    Macro,
    Thumbnail,
    AlternateFormatChunk,
}

impl ResourceType {
    fn name(self) -> &'static str {
        match self {
            Self::Attachment => "ATTACHMENT",
            Self::Inline => "INLINE",
            Self::Macro => "MACRO",
            Self::Thumbnail => "THUMBNAIL",
            Self::AlternateFormatChunk => "ALTERNATE_FORMAT_CHUNK",
        }
    }

    /// Records this as the resource type of the document embedded with `metadata`.
    pub(crate) fn set(self, metadata: &mut TikaMetadata) {
        metadata.set(RESOURCE_TYPE, self.name());
    }

    /// Whether the document embedded with `metadata` is of this resource type.
    pub(crate) fn is_of(self, metadata: &TikaMetadata) -> bool {
        metadata.get(RESOURCE_TYPE) == Some(self.name())
    }

    /// Whether the document embedded with `metadata` has a resource type at all.
    pub(crate) fn is_set(metadata: &TikaMetadata) -> bool {
        metadata.get(RESOURCE_TYPE).is_some()
    }
}

/// Whether Tika parses a document embedded with `metadata`: every one but a thumbnail, which
/// would only add a garbled copy of the front page's text.
pub(crate) fn is_parsed(metadata: &TikaMetadata) -> bool {
    !ResourceType::Thumbnail.is_of(metadata)
}

/// `TikaPagedText.PAGE_NUMBER`: the page an embedded resource is on, when it is on one.
pub(crate) const PAGE_NUMBER: &str = "tk:page:number";
/// `TikaPagedText.PAGE_NUMBERS`: the pages an embedded resource is on.
pub(crate) const PAGE_NUMBERS: &str = "tk:page:numbers";

/// `PageAnchoring.applyPageMetadata`: the pages an embedded resource is on in order, -1 for
/// none, and the page when there is one.
pub(crate) fn apply_page_numbers(metadata: &mut TikaMetadata, pages: &[i32]) {
    let mut sorted = pages.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        sorted.push(-1);
    }
    metadata.remove(PAGE_NUMBERS);
    metadata.remove(PAGE_NUMBER);
    for page in &sorted {
        metadata.add(PAGE_NUMBERS, page.to_string());
    }
    if let [page] = sorted[..] {
        if page != -1 {
            metadata.set(PAGE_NUMBER, page.to_string());
        }
    }
}

/// `EmbeddedDocumentUtil.recordEmbeddedStreamException` of an exception a parser makes itself,
/// `class: message`: the first line of the stack trace Tika records, without the frames of the
/// Java parser, which the native one replaces.
pub(crate) fn record_stream_exception(metadata: &mut TikaMetadata, exception: &str) {
    metadata.add(EMBEDDED_STREAM_EXCEPTION, first_line(exception));
}

/// `EmbeddedDocumentUtil.recordException` of an exception a parser makes itself, `class:
/// message`: the first line of the stack trace Tika records as a warning, without the frames of
/// the Java parser, which the native one replaces.
pub(crate) fn record_exception(metadata: &mut TikaMetadata, exception: &str) {
    metadata.add(EXCEPTION_WARNING, first_line(exception));
}

/// The first line of the stack trace `printStackTrace` writes of `exception`.
fn first_line(exception: &str) -> String {
    format!("{exception}{LINE_SEPARATOR}")
}

/// The name a `package-entry` div is headed with, if the document has one.
pub(crate) fn heading(metadata: &TikaMetadata) -> Option<&str> {
    metadata.get(RESOURCE_NAME).filter(|name| !name.is_empty())
}

/// One SAX event of an embedded document's body, as `EventRecorder` encodes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Start {
        name: String,
        attributes: Vec<(String, String)>,
    },
    End(String),
    Characters(String),
    IgnorableWhitespace(String),
}

/// The events `EventRecorder` encoded in `bytes`.
pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<Event>, Failure> {
    let mut reader = Reader { bytes, at: 0 };
    let mut events = Vec::new();
    while let Some(&tag) = reader.bytes.get(reader.at) {
        reader.at += 1;
        events.push(match tag {
            b'S' => {
                let name = reader.string()?;
                let count = reader.int()?;
                let attributes = (0..count)
                    .map(|_| Ok((reader.string()?, reader.string()?)))
                    .collect::<Result<_, Failure>>()?;
                Event::Start { name, attributes }
            }
            b'E' => Event::End(reader.string()?),
            b'C' => Event::Characters(reader.string()?),
            b'W' => Event::IgnorableWhitespace(reader.string()?),
            other => return Err(corrupt(format!("an unknown event tag {other:#04x}"))),
        });
    }
    Ok(events)
}

fn corrupt(what: String) -> Failure {
    Failure::Malformed(format!(
        "The events Tika recorded for an embedded document hold {what}"
    ))
}

struct Reader<'b> {
    bytes: &'b [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], Failure> {
        let end = self
            .at
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| corrupt(format!("a field of {length} bytes past their end")))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn int(&mut self) -> Result<usize, Failure> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("four bytes were taken");
        usize::try_from(i32::from_be_bytes(bytes))
            .map_err(|_| corrupt("a negative length".to_owned()))
    }

    fn string(&mut self) -> Result<String, Failure> {
        let length = self.int()?;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| corrupt(format!("text that is not UTF-8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(out: &mut Vec<u8>, text: &str) {
        out.extend_from_slice(&i32::try_from(text.len()).unwrap().to_be_bytes());
        out.extend_from_slice(text.as_bytes());
    }

    #[test]
    fn decodes_what_the_event_recorder_encodes() {
        let mut bytes = vec![b'S'];
        string(&mut bytes, "p");
        bytes.extend_from_slice(&1i32.to_be_bytes());
        string(&mut bytes, "class");
        string(&mut bytes, "ø");
        bytes.push(b'C');
        string(&mut bytes, "Hej 😀");
        bytes.push(b'W');
        string(&mut bytes, "\n");
        bytes.push(b'E');
        string(&mut bytes, "p");
        assert_eq!(
            decode(&bytes).unwrap(),
            [
                Event::Start {
                    name: "p".to_owned(),
                    attributes: vec![("class".to_owned(), "ø".to_owned())]
                },
                Event::Characters("Hej 😀".to_owned()),
                Event::IgnorableWhitespace("\n".to_owned()),
                Event::End("p".to_owned()),
            ]
        );
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode(b"X").is_err());
    }

    #[test]
    fn records_parsers_once_and_failures_in_order() {
        let configs = ParserConfigs::default();
        let context = ParseContext::new(&configs);
        let mut embedding = Embedding::new(&context);
        let mut first = crate::Metadata::new();
        first.insert(
            PARSED_BY_FULL_SET.to_owned(),
            vec!["a".to_owned(), "b".to_owned()],
        );
        first.insert(EMBEDDED_EXCEPTION.to_owned(), vec!["nested".to_owned()]);
        embedding.absorb_parsers(&first);
        embedding.absorb_failures(&first, Some("own".to_owned()));
        let mut second = crate::Metadata::new();
        second.insert(
            PARSED_BY_FULL_SET.to_owned(),
            vec!["b".to_owned(), "c".to_owned()],
        );
        embedding.absorb_parsers(&second);
        embedding.absorb_failures(&second, None);
        let mut metadata = TikaMetadata::new();
        embedding.into_record().finish(&mut metadata, "native");
        assert_eq!(
            metadata.values(PARSED_BY_FULL_SET),
            ["native", "a", "b", "c"]
        );
        assert_eq!(metadata.values(EMBEDDED_EXCEPTION), ["nested", "own"]);
    }
}
