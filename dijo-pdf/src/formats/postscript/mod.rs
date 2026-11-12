//! PostScript and EPS documents: their description in the document structuring comments
//! ([`dsc`]), and the text of the strings their program shows, page by page at `showpage`,
//! followed by an interpreter of the language's data, control and font operators
//! ([`machine`]) that does not paint. A string is read through its font's encoding vector:
//! `StandardEncoding`, `ISOLatin1Encoding`, a resident font's own, or one the program builds,
//! such as an embedded font's, its glyph names read by the Adobe Glyph List. Where a page's text
//! cannot be resolved, because the program computes its strings, shows them in a font the
//! interpreter does not know or decode, or runs past what its size allows, or because the text
//! it resolves to is garbage by the measures the PDF text-layer check takes, the page's number
//! and the reason are listed in [`UNRESOLVED_TEXT`] and its unresolved text is left out. An EPS
//! file with a DOS binary header has its PostScript read, and its TIFF or WMF preview handed to
//! the embedded document extractor, so that OCR reads it, where the PostScript does not give
//! all of its text.

mod dsc;
mod fonts;
mod layout;
mod machine;
mod operators;
mod scanner;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{ResourceType, Wrapping, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::pdf::quality::{self, Unmapped, GARBAGE};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Failure, Parser, Prepared};

use dsc::Comments;
use machine::{Fault, Page};

/// Each page whose text is not all resolved, as `page:reason`.
const UNRESOLVED_TEXT: &str = "dijo:unresolved-text";
/// The share of a page's glyphs that may map to no character before its text is garbage.
const UNMAPPED_SHARE: f32 = 0.1;
/// The first bytes of an EPS file with a DOS binary header.
const DOS_EPS_MAGIC: [u8; 4] = [0xC5, 0xD0, 0xD3, 0xC6];
/// The length of the DOS binary header.
const DOS_EPS_HEADER: usize = 30;

/// The preview an EPS file's DOS binary header points to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewFormat {
    Tiff,
    Wmf,
}

impl PreviewFormat {
    fn resource_name(self) -> &'static str {
        match self {
            PreviewFormat::Tiff => "preview.tif",
            PreviewFormat::Wmf => "preview.wmf",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Preview<'a> {
    format: PreviewFormat,
    bytes: &'a [u8],
}

/// A PostScript document: its program, and the preview of an EPS file with a DOS binary header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Document<'a> {
    program: &'a [u8],
    preview: Option<Preview<'a>>,
}

impl<'a> Document<'a> {
    /// The document `input` holds, or why its DOS binary header does not read.
    fn read(input: &'a [u8]) -> Result<Self, String> {
        if !input.starts_with(&DOS_EPS_MAGIC) {
            return Ok(Document {
                program: input,
                preview: None,
            });
        }
        let header = input.get(..DOS_EPS_HEADER).ok_or_else(|| {
            format!(
                "the EPS file of {} bytes ends inside its {DOS_EPS_HEADER}-byte DOS binary header",
                input.len()
            )
        })?;
        let field = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
                as usize
        };
        let section = |name: &str, at: usize| -> Result<&'a [u8], String> {
            let (offset, length) = (field(at), field(at + 4));
            offset
                .checked_add(length)
                .and_then(|end| input.get(offset..end))
                .ok_or_else(|| {
                    format!(
                        "the EPS file's {name} section at {offset} of {length} bytes runs past \
                         the end of its {} bytes",
                        input.len()
                    )
                })
        };
        let program = section("PostScript", 4)?;
        let wmf = section("WMF preview", 12)?;
        let tiff = section("TIFF preview", 20)?;
        let preview = [(PreviewFormat::Tiff, tiff), (PreviewFormat::Wmf, wmf)]
            .into_iter()
            .find(|(_, bytes)| !bytes.is_empty())
            .map(|(format, bytes)| Preview { format, bytes });
        Ok(Document { program, preview })
    }
}

/// Our parser of PostScript and EPS, which Tika does not read.
pub struct PostScriptParser;

impl Parser for PostScriptParser {
    fn name(&self) -> &'static str {
        "dijo::PostScriptParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::PostScriptParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/postscript"]
    }

    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &["application/postscript"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let document =
            Document::read(input).map_err(|reason| Decline::Fails(Error::ParseError(reason)))?;
        Ok(Box::new(move |xhtml| write(document, xhtml)))
    }
}

/// Writes the document's description and pages, then its preview where its text is not all
/// resolved.
fn write(document: Document<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    Comments::read(document.program).record(xhtml.metadata_mut());
    let mut number = 0usize;
    let mut complete = true;
    let mut resolved = false;
    machine::run(document.program, &mut |page| {
        number += 1;
        let written = write_page(number, page, xhtml)?;
        complete &= written.complete;
        resolved |= written.resolved;
        Ok(())
    })?;
    let Some(preview) = document.preview else {
        return Ok(());
    };
    if complete && resolved {
        return Ok(());
    }
    let mut metadata = TikaMetadata::new();
    metadata.set(RESOURCE_NAME, preview.format.resource_name());
    ResourceType::Inline.set(&mut metadata);
    xhtml.parse_embedded(preview.bytes, &metadata, Wrapping::PackageEntry)
}

/// What became of a page's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Written {
    /// Every string the page shows is resolved.
    complete: bool,
    /// The page has resolved text.
    resolved: bool,
}

/// Writes the page counted `number` from 1, its text unless it is garbage, and lists what of
/// it is not resolved.
fn write_page(number: usize, page: Page, xhtml: &mut Xhtml) -> Result<Written, Stop> {
    let Page { text, faults } = page;
    let mut reasons: Vec<&str> = faults.iter().map(|fault| Fault::name(*fault)).collect();
    let joined = text.paragraphs.join("\n");
    let garbage = (text.glyphs > 0)
        .then(|| {
            let glyphs = i32::try_from(text.glyphs).unwrap_or(i32::MAX);
            let unmapped = i32::try_from(text.unmapped).unwrap_or(i32::MAX);
            quality::assess(&joined, glyphs, unmapped, Unmapped::Share(UNMAPPED_SHARE))
        })
        .filter(|assessment| assessment.badness >= GARBAGE);
    if let Some(assessment) = garbage {
        reasons.push(assessment.fault);
    }
    for reason in &reasons {
        xhtml
            .metadata_mut()
            .add(UNRESOLVED_TEXT, format!("{number}:{reason}"));
    }
    xhtml.start_element_with("div", &[("class", "page")])?;
    if garbage.is_none() {
        for paragraph in &text.paragraphs {
            xhtml.element("p", paragraph.trim())?;
        }
    }
    xhtml.end_element("div")?;
    Ok(Written {
        complete: reasons.is_empty(),
        resolved: garbage.is_none() && !joined.trim().is_empty(),
    })
}

#[cfg(test)]
mod tests;
