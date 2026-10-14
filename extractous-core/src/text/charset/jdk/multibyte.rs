//! The JDK's East Asian multi-byte decoders, ported: the decoder loops of `DoubleByte`, `HKSCS`,
//! `EUC_JP`, `EUC_TW` and `GB18030`, around tables of what each charset's own decoder makes of
//! each byte, each pair of bytes and each longer sequence (`models/java-charsets.bin`). Decoding
//! matches `new String(bytes, charset)`, replacement characters included, and the strict decoding
//! of a first character matches the JDK's results, which differ by family in how far past the
//! first character a decoder looks before it finds no room for the next.

use std::io::Read;
use std::sync::OnceLock;

use flate2::read::ZlibDecoder;

const ERROR: u32 = 0x8000_0000;
const MORE: u32 = 0xC000_0000;
const UNUSED: u32 = 0xFFFF_FFFF;
const LEADS: usize = 0x80;
const GB18030_SUPPLEMENTARY: std::ops::Range<u32> = 0x2E248..0x12E248;
const GB18030_SUPPLEMENTARY_BASE: u32 = 0x1E248;

/// The decoder loop a charset's JDK decoder runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Family {
    /// `DoubleByte.Decoder`: single bytes and pairs, stopping as soon as the output is full.
    DoubleByte,
    /// `HKSCS.Decoder`: single bytes and pairs, some of them supplementary characters, decoding
    /// the next character before it finds no room for it.
    Hkscs,
    /// `EUC_JP.Decoder`: pairs and JIS X 0212's three-byte sequences, decoding the next character
    /// before it finds no room for it.
    EucJp,
    /// `EUC_TW.Decoder`: pairs and the four-byte sequences of the CNS planes, decoding the next
    /// character before it finds no room for it.
    EucTw,
    /// `GB18030.Decoder`: pairs and four-byte sequences, checking the form of the next character
    /// but not what it maps to before it finds no room for it.
    Gb18030,
}

impl Family {
    pub(super) fn from_kind(kind: u8) -> Option<Family> {
        Some(match kind {
            2 => Family::DoubleByte,
            3 => Family::Hkscs,
            4 => Family::EucJp,
            5 => Family::EucTw,
            6 => Family::Gb18030,
            _ => return None,
        })
    }

    fn longer_tables(self) -> std::ops::RangeInclusive<usize> {
        match self {
            Family::DoubleByte | Family::Hkscs => 0..=0,
            Family::EucJp | Family::Gb18030 => 1..=1,
            Family::EucTw => 1..=usize::MAX,
        }
    }
}

/// A multi-byte charset whose tables inflate the first time it decodes.
pub(super) struct MultiByte {
    family: Family,
    name: String,
    compressed: &'static [u8],
    tables: OnceLock<Tables>,
}

/// What a charset's strict decoder makes of the bytes so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    Char(char),
    Error(usize),
    More,
}

struct Tables {
    singles: Vec<u32>,
    pairs: Vec<u32>,
    longer: Vec<Vec<u32>>,
}

/// One step of a decoder loop at the start of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Char {
        length: usize,
        c: char,
    },
    /// Malformed or unmappable input of a length.
    Malformed(usize),
    /// A GB18030 four-byte sequence of a valid form that maps to no character, which the decoder
    /// only finds out after it has checked for room.
    Unassigned(usize),
    /// The input ends inside a character.
    Truncated,
}

impl MultiByte {
    pub(super) fn new(family: Family, name: String, compressed: &'static [u8]) -> Self {
        MultiByte {
            family,
            name,
            compressed,
            tables: OnceLock::new(),
        }
    }

    fn tables(&self) -> &Tables {
        self.tables.get_or_init(|| {
            Tables::inflate(self.family, self.compressed).unwrap_or_else(|e| {
                panic!("The embedded JDK tables of {} are invalid: {e}", self.name)
            })
        })
    }

    /// The text `new String(bytes, charset)` gives: each malformed or unmappable input, and input
    /// that ends inside a character, replaced by one U+FFFD.
    pub(super) fn decode(&self, bytes: &[u8]) -> String {
        let tables = self.tables();
        let mut text = String::with_capacity(bytes.len());
        let mut at = 0;
        while at < bytes.len() {
            match self.step(tables, &bytes[at..]) {
                Step::Char { length, c } => {
                    text.push(c);
                    at += length;
                }
                Step::Malformed(length) | Step::Unassigned(length) => {
                    text.push(char::REPLACEMENT_CHARACTER);
                    at += length;
                }
                Step::Truncated => {
                    text.push(char::REPLACEMENT_CHARACTER);
                    break;
                }
            }
        }
        text
    }

    /// Whether the JDK's strict decoder, given `window` as the whole input and room for one
    /// UTF-16 unit, reports an error, and otherwise how many bytes it consumed.
    pub(super) fn decode_first(&self, window: &[u8]) -> Result<usize, ()> {
        let tables = self.tables();
        let Step::Char { length, c } = self.step(tables, window) else {
            return Err(());
        };
        if c.len_utf16() > 1 {
            return Ok(0);
        }
        if self.family == Family::DoubleByte || length == window.len() {
            return Ok(length);
        }
        match self.step(tables, &window[length..]) {
            Step::Char { .. } | Step::Unassigned(_) => Ok(length),
            Step::Malformed(_) | Step::Truncated => Err(()),
        }
    }

    fn step(&self, tables: &Tables, bytes: &[u8]) -> Step {
        let first = bytes[0];
        match cell(tables.singles[usize::from(first)]) {
            Cell::Char(c) => return Step::Char { length: 1, c },
            Cell::Error(length) => return Step::Malformed(length),
            Cell::More => {}
        }
        let Some(&second) = bytes.get(1) else {
            return Step::Truncated;
        };
        match cell(tables.pairs[pair_index(first, second)]) {
            Cell::Char(c) => Step::Char { length: 2, c },
            Cell::Error(length) => Step::Malformed(length),
            Cell::More => self.longer(tables, bytes),
        }
    }

    fn longer(&self, tables: &Tables, bytes: &[u8]) -> Step {
        match self.family {
            Family::DoubleByte | Family::Hkscs => {
                unreachable!("{} has no sequences longer than two bytes", self.name)
            }
            Family::EucJp => match bytes {
                [_, b1, b2, ..] => from_cell(tables.longer[0][square_index(*b1, *b2)], 3),
                _ => Step::Truncated,
            },
            Family::EucTw => match bytes {
                [_, plane, b1, b2, ..] => match cell(tables.longer[0][usize::from(*plane)]) {
                    Cell::Error(length) => Step::Malformed(length),
                    Cell::Char(index) => {
                        from_cell(tables.longer[1 + index as usize][square_index(*b1, *b2)], 4)
                    }
                    Cell::More => unreachable!("an EUC-TW plane needs more bytes"),
                },
                _ => Step::Truncated,
            },
            Family::Gb18030 => match bytes {
                [b0, b1, b2, b3, ..] => {
                    if !(0x81..=0xFE).contains(b2) {
                        return Step::Malformed(3);
                    }
                    if !(0x30..=0x39).contains(b3) {
                        return Step::Malformed(4);
                    }
                    let index = ((u32::from(*b0 - 0x81) * 10 + u32::from(*b1 - 0x30)) * 126
                        + u32::from(*b2 - 0x81))
                        * 10
                        + u32::from(*b3 - 0x30);
                    let bmp = &tables.longer[0];
                    let c = match bmp.get(index as usize) {
                        Some(&unit) => char::from_u32(unit),
                        None if GB18030_SUPPLEMENTARY.contains(&index) => {
                            char::from_u32(index - GB18030_SUPPLEMENTARY_BASE)
                        }
                        None => None,
                    };
                    c.map_or(Step::Unassigned(4), |c| Step::Char { length: 4, c })
                }
                _ => Step::Truncated,
            },
        }
    }
}

fn from_cell(raw: u32, length: usize) -> Step {
    match cell(raw) {
        Cell::Char(c) => Step::Char { length, c },
        Cell::Error(length) => Step::Malformed(length),
        Cell::More => unreachable!("a {length}-byte sequence needs more bytes"),
    }
}

fn pair_index(first: u8, second: u8) -> usize {
    usize::from(first - 0x80) * 256 + usize::from(second)
}

fn square_index(b1: u8, b2: u8) -> usize {
    usize::from(b1) * 256 + usize::from(b2)
}

/// A cell the tables were checked to hold when they inflated.
fn cell(raw: u32) -> Cell {
    match raw {
        MORE => Cell::More,
        UNUSED => unreachable!("a decoder read a cell its tables leave unused"),
        _ if raw & ERROR != 0 => Cell::Error((raw & !ERROR) as usize),
        _ => Cell::Char(char::from_u32(raw).expect("the tables hold no surrogates")),
    }
}

fn valid_cell(raw: u32) -> bool {
    match raw {
        MORE | UNUSED => true,
        _ if raw & MORE == ERROR => (1..=4).contains(&(raw & !ERROR)),
        _ => char::from_u32(raw).is_some(),
    }
}

impl Tables {
    fn inflate(family: Family, compressed: &[u8]) -> Result<Tables, String> {
        let mut bytes = Vec::new();
        ZlibDecoder::new(compressed)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("they do not inflate: {e}"))?;
        let mut at = 0usize;
        let mut take = |length: usize| -> Result<&[u8], String> {
            let slice = bytes
                .get(at..at + length)
                .ok_or_else(|| format!("truncated at byte {at}"))?;
            at += length;
            Ok(slice)
        };
        let count = usize::from(take(1)?[0]);
        let mut tables = Vec::with_capacity(count);
        for _ in 0..count {
            let cells = u32::from_be_bytes(take(4)?.try_into().expect("four bytes")) as usize;
            let mut previous = 0u32;
            let table: Vec<u32> = take(cells * 4)?
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&delta| {
                    previous = previous.wrapping_add(u32::from_be_bytes(delta));
                    previous
                })
                .collect();
            if let Some(bad) = table.iter().find(|&&raw| !valid_cell(raw)) {
                return Err(format!(
                    "cell {bad:#x} is neither a character nor a decoder result"
                ));
            }
            tables.push(table);
        }
        if at != bytes.len() {
            return Err(format!("{} bytes left over", bytes.len() - at));
        }
        let mut tables = tables.into_iter();
        let (Some(singles), Some(pairs)) = (tables.next(), tables.next()) else {
            return Err("the single byte and pair tables are missing".to_string());
        };
        let longer: Vec<Vec<u32>> = tables.collect();
        if singles.len() != 256 || pairs.len() != LEADS * 256 {
            return Err(format!(
                "{} single byte and {} pair cells",
                singles.len(),
                pairs.len()
            ));
        }
        if !family.longer_tables().contains(&longer.len()) {
            return Err(format!("{family:?} has {} longer tables", longer.len()));
        }
        if family == Family::EucTw {
            let planes = longer.len() - 1;
            let valid = |&raw: &u32| match cell(raw) {
                Cell::Char(index) => (index as usize) < planes,
                Cell::Error(_) => true,
                Cell::More => false,
            };
            if longer[0].len() != 256 || !longer[0].iter().all(valid) {
                return Err(format!("the planes do not index the {planes} plane tables"));
            }
        }
        Ok(Tables {
            singles,
            pairs,
            longer,
        })
    }
}
