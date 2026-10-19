//! The JDK's ISO-2022-KR and ISO-2022-CN decoders (`sun.nio.cs.ext.ISO2022_KR` and
//! `ISO2022_CN`), as `new String(bytes, charset)` runs them. Outside a shift-out each byte is
//! its own character, as in ISO-8859-1. Inside one, each pair of bytes, with their high bits
//! set, is a character of the set the shift-out designation names: KS X 1001 (the JDK's EUC-KR
//! table) for ISO-2022-KR; GB 2312 (`GB2312`) or plane 1 of CNS 11643 (`x-EUC-TW`) for
//! ISO-2022-CN, whose single shifts read planes 2 and 3 and whose every escape designates GB 2312
//! first. A pair the set does not map, an escape the decoder does not know, and input that ends
//! inside an escape or a pair are each replaced by one U+FFFD. The JDK decodes its
//! `x-ISO-2022-CN-CNS` and `x-ISO-2022-CN-GB` with the ISO-2022-CN decoder too.

use super::multibyte::MultiByte;

const ESC: u8 = 0x1B;
const SO: u8 = 0x0E;
const SI: u8 = 0x0F;
const SS2: u8 = 0x4E;
const SS3: u8 = 0x4F;
const MSB: u8 = 0x80;
const KR_DESIGNATION: &[u8] = b"$)C";

/// An ISO-2022 charset this port decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Variant {
    Kr,
    Cn,
}

impl Variant {
    pub(super) fn from_name(name: &str) -> Option<Variant> {
        Some(match name {
            "ISO-2022-KR" => Variant::Kr,
            "ISO-2022-CN" | "x-ISO-2022-CN-CNS" | "x-ISO-2022-CN-GB" => Variant::Cn,
            _ => return None,
        })
    }
}

/// The set ISO-2022-CN's shift-out reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CnDesignation {
    Gb2312,
    Cns1,
}

enum Sets {
    Kr {
        ks_x1001: &'static MultiByte,
    },
    Cn {
        gb2312: &'static MultiByte,
        cns: &'static MultiByte,
        designation: CnDesignation,
    },
}

/// What one step of the decoder writes.
enum Out {
    Nothing,
    Char(char),
    Replacement,
}

/// A decoder and the state its escapes and shifts leave it in.
pub(super) struct Decoder {
    sets: Sets,
    shifted_out: bool,
}

impl Decoder {
    pub(super) fn new(variant: Variant, table: impl Fn(&str) -> &'static MultiByte) -> Self {
        let sets = match variant {
            Variant::Kr => Sets::Kr {
                ks_x1001: table("EUC-KR"),
            },
            Variant::Cn => Sets::Cn {
                gb2312: table("GB2312"),
                cns: table("x-EUC-TW"),
                designation: CnDesignation::Gb2312,
            },
        };
        Decoder {
            sets,
            shifted_out: false,
        }
    }

    /// The text of the characters at the start of `bytes`, the rest of the input, that end at or
    /// past `at_least` bytes, or at its end, and the bytes they take.
    pub(super) fn decode_prefix(&mut self, bytes: &[u8], at_least: usize) -> (String, usize) {
        let mut text = String::with_capacity(at_least.min(bytes.len()));
        let mut at = 0;
        while at < at_least && at < bytes.len() {
            let (out, length) = self.step(&bytes[at..]);
            match out {
                Out::Nothing => {}
                Out::Char(c) => text.push(c),
                Out::Replacement => text.push(char::REPLACEMENT_CHARACTER),
            }
            at += length;
        }
        (text, at)
    }

    fn step(&mut self, bytes: &[u8]) -> (Out, usize) {
        match bytes[0] {
            SO => {
                self.shifted_out = true;
                (Out::Nothing, 1)
            }
            SI => {
                self.shifted_out = false;
                (Out::Nothing, 1)
            }
            ESC => match &mut self.sets {
                Sets::Kr { .. } => Self::kr_escape(bytes),
                Sets::Cn {
                    cns, designation, ..
                } => {
                    *designation = CnDesignation::Gb2312;
                    Self::cn_escape(cns, designation, bytes)
                }
            },
            byte if !self.shifted_out => (Out::Char(char::from(byte)), 1),
            first => {
                let Some(&second) = bytes.get(1) else {
                    return (Out::Replacement, bytes.len());
                };
                let (first, second) = (first | MSB, second | MSB);
                let c = match &self.sets {
                    Sets::Kr { ks_x1001 } => ks_x1001.decode_double(first, second),
                    Sets::Cn {
                        gb2312,
                        designation: CnDesignation::Gb2312,
                        ..
                    } => gb2312.decode_double(first, second),
                    Sets::Cn {
                        cns,
                        designation: CnDesignation::Cns1,
                        ..
                    } => cns.decode_cns(0, first, second),
                };
                (c.map_or(Out::Replacement, Out::Char), 2)
            }
        }
    }

    /// An ISO-2022-KR escape: its designation, or a single shift, which the decoder replaces
    /// whatever follows.
    fn kr_escape(bytes: &[u8]) -> (Out, usize) {
        if bytes.len() <= KR_DESIGNATION.len() {
            return (Out::Replacement, bytes.len());
        }
        if bytes[1..].starts_with(KR_DESIGNATION) {
            return (Out::Nothing, 1 + KR_DESIGNATION.len());
        }
        match bytes[1] {
            SS2 | SS3 => (Out::Replacement, 4),
            _ => (Out::Replacement, 2),
        }
    }

    /// An ISO-2022-CN escape: a designation of the shift-out set, of the single shifts' planes,
    /// which the decoder accepts and ignores, or a single shift.
    fn cn_escape(cns: &MultiByte, designation: &mut CnDesignation, bytes: &[u8]) -> (Out, usize) {
        let truncated = (Out::Replacement, bytes.len());
        let Some(&second) = bytes.get(1) else {
            return truncated;
        };
        if second & MSB != 0 {
            return (Out::Replacement, 2);
        }
        match second {
            b'$' => {
                let Some(&third) = bytes.get(2) else {
                    return truncated;
                };
                let expected_final = match third {
                    b'A' => return (Out::Nothing, 3),
                    b')' => None,
                    b'*' => Some(b'H'),
                    b'+' => Some(b'I'),
                    _ => return (Out::Replacement, 3),
                };
                let Some(&last) = bytes.get(3) else {
                    return truncated;
                };
                match (expected_final, last) {
                    (None, b'A') => (Out::Nothing, 4),
                    (None, b'G') => {
                        *designation = CnDesignation::Cns1;
                        (Out::Nothing, 4)
                    }
                    (Some(expected), last) if last == expected => (Out::Nothing, 4),
                    _ => (Out::Replacement, 4),
                }
            }
            SS2 | SS3 => {
                let [_, _, b3, b4, ..] = *bytes else {
                    return truncated;
                };
                let plane = if second == SS2 { 1 } else { 2 };
                let c = cns.decode_cns(plane, b3 | MSB, b4 | MSB);
                (c.map_or(Out::Replacement, Out::Char), 4)
            }
            _ => (Out::Replacement, 2),
        }
    }
}
