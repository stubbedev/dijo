//! POI's `ComplexFileTable` and `TextPieceTable`: the grpprls of the CLX, the text pieces of the
//! document's text in its main stream, each Unicode or code page 1252, and the translation of
//! byte positions in the main stream to character positions in the text.
//!
//! The text is not held: [`Text`] keeps which piece gives each of its characters, as
//! `TextPieceTable.getText` puts the pieces together, and reads a range of it from the main
//! stream, where it lies, when it is needed.

use std::ops::Range;

use super::bytes::{index, safely_clone};
use crate::formats::poi::{i16_at, i32_at, safely_borrow, u8_at, Read, Thrown};
use crate::text::charset::Charset;

const MAX_GRPPRL_LENGTH: i64 = 500_000_000;
/// `TextPieceTable.getMaxRecordLength`.
pub(super) const MAX_PIECE_LENGTH: i64 = 100_000_000;
const MAX_PLEX_STRUCT: i64 = 10_485_760;
const MAX_PROPERTIES: i64 = 100_000;

/// How a piece's bytes are its characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Encoding {
    /// UTF-16LE, as Java decodes it.
    Unicode,
    /// Code page 1252, as Java decodes it.
    Windows1252,
    /// A byte each, undecoded, as the text of Word 6 and 95 is kept until a paragraph is cut
    /// out of it.
    Bytes,
}

impl Encoding {
    /// How many bytes a character takes.
    fn width(self) -> i32 {
        match self {
            Encoding::Unicode => 2,
            Encoding::Windows1252 | Encoding::Bytes => 1,
        }
    }

    /// How many characters Java decodes of `length` bytes.
    fn characters(self, length: usize) -> usize {
        match self {
            Encoding::Unicode => length.div_ceil(2),
            Encoding::Windows1252 | Encoding::Bytes => length,
        }
    }

    /// The characters of `bytes`, as Java decodes them.
    fn decode(self, bytes: &[u8]) -> Vec<u16> {
        let charset = match self {
            Encoding::Unicode => Charset::named("UTF-16LE"),
            Encoding::Windows1252 => Charset::named("windows-1252"),
            Encoding::Bytes => return bytes.iter().map(|&byte| u16::from(byte)).collect(),
        };
        charset
            .decode(bytes)
            .expect("the JDK decodes UTF-16LE and windows-1252")
            .encode_utf16()
            .collect()
    }
}

/// A text piece: its character range, where its bytes start in the main stream, and how they
/// are its characters.
#[derive(Debug, Clone)]
pub(super) struct TextPiece {
    pub(super) start: i32,
    pub(super) end: i32,
    /// The offset of its bytes in the main stream.
    pub(super) fc: i32,
    pub(super) encoding: Encoding,
    /// The property modifier of its piece descriptor.
    pub(super) prm: u16,
}

impl TextPiece {
    /// `new TextPiece(start, end, text, descriptor)` of the bytes at `fc` of `main`: failing
    /// where they are not as many characters as the range.
    pub(super) fn read(
        main: &[u8],
        start: i32,
        end: i32,
        fc: i32,
        encoding: Encoding,
        prm: u16,
        max: i64,
    ) -> Read<TextPiece> {
        let length = end.wrapping_sub(start).wrapping_mul(encoding.width());
        let bytes = safely_borrow(main, i64::from(fc), i64::from(length), max)?;
        let characters = encoding.characters(bytes.len());
        if characters as i64 != i64::from(end) - i64::from(start) {
            return Err(Thrown::new(format!(
                "Told we're for characters {start} -> {end}, but actually covers {characters} \
                 characters!"
            )));
        }
        Ok(TextPiece {
            start,
            end,
            fc,
            encoding,
            prm,
        })
    }

    /// How many characters the piece holds.
    fn len(&self) -> usize {
        (self.end - self.start) as usize
    }

    /// The piece's text, read from `main`.
    pub(super) fn text(&self, main: &[u8]) -> Vec<u16> {
        self.characters(main, 0..self.len())
    }

    /// The piece's characters `range`, counted from its start, read from `main`: a surrogate
    /// is read with its neighbours in the piece, as Java decodes the piece whole.
    fn characters(&self, main: &[u8], range: Range<usize>) -> Vec<u16> {
        let from = range.start.saturating_sub(1);
        let to = (range.end + 1).min(self.len());
        let decoded = self.encoding.decode(self.bytes(main, from..to));
        decoded[range.start - from..range.end - from].to_vec()
    }

    /// The bytes of the piece's characters `range`, counted from its start, in `main`, as far
    /// as the stream goes: the last character of a Unicode piece may lack its second byte.
    fn bytes<'m>(&self, main: &'m [u8], range: Range<usize>) -> &'m [u8] {
        let width = self.encoding.width() as usize;
        let at = self.fc as usize;
        let end = (at + range.end * width).min(main.len());
        &main[at + range.start * width..end]
    }

    fn bytes_length(&self) -> i32 {
        (self.end - self.start) * self.encoding.width()
    }

    /// `PropertyModifier.isComplex`.
    pub(super) fn prm_is_complex(&self) -> bool {
        self.prm & 1 != 0
    }

    /// `PropertyModifier.getIgrpprl`.
    pub(super) fn prm_igrpprl(&self) -> i32 {
        i32::from(((self.prm & 0xFFFE) >> 1) as i16)
    }
}

/// `PlexOfCps`: entries of a character range and a structure of `struct_size` bytes.
pub(super) struct Plex {
    pub(super) entries: Vec<(i32, i32, Vec<u8>)>,
}

impl Plex {
    /// `new PlexOfCps(buf, start, cb, cbStruct)`, with `GenericPropertyNode`'s clamping of
    /// starts below zero and ends before their starts.
    pub(super) fn read(buf: &[u8], start: i32, size: i32, struct_size: i32) -> Read<Plex> {
        let count = size.wrapping_sub(4) / (4 + struct_size);
        if count < 0 || i64::from(count) > MAX_PROPERTIES {
            return Err(Thrown::new(format!(
                "Tried to allocate an array of length {count}"
            )));
        }
        let mut entries = Vec::with_capacity(count as usize);
        for x in 0..count {
            let first = i32_at(buf, index(start.wrapping_add(x * 4))?)?;
            let last = i32_at(buf, index(start.wrapping_add((x + 1) * 4))?)?;
            let structure = safely_clone(
                buf,
                start
                    .wrapping_add(4 * (count + 1))
                    .wrapping_add(struct_size * x),
                struct_size,
                MAX_PLEX_STRUCT,
            )?;
            let first = first.max(0);
            entries.push((first, last.max(first), structure));
        }
        Ok(Plex { entries })
    }
}

/// A piece descriptor of the CLX, as it is stored: its character range, its file position with
/// its flags, and its property modifier.
pub(super) struct Descriptor {
    pub(super) start: i32,
    pub(super) end: i32,
    pub(super) fc: i32,
    pub(super) prm: u16,
}

/// `ComplexFileTable`'s reading of the CLX at `offset` of `table`: the grpprls and the piece
/// descriptors.
pub(super) fn read_clx(table: &[u8], offset: i32) -> Read<(Vec<Vec<u8>>, Vec<Descriptor>)> {
    let mut offset = offset;
    let mut grpprls = Vec::new();
    while u8_at(table, index(offset)?)? == 1 {
        offset += 1;
        let size = i32::from(i16_at(table, index(offset)?)?);
        offset += 2;
        grpprls.push(safely_clone(table, offset, size, MAX_GRPPRL_LENGTH)?);
        offset = offset.wrapping_add(size);
    }
    let kind = u8_at(table, index(offset)?)?;
    if kind != 2 {
        return Err(Thrown::new(format!(
            "The text piece table is corrupted, expected byte value 2 but had {kind}"
        )));
    }
    offset += 1;
    let size = i32_at(table, index(offset)?)?;
    offset += 4;
    let plex = Plex::read(table, offset, size, 8)?;
    if plex.entries.is_empty() {
        return Err(Thrown::new("Index 0 out of bounds for length 0"));
    }
    let mut descriptors = Vec::with_capacity(plex.entries.len());
    for (start, end, descriptor) in &plex.entries {
        let fc = i32_at(descriptor, 2)?;
        let prm = i16_at(descriptor, 6)? as u16;
        i16_at(descriptor, 0)?;
        descriptors.push(Descriptor {
            start: *start,
            end: *end,
            fc,
            prm,
        });
    }
    Ok((grpprls, descriptors))
}

/// The CLX: the grpprls its piece descriptors may refer to, and the text pieces.
pub(super) struct ComplexFileTable {
    pub(super) grpprls: Vec<Vec<u8>>,
    /// The pieces in the order of their ends, as `Collections.sort` leaves them.
    pub(super) pieces: Vec<TextPiece>,
    /// The indexes of the pieces in the order of their offsets in the main stream.
    fc_order: Vec<usize>,
}

impl ComplexFileTable {
    /// `new ComplexFileTable(mainStream, tableStream, fcClx, 0)`.
    pub(super) fn read(main: &[u8], table: &[u8], offset: i32) -> Read<ComplexFileTable> {
        let (grpprls, descriptors) = read_clx(table, offset)?;
        let mut pieces = Vec::with_capacity(descriptors.len());
        for descriptor in descriptors {
            let unicode = descriptor.fc & 0x4000_0000 == 0;
            let (fc, encoding) = if unicode {
                (descriptor.fc, Encoding::Unicode)
            } else {
                ((descriptor.fc & !0x4000_0000) / 2, Encoding::Windows1252)
            };
            pieces.push(TextPiece::read(
                main,
                descriptor.start,
                descriptor.end,
                fc,
                encoding,
                descriptor.prm,
                MAX_PIECE_LENGTH,
            )?);
        }
        Ok(ComplexFileTable::new(grpprls, pieces))
    }

    /// The table of `pieces`, ordered as `TextPieceTable` orders them: by their ends, and by
    /// their offsets in the main stream.
    pub(super) fn new(grpprls: Vec<Vec<u8>>, mut pieces: Vec<TextPiece>) -> ComplexFileTable {
        pieces.sort_by_key(|piece| piece.end);
        let mut fc_order: Vec<usize> = (0..pieces.len()).collect();
        fc_order.sort_by_key(|&i| pieces[i].fc);
        ComplexFileTable {
            grpprls,
            pieces,
            fc_order,
        }
    }

    /// `getCharIndexRanges`: the character ranges the bytes of the main stream between the
    /// positions map to, piece by piece in the order of their bytes.
    pub(super) fn char_ranges(&self, start_bytes: i32, end_bytes: i32) -> Vec<(i32, i32)> {
        let mut ranges = Vec::new();
        for &i in &self.fc_order {
            let piece = &self.pieces[i];
            let piece_start = piece.fc;
            if end_bytes <= piece_start {
                break;
            }
            let piece_end = piece.fc + piece.bytes_length();
            if start_bytes > piece_end {
                continue;
            }
            let range_start = piece_start.max(start_bytes);
            let range_end = piece_end.min(end_bytes);
            if range_start > range_end {
                continue;
            }
            let multiplier = piece.encoding.width();
            let start_cp = piece.start + (range_start - piece_start) / multiplier;
            let end_cp = start_cp + (range_end - range_start) / multiplier;
            ranges.push((start_cp, end_cp));
        }
        ranges
    }
}

/// Characters of the text one piece gives: those from `start` to `end`, the piece's from its
/// own start.
#[derive(Debug, Clone, Copy)]
struct Segment {
    start: usize,
    end: usize,
    piece: usize,
}

/// The text as `TextPieceTable.getText` makes it, each piece put at its start over what the
/// pieces before it put there, kept as which piece gives each character.
#[derive(Debug, Clone)]
pub(super) struct Text {
    pieces: Vec<TextPiece>,
    /// The segments in the order of the text, which they cover.
    segments: Vec<Segment>,
    length: usize,
}

impl Text {
    /// `getText`'s placing of the pieces, failing where one starts past the text before it.
    pub(super) fn of(clx: &ComplexFileTable) -> Read<Text> {
        let mut segments: Vec<Segment> = Vec::new();
        let mut length = 0usize;
        for (piece_index, piece) in clx.pieces.iter().enumerate() {
            let start = index(piece.start)?;
            if start > length {
                return Err(Thrown::new(format!(
                    "a text piece starts at {start}, past the text of {length} characters"
                )));
            }
            let end = start + (piece.end - piece.start) as usize;
            let mut placed = Vec::with_capacity(segments.len() + 2);
            for segment in segments {
                if segment.end <= start || segment.start >= end {
                    placed.push(segment);
                    continue;
                }
                if segment.start < start {
                    placed.push(Segment {
                        end: start,
                        ..segment
                    });
                }
                if segment.end > end {
                    placed.push(Segment {
                        start: end,
                        ..segment
                    });
                }
            }
            if end > start {
                placed.push(Segment {
                    start,
                    end,
                    piece: piece_index,
                });
            }
            placed.sort_by_key(|segment| segment.start);
            segments = placed;
            length = length.max(end);
        }
        Ok(Text {
            pieces: clx.pieces.clone(),
            segments,
            length,
        })
    }

    /// How many characters the text holds.
    pub(super) fn len(&self) -> usize {
        self.length
    }

    /// The segments of the characters `range`, each with the part of it the range covers.
    fn covering(&self, range: Range<usize>) -> impl Iterator<Item = (&TextPiece, Range<usize>)> {
        let first = self
            .segments
            .partition_point(|segment| segment.end <= range.start);
        self.segments[first..]
            .iter()
            .take_while(move |segment| segment.start < range.end)
            .map(move |segment| {
                let piece = &self.pieces[segment.piece];
                let from = range.start.max(segment.start);
                let to = range.end.min(segment.end);
                let base = piece.start as usize;
                (piece, from - base..to - base)
            })
    }

    /// The characters `range` of the text, read from `main`, as `getText` decodes them;
    /// `None` where the range runs past the text.
    pub(super) fn read(&self, main: &[u8], range: Range<usize>) -> Option<Vec<u16>> {
        if range.start > range.end || range.end > self.length {
            return None;
        }
        let mut text = Vec::with_capacity(range.len());
        for (piece, within) in self.covering(range) {
            text.extend(piece.characters(main, within));
        }
        Some(text)
    }

    /// The positions of the characters of `units` in the text, in order, read from `main`
    /// without decoding: each of them is an ASCII character Java decodes as it is.
    pub(super) fn positions_of(&self, main: &[u8], units: &[u8]) -> Vec<i32> {
        let mut positions = Vec::new();
        for segment in &self.segments {
            let piece = &self.pieces[segment.piece];
            let base = piece.start as usize;
            let bytes = piece.bytes(main, segment.start - base..segment.end - base);
            let width = piece.encoding.width() as usize;
            for (at, character) in bytes.chunks_exact(width).enumerate() {
                let is_unit = match character {
                    [low, 0] | [low] => units.contains(low),
                    _ => false,
                };
                if is_unit {
                    positions.push((segment.start + at) as i32);
                }
            }
        }
        positions
    }
}
