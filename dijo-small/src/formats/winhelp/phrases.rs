//! Phrase compression, which replaces the phrases a help compiler chose in the text of topic
//! records by their numbers: in `|Phrases` the old way of WinHelp 3 and of HCW 4 without Hall
//! compression, or in `|PhrIndex` and `|PhrImage` Hall compression's way of HCW 4.
//!
//! Old phrase compression writes a byte 1 to 15 and the byte after it for twice the number of
//! a phrase, plus one for a phrase a space follows; every other byte stands for itself. Hall
//! compression writes every byte as a code: an even byte for one of the first 128 phrases, a
//! byte whose low two bits are 01 and the byte after it for one of the next 16384, and bytes
//! whose low three or four bits are 011, 0111 or 1111 for a run of up to 32 bytes copied after
//! it, of up to 16 spaces or of up to 16 NULs.

use std::borrow::Cow;

use super::file_system::FileSystem;
use super::lz77;
use super::reader::Reader;
use super::system::Layout;
use super::Malformed;

/// The `|Phrases` structure of some Multimedia Viewer files, which starts with this.
const VIEWER_PHRASES: u16 = 0x0800;
const PHRASES_MAGIC: u16 = 0x0100;
const VIEWER_UNUSED: usize = 30;
/// The bytes of the `|PhrIndex` header before its bits.
const PHRASE_INDEX_HEADER: usize = 28;
/// The phrases Hall compression numbers in one byte.
const HALL_SHORT: usize = 128;
/// The most low bits a phrase length is coded in after its high part.
const MAX_LOW_BITS: u32 = 5;

/// How the text of topic records is compressed.
pub(super) enum Phrases<'a> {
    /// Not at all.
    None,
    /// The old way, with these phrases.
    Old(Table<'a>),
    /// By Hall compression, with these phrases.
    Hall(Table<'a>),
}

/// The phrases: their text one after the other, and where each starts, one offset more than
/// there are phrases for where the last ends.
pub(super) struct Table<'a> {
    text: Cow<'a, [u8]>,
    offsets: Vec<usize>,
}

impl<'a> Table<'a> {
    pub(super) fn new(text: Cow<'a, [u8]>, offsets: Vec<usize>) -> Self {
        Self { text, offsets }
    }

    /// The phrase `number`.
    pub(super) fn phrase(&self, number: usize) -> Result<&[u8], Malformed> {
        let (Some(&start), Some(&end)) = (self.offsets.get(number), self.offsets.get(number + 1))
        else {
            return Err(Malformed(format!(
                "a topic record names the phrase {number} of {} phrases",
                self.offsets.len().saturating_sub(1)
            )));
        };
        self.text.get(start..end).ok_or_else(|| {
            Malformed(format!(
                "the phrase {number} runs from {start} to {end} of the {} bytes of the phrases",
                self.text.len()
            ))
        })
    }
}

impl<'a> Phrases<'a> {
    /// The phrases of the help file of `files`, laid out as `layout` says.
    pub(super) fn read(files: &FileSystem<'a>, layout: Layout) -> Result<Self, Malformed> {
        if let Some(index) = files.file("|PhrIndex")? {
            let image = files.file("|PhrImage")?.ok_or_else(|| {
                Malformed("the help file has a |PhrIndex but no |PhrImage".to_owned())
            })?;
            return hall(index, image).map(Phrases::Hall);
        }
        match files.file("|Phrases")? {
            Some(phrases) => old(phrases, layout).map(Phrases::Old),
            None => Ok(Phrases::None),
        }
    }
}

/// The phrases of `|Phrases`: their count, offsets and text, LZ77-compressed but in WinHelp
/// 3.0, whose offsets count from the start of the offsets.
fn old(content: &[u8], layout: Layout) -> Result<Table<'_>, Malformed> {
    let mut fields = Reader::new(content, "the |Phrases file");
    let mut count = fields.u16()?;
    let viewer = count == VIEWER_PHRASES;
    if viewer {
        count = fields.u16()?;
    }
    let magic = fields.u16()?;
    if magic != PHRASES_MAGIC {
        return Err(Malformed(format!(
            "the |Phrases file has {magic:#06x} where {PHRASES_MAGIC:#06x} belongs"
        )));
    }
    let count = usize::from(count);
    let decompressed = match layout {
        Layout::WinHelp30 => None,
        Layout::WinHelp31 { .. } => {
            let size = fields.u32()? as usize;
            if viewer {
                fields.skip(VIEWER_UNUSED)?;
            }
            Some(size)
        }
    };
    let base = 2 * (count + 1);
    let mut offsets = Vec::with_capacity(count + 1);
    for _ in 0..=count {
        let offset = usize::from(fields.u16()?);
        offsets.push(offset.checked_sub(base).ok_or_else(|| {
            Malformed(format!(
                "a phrase offset {offset} lies within the {base} bytes of the phrase offsets"
            ))
        })?);
    }
    let text = match decompressed {
        None => Cow::Borrowed(fields.rest()),
        Some(size) => {
            let mut text = Vec::new();
            lz77::decompress(fields.rest(), size, &mut text);
            Cow::Owned(text)
        }
    };
    Ok(Table::new(text, offsets))
}

/// The phrases of `|PhrIndex` and `|PhrImage`: the lengths of the phrases, coded in bits, and
/// their text, LZ77-compressed where its compressed size is not its size.
fn hall<'a>(index: &[u8], image: &'a [u8]) -> Result<Table<'a>, Malformed> {
    let mut fields = Reader::new(index, "the |PhrIndex file");
    let _magic = fields.u32()?;
    let count = fields.u32()? as usize;
    let _compressed = fields.u32()?;
    let image_size = fields.u32()? as usize;
    let image_compressed = fields.u32()? as usize;
    let _zero = fields.u32()?;
    let bit_count = u32::from(fields.u16()? & 0x0F);
    let _unknown = fields.u16()?;
    let mut bits = Bits::new(fields.rest());
    let mut offsets = vec![0];
    let mut offset = 0usize;
    for _ in 0..count {
        let mut length = 1usize;
        while bits.next()? {
            length = length
                .checked_add(1 << bit_count)
                .ok_or_else(|| Malformed("a phrase longer than memory".to_owned()))?;
        }
        for bit in 0..bit_count.clamp(1, MAX_LOW_BITS) {
            if bits.next()? {
                length += 1 << bit;
            }
        }
        offset = offset
            .checked_add(length)
            .ok_or_else(|| Malformed("phrases longer than memory".to_owned()))?;
        offsets.push(offset);
    }
    let text = if image_size == image_compressed {
        Cow::Borrowed(image.get(..image_size).ok_or_else(|| {
            Malformed(format!(
                "the |PhrImage file of {} bytes holds fewer than its {image_size}",
                image.len()
            ))
        })?)
    } else {
        let mut text = Vec::new();
        lz77::decompress(image, image_size, &mut text);
        Cow::Owned(text)
    };
    Ok(Table::new(text, offsets))
}

/// The bits of `bytes`, from the least significant bit of each on.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Bits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn next(&mut self) -> Result<bool, Malformed> {
        let byte = self.bytes.get(self.at / 8).ok_or_else(|| {
            Malformed(format!(
                "the phrase lengths run past the {} bytes of the |PhrIndex file",
                self.bytes.len() + PHRASE_INDEX_HEADER
            ))
        })?;
        let bit = byte >> (self.at % 8) & 1 == 1;
        self.at += 1;
        Ok(bit)
    }
}

/// The text of a topic record expanded from what is stored, a byte at a time, up to the length
/// the record gives its text: expanding to more fails.
pub(super) struct Expanded<'t> {
    phrases: &'t Phrases<'t>,
    stored: &'t [u8],
    at: usize,
    /// Whether `stored` is the text itself rather than phrase-compressed.
    plain: bool,
    /// What is left to give of a phrase or a run copied.
    pending: &'t [u8],
    /// A space to give after `pending`, after an odd old phrase code.
    space: bool,
    /// A byte to give `repeat` times, of a Hall run of spaces or NULs.
    repeat: (u8, usize),
    given: usize,
    length: usize,
}

impl<'t> Expanded<'t> {
    /// The text of `length` bytes of a record that stores `stored` of it: as it is where that
    /// is no shorter, else phrase-compressed.
    pub(super) fn new(phrases: &'t Phrases<'t>, stored: &'t [u8], length: usize) -> Self {
        let plain = length <= stored.len();
        Self {
            phrases,
            stored: if plain { &stored[..length] } else { stored },
            at: 0,
            plain,
            pending: &[],
            space: false,
            repeat: (0, 0),
            given: 0,
            length,
        }
    }

    /// The next byte of the text, or none at its end.
    pub(super) fn next_byte(&mut self) -> Result<Option<u8>, Malformed> {
        loop {
            if let Some((&byte, rest)) = self.pending.split_first() {
                self.pending = rest;
                return self.give(byte);
            }
            if std::mem::take(&mut self.space) {
                return self.give(b' ');
            }
            if self.repeat.1 > 0 {
                self.repeat.1 -= 1;
                return self.give(self.repeat.0);
            }
            if self.at >= self.stored.len() {
                return Ok(None);
            }
            let code = self.stored_byte()?;
            if self.plain {
                return self.give(code);
            }
            let literal = match self.phrases {
                Phrases::Hall(table) => {
                    self.hall(table, code)?;
                    None
                }
                Phrases::Old(table) => self.old(Some(table), code)?,
                Phrases::None => self.old(None, code)?,
            };
            if let Some(literal) = literal {
                return self.give(literal);
            }
        }
    }

    /// Gives `byte`, which must not make the text longer than its length.
    fn give(&mut self, byte: u8) -> Result<Option<u8>, Malformed> {
        self.given += 1;
        if self.given > self.length {
            return Err(Malformed(format!(
                "the phrase-compressed text of a topic record expands to more than its {} bytes",
                self.length
            )));
        }
        Ok(Some(byte))
    }

    fn stored_byte(&mut self) -> Result<u8, Malformed> {
        let byte = self.stored.get(self.at).copied().ok_or_else(|| {
            Malformed(format!(
                "the phrase-compressed text of a topic record ends within a code at {}",
                self.at
            ))
        })?;
        self.at += 1;
        Ok(byte)
    }

    /// The byte the old code `code` stands for itself, or none where it names a phrase, which
    /// it makes pending.
    fn old(&mut self, table: Option<&'t Table<'t>>, code: u8) -> Result<Option<u8>, Malformed> {
        if !(1..16).contains(&code) {
            return Ok(Some(code));
        }
        let number = (usize::from(code) - 1) << 8 | usize::from(self.stored_byte()?);
        let table = table.ok_or_else(|| {
            Malformed(format!(
                "a topic record names the phrase {} of a help file without phrases",
                number / 2
            ))
        })?;
        self.pending = table.phrase(number / 2)?;
        self.space = number % 2 == 1;
        Ok(None)
    }

    /// Makes what the Hall code `code` stands for pending.
    fn hall(&mut self, table: &'t Table<'t>, code: u8) -> Result<(), Malformed> {
        let code = usize::from(code);
        if code & 1 == 0 {
            self.pending = table.phrase(code / 2)?;
        } else if code & 3 == 1 {
            let number = HALL_SHORT + ((code / 4) << 8) + usize::from(self.stored_byte()?);
            self.pending = table.phrase(number)?;
        } else if code & 7 == 3 {
            let length = code / 8 + 1;
            let run = self
                .at
                .checked_add(length)
                .and_then(|end| self.stored.get(self.at..end))
                .ok_or_else(|| {
                    Malformed(format!(
                        "a run of {length} bytes at {} runs past the {} bytes of a topic record",
                        self.at,
                        self.stored.len()
                    ))
                })?;
            self.at += length;
            self.pending = run;
        } else {
            let byte = if code & 0x0F == 0x07 { b' ' } else { 0 };
            self.repeat = (byte, code / 16 + 1);
        }
        Ok(())
    }
}
