//! Compiled HTML Help (CHM) files, as Tika's `ChmParser` reads them with its `ChmExtractor`: the
//! entries of the directory listing whose names end in `.html` or `.htm`, in the listing's
//! order, each extracted ([`directory`] reads the structures that say where) and parsed by the
//! HTML parser into the document's body. The LZX-compressed content is decompressed with the
//! `lzxd` crate, frame by frame from the reset point before the entry, as Tika's decoder
//! decodes its blocks.

mod directory;

use std::cell::RefCell;

use directory::{
    copy_of_range, reset_data_index, ControlData, Entry, EntryType, ItsfHeader, ItspHeader,
    Listing, ResetTable,
};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::handlers::parse_inline;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{html, Declined, Failure, Parser, Prepared};

const MEDIA_TYPE: &str = "application/vnd.ms-htmlhelp";

/// What Tika's CHM classes throw, which decides whether the directory listing stops quietly.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Thrown {
    /// A `ChmParsingException`, which ends the directory listing with what it read.
    Parsing(String),
    /// Any other `TikaException`.
    Tika(String),
    /// A Java runtime exception, such as an index out of bounds.
    Runtime(String),
}

impl Thrown {
    fn reason(&self) -> &str {
        match self {
            Thrown::Parsing(reason) | Thrown::Tika(reason) | Thrown::Runtime(reason) => reason,
        }
    }
}

/// Tika's `ChmParser`.
pub(crate) struct ChmParser;

impl Parser for ChmParser {
    fn name(&self) -> &'static str {
        "extractous::ChmParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.chm.ChmParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE, "application/chm", "application/x-chm"]
    }

    /// Extracts and prepares every page before writing any, so a page the HTML parser leaves to
    /// Tika defers the file; a page that cannot be extracted fails the parse after the pages
    /// before it, as Tika's extraction of it throws there. Each page is prepared again as it is
    /// written: holding every page's document at once would hold a help file's whole content
    /// parsed, where writing holds one page at a time.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let extractor = Extractor::new(input)
            .map_err(|thrown| Declined::Fails(Error::ParseError(thrown.reason().to_owned())))?;
        for page in extractor.pages() {
            let Ok((name, data)) = page else {
                break;
            };
            drop(
                html::prepare(&data, &TikaMetadata::new()).map_err(|reason| {
                    Declined::Deferred(format!("the page {name} defers: {reason}"))
                })?,
            );
        }
        Ok(Box::new(move |xhtml| {
            xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
            for page in extractor.pages() {
                let (name, data) = page.map_err(|thrown| {
                    Failure::Malformed(format!("TikaException: {}", thrown.reason()))
                })?;
                let prepared = html::prepare(&data, &TikaMetadata::new()).map_err(|reason| {
                    Failure::Malformed(format!(
                        "the page {name} defers, which it did not as it was prepared: {reason}"
                    ))
                })?;
                parse_inline(prepared, xhtml)?;
            }
            Ok(())
        }))
    }
}

/// `ChmExtractor`: the file's structures, and the frames of the content decoded so far.
struct Extractor<'a> {
    data: &'a [u8],
    itsf: ItsfHeader,
    listing: Listing,
    control: ControlData,
    reset_table: ResetTable,
    /// The offset of the LZX-compressed content in the file, and its length.
    content: (i64, i64),
    frames: RefCell<Frames>,
}

/// The decoded frames of the content, by block number, and the decoder of the reset interval
/// decoded last, with the block it decodes next.
struct Frames {
    decoded: Vec<Option<Vec<u8>>>,
    decoder: Option<(lzxd::Lzxd, usize)>,
}

impl<'a> Extractor<'a> {
    /// `new ChmExtractor(stream)`.
    fn new(data: &'a [u8]) -> Result<Self, Thrown> {
        let itsf = ItsfHeader::read(data)?;
        let itsp = ItspHeader::read(data, &itsf)?;
        let listing = Listing::read(data, &itsf, &itsp)?;
        let entry = |index: Option<usize>| -> Result<&Entry, Thrown> {
            index
                .map(|index| &listing.entries[index])
                .ok_or_else(|| Thrown::Runtime("IndexOutOfBoundsException: Index -1".to_owned()))
        };
        let reset_data = reset_data_index(data);
        let control_chunk = if reset_data > 0 {
            let length = entry(listing.control_data)?.length;
            Some(copy_of_range(
                data,
                reset_data as i32,
                (reset_data as i32).wrapping_add(length),
            )?)
        } else {
            None
        };
        let control = ControlData::read(control_chunk)?;
        let reset_entry = entry(listing.reset_table)?;
        let start = (itsf.data_offset as i32).wrapping_add(reset_entry.offset);
        if start as i64 >= data.len() as i64 {
            return Err(Thrown::Parsing(
                "cannot parse chm file index > data.length".to_owned(),
            ));
        }
        let reset_table = ResetTable::read(copy_of_range(
            data,
            start,
            start.wrapping_add(reset_entry.length),
        )?)?;
        let content = listing
            .storage("Content")
            .ok_or_else(|| Thrown::Runtime("IndexOutOfBoundsException: Index -1".to_owned()))?;
        let content = (
            i64::from(content.offset) + itsf.data_offset,
            i64::from(content.length),
        );
        let blocks = reset_table.addresses.len();
        Ok(Self {
            data,
            itsf,
            listing,
            control,
            reset_table,
            content,
            frames: RefCell::new(Frames {
                decoded: vec![None; blocks],
                decoder: None,
            }),
        })
    }

    /// The pages of the directory listing, the entries whose names end in `.html` or `.htm`,
    /// each by its name and data, in the listing's order, up to the first that cannot be
    /// extracted, whose failure ends them.
    fn pages(&self) -> impl Iterator<Item = Result<(&str, Vec<u8>), Thrown>> + '_ {
        let mut failed = false;
        self.listing
            .entries
            .iter()
            .filter(|entry| entry.name.ends_with(".html") || entry.name.ends_with(".htm"))
            .map_while(move |entry| {
                if failed {
                    return None;
                }
                let page = self.extract(entry).map(|data| (entry.name.as_str(), data));
                failed = page.is_err();
                Some(page)
            })
    }

    /// `extractChmEntry`: the data of an entry, stored or decompressed, which must come to the
    /// entry's length; none for a system entry or an empty stored one.
    fn extract(&self, entry: &Entry) -> Result<Vec<u8>, Thrown> {
        if entry.is_skipped() {
            return Ok(Vec::new());
        }
        match entry.kind {
            EntryType::Uncompressed if entry.length > 0 => {
                let offset = (self.itsf.data_offset + i64::from(entry.offset)) as i32;
                Ok(copy_of_range(self.data, offset, offset.wrapping_add(entry.length))?.to_vec())
            }
            EntryType::Uncompressed => Ok(Vec::new()),
            EntryType::Compressed => self.decompress(entry),
        }
    }

    fn decompress(&self, entry: &Entry) -> Result<Vec<u8>, Thrown> {
        let block_length = self.reset_table.block_length as i32;
        if block_length <= 0 {
            return Err(Thrown::Parsing("Please check you parameters".to_owned()));
        }
        let (content_offset, content_length) = self.content;
        let mut out = Vec::new();
        if content_length < i64::from(i32::MAX) && content_offset < i64::from(i32::MAX) {
            let start = entry.offset;
            let end = entry.offset.wrapping_add(entry.length);
            let (first, last) = (start / block_length, end / block_length);
            let (first_offset, last_offset) = (start % block_length, end % block_length);
            if first < 0 || last < first {
                return Err(Thrown::Tika(
                    "blockNumber should be positive number".to_owned(),
                ));
            }
            for block in first..=last {
                let frame = self.frame(block as usize)?;
                let from = if block == first { first_offset } else { 0 } as usize;
                let to = if block == last {
                    last_offset as usize
                } else {
                    frame.len()
                };
                let piece = frame.get(from..to).ok_or_else(|| {
                    Thrown::Runtime(format!(
                        "the block {block} decodes to {} bytes, not {to}",
                        frame.len()
                    ))
                })?;
                out.extend_from_slice(piece);
            }
        }
        if out.len() != entry.length as usize {
            return Err(Thrown::Tika(
                "CHM file extract error: extracted Length is wrong.".to_owned(),
            ));
        }
        Ok(out)
    }

    /// The decoded frame of `block`, decoded from the reset point before it as needed.
    fn frame(&self, block: usize) -> Result<Vec<u8>, Thrown> {
        let mut frames = self.frames.borrow_mut();
        if let Some(Some(frame)) = frames.decoded.get(block) {
            return Ok(frame.clone());
        }
        if block >= frames.decoded.len() {
            return Err(Thrown::Runtime(format!(
                "ArrayIndexOutOfBoundsException: the reset table has no block {block}"
            )));
        }
        let interval = self.control.reset_interval as usize;
        let reset = block - block % interval;
        let resumes = matches!(&frames.decoder, Some((_, next)) if *next > reset && *next <= block);
        if !resumes {
            frames.decoder = Some((lzxd::Lzxd::new(self.window()?), reset));
        }
        loop {
            let (decoder, next) = frames.decoder.as_mut().expect("a decoder was set");
            let current = *next;
            let segment = self.segment(current)?;
            let length = self.frame_length(current);
            let frame = decoder
                .decompress_next(segment, length)
                .map_err(|error| Thrown::Tika(format!("LZX block {current}: {error}")))?
                .to_vec();
            *next += 1;
            if current == block {
                frames.decoded[current] = Some(frame.clone());
                return Ok(frame);
            }
            frames.decoded[current] = Some(frame);
        }
    }

    /// The uncompressed length of the frame of `block`: a whole block, but for the last.
    fn frame_length(&self, block: usize) -> usize {
        let block_length = self.reset_table.block_length;
        let remaining = self.reset_table.uncompressed_length - block as i64 * block_length;
        remaining.clamp(0, block_length) as usize
    }

    /// `ChmCommons.getChmBlockSegment`: the compressed data of `block`, up to the next block's
    /// address or the end of the content.
    fn segment(&self, block: usize) -> Result<&'a [u8], Thrown> {
        let addresses = &self.reset_table.addresses;
        if addresses.len() <= 1 {
            return Err(Thrown::Tika(
                "resetTable.getBlockAddress().length should be greater than zero".to_owned(),
            ));
        }
        let (offset, content_length) = (self.content.0 as i32, self.content.1 as i32);
        if offset < 0 || content_length < 0 {
            return Err(Thrown::Tika(
                "lzxcBlockOffset should be positive number".to_owned(),
            ));
        }
        let address = addresses[block];
        let length = match addresses.get(block + 1) {
            Some(next) => next - address,
            None => i64::from(content_length) - address,
        };
        let from = (i64::from(offset) + address) as i32;
        let to = (i64::from(offset) + address + length) as i32;
        copy_of_range(self.data, from, to)
    }

    /// The LZX window the control data names.
    fn window(&self) -> Result<lzxd::WindowSize, Thrown> {
        use lzxd::WindowSize;
        Ok(match self.control.window_size {
            0x8000 => WindowSize::KB32,
            0x1_0000 => WindowSize::KB64,
            0x2_0000 => WindowSize::KB128,
            0x4_0000 => WindowSize::KB256,
            0x8_0000 => WindowSize::KB512,
            0x10_0000 => WindowSize::MB1,
            0x20_0000 => WindowSize::MB2,
            other => {
                return Err(Thrown::Parsing(format!(
                    "an LZX window of {other} bytes, which is no power of two from 2^15 to 2^21"
                )))
            }
        })
    }
}

#[cfg(test)]
mod tests;
