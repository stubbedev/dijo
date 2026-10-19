//! Ports of Tika's structural encoding checks (`StructuralEncodingRules`,
//! `WideUnicodeDetector`): what the bytes alone prove or rule out, before any statistics.

const ESC: u8 = 0x1B;
const MIN_COLUMN_EVIDENCE_PROBE: usize = 16;
const COLUMN_SAMPLE: usize = 4096;
const WIDE_SAMPLE: usize = 512;
const UTF32_MIN_VALID: usize = 2;

/// How the bytes read as UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Utf8Stats {
    /// Malformed sequences.
    pub errors: usize,
    /// Well-formed multi-byte sequences.
    pub sequences: usize,
    /// Whether a sequence cut off by the end of the bytes is already malformed.
    pub truncated_tail_invalid: bool,
}

/// The verdict of the UTF-8 grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Utf8Verdict {
    Likely,
    Ambiguous,
    Not,
}

impl Utf8Stats {
    pub(super) fn verdict(self) -> Utf8Verdict {
        if self.errors > 0 || self.truncated_tail_invalid {
            Utf8Verdict::Not
        } else if self.sequences > 0 {
            Utf8Verdict::Likely
        } else {
            Utf8Verdict::Ambiguous
        }
    }
}

fn is_continuation(b: u8) -> bool {
    (0x80..=0xBF).contains(&b)
}

pub(super) fn utf8_stats(bytes: &[u8]) -> Utf8Stats {
    let mut stats = Utf8Stats {
        errors: 0,
        sequences: 0,
        truncated_tail_invalid: false,
    };
    let end = bytes.len();
    let mut i = 0;
    while i < end {
        let b = bytes[i];
        if b < 0x80 {
            i += 1;
            continue;
        }
        let length = match b {
            0xF8..=0xFF | 0x80..=0xBF | 0xC0 | 0xC1 => {
                stats.errors += 1;
                i += 1;
                continue;
            }
            0xF0..=0xF7 => 4,
            0xE0..=0xEF => 3,
            _ => 2,
        };
        if end - i < length {
            stats.truncated_tail_invalid = bytes[i + 1..].iter().any(|&cb| !is_continuation(cb));
            break;
        }
        if !bytes[i + 1..i + length]
            .iter()
            .all(|&cb| is_continuation(cb))
        {
            stats.errors += 1;
            i += length;
            continue;
        }
        let malformed = match length {
            3 => {
                let cp = (u32::from(b & 0x0F) << 12)
                    | (u32::from(bytes[i + 1] & 0x3F) << 6)
                    | u32::from(bytes[i + 2] & 0x3F);
                cp < 0x0800 || (0xD800..=0xDFFF).contains(&cp)
            }
            4 => {
                let cp = (u32::from(b & 0x07) << 18)
                    | (u32::from(bytes[i + 1] & 0x3F) << 12)
                    | (u32::from(bytes[i + 2] & 0x3F) << 6)
                    | u32::from(bytes[i + 3] & 0x3F);
                !(0x1_0000..=0x10_FFFF).contains(&cp)
            }
            _ => false,
        };
        if malformed {
            stats.errors += 1;
        } else {
            stats.sequences += 1;
        }
        i += length;
    }
    stats
}

/// The length of a well-formed UTF-8 sequence starting at `i`, or 0.
pub(super) fn utf8_sequence_length(bytes: &[u8], i: usize) -> usize {
    let length = match bytes[i] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return 0,
    };
    match bytes.get(i + 1..i + length) {
        Some(tail) if tail.iter().all(|&b| is_continuation(b)) => length,
        _ => 0,
    }
}

/// Whether the bytes hold a complete UTF-8 sequence of three or four bytes, proof a short or
/// markup-heavy probe is UTF-8 rather than a two-byte East Asian charset.
pub(super) fn has_wide_utf8_sequence(bytes: &[u8]) -> bool {
    (0..bytes.len()).any(|i| {
        let length = match bytes[i] {
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return false,
        };
        bytes
            .get(i + 1..i + length)
            .is_some_and(|tail| tail.iter().all(|&b| is_continuation(b)))
    })
}

/// The ISO-2022 charset an escape sequence designates, by its first `ESC $`.
pub(super) fn iso2022(bytes: &[u8]) -> Option<&'static str> {
    let end = bytes.len();
    for i in 0..end.saturating_sub(1) {
        if bytes[i] != ESC || bytes[i + 1] != 0x24 {
            continue;
        }
        let Some(&designation) = bytes.get(i + 2) else {
            return Some("ISO-2022-JP");
        };
        return Some(match designation {
            0x29 => match bytes.get(i + 3) {
                Some(0x43) => "ISO-2022-KR",
                Some(0x41 | 0x47) => "ISO-2022-CN",
                _ => "ISO-2022-JP",
            },
            0x2A => "ISO-2022-CN",
            _ => "ISO-2022-JP",
        });
    }
    None
}

/// Whether the even and odd bytes differ in variety as text in a two-byte Unicode encoding
/// does: one column holds three times the distinct values of the other. As the dijo native
/// image substitutes Tika's rule (`Target_org_apache_tika_ml_chardetect_StructuralEncodingRules`),
/// only bytes holding a NUL count: UTF-16 without a byte order mark holds one for every ASCII
/// character, while a legacy double-byte encoding such as Shift_JIS, whose columns differ alike,
/// holds none.
pub(super) fn has_two_byte_column_asymmetry_evidence(bytes: &[u8]) -> bool {
    if bytes.len() < MIN_COLUMN_EVIDENCE_PROBE || !bytes.contains(&0) {
        return false;
    }
    let mut seen = [[false; 256]; 2];
    let mut distinct = [0usize; 2];
    for (i, &b) in bytes.iter().take(COLUMN_SAMPLE).enumerate() {
        let column = i & 1;
        if !seen[column][usize::from(b)] {
            seen[column][usize::from(b)] = true;
            distinct[column] += 1;
        }
    }
    let min = distinct[0].min(distinct[1]);
    let max = distinct[0].max(distinct[1]);
    min > 0 && max >= min * 3
}

/// What the bytes say of the wide Unicode encodings: UTF-32 when every four-byte group of the
/// sample is a code point in one byte order, and whether the surrogates of each UTF-16 byte
/// order pair up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Wide {
    pub utf32: Option<&'static str>,
    pub invalid_utf16_be: bool,
    pub invalid_utf16_le: bool,
}

pub(super) fn wide(bytes: &[u8]) -> Wide {
    let none = Wide {
        utf32: None,
        invalid_utf16_be: false,
        invalid_utf16_le: false,
    };
    if bytes.len() < 8 {
        return none;
    }
    if let Some(utf32) = utf32(bytes) {
        return Wide {
            utf32: Some(utf32),
            ..none
        };
    }
    let sample = (bytes.len().min(WIDE_SAMPLE) / 2) * 2;
    if sample < 8 {
        return none;
    }
    let mut be = SurrogatePairing::default();
    let mut le = SurrogatePairing::default();
    for pair in bytes[..sample].as_chunks::<2>().0 {
        be.unit(pair[0]);
        le.unit(pair[1]);
    }
    Wide {
        utf32: None,
        invalid_utf16_be: be.invalid(),
        invalid_utf16_le: le.invalid(),
    }
}

fn utf32(bytes: &[u8]) -> Option<&'static str> {
    let sample = (bytes.len().min(WIDE_SAMPLE) / 4) * 4;
    if sample < 8 {
        return None;
    }
    let valid = |cp: u32| cp <= 0x10_FFFF && !(0xD800..=0xDFFF).contains(&cp);
    let mut valid_be = 0;
    let mut valid_le = 0;
    let groups = sample / 4;
    for &unit in bytes[..sample].as_chunks::<4>().0 {
        valid_be += usize::from(valid(u32::from_be_bytes(unit)));
        valid_le += usize::from(valid(u32::from_le_bytes(unit)));
    }
    if valid_be == groups && valid_be >= UTF32_MIN_VALID {
        Some("UTF-32BE")
    } else if valid_le == groups && valid_le >= UTF32_MIN_VALID {
        Some("UTF-32LE")
    } else {
        None
    }
}

/// Whether the high bytes of one UTF-16 byte order pair their surrogates.
#[derive(Debug, Default)]
struct SurrogatePairing {
    awaiting_low: bool,
    invalid: bool,
}

impl SurrogatePairing {
    fn unit(&mut self, high: u8) {
        if self.invalid {
            return;
        }
        let low = (0xDC..=0xDF).contains(&high);
        if self.awaiting_low {
            if low {
                self.awaiting_low = false;
            } else {
                self.invalid = true;
            }
        } else if (0xD8..=0xDB).contains(&high) {
            self.awaiting_low = true;
        } else if low {
            self.invalid = true;
        }
    }

    fn invalid(&self) -> bool {
        self.invalid || self.awaiting_low
    }
}
