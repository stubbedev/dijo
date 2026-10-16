//! Tika's `Mp4SampleEntries` and what `TikaMp4SoundHandler` and `TikaMp4VideoHandler` read of
//! each entry of a sample description box: its format, the original format of a protected
//! entry, and the average bit rate its `esds` or `btrt` box gives.

use super::reader::u32_at;

/// `SAMPLE_ENTRY_FIELDS`: the reserved bytes and data reference index every entry starts with.
const SAMPLE_ENTRY_FIELDS: usize = 8;
/// The size of a visual sample entry's fields, before its boxes.
const VISUAL_ENTRY_SIZE: usize = 78;
/// How deep `findEsdsAverageBitRate` looks into nested `wave` boxes.
const MAX_BOX_DEPTH: u32 = 10;

/// What Tika sets of one sample entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SampleEntry {
    /// Whether the entry is a protected one, which Tika records for sound only.
    pub(super) protected: bool,
    pub(super) four_cc: Option<String>,
    /// A positive average bit rate.
    pub(super) bit_rate: Option<i32>,
}

/// `Mp4SampleEntries.walk`: each entry of the sample description `b` as its printable format
/// and the range of its fields and boxes, up to the first that does not fit.
pub(super) fn walk(b: &[u8], mut visit: impl FnMut(Option<String>, usize, usize)) {
    if b.len() < 8 {
        return;
    }
    let count = u32_at(b, 4);
    let mut pos = 8;
    let mut visited = 0;
    while visited < count && pos + 8 <= b.len() {
        let mut size = u32_at(b, pos);
        let mut header = 8;
        if size == 1 {
            if pos + 16 > b.len() {
                return;
            }
            size = u32_at(b, pos + 8) << 32 | u32_at(b, pos + 12);
            header = 16;
        } else if size == 0 {
            size = (b.len() - pos) as u64;
        }
        if size < (header + SAMPLE_ENTRY_FIELDS) as u64 || size > (b.len() - pos) as u64 {
            return;
        }
        let end = pos + size as usize;
        visit(printable_four_cc(b, pos + 4), pos + header, end);
        pos = end;
        visited += 1;
    }
}

/// `TikaMp4SoundHandler.sampleEntry`: the audio sample entry in `start..end` of `b`, its boxes
/// after the fields its version gives it.
pub(super) fn sound(four_cc: Option<String>, b: &[u8], start: usize, end: usize) -> SampleEntry {
    let children = (start + SAMPLE_ENTRY_FIELDS + 2 <= end).then(|| {
        let version = u16::from_be_bytes([
            b[start + SAMPLE_ENTRY_FIELDS],
            b[start + SAMPLE_ENTRY_FIELDS + 1],
        ]);
        start
            + match version {
                1 => 44,
                2 => 64,
                _ => 28,
            }
    });
    let protected = is_protected(four_cc.as_deref());
    let original = children
        .filter(|_| protected)
        .and_then(|children| original_format(b, children, end));
    SampleEntry {
        protected,
        four_cc: original.or(four_cc),
        bit_rate: children
            .map(|children| esds_average_bit_rate(b, children, end, 0))
            .filter(|&bit_rate| bit_rate > 0),
    }
}

/// `TikaMp4VideoHandler.sampleEntry`: the visual sample entry in `start..end` of `b`.
pub(super) fn video(four_cc: Option<String>, b: &[u8], start: usize, end: usize) -> SampleEntry {
    let children = start + VISUAL_ENTRY_SIZE;
    let protected = is_protected(four_cc.as_deref());
    let original = protected
        .then(|| original_format(b, children, end))
        .flatten();
    SampleEntry {
        protected,
        four_cc: original.or(four_cc),
        bit_rate: Some(btrt_average_bit_rate(b, children, end)).filter(|&bit_rate| bit_rate > 0),
    }
}

/// `isProtected`: the formats of encrypted entries.
fn is_protected(four_cc: Option<&str>) -> bool {
    matches!(four_cc, Some("drms" | "enca" | "encv" | "drmi"))
}

/// `originalFormat`: the format the `frma` box in the `sinf` box among the boxes from `pos`
/// names.
fn original_format(b: &[u8], pos: usize, end: usize) -> Option<String> {
    let sinf = find_box(b, pos, end, b"sinf")?;
    let sinf_end = box_end(b, sinf, end)?;
    let frma = find_box(b, sinf + 8, sinf_end, b"frma")?;
    if box_end(b, frma, sinf_end)? < frma + 12 {
        return None;
    }
    printable_four_cc(b, frma + 8)
}

/// `findBox`: where the first box of `kind` among the boxes from `pos` starts, up to the first
/// that does not fit.
fn find_box(b: &[u8], mut pos: usize, end: usize, kind: &[u8; 4]) -> Option<usize> {
    while pos + 8 <= end {
        let next = box_end(b, pos, end)?;
        if &b[pos + 4..pos + 8] == kind {
            return Some(pos);
        }
        pos = next;
    }
    None
}

/// `boxEnd`: where the box at `pos` ends, if it fits before `end`.
fn box_end(b: &[u8], pos: usize, end: usize) -> Option<usize> {
    let size = u32_at(b, pos);
    (size >= 8 && size <= (end - pos) as u64).then(|| pos + size as usize)
}

/// `printableFourCC`: the format without its trailing spaces, if it is printable ASCII and not
/// blank.
fn printable_four_cc(b: &[u8], pos: usize) -> Option<String> {
    let field = &b[pos..pos + 4];
    let length = field.iter().rposition(|&byte| byte != b' ')? + 1;
    let printable = &field[..length];
    printable
        .iter()
        .all(|byte| (0x20..=0x7E).contains(byte))
        .then(|| printable.iter().map(|&byte| char::from(byte)).collect())
}

/// `findEsdsAverageBitRate`: the average bit rate of the first `esds` box among the boxes from
/// `pos`, or in a `wave` box among them; 0 where there is none.
fn esds_average_bit_rate(b: &[u8], mut pos: usize, end: usize, depth: u32) -> i32 {
    if depth > MAX_BOX_DEPTH {
        return 0;
    }
    while pos + 8 <= end {
        let size = u32_at(b, pos);
        if size < 8 || size > (end - pos) as u64 {
            return 0;
        }
        let box_end = pos + size as usize;
        match &b[pos + 4..pos + 8] {
            b"esds" => return esds_descriptor_bit_rate(b, pos + 8, box_end),
            b"wave" => {
                let nested = esds_average_bit_rate(b, pos + 8, box_end, depth + 1);
                if nested > 0 {
                    return nested;
                }
            }
            _ => {}
        }
        pos = box_end;
    }
    0
}

/// `readEsdsAverageBitRate`: the average bit rate of the decoder configuration in the
/// elementary stream descriptor of the `esds` box in `pos..end`; 0 where it is not there.
fn esds_descriptor_bit_rate(b: &[u8], pos: usize, end: usize) -> i32 {
    let mut pos = pos + 4;
    if pos >= end || b[pos] != 0x03 {
        return 0;
    }
    pos = skip_descriptor_length(b, pos + 1);
    if pos + 3 > end {
        return 0;
    }
    let flags = b[pos + 2];
    pos += 3;
    if flags & 0x80 != 0 {
        pos += 2;
    }
    if flags & 0x40 != 0 {
        if pos >= end {
            return 0;
        }
        pos += 1 + usize::from(b[pos]);
    }
    if flags & 0x20 != 0 {
        pos += 2;
    }
    if pos >= end || b[pos] != 0x04 {
        return 0;
    }
    pos = skip_descriptor_length(b, pos + 1) + 9;
    if pos + 4 > end {
        return 0;
    }
    positive_bit_rate(u32_at(b, pos))
}

/// `skipDescriptorLength`: past a descriptor's length, whose bytes but the last have the high
/// bit set.
fn skip_descriptor_length(b: &[u8], mut pos: usize) -> usize {
    while pos < b.len() && b[pos] & 0x80 != 0 {
        pos += 1;
    }
    pos + 1
}

/// `findBtrtAverageBitRate`: the average bit rate of the first `btrt` box among the boxes from
/// `pos`; 0 where there is none.
fn btrt_average_bit_rate(b: &[u8], mut pos: usize, end: usize) -> i32 {
    while pos + 8 <= end {
        let size = u32_at(b, pos);
        if size < 8 || size > (end - pos) as u64 {
            return 0;
        }
        if &b[pos + 4..pos + 8] == b"btrt" && pos + 20 <= end {
            return positive_bit_rate(u32_at(b, pos + 16));
        }
        pos += size as usize;
    }
    0
}

/// A bit rate as an `int`, 0 where it is none or does not fit.
fn positive_bit_rate(bit_rate: u64) -> i32 {
    i32::try_from(bit_rate).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut boxed = u32::try_from(body.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        boxed.extend_from_slice(kind);
        boxed.extend_from_slice(body);
        boxed
    }

    #[test]
    fn reads_sample_entries_as_tika_does() {
        let esds = boxed(
            b"esds",
            &[
                0, 0, 0, 0, 0x03, 0x80, 0x19, 0, 1, 0xC0, 0xAA, 0xBB, 2, b'h', b'i', 0x04, 0x11,
                0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0xF4,
            ],
        );
        let frma = boxed(b"frma", b"mp4a");
        let sinf = boxed(b"sinf", &frma);
        let mut fields = vec![0u8; 28];
        fields.extend_from_slice(&sinf);
        fields.extend_from_slice(&esds);
        let entry = boxed(b"enca", &fields);
        let mut description = vec![0, 0, 0, 0, 0, 0, 0, 1];
        description.extend_from_slice(&entry);
        let mut entries = Vec::new();
        walk(&description, |four_cc, start, end| {
            entries.push(sound(four_cc, &description, start, end));
        });
        assert_eq!(
            entries,
            [SampleEntry {
                protected: true,
                four_cc: Some("mp4a".to_owned()),
                bit_rate: Some(500),
            }]
        );
        assert_eq!(printable_four_cc(b"ac-3", 0).as_deref(), Some("ac-3"));
        assert_eq!(printable_four_cc(b"ab  ", 0).as_deref(), Some("ab"));
        assert_eq!(printable_four_cc(b"    ", 0), None);
        assert_eq!(printable_four_cc(b"a\x01bc", 0), None);
        assert_eq!(positive_bit_rate(0x8000_0000), 0);
    }
}
