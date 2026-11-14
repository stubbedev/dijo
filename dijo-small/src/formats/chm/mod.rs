//! Compiled HTML Help (CHM) files, as Tika's `ChmParser` reads them with its `ChmExtractor`: the
//! entries of the directory listing whose names end in `.html` or `.htm`, in the listing's
//! order, each extracted ([`directory`] reads the structures that say where) and parsed by the
//! HTML parser into the document's body. The LZX-compressed content is decompressed with
//! [`Lzx`], frame by frame from the reset point before the entry, as Tika's decoder decodes its
//! blocks.

mod directory;

use std::cell::RefCell;

use directory::{
    copy_of_range, reset_data_index, ControlData, Entry, EntryType, ItsfHeader, ItspHeader,
    Listing, ResetTable,
};

use crate::compress::lzx::{Lzx, WindowSize};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::handlers::parse_inline;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{
    decision, html, reading, unless_different, Decline, Difference, Failure, Footprint, Parser,
    Prepared,
};
use crate::input::{Pages, Paging};

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
pub struct ChmParser;

impl Parser for ChmParser {
    fn name(&self) -> &'static str {
        "dijo::ChmParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.chm.ChmParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE, "application/chm", "application/x-chm"]
    }

    /// A page at a time, and the frames of one reset interval of the content.
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

    /// Extracts and prepares every page before writing any, so a page the HTML parser does not
    /// read leaves the file unread; a page that cannot be extracted fails the parse after the pages
    /// before it, as Tika's extraction of it throws there. Each page is prepared again as it is
    /// written: holding every page's document at once would hold a help file's whole content
    /// parsed, where writing holds one page at a time.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let extractor = Extractor::new(input, pages)
            .map_err(|thrown| Decline::Fails(Error::ParseError(thrown.reason().to_owned())))?;
        let mut difference = None;
        let mut data = Vec::new();
        for entry in extractor.pages() {
            if extractor.extract(entry, &mut data).is_err() {
                break;
            }
            let name = &entry.name;
            let read_otherwise = decision(html::prepare(&data, &TikaMetadata::new()))
                .map_err(|reason| {
                    Decline::Unread(format!("the page {name} is not read: {reason}"))
                })?
                .map(|Difference(reason)| Difference(format!("the page {name}: {reason}")));
            difference = difference.or(read_otherwise);
        }
        unless_different(
            Box::new(move |xhtml| {
                xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
                for entry in extractor.pages() {
                    extractor.extract(entry, &mut data).map_err(|thrown| {
                        Failure::Malformed(format!("TikaException: {}", thrown.reason()))
                    })?;
                    let name = &entry.name;
                    let prepared =
                        reading(html::prepare(&data, &TikaMetadata::new())).map_err(|reason| {
                            Failure::Malformed(format!(
                                "the page {name} is not read, which it was as it was prepared: \
                                 {reason}"
                            ))
                        })?;
                    parse_inline(prepared, xhtml)?;
                }
                Ok(())
            }),
            difference,
        )
    }
}

/// `ChmExtractor`: the file's structures, and the frames of the content decoded last.
struct Extractor<'a> {
    data: &'a [u8],
    /// The pages of `data`, released as the content is decoded.
    paging: Paging<'a>,
    itsf: ItsfHeader,
    listing: Listing,
    control: ControlData,
    reset_table: ResetTable,
    /// The offset of the LZX-compressed content in the file, and its length.
    content: (i64, i64),
    frames: RefCell<Frames>,
}

/// The decoder of the reset interval decoded last, and the frames it decoded, from the block of
/// its reset point on, one after another in one buffer: a page in another interval is decoded
/// again from that interval's reset point, by the same decoder reset into the same buffer, so
/// that only one interval's frames are held, however long the content.
struct Frames {
    decoder: Option<Lzx>,
    reset: usize,
    /// Where each frame decoded starts in `decoded`.
    starts: Vec<usize>,
    decoded: Vec<u8>,
}

impl<'a> Extractor<'a> {
    /// `new ChmExtractor(stream)`.
    fn new(data: &'a [u8], pages: Pages<'a>) -> Result<Self, Thrown> {
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
        Ok(Self {
            data,
            paging: Paging::new(pages),
            itsf,
            listing,
            control,
            reset_table,
            content,
            frames: RefCell::new(Frames {
                decoder: None,
                reset: 0,
                starts: Vec::new(),
                decoded: Vec::new(),
            }),
        })
    }

    /// The pages of the directory listing, the entries whose names end in `.html` or `.htm`, in
    /// the listing's order.
    fn pages(&self) -> impl Iterator<Item = &Entry> + '_ {
        self.listing
            .entries
            .iter()
            .filter(|entry| entry.name.ends_with(".html") || entry.name.ends_with(".htm"))
    }

    /// `extractChmEntry`: the data of an entry, stored or decompressed, which must come to the
    /// entry's length, in place of what `data` held; none for a system entry or an empty stored
    /// one.
    fn extract(&self, entry: &Entry, data: &mut Vec<u8>) -> Result<(), Thrown> {
        data.clear();
        if entry.is_skipped() {
            return Ok(());
        }
        match entry.kind {
            EntryType::Uncompressed if entry.length > 0 => {
                let offset = (self.itsf.data_offset + i64::from(entry.offset)) as i32;
                data.extend_from_slice(copy_of_range(
                    self.data,
                    offset,
                    offset.wrapping_add(entry.length),
                )?);
                Ok(())
            }
            EntryType::Uncompressed => Ok(()),
            EntryType::Compressed => self.decompress(entry, data),
        }
    }

    fn decompress(&self, entry: &Entry, data: &mut Vec<u8>) -> Result<(), Thrown> {
        let block_length = self.reset_table.block_length as i32;
        if block_length <= 0 {
            return Err(Thrown::Parsing("Please check you parameters".to_owned()));
        }
        let (content_offset, content_length) = self.content;
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
                let from = if block == first { first_offset } else { 0 } as usize;
                let to = (block == last).then_some(last_offset as usize);
                self.frame(block as usize, from, to, data)?;
            }
        }
        if data.len() != entry.length as usize {
            return Err(Thrown::Tika(
                "CHM file extract error: extracted Length is wrong.".to_owned(),
            ));
        }
        Ok(())
    }

    /// Appends to `data` the bytes `from` up to `to`, or else to its end, of the decoded frame
    /// of `block`, decoding the frames of its reset interval up to it as needed.
    fn frame(
        &self,
        block: usize,
        from: usize,
        to: Option<usize>,
        data: &mut Vec<u8>,
    ) -> Result<(), Thrown> {
        if block >= self.reset_table.addresses.len() {
            return Err(Thrown::Runtime(format!(
                "ArrayIndexOutOfBoundsException: the reset table has no block {block}"
            )));
        }
        let interval = self.control.reset_interval as usize;
        let reset = block - block % interval;
        let mut frames = self.frames.borrow_mut();
        let Frames {
            decoder,
            reset: decoded_from,
            starts,
            decoded,
        } = &mut *frames;
        if decoder.is_none() || *decoded_from != reset {
            if let Some(decoder) = decoder.as_mut() {
                decoder.reset();
            } else {
                *decoder = Some(Lzx::new(self.window()?));
            }
            *decoded_from = reset;
            starts.clear();
            decoded.clear();
        }
        let decoder = decoder.as_mut().expect("a decoder was set");
        while reset + starts.len() <= block {
            let current = reset + starts.len();
            let segment = self.segment(current)?;
            let length = self.frame_length(current);
            let frame = decoder
                .decode_frame(segment, length)
                .map_err(|error| Thrown::Tika(format!("LZX block {current}: {error}")))?;
            self.paging.read(segment.len());
            starts.push(decoded.len());
            decoded.extend_from_slice(frame);
        }
        let index = block - reset;
        let frame_start = starts[index];
        let frame_end = starts.get(index + 1).copied().unwrap_or(decoded.len());
        let frame = &decoded[frame_start..frame_end];
        let to = to.unwrap_or(frame.len());
        let piece = frame.get(from..to).ok_or_else(|| {
            Thrown::Runtime(format!(
                "the block {block} decodes to {} bytes, not {to}",
                frame.len()
            ))
        })?;
        data.extend_from_slice(piece);
        Ok(())
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
    fn window(&self) -> Result<WindowSize, Thrown> {
        let bytes = self.control.window_size;
        WindowSize::from_bytes(bytes).ok_or_else(|| {
            Thrown::Parsing(format!(
                "an LZX window of {bytes} bytes, which is no power of two from 2^15 to 2^21"
            ))
        })
    }
}

#[cfg(test)]
mod tests;
