//! JPEG XL files read in place: the container's boxes walked by their headers with
//! jxl-bitstream's parser, the codestream's pieces and the first Exif and XML boxes kept as
//! slices of the file, and the codestream's header parsed from as few of its first bytes as it
//! takes.

use std::borrow::Cow;
use std::io::Read;

use jxl_bitstream::container::ContainerBoxType;
use jxl_bitstream::{Bitstream, ContainerParser, ParseEvent};
use jxl_oxide::ImageHeader;
use jxl_oxide_common::Bundle;

use crate::compress::compressor::Compressor;
use crate::compress::Limited;

/// The bytes of the codestream its header is first parsed from, doubled for as long as the
/// header runs past them.
const HEADER_PREFIX: usize = 4096;

/// A run of the file's bytes held in pieces in place, as a codestream split over `jxlp` boxes
/// is.
#[derive(Default)]
struct Pieces<'a>(Vec<&'a [u8]>);

impl<'a> Pieces<'a> {
    /// The first `length` bytes, or all of them where there are fewer, in place where the first
    /// piece holds them.
    fn prefix(&self, length: usize) -> Cow<'a, [u8]> {
        match self.0.as_slice() {
            [] => Cow::Borrowed(&[]),
            [first, ..] if first.len() >= length => Cow::Borrowed(&first[..length]),
            [only] => Cow::Borrowed(only),
            pieces => {
                let available = pieces.iter().map(|piece| piece.len()).sum::<usize>();
                let mut joined = Vec::with_capacity(length.min(available));
                for piece in pieces {
                    let wanted = length - joined.len();
                    if wanted == 0 {
                        break;
                    }
                    joined.extend_from_slice(&piece[..wanted.min(piece.len())]);
                }
                Cow::Owned(joined)
            }
        }
    }

    fn whole(&self) -> Cow<'a, [u8]> {
        self.prefix(usize::MAX)
    }
}

/// A reader of [`Pieces`], one after the other.
pub(crate) struct PiecesReader<'p, 'a> {
    current: &'a [u8],
    rest: &'p [&'a [u8]],
}

impl Read for PiecesReader<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.current.is_empty() {
            let Some((next, rest)) = self.rest.split_first() else {
                return Ok(0);
            };
            self.current = next;
            self.rest = rest;
        }
        self.current.read(buf)
    }
}

/// The metadata boxes the image's metadata is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Exif,
    Xml,
}

impl Kind {
    fn of(ty: ContainerBoxType) -> Option<Self> {
        match ty {
            ContainerBoxType::EXIF => Some(Self::Exif),
            ContainerBoxType::XML => Some(Self::Xml),
            _ => None,
        }
    }
}

/// A metadata box, its payload in place, Brotli-compressed in a `brob` box or not.
pub(crate) struct AuxBox<'a> {
    compressed: bool,
    payload: Pieces<'a>,
}

impl<'a> AuxBox<'a> {
    /// The box's content, decompressed where it is compressed.
    pub(crate) fn content(&self) -> Result<Cow<'a, [u8]>, String> {
        let payload = self.payload.whole();
        if !self.compressed {
            return Ok(payload);
        }
        Compressor::Brotli
            .decode(&payload)
            .and_then(Limited::whole)
            .map(|decoded| Cow::Owned(decoded.content.to_vec()))
            .map_err(|undecoded| format!("its Brotli compression cannot be undone: {undecoded}"))
    }
}

/// The parts of a JPEG XL file, a container of boxes or a bare codestream, that its metadata
/// and its decoding read, in place.
pub(crate) struct Container<'a> {
    codestream: Pieces<'a>,
    exif: Option<AuxBox<'a>>,
    xml: Option<AuxBox<'a>>,
}

impl<'a> Container<'a> {
    /// Walks the boxes of `input` by their headers, keeping the codestream's pieces and the
    /// first Exif and XML boxes. A box cut short by the end of the file keeps what it has.
    pub(crate) fn read(input: &'a [u8]) -> Result<Self, String> {
        let mut container = Self {
            codestream: Pieces::default(),
            exif: None,
            xml: None,
        };
        let mut current: Option<(Kind, AuxBox<'a>)> = None;
        let mut parser = ContainerParser::new();
        for event in parser.feed_bytes(input) {
            match event.map_err(|e| format!("cannot read the boxes: {e}"))? {
                ParseEvent::BitstreamKind(_) | ParseEvent::NoMoreAuxBox => {}
                ParseEvent::Codestream(piece) => container.codestream.0.push(piece),
                ParseEvent::AuxBoxStart {
                    ty,
                    brotli_compressed,
                    ..
                } => {
                    container.keep(current.take());
                    current = Kind::of(ty).map(|kind| {
                        let aux = AuxBox {
                            compressed: brotli_compressed,
                            payload: Pieces::default(),
                        };
                        (kind, aux)
                    });
                }
                ParseEvent::AuxBoxData(_, piece) => {
                    if let Some((_, aux)) = &mut current {
                        aux.payload.0.push(piece);
                    }
                }
                ParseEvent::AuxBoxEnd(_) => container.keep(current.take()),
            }
        }
        container.keep(current);
        Ok(container)
    }

    /// Keeps a metadata box that has ended, unless one of its kind came before it.
    fn keep(&mut self, ended: Option<(Kind, AuxBox<'a>)>) {
        let Some((kind, aux)) = ended else {
            return;
        };
        let slot = match kind {
            Kind::Exif => &mut self.exif,
            Kind::Xml => &mut self.xml,
        };
        slot.get_or_insert(aux);
    }

    /// The first Exif box.
    pub(crate) fn exif(&self) -> Option<&AuxBox<'a>> {
        self.exif.as_ref()
    }

    /// The first XML box, which holds the image's XMP.
    pub(crate) fn xml(&self) -> Option<&AuxBox<'a>> {
        self.xml.as_ref()
    }

    /// The codestream's header, its SizeHeader and ImageMetadata, parsed from its first bytes.
    pub(crate) fn header(&self) -> Result<ImageHeader, String> {
        let mut length = HEADER_PREFIX;
        loop {
            let prefix = self.codestream.prefix(length);
            match ImageHeader::parse(&mut Bitstream::new(&prefix), ()) {
                Ok(header) => return Ok(header),
                Err(e) if e.unexpected_eof() && prefix.len() == length => {
                    length = length.saturating_mul(2);
                }
                Err(e) => return Err(format!("cannot read the codestream's header: {e}")),
            }
        }
    }

    /// The codestream as a bare JPEG XL file, its pieces read in place.
    pub(crate) fn codestream(&self) -> PiecesReader<'_, 'a> {
        PiecesReader {
            current: &[],
            rest: &self.codestream.0,
        }
    }
}
