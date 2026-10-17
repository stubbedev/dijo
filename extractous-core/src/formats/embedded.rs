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
use crate::formats::{Failure, InputFile};
use crate::input::Pages;

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
/// whether its embedded documents are parsed or spooled, and the script OCR last found in its
/// images, as our Java `OcrConfig` keeps it.
#[derive(Debug)]
pub(crate) struct ParseContext<'c> {
    configs: &'c ParserConfigs,
    spooler: Option<RefCell<Spooler>>,
    file: InputFile,
    ocr_script: RefCell<Option<String>>,
    write_limit: Option<usize>,
    pages: Pages<'c>,
}

impl<'c> ParseContext<'c> {
    /// An extraction that parses its embedded documents under `configs`.
    pub(crate) fn new(configs: &'c ParserConfigs) -> Self {
        Self {
            configs,
            spooler: None,
            file: InputFile::Spooled,
            ocr_script: RefCell::new(None),
            write_limit: None,
            pages: Pages::IN_MEMORY,
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
            file: InputFile::Spooled,
            ocr_script: RefCell::new(None),
            write_limit: None,
            pages: Pages::IN_MEMORY,
        }
    }

    /// The extraction with its text limited to `limit` UTF-16 code units, as the write limit of
    /// Tika's `WriteOutContentHandler`.
    pub(crate) fn with_write_limit(self, limit: usize) -> Self {
        Self {
            write_limit: Some(limit),
            ..self
        }
    }

    pub(crate) fn configs(&self) -> &'c ParserConfigs {
        self.configs
    }

    /// The extraction of a document Tika holds in `file`.
    pub(crate) fn reading(self, file: InputFile) -> Self {
        Self { file, ..self }
    }

    /// The file Tika holds the document in.
    pub(crate) fn input_file(&self) -> &InputFile {
        &self.file
    }

    /// The extraction of a document whose bytes are `pages`.
    pub(crate) fn paging(self, pages: Pages<'c>) -> Self {
        Self { pages, ..self }
    }

    /// The pages of the document's bytes.
    pub(crate) fn pages(&self) -> Pages<'c> {
        self.pages
    }

    /// The script the next image of this extraction is first read in with the automatic OCR
    /// language: the one detection last found in an earlier image, or none before any.
    pub(crate) fn assumed_ocr_script(&self) -> Option<String> {
        self.ocr_script.borrow().clone()
    }

    /// Remembers the script OCR detection found, for the next images of this extraction.
    pub(crate) fn learn_ocr_script(&self, script: String) {
        *self.ocr_script.borrow_mut() = Some(script);
    }

    /// The limit of the extraction's text, if it has one.
    pub(crate) fn write_limit(&self) -> Option<usize> {
        self.write_limit
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

    /// Records the failure of an embedded document of no bytes, as Tika's
    /// `ParsingEmbeddedDocumentExtractor` records the [`ZERO_BYTES`] exception its
    /// `AutoDetectParser` throws on one before any parser starts it.
    pub(crate) fn record_zero_bytes(&mut self) {
        self.record.exceptions.push(first_line(ZERO_BYTES));
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

/// The first line of the stack trace of the `ZeroByteFileException` Tika's `AutoDetectParser`
/// throws on an embedded document of no bytes, which its `ParsingEmbeddedDocumentExtractor`
/// records in the embedding document's metadata. Its frames, all of them `AutoDetectParser`'s
/// and those of the parsers below it, are Java's own, which the native routing replaces (#68).
pub(crate) const ZERO_BYTES: &str =
    "org.apache.tika.exception.ZeroByteFileException: InputStream must have > 0 bytes";

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

/// The first line of the stack trace `printStackTrace` writes of `exception`, ended by `\n` on
/// every platform: Java ends it with the platform's line separator (#108).
fn first_line(exception: &str) -> String {
    format!("{exception}\n")
}

/// How the failure of the parser named `parser` on a document embedded with `metadata` is
/// recorded in `tk:exception:embedded-exception` of the document embedding it: the parser, the
/// embedded document by its resource name or internal path, and the `reason`, on one line.
/// Tika records the stack trace of the exception its parser throws there (#68).
pub(crate) fn describe_failure(parser: &str, metadata: &TikaMetadata, reason: &str) -> String {
    let document = [RESOURCE_NAME, INTERNAL_PATH]
        .iter()
        .find_map(|name| metadata.get(name).filter(|value| !value.is_empty()))
        .map_or_else(
            || "an embedded document without a name".to_owned(),
            |name| format!("the embedded document \"{name}\""),
        );
    let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{parser} could not parse {document}: {reason}")
}

/// The name a `package-entry` div is headed with, if the document has one.
pub(crate) fn heading(metadata: &TikaMetadata) -> Option<&str> {
    metadata.get(RESOURCE_NAME).filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_failures_on_one_line() {
        let mut metadata = TikaMetadata::new();
        assert_eq!(
            describe_failure("p", &metadata, "no\n\tgood"),
            "p could not parse an embedded document without a name: no good"
        );
        metadata.set(INTERNAL_PATH, "/a/b.xml");
        assert_eq!(
            describe_failure("p", &metadata, "why"),
            "p could not parse the embedded document \"/a/b.xml\": why"
        );
        metadata.set(RESOURCE_NAME, "b.xml");
        assert_eq!(
            describe_failure("p", &metadata, "why"),
            "p could not parse the embedded document \"b.xml\": why"
        );
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
