//! Tika's `QuattroProParser`: the records of a Quattro Pro 9 workbook's `NativeContent_MAIN`
//! stream, read by its `QPWTextExtractor` into metadata and the text of its labels, links,
//! sheet names, headers, footers and comments, in one paragraph.

use super::Stream;
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::ole2::{Ole2, Unreadable};

/// `OLE_DOCUMENT_NAME`, the stream of the workbook's records.
const DOCUMENT: &str = "NativeContent_MAIN";
/// `System.lineSeparator()`, which ends each name and label of a list.
const LINE_SEPARATOR: &str = if cfg!(windows) { "\r\n" } else { "\n" };

/// Tika's `QuattroProParser`, which takes Quattro Pro 9 workbooks.
pub(crate) struct QuattroProParser;

impl Parser for QuattroProParser {
    fn name(&self) -> &'static str {
        "extractous::QuattroProParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.wordperfect.QuattroProParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-quattro-pro; version=9"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let records = records(input).map_err(Declined::Fails)?;
        Ok(Box::new(move |xhtml| {
            xhtml.start_element("p")?;
            Records {
                xhtml: &mut *xhtml,
                stream: Stream::new(&records),
            }
            .extract()?;
            xhtml.end_element("p")?;
            Ok(())
        }))
    }
}

/// The workbook's record stream, read whole as `POIFSFileSystem` reads it from a stream, or
/// what POI or Tika throw on the compound document.
fn records(input: &[u8]) -> Result<Vec<u8>, Error> {
    let ole = Ole2::open_stream(input).map_err(|Unreadable(reason)| {
        Error::IoError(format!("POI cannot read the compound document: {reason}"))
    })?;
    if !ole.has_root_entry(DOCUMENT) {
        return Err(Error::ParseError(format!(
            "UnsupportedFormatException: Unsupported QuattroPro file format. Looking for OLE \
             entry \"{DOCUMENT}\". Found: {:?}",
            ole.top_level_names()
        )));
    }
    ole.root_document(DOCUMENT).map_err(Error::IoError)
}

/// `QPWTextExtractor.extract`'s loop over the records, with the context its extractors share.
struct Records<'x, 'h, 's> {
    xhtml: &'x mut Xhtml<'h>,
    stream: Stream<'s>,
}

impl Records<'_, '_, '_> {
    fn extract(mut self) -> Result<(), Failure> {
        while self.stream.at < self.stream.data.len() {
            let kind = self.stream.short()?;
            let length = self.stream.short()?;
            self.record(kind, length)?;
        }
        Ok(())
    }

    /// The extractor `EXTRACTORS` maps a record's type to, `IGNORE` for any other.
    fn record(&mut self, kind: u16, length: u16) -> Result<(), Failure> {
        match kind {
            0x0001 => {
                let id = self.stream.string(4)?;
                self.metadata().set("quattropro:id", id);
                let version = self.stream.short()?;
                self.metadata()
                    .set("quattropro:version", version.to_string());
                let build = self.stream.short()?;
                self.metadata().set("quattropro:build", build.to_string());
                self.stream.short()?;
                let lowest = self.stream.short()?;
                self.metadata()
                    .set("quattropro:lowest-version", lowest.to_string());
                let pages = self.stream.short()?;
                self.metadata().set("meta:page-count", pages.to_string());
                self.stream.skip(usize::from(length).saturating_sub(14))?;
            }
            0x0005 => {
                let creator = self.label()?;
                self.metadata().set("dc:creator", creator);
                let modifier = self.label()?;
                self.metadata().set("meta:last-author", modifier);
            }
            0x0403 => {
                self.skip_shorts(3)?;
                self.line()?;
            }
            0x0407 => {
                let entries = self.stream.long()?;
                self.stream.long()?;
                self.stream.long()?;
                for _ in 0..entries.max(0) {
                    self.line()?;
                }
            }
            0x0601 => {
                self.skip_shorts(3)?;
                self.stream.long()?;
                self.stream.long()?;
                self.skip_shorts(2)?;
                self.line()?;
            }
            0x0605 | 0x0606 => {
                self.skip_shorts(1)?;
                self.line()?;
            }
            0x0c02 => {
                self.skip_shorts(1)?;
                self.stream.long()?;
                self.text()?;
            }
            0x0c72 => {
                self.skip_shorts(1)?;
                self.stream.long()?;
                self.skip_shorts(1)?;
                self.text()?;
            }
            0x0c80 => {
                self.skip_shorts(1)?;
                self.stream.long()?;
                self.stream.long()?;
                self.text()?;
                self.text()?;
            }
            _ => self.stream.skip(usize::from(length))?,
        }
        Ok(())
    }

    fn metadata(&mut self) -> &mut TikaMetadata {
        self.xhtml.metadata_mut()
    }

    fn skip_shorts(&mut self, count: usize) -> Result<(), Failure> {
        for _ in 0..count {
            self.stream.short()?;
        }
        Ok(())
    }

    /// `getQstrLabel`: a length, the string's type, then one character more than the length.
    fn label(&mut self) -> Result<String, Failure> {
        let count = self.stream.short()?;
        self.stream.byte()?;
        Ok(self.stream.string(usize::from(count) + 1)?)
    }

    /// A label written as text.
    fn text(&mut self) -> Result<(), Failure> {
        let label = self.label()?;
        Ok(self.xhtml.characters(&label)?)
    }

    /// A label written as text, then a line separator.
    fn line(&mut self) -> Result<(), Failure> {
        self.text()?;
        Ok(self.xhtml.characters(LINE_SEPARATOR)?)
    }
}
