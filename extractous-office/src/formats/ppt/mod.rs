//! PowerPoint 97–2003 presentations (PPT) as Tika's `OfficeParser` reads them with its
//! `HSLFExtractor` on POI's HSLF: the summary information as metadata, then each slide with its
//! header, its master's text, its text, tables and grouped text, its footer, comments and notes
//! and the OLE objects its shapes show, then the pictures and the objects no shape showed, and
//! the hidden slides, animations and comment authors as metadata.
//!
//! An encrypted presentation is decrypted with the password, or failed as Tika's path fails it
//! where the password does not open it. A presentation POI or Tika throws on is left to Tika, as
//! is one before PowerPoint 97, one whose pictures or objects Tika fails on and records, and
//! macros extracted when asked to: see [`PptParser::prepare`].
//!
//! The presentation is read twice: once as far as deciding whether it is extracted, every slide
//! read and its events let go, then as it is written, a slide, a picture or an object at a time.

mod drawing;
mod encryption;
mod extractor;
mod headers;
mod pictures;
mod records;
mod shapes;
mod show;
mod text;

use std::convert::Infallible;

use extractor::{extract, Flush, Out};

use crate::config::ParserConfigs;
use crate::formats::poi::{planned, written, Halt};

use crate::formats::embedded::{record_exception, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::office;
use crate::formats::ole2::Summaries;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{unless_different, Decline, Difference, Failure, Footprint, Parser, Prepared};
use crate::ole2::Ole2;

const MEDIA_TYPE: &str = "application/vnd.ms-powerpoint";
/// `TikaCoreProperties.TIKA_META_EXCEPTION_WARNING`.
const WARNING: &str = "tk:exception:warn";

/// For the parity tests: the placeable header of each WMF picture, which gives the picture's
/// own units per inch where POI writes 72 whatever its units (#119); and an object whose data
/// does not inflate, which is recorded and not spooled, where Tika's path spools what the
/// budget reaches of it or fails on the next document's spool file (#167).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const DEPARTURES: crate::formats::parity::Departures = &[
    crate::formats::parity::Departure::PlaceableHeaders("#119"),
    crate::formats::parity::Departure::UnreadEmbedded("#167"),
];

pub struct PptParser;

impl Parser for PptParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// It reads the document stream in place, keeping an index of its records, the document
    /// record and the masters built, and builds each slide and notes from the stream as it is
    /// read; the pictures are read from their stream in place as they are written, and the
    /// embedded objects from the document stream. What grows is what POI's model holds that
    /// Tika's output needs: the document record, with its slide lists' text, the index of the
    /// records, and an encrypted presentation's streams, decrypted whole as POI decrypts them,
    /// and a document stream whose blocks are scattered, copied together.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Fails encrypted presentations the password does not open and those POI throws on, those
    /// before PowerPoint 97 among them, and defers macros extracted when asked to. Pictures or
    /// objects Tika records failing on, which the native reading leaves out, differ from Tika's.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let (file, summaries, difference) = plan(input, configs)?;
        unless_different(
            Box::new(move |xhtml| {
                summaries.write(xhtml.metadata_mut());
                xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
                written(read(&file, configs, &mut |out| write(out, xhtml))).map(|_| ())
            }),
            difference,
        )
    }
}

/// The compound document of a presentation, its summary information, and where Tika's
/// extractor writes it otherwise than the native reading, if it does: POI fails on a picture or
/// object Tika records. The presentation is read as far as writing it, its events let go.
fn plan<'a>(
    input: &'a [u8],
    configs: &ParserConfigs,
) -> Result<(Ole2<'a>, Summaries, Option<Difference>), Decline<'static>> {
    let file = office::open(input, configs, MEDIA_TYPE, "presentation")?;
    let summaries = Summaries::read(&file, file.root())?;
    let difference = planned(read(&file, configs, &mut |_| Ok::<(), Infallible>(())))?;
    Ok((file, summaries, difference))
}

/// What Tika's extractor makes of the presentation in a compound document, handed to `flush` as
/// it is read, and where it differs from the native reading, if it does.
fn read<E>(
    file: &Ole2<'_>,
    configs: &ParserConfigs,
    flush: &mut Flush<'_, E>,
) -> Result<Option<Difference>, Halt<E>> {
    let directory = show::presentation_directory(file, file.root())?;
    let (mut document, edit) = show::document_stream(file, directory)?;
    let records = show::read_records(&mut document, edit, configs)?;
    let source: &[u8] = &document;
    let kept = show::Kept::read(source, &records)?;
    let show = show::Show::new(source, &records, &kept)?;
    let mut decrypted = Vec::new();
    let pictures = pictures::read(
        file,
        directory,
        &records,
        source,
        records.encryption.as_ref(),
        &mut decrypted,
    )?;
    let objects = pictures::objects(&records);
    extract(&show, &records, &objects, &pictures, configs, flush)
}

/// Writes events of the presentation as Tika's extractor does while it reads it. Where the
/// length limit of a string extraction is reached in a shape's object, Tika's extractor records
/// the `WriteLimitReachedException` and goes on, the text it writes after it dropped.
fn write(out: Vec<Out<'_>>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    for event in out {
        match event {
            Out::Start(tag) => xhtml.start_element(tag)?,
            Out::StartClass(tag, class) => xhtml.start_element_with(tag, &[("class", class)])?,
            Out::StartWith(tag, attributes) => {
                let attributes: Vec<(&str, &str)> = attributes
                    .iter()
                    .map(|(name, value)| (*name, value.as_str()))
                    .collect();
                xhtml.start_element_with(tag, &attributes)?;
            }
            Out::End(tag) => xhtml.end_element(tag)?,
            Out::Text(text) => xhtml.characters_utf16(&text)?,
            Out::Embedded(handed, wrapping) => handed.write(xhtml, wrapping)?,
            Out::Object(handed) => match handed.write(xhtml, Wrapping::Bare) {
                Err(Failure::Stopped(Stop::LimitReached)) => record_write_limit(xhtml),
                other => other?,
            },
            Out::Add(name, value) => xhtml.metadata_mut().add(name, value),
            Out::Set(name, value) => xhtml.metadata_mut().set(name, value),
            Out::Warning(exception) => record_exception(xhtml.metadata_mut(), &exception),
        }
    }
    Ok(())
}

/// `EmbeddedDocumentUtil.recordException` of the `WriteLimitReachedException` of a string
/// extraction's limit: its first line, without the frames of Tika's Java handlers, which the
/// native path does not run.
fn record_write_limit(xhtml: &mut Xhtml) {
    let limit = xhtml
        .context()
        .write_limit()
        .expect("the limit is reached only of an extraction that has one");
    xhtml.metadata_mut().add(
        WARNING,
        format!(
            "org.apache.tika.exception.WriteLimitReachedException: Your document contained more \
             than {limit} characters, and so your requested limit has been reached. To receive \
             the full text of the document, increase your limit. (Text up to the limit is \
             however available).\n"
        ),
    );
}

#[cfg(test)]
mod tests;
