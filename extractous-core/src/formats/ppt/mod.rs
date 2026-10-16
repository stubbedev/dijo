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

mod drawing;
mod encryption;
mod extractor;
mod headers;
mod pictures;
mod records;
mod shapes;
mod show;
mod text;

use extractor::{extract, Out};

use crate::config::ParserConfigs;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::Summaries;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Declined, Failure, Parser, Prepared, Unported};
use crate::java::LINE_SEPARATOR;
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::{Ole2, Unreadable};

const MEDIA_TYPE: &str = "application/vnd.ms-powerpoint";
/// `TikaCoreProperties.TIKA_META_EXCEPTION_WARNING`.
const WARNING: &str = "tk:exception:warn";

pub(crate) struct PptParser;

impl Parser for PptParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Fails encrypted presentations the password does not open, and defers those POI or Tika
    /// throws on, those before PowerPoint 97, those with pictures or objects Tika records
    /// failing on, and macros extracted when asked to.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let (summaries, out) = read(input, configs)?;
        Ok(Box::new(move |xhtml| {
            summaries.write(xhtml.metadata_mut());
            xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
            write(out, xhtml)
        }))
    }
}

/// The summary information and what Tika's extractor makes of the presentation in a compound
/// document.
fn read(input: &[u8], configs: &ParserConfigs) -> Result<(Summaries, Vec<Out>), Declined> {
    if configs.office.extracts_macros() {
        return Err(
            Unported("macros are extracted, which this port does not do".to_owned()).into(),
        );
    }
    let file = Ole2::open(input).map_err(|Unreadable(reason)| {
        Unported(format!("POI cannot open the compound document: {reason}"))
    })?;
    if let Err(Unreadable(reason)) = Ole2::open_stream(input) {
        return Err(Unported(format!(
            "POI reads the compound document from a stream differently: {reason}"
        ))
        .into());
    }
    let root = file.root();
    if ole_directory_type(&file, root) != MediaType::named(MEDIA_TYPE) {
        return Err(Unported(
            "the compound document is no presentation to POI's container detection".to_owned(),
        )
        .into());
    }
    let summaries = Summaries::read(&file, root)?;
    let directory = show::presentation_directory(&file, root)?;
    let show::Records {
        records,
        encryption,
    } = show::read_records(&file, directory, configs)?;
    let show = show::Show::new(&records)?;
    let pictures = pictures::read(&file, directory, &records, encryption.as_ref())?;
    let objects = pictures::objects(&records);
    let out = extract(&show, objects, &pictures, configs)?;
    Ok((summaries, out))
}

/// Writes the presentation as Tika's extractor does while it reads it. Where the length limit
/// of a string extraction is reached in a shape's object, Tika's extractor records the
/// `WriteLimitReachedException` and goes on, the text it writes after it dropped.
fn write(out: Vec<Out>, xhtml: &mut Xhtml) -> Result<(), Failure> {
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
             however available).{LINE_SEPARATOR}"
        ),
    );
}

#[cfg(test)]
mod tests;
