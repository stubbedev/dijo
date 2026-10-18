//! The pictures of a presentation as POI's `HSLFSlideShowImpl.readPictures` reads them from its
//! "Pictures" stream, matched to the records of its blip store, and the picture data each type
//! gives (`getData`); and the data of its embedded OLE objects (`ExOleObjStg.getData`).

use std::borrow::Cow;

use crate::formats::escher::{kind as escher_kind, Content as EscherContent, Escher};
use crate::formats::poi::{i32_at, inflate, Thrown, Unread};
use crate::formats::{wmf_placeable, Unported};
use crate::ole2::{Content, EntryId, EntryKind, Ole2, Unreadable};

use super::encryption::Encryption;
use super::records::{kind, Record};
use super::show::{build, Records, Show};

const PICTURES: &str = "Pictures";
/// `HSLFSlideShowImpl.MAX_IMAGE_LENGTH`.
const MAX_IMAGE: usize = 150_000_000;
/// `HSLFPictureData.PREAMBLE_SIZE`.
const PREAMBLE: usize = 8;
/// `HSLFPictureData.CHECKSUM_SIZE`.
const CHECKSUM: usize = 16;
const BSTORE_CONTAINER: u16 = 0xF001;

fn poi_throws(reason: &str) -> Unread {
    Thrown(format!("reading the presentation's pictures: {reason}")).into()
}

pub(super) fn recorded(reason: &str) -> Unported {
    Unported(format!(
        "a picture or object whose data POI fails to read, which Tika records with Java's stack \
         trace: {reason}"
    ))
}

/// Why the data of a picture or an object is not read as POI reads it.
#[derive(Debug)]
pub(super) enum UnreadData {
    /// POI throws for this reason, which Tika records before it goes on past the picture or
    /// object.
    Recorded(&'static str),
    /// Java's stream of the data throws this exception, its first line as Java prints it, where
    /// Tika reads it, which Tika records as a warning before it goes on past the object.
    Thrown(String),
    /// What this port does not read as POI does.
    Unported(Unported),
}

impl From<Unported> for UnreadData {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

impl From<UnreadData> for Unread {
    fn from(unread: UnreadData) -> Self {
        match unread {
            UnreadData::Recorded(reason) => recorded(reason).into(),
            UnreadData::Thrown(exception) => recorded(&exception).into(),
            UnreadData::Unported(unported) => unported.into(),
        }
    }
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

/// A picture of the stream (`HSLFPictureData`), by where its data is.
#[derive(Debug, Clone)]
pub(super) struct Picture {
    /// `getIndex`: its number among the pictures POI built, from one.
    pub(super) index: usize,
    pub(super) kind: PictureType,
    checksums: usize,
    /// Where its data starts in the stream.
    at: usize,
    size: usize,
    /// The entry of the blip store matched to it.
    pub(super) entry: usize,
}

/// The pictures, the stream they are read from where they are written, and the blip store as
/// matching them left it.
#[derive(Debug)]
pub(super) struct Pictures<'p> {
    stream: Content<'p>,
    pub(super) pictures: Vec<Picture>,
    pub(super) store: Option<Vec<StoreEntry>>,
}

impl Pictures<'_> {
    fn none() -> Self {
        Self {
            stream: Content::from(&[][..]),
            pictures: Vec::new(),
            store: None,
        }
    }
}

impl<'p> Pictures<'p> {
    /// `HSLFPictureShape.getPictureData`: the first picture whose entry's offset is that of the
    /// blip store's entry `index` names, from one; `Err` where POI's lookup throws.
    pub(super) fn of_entry(
        &self,
        store: Option<&[StoreEntry]>,
        index: i32,
    ) -> Result<Option<&Picture>, Unread> {
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

    /// `getData` of a picture: its data as its type gives it, read from the stream where it is
    /// written, or why POI fails, which Tika records.
    pub(super) fn data(&self, picture: &Picture) -> Result<Cow<'p, [u8]>, UnreadData> {
        let raw = self
            .stream
            .range(picture.at, picture.size)
            .expect("a picture lies within the stream it was found in");
        picture.data(raw)
    }
}

/// The entries of a drawing group's blip store, `None` where it has none.
pub(super) fn store_of(group: &Escher) -> Result<Option<Vec<StoreEntry>>, Unread> {
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
            EscherContent::Stored(stored) if child.record_id == escher_kind::BSE => {
                Ok(StoreEntry {
                    uid: stored.uid,
                    offset: stored.offset,
                })
            }
            _ => Err(poi_throws("Did not have a EscherBSERecord")),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// A picture of the stream before it is matched: `PictureFactory`.
struct Factory {
    native: i32,
    /// Where its data starts in the stream.
    at: usize,
    size: usize,
    offset: i32,
    signature: u16,
    /// The first 16 bytes of its data.
    uid: [u8; 16],
    entry: Option<usize>,
}

/// A "Pictures" stream as `readPictures` walks it: its pictures' preambles, each decrypted in
/// place first where the presentation is encrypted.
trait Walked {
    fn len(&self) -> usize;
    /// The preamble at `at`, which runs within the stream.
    fn preamble(&mut self, at: usize) -> Result<[u8; PREAMBLE], Unread>;
    /// The first 16 bytes of `size` bytes of data at `at`, which run within the stream.
    fn uid(&self, at: usize, size: usize) -> [u8; 16];
}

/// A stream read in place.
struct InPlace<'c, 'p>(&'c Content<'p>);

impl Walked for InPlace<'_, '_> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn preamble(&mut self, at: usize) -> Result<[u8; PREAMBLE], Unread> {
        let bytes = self
            .0
            .range(at, PREAMBLE)
            .expect("a preamble within the stream");
        Ok(bytes[..].try_into().expect("a preamble's bytes"))
    }

    fn uid(&self, at: usize, size: usize) -> [u8; 16] {
        uid_of(
            &self
                .0
                .range(at, size.min(16))
                .expect("data within the stream"),
        )
    }
}

/// A stream copied, its pictures decrypted in place as they are walked.
struct Decrypting<'s> {
    stream: &'s mut [u8],
    encryption: &'s Encryption,
}

impl Walked for Decrypting<'_> {
    fn len(&self) -> usize {
        self.stream.len()
    }

    fn preamble(&mut self, at: usize) -> Result<[u8; PREAMBLE], Unread> {
        self.encryption.decrypt_picture(self.stream, at)?;
        Ok(self.stream[at..at + PREAMBLE]
            .try_into()
            .expect("a preamble's bytes"))
    }

    fn uid(&self, at: usize, size: usize) -> [u8; 16] {
        uid_of(&self.stream[at..at + size.min(16)])
    }
}

/// A checksum of up to 16 bytes, zeros after them.
fn uid_of(bytes: &[u8]) -> [u8; 16] {
    let mut uid = [0; 16];
    uid[..bytes.len()].copy_from_slice(bytes);
    uid
}

/// `readPictures`' walk of a stream: each picture after its preamble, up to the first that is
/// no picture or runs past the stream.
fn walk(stream: &mut impl Walked) -> Result<Vec<Factory>, Unread> {
    let mut factories = Vec::new();
    let mut position = 0usize;
    while position + PREAMBLE <= stream.len() {
        let preamble = stream.preamble(position)?;
        let offset = position as i32;
        let signature = u16::from_le_bytes([preamble[0], preamble[1]]);
        let record_type = u16::from_le_bytes([preamble[2], preamble[3]]);
        let size = i32::from_le_bytes([preamble[4], preamble[5], preamble[6], preamble[7]]);
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
            at: position,
            size,
            offset,
            signature,
            uid: stream.uid(position, size),
            entry: None,
        });
        position += size;
    }
    Ok(factories)
}

/// `readPictures`: the pictures of the "Pictures" stream of `directory`, matched to the blip
/// store of the first document record, read in place, or copied into `decrypted` and each
/// decrypted in place where the presentation is encrypted.
pub(super) fn read<'p>(
    file: &Ole2<'p>,
    directory: EntryId,
    records: &Records,
    source: &[u8],
    encryption: Option<&Encryption>,
    decrypted: &'p mut Vec<u8>,
) -> Result<Pictures<'p>, Unread> {
    let Some(entry) = file.child_ignoring_case(directory, PICTURES) else {
        return Ok(Pictures::none());
    };
    if file.kind(entry) != EntryKind::Document {
        return Err(poi_throws("the Pictures entry is no document"));
    }
    let mut store = first_document_store(records, source)?;
    let encryption = pictures_encryption(records, encryption)?;
    let content = file.content(entry).map_err(|Unreadable(reason)| {
        poi_throws(&format!("cannot read the Pictures stream: {reason}"))
    })?;
    if content.len() > MAX_IMAGE {
        return Err(poi_throws("the Pictures stream is longer than POI reads"));
    }
    let (stream, mut factories) = match encryption {
        None => {
            let factories = walk(&mut InPlace(&content))?;
            (content, factories)
        }
        Some(encryption) => {
            *decrypted = content.to_vec();
            let factories = walk(&mut Decrypting {
                stream: decrypted,
                encryption,
            })?;
            (Content::from(&decrypted[..]), factories)
        }
    };
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
            at: factory.at,
            size: factory.size,
            entry: factory.entry.expect("every factory is matched"),
        });
    }
    Ok(Pictures {
        stream,
        pictures,
        store: Some(store),
    })
}

/// The encryption `readPictures` decrypts the pictures with: that of the first
/// `DocumentEncryptionAtom` of the records (`getDocumentEncryptionAtom`), which must be the one
/// opened, as POI's decryptor of any other has no key.
fn pictures_encryption<'e>(
    records: &Records,
    encryption: Option<&'e Encryption>,
) -> Result<Option<&'e Encryption>, Unread> {
    let first = records
        .tops
        .iter()
        .find(|top| top.kind == Some(kind::DOCUMENT_ENCRYPTION_ATOM));
    match (first, encryption) {
        (None, _) => Ok(None),
        (Some(top), Some(encryption)) if top.offset == encryption.atom => Ok(Some(encryption)),
        (Some(_), _) => Err(poi_throws(
            "the first DocumentEncryptionAtom is not the one opened, whose decryptor has no key",
        )),
    }
}

/// `getBlipStore`: the blip store of the first document record by offset, failing where a
/// record before it is missing or it has no drawing group.
fn first_document_store(records: &Records, source: &[u8]) -> Result<Vec<StoreEntry>, Unread> {
    let mut document = None;
    for top in &records.tops {
        let Some(record_kind) = top.kind else {
            return Err(poi_throws("Did not have a valid record"));
        };
        if record_kind == kind::DOCUMENT {
            document = Some(build(source, top.offset)?);
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
fn match_records(factories: &mut [Factory], store: &mut Vec<StoreEntry>) -> Result<(), Unread> {
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
                let uid = factory.uid;
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
        let uid = factories[at].uid;
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
                    )
                    .into());
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

/// The part `part` takes of bytes, in place where they are.
fn narrowed<'p>(bytes: Cow<'p, [u8]>, part: impl Fn(&[u8]) -> &[u8]) -> Cow<'p, [u8]> {
    match bytes {
        Cow::Borrowed(bytes) => Cow::Borrowed(part(bytes)),
        Cow::Owned(bytes) => Cow::Owned(part(&bytes).to_vec()),
    }
}

impl Picture {
    /// `getData` of the picture's `raw` bytes: its data as its type gives it, in place where it
    /// is stored as it is, or why POI fails, which Tika records.
    fn data<'p>(&self, raw: Cow<'p, [u8]>) -> Result<Cow<'p, [u8]>, UnreadData> {
        match self.kind {
            PictureType::Jpeg | PictureType::Png | PictureType::Dib => {
                let prefix = CHECKSUM * self.checksums + 1;
                if raw.len() < prefix {
                    return Err(UnreadData::Recorded("a bitmap shorter than its header"));
                }
                Ok(match self.kind {
                    PictureType::Png => narrowed(raw, |raw| {
                        crate::formats::escher::picture_data(&raw[prefix..])
                    }),
                    PictureType::Dib => Cow::Owned(bmp(&raw[prefix..])?),
                    _ => narrowed(raw, |raw| &raw[prefix..]),
                })
            }
            PictureType::Emf => {
                let header = MetafileHeader::read(&raw, CHECKSUM)?;

                let _ = header;
                inflate(raw.get(CHECKSUM + 34..).unwrap_or_default(), None)
                    .map(Cow::Owned)
                    .map_err(|_| UnreadData::Recorded("an EMF picture that does not inflate"))
            }
            PictureType::Wmf => {
                let at = CHECKSUM * self.checksums;
                let header = MetafileHeader::read(&raw, at)?;
                let mut data = header.placeable()?;
                data.extend(
                    inflate(raw.get(at + 34..).unwrap_or_default(), None)
                        .map_err(|_| UnreadData::Recorded("a WMF picture that does not inflate"))?,
                );
                Ok(Cow::Owned(data))
            }
            PictureType::Pict => {
                let at = CHECKSUM * self.checksums;
                MetafileHeader::read(&raw, at)?;
                let mut data = vec![0; 512];
                data.extend(
                    inflate(raw.get(at + 34..).unwrap_or_default(), None).map_err(|_| {
                        UnreadData::Recorded("a PICT picture that does not inflate")
                    })?,
                );
                Ok(Cow::Owned(data))
            }
        }
    }
}

/// `DIB.addBMPHeader`: the file header of a bitmap before its data.
fn bmp(data: &[u8]) -> Result<Vec<u8>, UnreadData> {
    let image_size = i32_at(data, 0x22 - 14)
        .map_err(|_| UnreadData::Recorded("a DIB shorter than its header"))?;
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

/// `Metafile.Header`: the bounds of a metafile picture, in its logical units, and its width in
/// EMUs.
struct MetafileHeader {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    width_emus: i32,
}

/// EMUs in an inch.
const EMUS_PER_INCH: i64 = 914_400;

impl MetafileHeader {
    fn read(raw: &[u8], at: usize) -> Result<MetafileHeader, UnreadData> {
        if raw.len() < at + 34 {
            return Err(UnreadData::Recorded(
                "a metafile picture shorter than its header",
            ));
        }
        let int = |offset: usize| i32_at(raw, at + offset).expect("the header's bytes");
        Ok(MetafileHeader {
            left: int(4),
            top: int(8),
            right: int(12),
            bottom: int(16),
            width_emus: int(20),
        })
    }

    /// How many of the picture's logical units make an inch: its width in them over its width
    /// in inches, where those give one a placeable header holds.
    fn units_per_inch(&self) -> Option<u16> {
        let units = i64::from(self.right) - i64::from(self.left);
        let emus = i64::from(self.width_emus);
        if units <= 0 || emus <= 0 {
            return None;
        }
        let inch = (units * EMUS_PER_INCH + emus / 2) / emus;
        u16::try_from(inch).ok().filter(|&inch| inch > 0)
    }

    /// `ImageHeaderWMF(getBounds()).write`: the placeable header of a WMF of these bounds, its
    /// units per inch those the picture's size gives, where POI writes 72 whatever the units
    /// (#119); none where the size gives none.
    fn placeable(&self) -> Result<Vec<u8>, Unported> {
        let units_per_inch = self.units_per_inch().ok_or_else(|| {
            Unported(
                "a WMF picture whose bounds and size give no units per inch for its placeable \
                 header"
                    .to_owned(),
            )
        })?;
        let bounds = [self.left, self.top, self.right, self.bottom].map(|value| value as u16);
        Ok(wmf_placeable::header(bounds, units_per_inch).to_vec())
    }
}

/// An embedded OLE object of the document stream (`HSLFObjectData`): its persist id, and where
/// its storage is.
#[derive(Debug)]
pub(super) struct Object {
    pub(super) persist_id: i32,
    offset: i32,
}

impl Object {
    /// `ExOleObjStg.getData` read whole, its storage built from the document stream: the object
    /// in place where it is stored as it is, a compressed object inflated to the length it
    /// gives, or why Java's stream fails reading it.
    pub(super) fn data<'a>(
        &self,
        show: &Show<'a>,
    ) -> Result<Result<Cow<'a, [u8]>, UnreadData>, Unread> {
        let record = show.build(self.offset)?;
        let data = record
            .in_place()
            .expect("an object's storage is an atom of the document stream");
        if record.options == 0 {
            return Ok(Ok(Cow::Borrowed(data)));
        }
        Ok(inflated(data))
    }
}

/// The data of a compressed object, or why it does not inflate.
fn inflated(data: &[u8]) -> Result<Cow<'static, [u8]>, UnreadData> {
    let size = i32_at(data, 0)
        .map_err(|_| UnreadData::Recorded("a compressed object without its length"))?;
    let size = usize::try_from(size)
        .map_err(|_| UnreadData::Recorded("a compressed object of a negative length"))?;
    inflate(&data[4..], Some(size))
        .map(Cow::Owned)
        .map_err(|Thrown(exception)| UnreadData::Thrown(exception))
}

/// `getEmbeddedObjects`: the storages of embedded objects among the stream's records.
pub(super) fn objects(records: &Records) -> Vec<Object> {
    records
        .tops
        .iter()
        .filter(|top| top.kind == Some(kind::EX_OLE_OBJ_STG))
        .map(|top| Object {
            persist_id: top.persist_id.unwrap_or_default(),
            offset: top.offset,
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
            at: 0,
            size: 20,
            offset,
            signature: 0x46A0,
            uid: [uid; 16],
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
    fn writes_a_placeable_header_of_the_pictures_units_per_inch() {
        let header = |right, width_emus| MetafileHeader {
            left: 0,
            top: 0,
            right,
            bottom: 50,
            width_emus,
        };
        let placeable = header(100, 1_270_000).placeable().unwrap();
        assert_eq!(placeable.len(), 22);
        assert!(wmf_placeable::starts(&placeable));
        assert_eq!(&placeable[10..12], &100u16.to_le_bytes());
        assert_eq!(&placeable[14..16], &72u16.to_le_bytes());
        assert_eq!(&placeable[20..], &[0x0F, 0x57]);
        let twips = header(2880, 2 * 914_400).placeable().unwrap();
        assert_eq!(&twips[14..16], &1440u16.to_le_bytes());
        let checksum = twips[..20].chunks(2).fold(0u16, |sum, word| {
            sum ^ u16::from_le_bytes([word[0], word[1]])
        });
        assert_eq!(&twips[20..], &checksum.to_le_bytes());
        assert!(header(100, 0).placeable().is_err());
        assert!(header(0, 914_400).placeable().is_err());
    }
}
