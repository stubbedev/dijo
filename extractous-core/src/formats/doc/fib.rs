//! POI's `FileInformationBlock`: the base of a Word document's FIB with its flags and version,
//! the character counts of its stories, and the offsets and sizes of the structures in its table
//! stream.

use crate::formats::poi::{i16_at, i32_at, u16_at, Read, Thrown};

/// Indexes of the offset and size pairs, as POI's `FIBFieldHandler` names them.
pub(super) mod field {
    pub(in crate::formats::doc) const STSHF: usize = 1;
    pub(in crate::formats::doc) const PLCFFNDREF: usize = 2;
    pub(in crate::formats::doc) const PLCFFNDTXT: usize = 3;
    pub(in crate::formats::doc) const PLCFSED: usize = 6;
    pub(in crate::formats::doc) const PLCFHDD: usize = 11;
    pub(in crate::formats::doc) const PLCFBTECHPX: usize = 12;
    pub(in crate::formats::doc) const PLCFBTEPAPX: usize = 13;
    pub(in crate::formats::doc) const STTBFFFN: usize = 15;
    pub(in crate::formats::doc) const PLCFFLDMOM: usize = 16;
    pub(in crate::formats::doc) const PLCFFLDHDR: usize = 17;
    pub(in crate::formats::doc) const PLCFFLDFTN: usize = 18;
    pub(in crate::formats::doc) const PLCFFLDATN: usize = 19;
    pub(in crate::formats::doc) const STTBFBKMK: usize = 21;
    pub(in crate::formats::doc) const PLCFBKF: usize = 22;
    pub(in crate::formats::doc) const PLCFBKL: usize = 23;
    pub(in crate::formats::doc) const DOP: usize = 31;
    pub(in crate::formats::doc) const CLX: usize = 33;
    pub(in crate::formats::doc) const PLCSPAMOM: usize = 40;
    pub(in crate::formats::doc) const PLCSPAHDR: usize = 41;
    pub(in crate::formats::doc) const PLCFENDREF: usize = 46;
    pub(in crate::formats::doc) const PLCFENDTXT: usize = 47;
    pub(in crate::formats::doc) const PLCFFLDEDN: usize = 48;
    pub(in crate::formats::doc) const DGGINFO: usize = 50;
    pub(in crate::formats::doc) const STTBFRMARK: usize = 51;
    pub(in crate::formats::doc) const PLCFTXBXTXT: usize = 56;
    pub(in crate::formats::doc) const PLCFFLDTXBX: usize = 57;
    pub(in crate::formats::doc) const PLCFFLDHDRTXBX: usize = 59;
    pub(in crate::formats::doc) const STTBSAVEDBY: usize = 71;
    pub(in crate::formats::doc) const PLFLST: usize = 73;
    pub(in crate::formats::doc) const PLFLFO: usize = 74;
}

/// The stories of a document, in the order their text follows each other, as POI's
/// `SubdocumentType.ORDERED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Story {
    Main,
    Footnote,
    Header,
    Macro,
    Annotation,
    Endnote,
    Textbox,
    HeaderTextbox,
}

impl Story {
    pub(super) const ORDERED: [Story; 8] = [
        Story::Main,
        Story::Footnote,
        Story::Header,
        Story::Macro,
        Story::Annotation,
        Story::Endnote,
        Story::Textbox,
        Story::HeaderTextbox,
    ];
}

/// `fWhichTblStm` of the FIB's base flags: whether the table stream is "1Table".
pub(super) const TABLE_ONE_FLAG: u16 = 0x0200;
/// `fObfuscated` of the FIB's base flags: whether an encrypted document is XOR obfuscated.
pub(super) const OBFUSCATED: u16 = 0x8000;
/// Where the FIB's base holds `lKey`.
pub(super) const ENCRYPTION_KEY_LENGTH: usize = 0xE;

/// `fEncrypted` of the FIB's base: whether the stream past the base is encrypted, which POI
/// decrypts before it reads the rest of the FIB.
pub(super) fn is_encrypted(main: &[u8]) -> Read<bool> {
    Ok(u16_at(main, 0xA)? & 0x0100 != 0)
}

/// The FIB, as far as Tika's path reads it.
#[derive(Debug, Clone)]
pub(super) struct Fib {
    pub(super) n_fib: i16,
    flags: u16,
    /// The character counts of the stories, in the order of [`Story::ORDERED`].
    story_lengths: [i32; 8],
    /// The offset and size of each structure, by its index.
    fields: Vec<(i32, i32)>,
}

const FC_LCB_START: usize = 154;

impl Fib {
    /// `new FileInformationBlock(mainStream)`, with the fields `fillVariableFields` reads.
    pub(super) fn read(main: &[u8]) -> Read<Fib> {
        let n_fib = i16_at(main, 2)?;
        let flags = u16_at(main, 0xA)?;
        i32_at(main, 0xE)?;
        let lengths_at = 64;
        let mut story_lengths = [0; 8];
        for (index, offset) in [0xC, 0x10, 0x14, 0x18, 0x1C, 0x20, 0x24, 0x28]
            .iter()
            .enumerate()
        {
            story_lengths[index] = i32_at(main, lengths_at + offset)?;
        }
        if n_fib < 105 {
            return Ok(Fib {
                n_fib,
                flags,
                story_lengths,
                fields: Vec::new(),
            });
        }
        let count = usize::from(u16_at(main, 152)?);
        let mut fields = Vec::with_capacity(count);
        for index in 0..count {
            let at = FC_LCB_START + index * 8;
            fields.push((i32_at(main, at)?, i32_at(main, at + 4)?));
        }
        let new_at = FC_LCB_START + count * 8;
        let csw_new = usize::from(u16_at(main, new_at)?);
        if csw_new != 0 {
            u16_at(main, new_at + 2)?;
            let length = (csw_new as i64 - 1) * 2;
            if length < 0 || main.len() < new_at + 4 {
                return Err(Thrown::new("the FIB's fibRgCswNew runs past the stream"));
            }
        }
        Ok(Fib {
            n_fib,
            flags,
            story_lengths,
            fields,
        })
    }

    /// `fillVariableFields`' copy of the structures POI does not read, which fails where one
    /// lies before the table stream or wraps past its offsets.
    pub(super) fn check_unread_fields(&self, table: &[u8]) -> Read<()> {
        const READ: [usize; 25] = [
            1, 33, 31, 12, 13, 6, 73, 74, 16, 17, 18, 19, 48, 57, 59, 22, 23, 21, 2, 3, 46, 47, 15,
            51, 71,
        ];
        const MODIFIED: usize = 87;
        for (index, &(offset, size)) in self.fields.iter().enumerate() {
            if READ.contains(&index) || index == MODIFIED || size <= 0 {
                continue;
            }
            let end = offset.wrapping_add(size);
            if end > table.len() as i32 {
                continue;
            }
            if end < 0 || offset < 0 {
                return Err(Thrown(format!(
                    "the FIB's structure {index} lies outside the table stream"
                )));
            }
        }
        Ok(())
    }

    /// `fWhichTblStm`: whether the table stream is `1Table`.
    pub(super) fn uses_table_one(&self) -> bool {
        self.flags & TABLE_ONE_FLAG != 0
    }

    /// `getSubdocumentTextStreamLength`.
    pub(super) fn story_length(&self, story: Story) -> i32 {
        let index = Story::ORDERED
            .iter()
            .position(|&known| known == story)
            .expect("an ordered story");
        self.story_lengths[index]
    }

    /// The offset of a structure, failing where the FIB has none of that index.
    pub(super) fn offset(&self, field: usize) -> Read<i32> {
        self.fields
            .get(field)
            .map(|&(offset, _)| offset)
            .ok_or_else(|| Thrown(format!("the FIB has no field {field}")))
    }

    /// The size of a structure.
    pub(super) fn size(&self, field: usize) -> Read<i32> {
        self.fields
            .get(field)
            .map(|&(_, size)| size)
            .ok_or_else(|| Thrown(format!("the FIB has no field {field}")))
    }
}
