//! POI's `PicturesTable` and `Picture`, and Tika's `PicturesSource` over them: the pictures
//! character runs place in the data stream and those the drawing group stores, their types and
//! content, and the order Tika numbers and claims them in.

use std::collections::{HashMap, HashSet};

use super::document::Document;
use super::range::CharacterRun;
use crate::formats::escher::{self, kind, Blip, Content, Escher};
use crate::formats::poi::{i16_at, i32_at, safely_clone, u8_at, Read, Thrown};

const PICF_SIZE: usize = 68;
/// `PICF.mm` of a picture whose name follows its header.
const MM_SHAPE_FILE: i16 = 0x0066;
const BSE: u16 = 0xF007;
/// `HWPFDocument.getMaxRecordLength`.
const MAX_RECORD_LENGTH: i64 = 100_000;
/// The signatures of a compressed picture at offset 32 of its content.
const COMPRESSED: [[u8; 3]; 2] = [[0xFE, 0x78, 0xDA], [0xFE, 0x78, 0x9C]];

/// POI's `PictureType`: the media type and extension of a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PictureType {
    Bmp,
    Emf,
    Jpeg,
    Pict,
    Png,
    Tiff,
    Unknown,
    Wmf,
}

impl PictureType {
    pub(super) fn media_type(self) -> &'static str {
        match self {
            PictureType::Bmp => "image/bmp",
            PictureType::Emf => "image/x-emf",
            PictureType::Jpeg => "image/jpeg",
            PictureType::Pict => "image/x-pict",
            PictureType::Png => "image/png",
            PictureType::Tiff => "image/tiff",
            PictureType::Unknown => "image/unknown",
            PictureType::Wmf => "image/x-wmf",
        }
    }

    /// `getExtension`, which for PICT pictures starts with a dot of its own.
    pub(super) fn extension(self) -> &'static str {
        match self {
            PictureType::Bmp => "bmp",
            PictureType::Emf => "emf",
            PictureType::Jpeg => "jpg",
            PictureType::Pict => ".pict",
            PictureType::Png => "png",
            PictureType::Tiff => "tiff",
            PictureType::Unknown => "",
            PictureType::Wmf => "wmf",
        }
    }

    /// `suggestPictureType` of a BSE record, by its Windows blip type.
    fn of_stored(blip_type: u8) -> PictureType {
        match blip_type {
            0x02 => PictureType::Emf,
            0x03 => PictureType::Wmf,
            0x04 => PictureType::Pict,
            0x05 | 0x12 => PictureType::Jpeg,
            0x06 => PictureType::Png,
            0x07 => PictureType::Bmp,
            0x11 => PictureType::Tiff,
            _ => PictureType::Unknown,
        }
    }

    /// `suggestPictureType` of a blip record, by its type.
    fn of_blip(record_id: u16) -> PictureType {
        match record_id {
            kind::BLIP_EMF => PictureType::Emf,
            kind::BLIP_WMF => PictureType::Wmf,
            kind::BLIP_PICT => PictureType::Pict,
            kind::BLIP_JPEG => PictureType::Jpeg,
            kind::BLIP_PNG => PictureType::Png,
            kind::BLIP_DIB => PictureType::Bmp,
            kind::BLIP_TIFF => PictureType::Tiff,
            _ => PictureType::Unknown,
        }
    }
}

/// A picture, `Picture`: where its data starts in the data stream, 0 for one the drawing
/// group stores, and the records of its blips.
#[derive(Debug, Clone)]
pub(super) struct Picture {
    start: i32,
    blips: Vec<Escher>,
}

impl Picture {
    /// `suggestPictureType`.
    pub(super) fn kind(&self) -> PictureType {
        let [record] = &self.blips[..] else {
            return PictureType::Unknown;
        };
        match &record.content {
            Content::Stored(stored) => PictureType::of_stored(stored.blip_type),
            _ => PictureType::of_blip(record.record_id),
        }
    }

    /// `getRawContent`.
    fn raw_content(&self) -> &[u8] {
        let [record] = &self.blips[..] else {
            return &[];
        };
        match &record.content {
            Content::Picture(blip) => &blip.data,
            Content::Stored(stored) => stored.blip.as_ref().map_or(&[], |blip| &blip.data),
            _ => &[],
        }
    }

    /// `getContent`: a compressed picture inflated, a PNG without the header before it, any
    /// other as it is. A compressed picture that does not inflate leaves no content, which
    /// fails Tika's parse when it hands the picture on.
    pub(super) fn content(&self) -> Read<Vec<u8>> {
        let raw = self.raw_content();
        if COMPRESSED
            .iter()
            .any(|signature| matches_signature(raw, signature, 32))
        {
            return inflate(&raw[33..])
                .ok_or_else(|| Thrown::new("a compressed picture does not inflate"));
        }
        Ok(escher::picture_data(raw).to_vec())
    }
}

/// `Picture.matchSignature`: whether the data has the signature at the offset, as far as the
/// data goes.
fn matches_signature(data: &[u8], signature: &[u8], offset: usize) -> bool {
    offset < data.len()
        && data[offset..]
            .iter()
            .zip(signature)
            .all(|(byte, expected)| byte == expected)
}

/// `InflaterInputStream` read to its end: the inflated data of a complete zlib stream.
fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut inflater = flate2::Decompress::new(true);
    let mut out = Vec::with_capacity(data.len().saturating_mul(4).max(64));
    loop {
        if out.capacity() == out.len() {
            out.reserve(out.len().max(64));
        }
        let consumed = inflater.total_in() as usize;
        let written = inflater.total_out();
        match inflater.decompress_vec(&data[consumed..], &mut out, flate2::FlushDecompress::None) {
            Ok(flate2::Status::StreamEnd) => return Some(out),
            Ok(_)
                if inflater.total_in() as usize == data.len()
                    && inflater.total_out() == written =>
            {
                return None
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

/// Tika's `PicturesSource`: the document's pictures, which character runs claim them, and
/// which have been handed on.
pub(super) struct Pictures {
    all: Vec<Picture>,
    /// The index of the picture each start offset finds, the last of those starting there.
    by_start: HashMap<i32, usize>,
    /// The pictures no run of the main text claims, `None` for those claimed.
    unclaimed: Vec<Option<usize>>,
    next: usize,
    output: HashSet<usize>,
}

impl Pictures {
    /// `new PicturesSource(document)`.
    pub(super) fn read(document: &Document) -> Read<Pictures> {
        let all = all_pictures(document)?;
        let mut by_start = HashMap::new();
        for (index, picture) in all.iter().enumerate() {
            by_start.insert(picture.start, index);
        }
        let mut unclaimed: Vec<Option<usize>> = (0..all.len()).map(Some).collect();
        let (start, end) = document.story(super::fib::Story::Main);
        let main = document.range(start, end)?;
        for index in 0..document.run_count(main)? {
            let Some(run) = document.range_run(main, index)? else {
                continue;
            };
            if has_picture(document, &run)? {
                let picture = by_start.get(&run.chp.fc_pic).copied();
                let at = unclaimed
                    .iter()
                    .position(|&slot| slot == picture)
                    .ok_or_else(|| Thrown::new("Index -1 out of bounds for length"))?;
                unclaimed[at] = None;
            }
        }
        Ok(Pictures {
            all,
            by_start,
            unclaimed,
            next: 0,
            output: HashSet::new(),
        })
    }

    /// `getFor`: the picture a run's picture offset finds.
    pub(super) fn picture_of(&self, run: &CharacterRun) -> Option<usize> {
        self.by_start.get(&run.chp.fc_pic).copied()
    }

    /// `nextUnclaimed`.
    pub(super) fn next_unclaimed(&mut self) -> Option<usize> {
        while self.next < self.unclaimed.len() {
            let picture = self.unclaimed[self.next];
            self.next += 1;
            if picture.is_some() {
                return picture;
            }
        }
        None
    }

    pub(super) fn get(&self, picture: usize) -> &Picture {
        &self.all[picture]
    }

    /// `pictureNumber`.
    pub(super) fn number(&self, picture: usize) -> usize {
        picture + 1
    }

    /// `hasOutput`, then `recordOutput`: whether the picture is handed on for the first time.
    pub(super) fn first_output(&mut self, picture: usize) -> bool {
        self.output.insert(picture)
    }
}

/// `PicturesTable.hasPicture`: whether a run is a picture character whose data block holds an
/// image.
pub(super) fn has_picture(document: &Document, run: &CharacterRun) -> Read<bool> {
    let chp = &run.chp;
    if !chp.special || chp.object || chp.ole2 || chp.data {
        return Ok(false);
    }
    let text = document.text_of(run.span)?;
    if text != [0x01] && text != [0x01, 0x15] {
        return Ok(false);
    }
    let offset = chp.fc_pic;
    let block_type = i16_at(&document.data, at(offset, 0xE)?)?;
    let mm = i16_at(&document.data, at(offset, 0x6)?)?;
    Ok(match block_type {
        0x08 | 0x0A => true,
        0x00 | 0x02 => mm == 0x64,
        _ => false,
    })
}

/// An offset into the data stream past a picture's start, failing where Java's index would be
/// negative.
fn at(offset: i32, past: i32) -> Read<usize> {
    usize::try_from(offset.wrapping_add(past))
        .map_err(|_| Thrown(format!("Index {} out of bounds", offset.wrapping_add(past))))
}

/// `PicturesTable.getAllPictures`: the pictures the runs of the whole text place, then those
/// the drawing group stores.
fn all_pictures(document: &Document) -> Read<Vec<Picture>> {
    let mut pictures = Vec::new();
    let overall = document.range(0, document.text.len() as i32)?;
    for index in 0..document.run_count(overall)? {
        let Some(run) = document.range_run(overall, index)? else {
            continue;
        };
        if has_picture(document, &run)? {
            pictures.push(read_picture(&document.data, run.chp.fc_pic)?);
        }
    }
    if let Some(store) = &document.picture_store {
        for record in &store.children {
            let Content::Stored(stored) = &record.content else {
                continue;
            };
            if let Some(blip) = &stored.blip {
                pictures.push(stored_picture(blip.clone()));
            } else if stored.offset > 0 {
                if let Ok((delayed, _)) = escher::record_at(&document.main, stored.offset as usize)
                {
                    if let Content::Picture(blip) = delayed.content {
                        pictures.push(stored_picture(blip));
                    }
                }
            }
        }
    }
    Ok(pictures)
}

/// `new Picture(blipRecord)`.
fn stored_picture(blip: Blip) -> Picture {
    Picture {
        start: 0,
        blips: vec![Escher {
            record_id: blip.record_id,
            content: Content::Picture(blip),
            children: Vec::new(),
        }],
    }
}

/// `new Picture(dataBlockStartOffset, dataStream, false)`: its `PICFAndOfficeArtData`, the
/// picture's header, its name, its shape and the blip records after it.
fn read_picture(data: &[u8], start: i32) -> Read<Picture> {
    let mut offset = at(start, 0)?;
    let length = i64::from(i32_at(data, offset)?);
    let mm = i16_at(data, offset + 6)?;
    i16_at(data, offset + PICF_SIZE - 2)?;
    offset += PICF_SIZE;
    if mm == MM_SHAPE_FILE {
        let name_length = u8_at(data, offset)?;
        offset += 1;
        safely_clone(
            data,
            offset as i64,
            i64::from(name_length),
            MAX_RECORD_LENGTH,
        )?;
        offset += usize::from(name_length);
    }
    let (_, shape_length) = escher::container_at(data, offset)?;
    offset = advance(offset, shape_length)?;
    let mut blips = Vec::new();
    while (offset as i64 - i64::from(start)) < length {
        let record_id = escher::record_id_at(data, offset)?;
        if record_id != BSE && !(kind::BLIP_START..=kind::BLIP_END).contains(&record_id) {
            break;
        }
        let (record, read) = escher::record_at(data, offset)?;
        offset = advance(offset, read)?;
        blips.push(record);
    }
    Ok(Picture { start, blips })
}

fn advance(position: usize, length: isize) -> Read<usize> {
    position
        .checked_add_signed(length)
        .ok_or_else(|| Thrown::new("Escher records run back before the data"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_a_signature_as_far_as_the_data_goes() {
        let mut data = vec![0; 32];
        data.extend([0xFE, 0x78]);
        assert!(matches_signature(&data, &COMPRESSED[0], 32));
        assert!(!matches_signature(&data, &COMPRESSED[0], 34));
        data[33] = 0;
        assert!(!matches_signature(&data, &COMPRESSED[1], 32));
    }

    #[test]
    fn inflates_only_a_complete_stream() {
        use std::io::Write;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"metafile").unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(inflate(&compressed).as_deref(), Some(&b"metafile"[..]));
        assert_eq!(inflate(&compressed[..compressed.len() - 5]), None);
        assert_eq!(inflate(b"not zlib"), None);
    }

    #[test]
    fn names_the_types_of_pictures_as_poi_does() {
        assert_eq!(PictureType::of_stored(0x12), PictureType::Jpeg);
        assert_eq!(PictureType::of_blip(kind::BLIP_DIB).extension(), "bmp");
        assert_eq!(PictureType::Pict.extension(), ".pict");
        assert_eq!(PictureType::Unknown.media_type(), "image/unknown");
    }
}
