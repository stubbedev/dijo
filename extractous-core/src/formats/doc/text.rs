//! POI's `ComplexFileTable` and `TextPieceTable`: the grpprls of the CLX, the text pieces of the
//! document's text in its main stream, each Unicode or code page 1252, and the translation of
//! byte positions in the main stream to character positions in the text.

use super::bytes::{index, safely_clone};
use crate::formats::poi::{i16_at, i32_at, u8_at, Read, Thrown};
use crate::text::charset::Charset;

const MAX_GRPPRL_LENGTH: i64 = 500_000_000;
const MAX_PIECE_LENGTH: i64 = 100_000_000;
const MAX_PLEX_STRUCT: i64 = 10_485_760;
const MAX_PROPERTIES: i64 = 100_000;

/// A text piece: its character range, where its bytes start in the main stream, and its text.
#[derive(Debug, Clone)]
pub(super) struct TextPiece {
    pub(super) start: i32,
    pub(super) end: i32,
    /// The offset of its bytes in the main stream.
    pub(super) fc: i32,
    pub(super) unicode: bool,
    /// The property modifier of its piece descriptor.
    pub(super) prm: u16,
    text: Vec<u16>,
}

impl TextPiece {
    fn bytes_length(&self) -> i32 {
        (self.end - self.start) * if self.unicode { 2 } else { 1 }
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
            return Err(Thrown(format!(
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
            return Err(Thrown(format!(
                "The text piece table is corrupted, expected byte value 2 but had {kind}"
            )));
        }
        offset += 1;
        let size = i32_at(table, index(offset)?)?;
        offset += 4;
        let plex = Plex::read(table, offset, size, 8)?;
        let mut pieces = Vec::with_capacity(plex.entries.len());
        if plex.entries.is_empty() {
            return Err(Thrown::new("Index 0 out of bounds for length 0"));
        }
        for (start, end, descriptor) in &plex.entries {
            let raw_fc = i32_at(descriptor, 2)?;
            let prm = i16_at(descriptor, 6)? as u16;
            i16_at(descriptor, 0)?;
            let unicode = raw_fc & 0x4000_0000 == 0;
            let fc = if unicode {
                raw_fc
            } else {
                (raw_fc & !0x4000_0000) / 2
            };
            let length = end - start;
            let bytes = safely_clone(
                main,
                fc,
                length.wrapping_mul(if unicode { 2 } else { 1 }),
                MAX_PIECE_LENGTH,
            )?;
            let text = decode(&bytes, unicode);
            if text.len() as i64 != i64::from(length) {
                return Err(Thrown(format!(
                    "Told we're for characters {start} -> {end}, but actually covers {} characters!",
                    text.len()
                )));
            }
            pieces.push(TextPiece {
                start: *start,
                end: *end,
                fc,
                unicode,
                prm,
                text,
            });
        }
        pieces.sort_by_key(|piece| piece.end);
        let mut fc_order: Vec<usize> = (0..pieces.len()).collect();
        fc_order.sort_by_key(|&i| pieces[i].fc);
        Ok(ComplexFileTable {
            grpprls,
            pieces,
            fc_order,
        })
    }

    /// `TextPieceTable.getText`: each piece put at its start, failing where one starts past the
    /// text before it.
    pub(super) fn text(&self) -> Read<Vec<u16>> {
        let mut text: Vec<u16> = Vec::new();
        for piece in &self.pieces {
            let start = index(piece.start)?;
            if start > text.len() {
                return Err(Thrown(format!(
                    "a text piece starts at {start}, past the text of {} characters",
                    text.len()
                )));
            }
            let end = (start + piece.text.len()).min(text.len());
            text.splice(start..end, piece.text.iter().copied());
        }
        Ok(text)
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
            let multiplier = if piece.unicode { 2 } else { 1 };
            let start_cp = piece.start + (range_start - piece_start) / multiplier;
            let end_cp = start_cp + (range_end - range_start) / multiplier;
            ranges.push((start_cp, end_cp));
        }
        ranges
    }
}

/// The text of a piece's bytes, as Java decodes UTF-16LE or code page 1252.
fn decode(bytes: &[u8], unicode: bool) -> Vec<u16> {
    let charset = Charset::named(if unicode { "UTF-16LE" } else { "windows-1252" });
    charset
        .decode(bytes)
        .expect("the JDK decodes UTF-16LE and windows-1252")
        .encode_utf16()
        .collect()
}
