//! Whether a drawing group holds pictures: the Escher records of its data, walked as POI's
//! containers hold them, for a BSE record with its blip, which Tika's `findPictures` parses as
//! an embedded image.

/// The type of a BSE record, which stores a picture of the drawing group.
const BSE: u16 = 0xF007;
/// The length of a BSE record's fields before its blip.
const BSE_FIELDS: i64 = 36;
const HEADER: usize = 8;

/// Whether the Escher records of `data` hold a picture; `None` where they run past the data,
/// which this port does not read as POI does.
pub(super) fn has_pictures(data: &[u8]) -> Option<bool> {
    let mut position = 0;
    while position < data.len() {
        let (found, length) = walk(data, position)?;
        if found {
            return Some(true);
        }
        position += length;
    }
    Some(false)
}

/// The record at `position`: whether it or a record it contains is a BSE record with its blip,
/// and its length with its header.
fn walk(data: &[u8], position: usize) -> Option<(bool, usize)> {
    let header = data.get(position..position + HEADER)?;
    let options = u16::from_le_bytes([header[0], header[1]]);
    let kind = u16::from_le_bytes([header[2], header[3]]);
    let length = i64::from(i32::from_le_bytes([
        header[4], header[5], header[6], header[7],
    ]));
    let end = usize::try_from(length)
        .ok()?
        .checked_add(position + HEADER)?;
    if end > data.len() {
        return None;
    }
    if kind == BSE {
        return Some((length > BSE_FIELDS, end - position));
    }
    if options & 0x000F == 0x000F {
        let mut child = position + HEADER;
        while child < end {
            let (found, child_length) = walk(&data[..end], child)?;
            if found {
                return Some((true, end - position));
            }
            child += child_length;
        }
    }
    Some((false, end - position))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(options: u16, kind: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = options.to_le_bytes().to_vec();
        bytes.extend(kind.to_le_bytes());
        bytes.extend((body.len() as i32).to_le_bytes());
        bytes.extend(body);
        bytes
    }

    #[test]
    fn finds_bse_records_with_their_blips() {
        let bse = record(0x0062, BSE, &[0; 36 + 8]);
        let store = record(0x000F, 0xF001, &bse);
        let group = record(0x000F, 0xF000, &store);
        assert_eq!(has_pictures(&group), Some(true));
        let empty = record(0x0062, BSE, &[0; 36]);
        let group = record(0x000F, 0xF000, &record(0x000F, 0xF001, &empty));
        assert_eq!(has_pictures(&group), Some(false));
        assert_eq!(has_pictures(&group[..10]), None);
    }
}
