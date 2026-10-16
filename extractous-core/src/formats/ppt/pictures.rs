//! The pictures of a presentation as POI's `HSLFSlideShowImpl.readPictures` reads them from its
//! "Pictures" stream, matched to the records of its blip store, and the picture data each type
//! gives (`getData`); and the data of its embedded OLE objects (`ExOleObjStg.getData`).

use crate::formats::escher::{kind as escher_kind, Content, Escher};
use crate::formats::poi::{i32_at, inflate};
use crate::formats::Unported;
use crate::ole2::{EntryId, EntryKind, Ole2, Unreadable};

use super::records::{kind, Record};
use super::show::Top;

const PICTURES: &str = "Pictures";
/// `HSLFSlideShowImpl.MAX_IMAGE_LENGTH`.
const MAX_IMAGE: usize = 150_000_000;
/// `HSLFPictureData.PREAMBLE_SIZE`.
const PREAMBLE: usize = 8;
/// `HSLFPictureData.CHECKSUM_SIZE`.
const CHECKSUM: usize = 16;
const BSTORE_CONTAINER: u16 = 0xF001;

fn poi_throws(reason: &str) -> Unported {
    Unported(format!(
        "POI throws reading the presentation's pictures, which fails Tika's parse: {reason}"
    ))
}

fn recorded(reason: &str) -> Unported {
    Unported(format!(
        "a picture or object whose data POI fails to read, which Tika records with Java's stack \
         trace: {reason}"
    ))
}

/// The picture types POI builds (`PictureType`), by their native ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PictureType {
    Emf,
    Wmf,
    Pict,
    Jpeg,
    Png,
    Dib,
}

impl PictureType {
    /// `newInstance` of a native id, `None` where POI cannot build one.
    fn of(native: i32) -> Option<PictureType> {
        Some(match native {
            2 => PictureType::Emf,
            3 => PictureType::Wmf,
            4 => PictureType::Pict,
            5 => PictureType::Jpeg,
            6 => PictureType::Png,
            7 => PictureType::Dib,
            _ => return None,
        })
    }

    /// `setSignature`: how many checksums the picture's data starts with, `None` where the
    /// signature is not the type's.
    fn checksums(self, signature: u16) -> Option<usize> {
        let (one, two): (&[u16], &[u16]) = match self {
            PictureType::Emf => (&[0x3D40], &[0x3D50]),
            PictureType::Wmf => (&[0x2160], &[0x2170]),
            PictureType::Pict => (&[0x5420], &[0x5430]),
            PictureType::Jpeg => (&[0x46A0, 0x6E20], &[0x46B0, 0x6E30]),
            PictureType::Png => (&[0x6E00], &[0x6E10]),
            PictureType::Dib => (&[0x7A80], &[0x7A90]),
        };
        if one.contains(&signature) {
            Some(1)
        } else if two.contains(&signature) {
            Some(2)
        } else {
            None
        }
    }

    /// The media type Tika's extractor hands the picture on with.
    pub(super) fn media_type(self) -> &'static str {
        match self {
            PictureType::Emf => "image/emf",
            PictureType::Wmf => "image/wmf",
            PictureType::Dib => "image/bmp",
            PictureType::Pict => "image/x-pict",
            PictureType::Jpeg => "image/jpeg",
            PictureType::Png => "image/png",
        }
    }
}

/// An entry of a blip store (`EscherBSERecord`): its picture's checksum and offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoreEntry {
    pub(super) uid: [u8; 16],
    pub(super) offset: i32,
}

/// A picture of the stream (`HSLFPictureData`).
#[derive(Debug, Clone)]
pub(super) struct Picture {
    /// `getIndex`: its number among the pictures POI built, from one.
    pub(super) index: usize,
    pub(super) kind: PictureType,
    checksums: usize,
    raw: Vec<u8>,
    /// The entry of the blip store matched to it.
    pub(super) entry: usize,
}

/// The pictures, and the blip store as matching them left it.
#[derive(Debug, Default)]
pub(super) struct Pictures {
    pub(super) pictures: Vec<Picture>,
    pub(super) store: Option<Vec<StoreEntry>>,
}

impl Pictures {
    /// `HSLFPictureShape.getPictureData`: the first picture whose entry's offset is that of the
    /// blip store's entry `index` names, from one; `Err` where POI's lookup throws.
    pub(super) fn of_entry(
        &self,
        store: Option<&[StoreEntry]>,
        index: i32,
    ) -> Result<Option<&Picture>, Unported> {
        let Some(store) = store else {
            return Ok(None);
        };
        if index == 0 {
            return Ok(None);
        }
        let entry = usize::try_from(index - 1)
            .ok()
            .and_then(|at| store.get(at))
            .ok_or_else(|| recorded("a picture shape names a blip store entry past the store's"))?;
        let mine = self.store.as_deref().unwrap_or_default();
        Ok(self
            .pictures
            .iter()
            .find(|picture| mine[picture.entry].offset == entry.offset))
    }
}

/// The entries of a drawing group's blip store, `None` where it has none.
pub(super) fn store_of(group: &Escher) -> Result<Option<Vec<StoreEntry>>, Unported> {
    let Some(store) = group
        .children
        .iter()
        .find(|child| child.record_id == BSTORE_CONTAINER)
    else {
        return Ok(None);
    };
    store
        .children
        .iter()
        .map(|child| match &child.content {
            Content::Stored(stored) if child.record_id == escher_kind::BSE => Ok(StoreEntry {
                uid: stored.uid,
                offset: stored.offset,
            }),
            _ => Err(poi_throws("Did not have a EscherBSERecord")),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// A picture of the stream before it is matched: `PictureFactory`.
struct Factory {
    native: i32,
    data: Vec<u8>,
    offset: i32,
    signature: u16,
    entry: Option<usize>,
}

impl Factory {
    fn uid(&self) -> [u8; 16] {
        let mut uid = [0; 16];
        let length = self.data.len().min(16);
        uid[..length].copy_from_slice(&self.data[..length]);
        uid
    }
}

/// `readPictures`: the pictures of the "Pictures" stream of `directory`, matched to the blip
/// store of the first document record.
pub(super) fn read(
    file: &Ole2<'_>,
    directory: EntryId,
    records: &[Top],
) -> Result<Pictures, Unported> {
    let Some(entry) = file.child_ignoring_case(directory, PICTURES) else {
        return Ok(Pictures::default());
    };
    if file.kind(entry) != EntryKind::Document {
        return Err(poi_throws("the Pictures entry is no document"));
    }
    let mut store = first_document_store(records)?;
    let stream = file.read(entry).map_err(|Unreadable(reason)| {
        Unported(format!("POI cannot read the Pictures stream: {reason}"))
    })?;
    if stream.len() > MAX_IMAGE {
        return Err(poi_throws("the Pictures stream is longer than POI reads"));
    }
    let mut factories = Vec::new();
    let mut position = 0usize;
    while position + PREAMBLE <= stream.len() {
        let offset = position as i32;
        let signature = u16::from_le_bytes([stream[position], stream[position + 1]]);
        let record_type = u16::from_le_bytes([stream[position + 2], stream[position + 3]]);
        let size = i32_at(&stream, position + 4).expect("a preamble");
        position += PREAMBLE;
        if !(record_type == 0xF007 || (0xF018..=0xF117).contains(&record_type)) {
            break;
        }
        if size < 0 {
            return Err(poi_throws("a picture of a negative length"));
        }
        let size = size as usize;
        if position + size > stream.len() {
            break;
        }
        factories.push(Factory {
            native: i32::from(record_type) - 0xF018,
            data: stream[position..position + size].to_vec(),
            offset,
            signature,
            entry: None,
        });
        position += size;
    }
    match_records(&mut factories, &mut store)?;
    let mut pictures = Vec::new();
    for factory in factories {
        let Some(kind) = PictureType::of(factory.native) else {
            continue;
        };
        let Some(checksums) = kind.checksums(factory.signature) else {
            continue;
        };
        pictures.push(Picture {
            index: pictures.len() + 1,
            kind,
            checksums,
            raw: factory.data,
            entry: factory.entry.expect("every factory is matched"),
        });
    }
    Ok(Pictures {
        pictures,
        store: Some(store),
    })
}

/// `getBlipStore`: the blip store of the first document record by offset, failing where a
/// record before it is missing or it has no drawing group.
fn first_document_store(records: &[Top]) -> Result<Vec<StoreEntry>, Unported> {
    let mut document = None;
    for top in records {
        let Some(record) = &top.record else {
            return Err(poi_throws("Did not have a valid record"));
        };
        if record.kind == kind::DOCUMENT {
            document = Some(record);
            break;
        }
    }
    let document = document.ok_or_else(|| poi_throws("Document record is missing"))?;
    let group = document
        .last_after_first(kind::PP_DRAWING_GROUP)
        .and_then(Record::drawing_group)
        .ok_or_else(|| poi_throws("Drawing group is missing"))?;
    Ok(store_of(group)?.unwrap_or_default())
}

/// `matchPicturesAndRecords`: each picture's blip store entry, by offset, then by checksum, else
/// a new entry.
fn match_records(factories: &mut [Factory], store: &mut Vec<StoreEntry>) -> Result<(), Unported> {
    let mut unmatched: Vec<usize> = (0..factories.len()).collect();
    unmatched.sort_by_key(|&at| factories[at].offset);
    let mut by_offset: Vec<(i32, Vec<usize>)> = Vec::new();
    for (index, entry) in store.iter().enumerate() {
        match by_offset
            .iter_mut()
            .find(|(offset, _)| *offset == entry.offset)
        {
            Some((_, entries)) => entries.push(index),
            None => by_offset.push((entry.offset, vec![index])),
        }
    }
    unmatched.retain(|&at| {
        let factory = &mut factories[at];
        let Some(slot) = by_offset
            .iter()
            .position(|(offset, _)| *offset == factory.offset)
        else {
            return true;
        };
        let entries = &mut by_offset[slot].1;
        match entries.len() {
            0 => true,
            1 => {
                factory.entry = Some(entries[0]);
                by_offset.remove(slot);
                false
            }
            _ => {
                let uid = factory.uid();
                match entries.iter().position(|&entry| store[entry].uid == uid) {
                    Some(found) => {
                        factory.entry = Some(entries.remove(found));
                        false
                    }
                    None => true,
                }
            }
        }
    });
    let buckets = by_offset
        .iter()
        .filter(|(_, entries)| !entries.is_empty())
        .count();
    let mut remaining: Vec<usize> = by_offset
        .into_iter()
        .flat_map(|(_, entries)| entries)
        .collect();
    for at in unmatched {
        let uid = factories[at].uid();
        let mut matched = false;
        let mut i = remaining.len();
        while i > 0 {
            i -= 1;
            if store[remaining[i]].uid == uid {
                if buckets > 1 {
                    return Err(Unported(
                        "pictures matched by checksum among blip store entries POI orders by a \
                         hash map"
                            .to_owned(),
                    ));
                }
                let entry = remaining.remove(i);
                store[entry].offset = factories[at].offset;
                factories[at].entry = Some(entry);
                matched = true;
            }
        }
        if !matched {
            store.push(StoreEntry {
                uid,
                offset: factories[at].offset,
            });
            factories[at].entry = Some(store.len() - 1);
        }
    }
    Ok(())
}

impl Picture {
    /// `getData`: the picture's data as its type gives it, or why POI fails, which Tika records.
    pub(super) fn data(&self) -> Result<Vec<u8>, Unported> {
        let raw = &self.raw;
        match self.kind {
            PictureType::Jpeg | PictureType::Png | PictureType::Dib => {
                let prefix = CHECKSUM * self.checksums + 1;
                if raw.len() < prefix {
                    return Err(recorded("a bitmap shorter than its header"));
                }
                let data = &raw[prefix..];
                Ok(match self.kind {
                    PictureType::Png => crate::formats::escher::picture_data(data).to_vec(),
                    PictureType::Dib => bmp(data)?,
                    _ => data.to_vec(),
                })
            }
            PictureType::Emf => {
                let header = MetafileHeader::read(raw, CHECKSUM)?;
                let _ = header;
                inflate(raw.get(CHECKSUM + 34..).unwrap_or_default(), None)
                    .ok_or_else(|| recorded("an EMF picture that does not inflate"))
            }
            PictureType::Wmf => {
                let at = CHECKSUM * self.checksums;
                let header = MetafileHeader::read(raw, at)?;
                let mut data = header.placeable();
                data.extend(
                    inflate(raw.get(at + 34..).unwrap_or_default(), None)
                        .ok_or_else(|| recorded("a WMF picture that does not inflate"))?,
                );
                Ok(data)
            }
            PictureType::Pict => {
                let at = CHECKSUM * self.checksums;
                MetafileHeader::read(raw, at)?;
                let mut data = vec![0; 512];
                data.extend(
                    inflate(raw.get(at + 34..).unwrap_or_default(), None)
                        .ok_or_else(|| recorded("a PICT picture that does not inflate"))?,
                );
                Ok(data)
            }
        }
    }
}

/// `DIB.addBMPHeader`: the file header of a bitmap before its data.
fn bmp(data: &[u8]) -> Result<Vec<u8>, Unported> {
    let image_size =
        i32_at(data, 0x22 - 14).map_err(|_| recorded("a DIB shorter than its header"))?;
    let file_size = (data.len() as i32).wrapping_add(14);
    let offset = file_size.wrapping_sub(image_size);
    let mut out = Vec::with_capacity(data.len() + 14);
    out.extend(0x4D42i32.to_le_bytes());
    out.truncate(2);
    out.extend(file_size.to_le_bytes());
    out.extend(0i32.to_le_bytes());
    out.extend(offset.to_le_bytes());
    out.truncate(14);
    out.extend_from_slice(data);
    Ok(out)
}

/// `Metafile.Header`: the bounds of a metafile picture.
struct MetafileHeader {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl MetafileHeader {
    fn read(raw: &[u8], at: usize) -> Result<MetafileHeader, Unported> {
        if raw.len() < at + 34 {
            return Err(recorded("a metafile picture shorter than its header"));
        }
        let int = |offset: usize| i32_at(raw, at + offset).expect("the header's bytes");
        Ok(MetafileHeader {
            left: int(4),
            top: int(8),
            right: int(12),
            bottom: int(16),
        })
    }

    /// `ImageHeaderWMF(getBounds()).write`: the placeable header of a WMF of these bounds.
    fn placeable(&self) -> Vec<u8> {
        const KEY: i32 = 0x9AC6_CDD7_u32 as i32;
        let width = self.right.wrapping_sub(self.left);
        let height = self.bottom.wrapping_sub(self.top);
        let left = self.left;
        let top = self.top;
        let right = left.wrapping_add(width);
        let bottom = top.wrapping_add(height);
        let inch = 72i32;
        let mut checksum = 0i32;
        checksum ^= KEY & 0x0000_FFFF;
        checksum ^= (KEY & (0xFFFF_0000_u32 as i32)) >> 16;
        for value in [left, top, right, bottom, inch] {
            checksum ^= value;
        }
        let mut out = Vec::with_capacity(22);
        out.extend(KEY.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        for value in [left, top, right, bottom, inch] {
            out.extend((value as u16).to_le_bytes());
        }
        out.extend(0i32.to_le_bytes());
        out.extend((checksum as u16).to_le_bytes());
        out
    }
}

/// An embedded OLE object of the document stream (`HSLFObjectData`): its persist id and data.
#[derive(Debug)]
pub(super) struct Object<'a> {
    pub(super) persist_id: i32,
    pub(super) record: &'a Record,
}

impl Object<'_> {
    /// `ExOleObjStg.getData` read whole: a compressed object inflated to the length it gives, or
    /// why Java's stream fails reading it.
    pub(super) fn data(&self) -> Result<Vec<u8>, Unported> {
        let data = self.record.data();
        if self.record.options == 0 {
            return Ok(data.to_vec());
        }
        let size =
            i32_at(data, 0).map_err(|_| recorded("a compressed object without its length"))?;
        let size = usize::try_from(size)
            .map_err(|_| recorded("a compressed object of a negative length"))?;
        inflate(&data[4..], Some(size))
            .ok_or_else(|| recorded("an embedded object whose compressed data does not inflate"))
    }
}

/// `getEmbeddedObjects`: the storages of embedded objects among the stream's records.
pub(super) fn objects(records: &[Top]) -> Vec<Object<'_>> {
    records
        .iter()
        .filter_map(|top| {
            let record = top.record.as_ref()?;
            (record.kind == kind::EX_OLE_OBJ_STG).then_some(Object {
                persist_id: top.persist_id.unwrap_or_default(),
                record,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(uid: u8, offset: i32) -> StoreEntry {
        StoreEntry {
            uid: [uid; 16],
            offset,
        }
    }

    fn factory(uid: u8, offset: i32) -> Factory {
        Factory {
            native: 5,
            data: vec![uid; 20],
            offset,
            signature: 0x46A0,
            entry: None,
        }
    }

    #[test]
    fn matches_pictures_by_offset_then_checksum_else_adds_an_entry() {
        let mut store = vec![entry(1, 0), entry(2, 99)];
        let mut factories = vec![factory(1, 0), factory(2, 50), factory(3, 80)];
        match_records(&mut factories, &mut store).unwrap();
        let entries: Vec<Option<usize>> = factories.iter().map(|f| f.entry).collect();
        assert_eq!(entries, [Some(0), Some(1), Some(2)]);
        assert_eq!(store[1].offset, 50);
        assert_eq!(store[2], entry(3, 80));
    }

    #[test]
    fn writes_a_placeable_header_as_poi_does() {
        let header = MetafileHeader {
            left: 0,
            top: 0,
            right: 100,
            bottom: 50,
        };
        let placeable = header.placeable();
        assert_eq!(placeable.len(), 22);
        assert_eq!(&placeable[..4], &[0xD7, 0xCD, 0xC6, 0x9A]);
        assert_eq!(&placeable[10..12], &100u16.to_le_bytes());
        assert_eq!(&placeable[20..], &[0x0F, 0x57]);
    }
}
