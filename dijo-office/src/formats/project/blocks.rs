//! The structures Project's backing stores are written in, as MPXJ reads them: `Props` streams
//! of keyed values, the field maps they hold, which say where each field of a task or a
//! resource lies, the fixed blocks (`FixedMeta` and `FixedData`), one record per item, and the
//! variable data (`VarMeta` and `Var2Data`), values keyed by an item's unique ID and a field.

use std::collections::{BTreeMap, BTreeSet};

use super::{Malformed, Version};

/// What `FixedMeta`, `VarMeta` and the Props of a field map start their structures with.
const MAGIC: u32 = 0xFADF_ADBA;
/// The length of `FixedMeta`'s header.
const FIXED_META_HEADER: usize = 16;
/// The length of `VarMeta`'s header.
const VAR_META_HEADER: usize = 24;
/// The length of each entry of a field map.
const FIELD_MAP_ENTRY: usize = 28;
/// The offset a field map gives a field that does not lie in the fixed data.
const NOT_FIXED: u16 = 0xFFFF;

/// `N` little-endian bytes of `data` at `at`, if it holds them.
pub(super) fn le<const N: usize>(data: &[u8], at: usize) -> Option<[u8; N]> {
    data.get(at..at.checked_add(N)?)?.try_into().ok()
}

/// `N` bytes of a fixed record at `at`, zeros past its end, as MPXJ pads a record shorter than
/// its field map to the map's length before it reads fields from it.
pub(super) fn padded<const N: usize>(record: &[u8], at: usize) -> [u8; N] {
    let mut bytes = [0; N];
    if let Some(present) = record.get(at..) {
        let length = present.len().min(N);
        bytes[..length].copy_from_slice(&present[..length]);
    }
    bytes
}

/// The text of a UTF-16LE string that ends at its first NUL or at the end of `data`.
pub(super) fn unicode(data: &[u8]) -> String {
    let units = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0);
    char::decode_utf16(units)
        .map(|decoded| decoded.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// The bytes of a single-byte string that ends at its first NUL or at the end of `data`.
pub(super) fn ansi(data: &[u8]) -> &[u8] {
    data.split(|&byte| byte == 0).next().unwrap_or_default()
}

/// A `Props` stream (`Props9`, `Props12`, `Props14`): values by key, in the order of the keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Props(BTreeMap<u32, Vec<u8>>);

impl Props {
    /// The values of the stream `data`: a header that counts them, then each its length, its
    /// key and a word MPXJ passes over, padded to an even length. A value the stream ends
    /// inside ends the reading, as MPXJ ends it.
    pub(super) fn read(data: &[u8], what: &str) -> Result<Props, Malformed> {
        let count = le::<2>(data, 12)
            .map(u16::from_le_bytes)
            .ok_or_else(|| Malformed(format!("{what} is shorter than its 16-byte header")))?;
        let mut values = BTreeMap::new();
        let mut at = FIXED_META_HEADER;
        for _ in 0..count {
            let (Some(size), Some(key)) = (le::<4>(data, at), le::<4>(data, at + 4)) else {
                break;
            };
            at += 12;
            let size = i32::from_le_bytes(size);
            let Some(value) = usize::try_from(size)
                .ok()
                .filter(|&size| size > 0)
                .and_then(|size| data.get(at..at.checked_add(size)?))
            else {
                break;
            };
            values.insert(u32::from_le_bytes(key), value.to_vec());
            at += value.len() + value.len() % 2;
        }
        Ok(Props(values))
    }

    pub(super) fn bytes(&self, key: u32) -> Option<&[u8]> {
        self.0.get(&key).map(Vec::as_slice)
    }

    /// The first byte of the value of `key`, or 0 where it has none, as MPXJ's `getByte`.
    pub(super) fn byte(&self, key: u32) -> u8 {
        self.bytes(key)
            .and_then(|value| value.first().copied())
            .unwrap_or(0)
    }

    /// The value of `key` as a UTF-16LE string, where it has one that is not blank.
    pub(super) fn text(&self, key: u32) -> Option<String> {
        self.bytes(key)
            .map(unicode)
            .filter(|text| !text.trim().is_empty())
    }
}

/// Where a field lies, as a field map places it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Location {
    /// In the fixed data: in which of an item's fixed blocks, at which offset.
    Fixed { block: usize, offset: usize },
    /// In the variable data, under this key.
    Var(u16),
    /// In the flags of the fixed meta data, or nowhere MPXJ reads.
    Elsewhere,
}

/// The field IDs that name a field, as `MPPTaskField` and `MPPResourceField` map them: a field
/// that two IDs name takes the place of the one the map lists last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Field {
    pub(super) name: &'static str,
    pub(super) ids: &'static [u16],
}

/// A field map (`FieldMap9`, `FieldMap12`, `FieldMap14`) of one class of items: where each of
/// its fields lies, and how long the first fixed block of a record of the class is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FieldMap {
    /// The ID within the class and the location of each field, in the order the map lists
    /// them.
    fields: Vec<(u16, Location)>,
    /// `getMaxFixedDataSize(0)`: the end of the field that ends last in the first fixed block,
    /// or 0 for the default map, as MPXJ leaves it.
    pub(super) fixed_size: usize,
    /// Where a field of the default map lies, for a project whose Props hold no map.
    defaults: &'static [(&'static [u16], Location)],
}

impl FieldMap {
    /// The map of the class whose field types start `class` that the first of `keys` in the
    /// project's `props` holds, as `createFieldMap` reads it; or the default map `defaults`
    /// where the Props hold none. Project 2007 and later key a field's variable data by its
    /// ID; Project 2000 to 2003 by a byte of the map's entry.
    pub(super) fn read(
        props: &Props,
        keys: &[u32],
        class: u16,
        version: Version,
        defaults: &'static [(&'static [u16], Location)],
    ) -> FieldMap {
        let Some(data) = keys.iter().find_map(|&key| props.bytes(key)) else {
            return FieldMap {
                fields: Vec::new(),
                fixed_size: 0,
                defaults,
            };
        };
        let mut fields = Vec::with_capacity(data.len() / FIELD_MAP_ENTRY);
        let mut fixed_size = 0;
        let mut block = 0;
        let mut last_offset = 0;
        for entry in data.as_chunks::<FIELD_MAP_ENTRY>().0 {
            let word = |at: usize| u16::from_le_bytes([entry[at], entry[at + 1]]);
            let offset = word(4);
            let id = word(12);
            let category = word(20);
            let size = usize::from(word(22));
            let var_key = match version {
                Version::Mpp9 => u16::from(entry[6]),
                Version::Mpp12 | Version::Mpp14 => id,
            };
            let location = match category {
                0x0B | 0x64 => Location::Elsewhere,
                _ if offset != NOT_FIXED => {
                    let offset = usize::from(offset);
                    if offset < last_offset {
                        block += 1;
                    }
                    last_offset = offset;
                    if block == 0 {
                        fixed_size = fixed_size.max(offset + size);
                    }
                    Location::Fixed { block, offset }
                }
                _ if var_key != 0 => Location::Var(var_key),
                _ => Location::Elsewhere,
            };
            if word(14) == class {
                fields.push((id, location));
            }
        }
        FieldMap {
            fields,
            fixed_size,
            defaults,
        }
    }

    /// Where `field` lies: as the map places it, or as the default map does where the
    /// project's Props hold none.
    fn location(&self, field: Field) -> Option<Location> {
        if self.fields.is_empty() {
            return self
                .defaults
                .iter()
                .find(|(ids, _)| *ids == field.ids)
                .map(|&(_, location)| location);
        }
        self.fields
            .iter()
            .rev()
            .find(|(id, _)| field.ids.contains(id))
            .map(|&(_, location)| location)
    }

    /// The offset of `field` in the first fixed block of a record.
    pub(super) fn fixed(&self, field: Field, what: &str) -> Result<usize, Malformed> {
        match self.location(field) {
            Some(Location::Fixed { block: 0, offset }) => Ok(offset),
            located => Err(Malformed(format!(
                "the {what} field map places the {} at {located:?}, not in the first fixed block",
                field.name
            ))),
        }
    }

    /// The key of `field` in the variable data.
    pub(super) fn var(&self, field: Field, what: &str) -> Result<u16, Malformed> {
        match self.location(field) {
            Some(Location::Var(key)) => Ok(key),
            located => Err(Malformed(format!(
                "the {what} field map places the {} at {located:?}, not in the variable data",
                field.name
            ))),
        }
    }
}

/// `FixedMeta`: a record of meta data for each item, its flags first and the offset of its
/// fixed data at 4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FixedMeta<'a> {
    /// The items the header counts, which may differ from those the stream holds.
    pub(super) declared: usize,
    pub(super) items: Vec<&'a [u8]>,
}

impl<'a> FixedMeta<'a> {
    /// The records of `data`, each `item_size` long, as many as the stream holds whole.
    pub(super) fn read(data: &'a [u8], item_size: usize, what: &str) -> Result<Self, Malformed> {
        let declared = Self::header(data, what)?;
        let items = data[FIXED_META_HEADER..].chunks_exact(item_size).collect();
        Ok(FixedMeta { declared, items })
    }

    /// The records of `data` of the first of `item_sizes` that divides the stream into as many
    /// records as `other` holds, else of the one that divides it whole into the fewest more
    /// than the header declares, else of the first, as `FixedMeta`'s size provider chooses.
    pub(super) fn read_sized_by(
        data: &'a [u8],
        item_sizes: &[usize],
        other: usize,
        what: &str,
    ) -> Result<Self, Malformed> {
        let declared = Self::header(data, what)?;
        let available = data.len() - FIXED_META_HEADER;
        let mut chosen = item_sizes[0];
        let mut distance = i64::MIN;
        for &size in item_sizes {
            if !available.is_multiple_of(size) {
                continue;
            }
            if available / size == other {
                chosen = size;
                break;
            }
            let test = declared as i64 * size as i64 - available as i64;
            if test <= 0 && test > distance {
                chosen = size;
                distance = test;
            }
        }
        Self::read(data, chosen, what)
    }

    fn header(data: &[u8], what: &str) -> Result<usize, Malformed> {
        let (Some(magic), Some(count)) = (le::<4>(data, 0), le::<4>(data, 8)) else {
            return Err(Malformed(format!(
                "{what} is shorter than its {FIXED_META_HEADER}-byte header"
            )));
        };
        if u32::from_le_bytes(magic) != MAGIC || data.len() < FIXED_META_HEADER {
            return Err(Malformed(format!(
                "{what} does not start with FixedMeta's magic number"
            )));
        }
        Ok(usize::try_from(i32::from_le_bytes(count)).unwrap_or(0))
    }

    /// The flags of item `index`.
    pub(super) fn flags(&self, index: usize) -> u32 {
        self.items
            .get(index)
            .and_then(|item| le::<4>(item, 0))
            .map_or(0, u32::from_le_bytes)
    }
}

/// How `FixedData` cuts an item's record out of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Sizing {
    /// The longest a record is, or 0 for no limit (`maxExpectedSize`).
    pub(super) max: usize,
    /// The length of a record that runs up to the next one's start (`minSize`).
    pub(super) min: usize,
}

impl Sizing {
    pub(super) const UNLIMITED: Sizing = Sizing { max: 0, min: 0 };
}

/// `FixedData`: each item's record, from where its meta data places it to where the next one
/// starts, or none where the meta data places it outside the stream.
pub(super) fn fixed_data<'a>(
    meta: &FixedMeta<'_>,
    data: &'a [u8],
    sizing: Sizing,
) -> Vec<Option<&'a [u8]>> {
    let offset = |index: usize| {
        meta.items
            .get(index)
            .and_then(|item| le::<4>(item, 4))
            .map(i32::from_le_bytes)
    };
    let count = meta.items.len();
    (0..count)
        .map(|index| {
            let start = usize::try_from(offset(index)?)
                .ok()
                .filter(|&start| start <= data.len())?;
            let available = data.len() - start;
            let mut size = if index + 1 == count {
                available as i64
            } else {
                i64::from(offset(index + 1)?) - start as i64
            };
            if size == 0 {
                size = sizing.min as i64;
            }
            if size < 0 || size > available as i64 {
                size = match sizing.max {
                    0 => available,
                    max => max.min(available),
                } as i64;
            }
            if sizing.max != 0 {
                size = size.min(sizing.max as i64);
            }
            let size = usize::try_from(size).ok().filter(|&size| size > 0)?;
            data.get(start..start + size)
        })
        .collect()
}

/// `VarMeta` and `Var2Data`: the variable data of a class of items, keyed by an item's unique
/// ID and a field's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VarData<'a> {
    offsets: BTreeMap<(i32, u16), usize>,
    items: BTreeSet<i32>,
    data: &'a [u8],
}

impl<'a> VarData<'a> {
    /// The entries of `meta`, each a unique ID, a key and an offset into `data`: in 8 bytes,
    /// the ID in 3, for Project 2000 to 2003, in 12 for Project 2007 and later. The entries
    /// stop where the stream ends.
    pub(super) fn read(
        meta: &[u8],
        data: &'a [u8],
        version: Version,
        what: &str,
    ) -> Result<Self, Malformed> {
        let (Some(magic), Some(count)) = (le::<4>(meta, 0), le::<4>(meta, 8)) else {
            return Err(Malformed(format!(
                "{what}'s VarMeta is shorter than its {VAR_META_HEADER}-byte header"
            )));
        };
        let magic = u32::from_le_bytes(magic);
        let magic_allowed = match version {
            Version::Mpp9 => magic == MAGIC,
            Version::Mpp12 | Version::Mpp14 => magic == MAGIC || magic == 0,
        };
        if !magic_allowed {
            return Err(Malformed(format!(
                "{what}'s VarMeta does not start with its magic number"
            )));
        }
        let count = usize::try_from(i32::from_le_bytes(count)).unwrap_or(0);
        let entry_size = match version {
            Version::Mpp9 => 8,
            Version::Mpp12 | Version::Mpp14 => 12,
        };
        let entries = meta
            .get(VAR_META_HEADER..)
            .unwrap_or_default()
            .chunks_exact(entry_size)
            .take(count);
        let mut offsets = BTreeMap::new();
        let mut items = BTreeSet::new();
        for entry in entries {
            let int = |at: usize| {
                i32::from_le_bytes([entry[at], entry[at + 1], entry[at + 2], entry[at + 3]])
            };
            let (id, key, offset) = match version {
                Version::Mpp9 => (
                    i32::from_le_bytes([entry[0], entry[1], entry[2], 0]),
                    u16::from(entry[3]),
                    int(4),
                ),
                Version::Mpp12 | Version::Mpp14 => {
                    (int(0), u16::from_le_bytes([entry[8], entry[9]]), int(4))
                }
            };
            items.insert(id);
            if let Ok(offset) = usize::try_from(offset) {
                offsets.insert((id, key), offset);
            }
        }
        Ok(VarData {
            offsets,
            items,
            data,
        })
    }

    /// Whether the item of unique ID `id` has any variable data.
    pub(super) fn has(&self, id: i32) -> bool {
        self.items.contains(&id)
    }

    /// The unique IDs of the items with variable data, in order.
    pub(super) fn ids(&self) -> impl Iterator<Item = i32> + '_ {
        self.items.iter().copied()
    }

    /// The value of the field `key` of the item `id`: its length, then its bytes; none where
    /// the length runs past the stream, as `Var2Data` passes such a value over.
    pub(super) fn get(&self, id: i32, key: u16) -> Option<&'a [u8]> {
        let &offset = self.offsets.get(&(id, key))?;
        let length = usize::try_from(i32::from_le_bytes(le::<4>(self.data, offset)?)).ok()?;
        let start = offset + 4;
        self.data.get(start..start.checked_add(length)?)
    }
}
