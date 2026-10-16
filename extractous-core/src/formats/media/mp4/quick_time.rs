//! What `TikaMp4BoxHandler` and `TikaMp4MetaHandler` read of QuickTime metadata: the names of
//! a `keys` box, the values an `ilst` box gives them, the empty edits an edit list starts with,
//! and the key names of a timed metadata track's `mebx` sample entry.

use super::location;
use super::reader::u32_at;
use super::Event;
use crate::formats::pdf::jfloat::float_to_string;
use crate::java;
use crate::java::dtoa::java_double_to_string;

/// The key whose value Tika also reads as the location.
const LOCATION_KEY: &str = "com.apple.quicktime.location.ISO6709";

/// The type of a value in a `data` atom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueType {
    Text,
    SignedInteger,
    UnsignedInteger,
    Float32,
    Float64,
}

impl ValueType {
    fn of(code: u64) -> Option<Self> {
        match code {
            1 => Some(Self::Text),
            21 => Some(Self::SignedInteger),
            22 => Some(Self::UnsignedInteger),
            23 => Some(Self::Float32),
            24 => Some(Self::Float64),
            _ => None,
        }
    }

    /// `decodeValue`: the value as Tika writes it, `None` for a number of a length the type
    /// does not have.
    fn decode(self, value: &[u8]) -> Option<String> {
        match self {
            Self::Text => Some(java::utf8(value)),
            Self::SignedInteger | Self::UnsignedInteger => {
                if value.is_empty() || value.len() > 8 {
                    return None;
                }
                let mut extended = if self == Self::SignedInteger && value[0] & 0x80 != 0 {
                    [0xFF; 8]
                } else {
                    [0; 8]
                };
                extended[8 - value.len()..].copy_from_slice(value);
                Some(if self == Self::SignedInteger {
                    i64::from_be_bytes(extended).to_string()
                } else {
                    u64::from_be_bytes(extended).to_string()
                })
            }
            Self::Float32 => Some(float_to_string(f32::from_be_bytes(value.try_into().ok()?))),
            Self::Float64 => Some(java_double_to_string(f64::from_be_bytes(
                value.try_into().ok()?,
            ))),
        }
    }
}

/// `processQuickTimeKeys`: the names of a `keys` box, up to the first that does not fit.
pub(super) fn keys(payload: &[u8]) -> Vec<String> {
    let mut keys = Vec::new();
    if payload.len() < 8 {
        return keys;
    }
    let count = u32_at(payload, 4);
    let mut pos = 8;
    while (keys.len() as u64) < count && pos + 8 <= payload.len() {
        let size = u32_at(payload, pos);
        if size < 8 || pos as u64 + size > payload.len() as u64 {
            break;
        }
        let end = pos + size as usize;
        keys.push(java::utf8(&payload[pos + 8..end]));
        pos = end;
    }
    keys
}

/// `processQuickTimeItemList`: each value of an `ilst` box under the `mp4:` name of its key,
/// and the location the location key gives, up to the first entry that does not fit.
pub(super) fn item_list(payload: &[u8], keys: &[String], events: &mut Vec<Event<'_>>) {
    let mut pos = 0;
    while pos + 8 <= payload.len() {
        let size = u32_at(payload, pos);
        if size < 8 || pos as u64 + size > payload.len() as u64 {
            return;
        }
        let index = u32_at(payload, pos + 4) as u32 as i32;
        let entry_end = pos + size as usize;
        let data = pos + 8;
        if data + 16 <= entry_end {
            let data_size = u32_at(payload, data);
            let is_data = &payload[data + 4..data + 8] == b"data";
            if is_data && data_size >= 16 && data as u64 + data_size <= entry_end as u64 {
                let key = usize::try_from(index)
                    .ok()
                    .and_then(|index| keys.get(index.checked_sub(1)?));
                let value = &payload[data + 16..data + data_size as usize];
                if let Some((key, value)) = key.zip(
                    ValueType::of(u32_at(payload, data + 8)).and_then(|kind| kind.decode(value)),
                ) {
                    events.push(Event::AddPrefixed(key.clone(), value.clone()));
                    if key == LOCATION_KEY {
                        locate(&value, events);
                    }
                }
            }
        }
        pos = entry_end;
    }
}

/// `addLocation`: the latitude, longitude and altitude an ISO 6709 location gives.
pub(super) fn locate(iso6709: &str, events: &mut Vec<Event<'_>>) {
    let Some(location) = location::parse(iso6709) else {
        return;
    };
    events.push(Event::Set(
        "geo:lat",
        java_double_to_string(location.latitude),
    ));
    events.push(Event::Set(
        "geo:long",
        java_double_to_string(location.longitude),
    ));
    if let Some(altitude) = location.altitude {
        events.push(Event::Set("geo:alt", java_double_to_string(altitude)));
    }
}

/// `processEditList`: the duration of the empty edits an edit list starts with, where there
/// are any.
pub(super) fn leading_empty_edits(payload: &[u8]) -> Option<i64> {
    if payload.len() < 8 {
        return None;
    }
    let long = payload[0] == 1;
    let count = u32_at(payload, 4);
    let entry_size = if long { 20 } else { 12 };
    let mut pos = 8;
    let mut read = 0;
    let mut empty: i64 = 0;
    while read < count && pos + entry_size <= payload.len() {
        let (duration, media_time) = if long {
            let at = |offset: usize| {
                i64::from_be_bytes(
                    payload[pos + offset..pos + offset + 8]
                        .try_into()
                        .expect("the entry holds eight bytes there"),
                )
            };
            (at(0), at(8))
        } else {
            (
                u32_at(payload, pos) as i64,
                i64::from(u32_at(payload, pos + 4) as u32 as i32),
            )
        };
        if media_time != -1 {
            break;
        }
        empty = empty.wrapping_add(duration);
        pos += entry_size;
        read += 1;
    }
    (empty > 0).then_some(empty)
}

/// `parseMebxKeyNames`: the names the `keyd` boxes in the `keys` boxes of a `mebx` sample
/// entry declare.
pub(super) fn mebx_key_names(entry: &[u8], names: &mut Vec<String>) {
    for keys in boxes(entry, 8, entry.len()).filter(|&(kind, ..)| kind == b"keys") {
        let (_, keys_start, keys_end) = keys;
        for (_, key_start, key_end) in boxes(entry, keys_start, keys_end) {
            for (kind, declaration_start, declaration_end) in boxes(entry, key_start, key_end) {
                if kind == b"keyd" && declaration_end - declaration_start > 4 {
                    names.push(java::utf8(&entry[declaration_start + 4..declaration_end]));
                }
            }
        }
    }
}

/// The boxes in `start..end` of `bytes`, each as its type and the range of its body, up to the
/// first that does not fit.
fn boxes(bytes: &[u8], start: usize, end: usize) -> impl Iterator<Item = (&[u8], usize, usize)> {
    let mut pos = start;
    std::iter::from_fn(move || {
        if pos + 8 > end {
            return None;
        }
        let size = u32_at(bytes, pos);
        if size < 8 || pos as u64 + size > end as u64 {
            return None;
        }
        let found = (&bytes[pos + 4..pos + 8], pos + 8, pos + size as usize);
        pos += size as usize;
        Some(found)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_values_as_tika_does() {
        assert_eq!(
            ValueType::SignedInteger.decode(&[0xFF, 0xF3]).as_deref(),
            Some("-13")
        );
        assert_eq!(
            ValueType::UnsignedInteger.decode(&[0xFF, 0xF3]).as_deref(),
            Some("65523")
        );
        assert_eq!(ValueType::SignedInteger.decode(&[0; 9]), None);
        assert_eq!(
            ValueType::Float32.decode(&1.5f32.to_be_bytes()).as_deref(),
            Some("1.5")
        );
        assert_eq!(ValueType::Float64.decode(&[0; 4]), None);
    }

    #[test]
    fn reads_edit_lists_as_tika_does() {
        let mut edits = vec![0, 0, 0, 0, 0, 0, 0, 3];
        for (duration, media_time) in [(600u32, -1i32), (300, -1), (900, 0)] {
            edits.extend_from_slice(&duration.to_be_bytes());
            edits.extend_from_slice(&media_time.to_be_bytes());
            edits.extend_from_slice(&[0, 1, 0, 0]);
        }
        assert_eq!(leading_empty_edits(&edits), Some(900));
        assert_eq!(leading_empty_edits(&edits[..20]), Some(600));
        assert_eq!(leading_empty_edits(&edits[..8]), None);
    }
}
