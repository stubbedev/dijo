//! The character, paragraph and table properties Tika's path looks at, as POI's
//! `CharacterSprmUncompressor`, `ParagraphSprmUncompressor` and `TableSprmUncompressor` apply
//! sprms to them, with POI's reads of each sprm's bytes so a sprm fails where POI's fails.

use super::sprm::{find_sprm, SprmIterator, SprmOperation, TYPE_CHP, TYPE_PAP, TYPE_TAP};
use crate::formats::poi::{i16_at, u8_at, Read, Thrown};

/// `CharacterProperties`, as far as Tika's path reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Chp {
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) strike: bool,
    pub(super) vanish: bool,
    /// The underline code.
    pub(super) kul: u8,
    /// `fRMarkDel`: deleted as a tracked change.
    pub(super) deleted: bool,
    /// `fRMark`: inserted as a tracked change.
    pub(super) inserted: bool,
    pub(super) field_vanish: bool,
    pub(super) fc_pic: i32,
    pub(super) special: bool,
    pub(super) data: bool,
    pub(super) ole2: bool,
    pub(super) object: bool,
}

impl Default for Chp {
    fn default() -> Self {
        Chp {
            bold: false,
            italic: false,
            strike: false,
            vanish: false,
            kul: 0,
            deleted: false,
            inserted: false,
            field_vanish: false,
            fc_pic: -1,
            special: false,
            data: false,
            ole2: false,
            object: false,
        }
    }
}

/// `getCHPFlag`: a toggle of the parent's value where the operand asks.
fn chp_flag(operand: i8, old: bool) -> bool {
    match operand {
        0 => false,
        1 => true,
        _ if operand as u8 & 0x81 == 0x80 => old,
        _ if operand as u8 & 0x81 == 0x81 => !old,
        _ => false,
    }
}

/// The bytes past an operand's start some character sprms read besides their operand.
fn require(sprm: &SprmOperation<'_>, length: usize) -> Read<()> {
    if length > 0 {
        u8_at(sprm.grpprl(), sprm.operand_offset + length - 1)?;
    }
    Ok(())
}

/// `unCompressCHPOperation` for the properties tracked, failing where POI's reads fail, with
/// `sprmCPlain` setting the formatting back to `plain`, the paragraph style's, which POI leaves
/// as it is (#111).
fn chp_operation(old: &Chp, plain: &Chp, new: &mut Chp, sprm: &SprmOperation<'_>) -> Read<()> {
    let operation = sprm.operation();
    let operand = || sprm.operand();
    match operation {
        0x00 => new.deleted = operand()? != 0,
        0x01 => new.inserted = operand()? != 0,
        0x02 => new.field_vanish = operand()? != 0,
        0x03 => {
            new.fc_pic = operand()?;
            new.special = true;
        }
        0x04 | 0x08 | 0x0e | 0x30 | 0x3d | 0x3f | 0x40 | 0x41 | 0x42 | 0x43 | 0x44 | 0x45
        | 0x46 | 0x48 | 0x4b | 0x4c | 0x4d | 0x4e | 0x4f | 0x50 | 0x51 | 0x54 | 0x58 | 0x59
        | 0x63 | 0x6d | 0x6e | 0x6f | 0x70 => {
            operand()?;
        }
        0x05 | 0x09 | 0x64 | 0x65 => require(sprm, 4)?,
        0x06 => new.data = operand()? != 0,
        0x0a => new.ole2 = operand()? != 0,
        0x0c => {
            operand()?;
        }
        0x32 => {
            new.bold = false;
            new.italic = false;
            new.strike = false;
            new.vanish = false;
            new.kul = 0;
        }
        0x33 => {
            new.bold = plain.bold;
            new.italic = plain.italic;
            new.strike = plain.strike;
            new.vanish = plain.vanish;
            new.kul = plain.kul;
        }
        0x35 => new.bold = chp_flag(operand()? as i8, old.bold),
        0x36 => new.italic = chp_flag(operand()? as i8, old.italic),
        0x37 => new.strike = chp_flag(operand()? as i8, old.strike),
        0x38 | 0x39 | 0x3a | 0x3b | 0x75 => {
            operand()?;
        }
        0x3c => new.vanish = chp_flag(operand()? as i8, old.vanish),
        0x3e => new.kul = operand()? as u8,
        0x49 | 0x4a | 0x66 => {
            i16_at(sprm.grpprl(), sprm.operand_offset)?;
        }
        0x53 => {
            operand()?;
        }
        0x55 => new.special = operand()? != 0,
        0x56 => new.object = operand()? != 0,
        0x57 => require(sprm, 7)?,
        0x62 => require(sprm, 39)?,
        _ => {}
    }
    Ok(())
}

/// `applySprms`: the character sprms of a grpprl from `offset` applied to `target`, toggles
/// against `parent`, `sprmCPlain` back to `plain`.
fn apply_chp(
    parent: &Chp,
    plain: &Chp,
    grpprl: &[u8],
    offset: usize,
    target: &mut Chp,
) -> Read<()> {
    let mut iterator = SprmIterator::new(grpprl, offset);
    while iterator.has_next() {
        let sprm = iterator.next_operation()?;
        if sprm.kind() != TYPE_CHP {
            continue;
        }
        chp_operation(parent, plain, target, &sprm)?;
    }
    Ok(())
}

/// The deprecated `uncompressCHP(parent, grpprl, offset)` a style's properties are made with,
/// which lets a failure through.
pub(super) fn style_chp(parent: &Chp, grpprl: &[u8]) -> Read<Chp> {
    let mut properties = parent.clone();
    apply_chp(parent, parent, grpprl, 0, &mut properties)?;
    Ok(properties)
}

/// `uncompressCHP(styleSheet, parStyle, grpprl, offset)`: the paragraph style's properties,
/// the character style a sprm names applied to them, then the run's own sprms; a failure of
/// either ends its sprms where it happened.
pub(super) fn run_chp(
    paragraph_style: Option<&Chp>,
    character_style: impl Fn(i32) -> Vec<u8>,
    grpprl: &[u8],
) -> Chp {
    let parent = paragraph_style.cloned().unwrap_or_default();
    let mut properties = parent.clone();
    if let Some(style) = character_style_index(grpprl) {
        let style_grpprl = character_style(style);
        let _ = apply_chp(&parent, &parent, &style_grpprl, 0, &mut properties);
    }
    let style_properties = properties.clone();
    let _ = apply_chp(&style_properties, &parent, grpprl, 0, &mut properties);
    properties
}

/// `getIstd`: the character style the last `sprmCIstd` of a grpprl names, if any, before a
/// failure.
fn character_style_index(grpprl: &[u8]) -> Option<i32> {
    let mut style = None;
    let mut iterator = SprmIterator::new(grpprl, 0);
    while iterator.has_next() {
        let Ok(sprm) = iterator.next_operation() else {
            break;
        };
        if sprm.kind() == TYPE_CHP && sprm.operation() == 0x30 {
            match sprm.operand() {
                Ok(operand) => style = Some(operand),
                Err(_) => break,
            }
        }
    }
    style
}

/// `ParagraphProperties`, as far as Tika's path reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pap {
    pub(super) istd: i32,
    pub(super) ilvl: i8,
    pub(super) ilfo: i32,
    pub(super) in_table: bool,
    pub(super) ttp: bool,
    pub(super) itap: i32,
    pub(super) inner_table_cell: bool,
    pub(super) ttp_embedded: bool,
    pub(super) lvl: i8,
}

impl Default for Pap {
    fn default() -> Self {
        Pap {
            istd: 0,
            ilvl: 0,
            ilfo: 0,
            in_table: false,
            ttp: false,
            itap: 0,
            inner_table_cell: false,
            ttp_embedded: false,
            lvl: 9,
        }
    }
}

/// `unCompressPAPOperation` for the properties tracked.
fn pap_operation(pap: &mut Pap, sprm: &SprmOperation<'_>) -> Read<()> {
    let operand = || sprm.operand();
    match sprm.operation() {
        0x00 => pap.istd = operand()?,
        0x02 => {
            let parameter = operand()? as i8;
            pap.istd += i32::from(parameter);
            pap.lvl = pap.lvl.wrapping_add(parameter);
            if (parameter >> 7) & 1 == 1 {
                pap.istd = pap.istd.max(1);
            } else {
                pap.istd = pap.istd.min(9);
            }
        }
        0x0a => pap.ilvl = operand()? as i8,
        0x0b => pap.ilfo = i32::from(sprm.operand_short()?),
        0x16 => pap.in_table = operand()? != 0,
        0x17 => pap.ttp = operand()? != 0,
        0x40 => pap.lvl = operand()? as i8,
        0x49 => pap.itap = operand()?,
        0x4a => pap.itap = i32::from((pap.itap.wrapping_add(operand()?)) as i8),
        0x4b => pap.inner_table_cell = operand()? != 0,
        0x4c => pap.ttp_embedded = operand()? != 0,
        _ => {}
    }
    Ok(())
}

/// `uncompressPAP(parent, grpprl, offset)`: a failure of one sprm skips it; a failure to read
/// the next sprm fails.
pub(super) fn uncompress_pap(parent: &Pap, grpprl: &[u8], offset: usize) -> Read<Pap> {
    let mut properties = parent.clone();
    let mut iterator = SprmIterator::new(grpprl, offset);
    while iterator.has_next() {
        let sprm = iterator.next_operation()?;
        if sprm.kind() == TYPE_PAP {
            let _ = pap_operation(&mut properties, &sprm);
        }
    }
    Ok(properties)
}

/// `TableSprmUncompressor.uncompressTAP`'s count of cells, with the failures it lets through.
pub(super) fn table_cell_count(grpprl: &[u8]) -> Read<i16> {
    let columns = match find_sprm(grpprl, 0xD608)? {
        Some(sprm) => i16::from(u8_at(sprm.grpprl(), sprm.operand_offset)? as i8),
        None => 1,
    };
    if columns < 0 {
        return Err(Thrown(format!("a table of {columns} columns")));
    }
    let mut cell_count = columns;
    let mut descriptors = vec![true; columns as usize];
    let mut iterator = SprmIterator::new(grpprl, 2);
    while iterator.has_next() {
        let sprm = iterator.next_operation()?;
        if sprm.kind() != TYPE_TAP {
            continue;
        }
        let grpprl = sprm.grpprl();
        let offset = sprm.operand_offset;
        match sprm.operation() {
            0x08 => {
                let Ok(count) = u8_at(grpprl, offset) else {
                    continue;
                };
                let count = i16::from(count as i8);
                if count < 0 {
                    return Err(Thrown(format!("a table definition of {count} cells")));
                }
                cell_count = count;
                descriptors = vec![false; count as usize];
                if (0..count as usize).any(|x| i16_at(grpprl, offset + 1 + x * 2).is_err()) {
                    continue;
                }
                let end_of_sprm = offset as i64 + sprm.size() as i64 - 6;
                let tcs_start = offset as i64 + 1 + (i64::from(count) + 1) * 2;
                let has_tcs = tcs_start < end_of_sprm;
                for (x, descriptor) in descriptors.iter_mut().enumerate() {
                    let at = offset + 1 + (count as usize + 1) * 2 + x * 20;
                    if has_tcs && at < grpprl.len() && u8_at(grpprl, at + 19).is_err() {
                        break;
                    }
                    *descriptor = true;
                }
            }
            0x34 => {
                let (Ok(first), Ok(limit)) = (u8_at(grpprl, offset), u8_at(grpprl, offset + 1))
                else {
                    continue;
                };
                if u8_at(grpprl, offset + 3).is_err() || i16_at(grpprl, offset + 4).is_err() {
                    continue;
                }
                for c in i32::from(first as i8)..i32::from(limit as i8) {
                    match usize::try_from(c).ok().and_then(|c| descriptors.get(c)) {
                        None => break,
                        Some(false) => {
                            return Err(Thrown::new(
                                "Cannot unCompress TAP, empty table cell descriptor",
                            ))
                        }
                        Some(true) => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(cell_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_plain_formatting_back_to_the_paragraph_styles() {
        let style = Chp {
            italic: true,
            ..Chp::default()
        };
        let bold = [0x35, 0x08, 0x01];
        let plain = [0x33, 0x2A, 0x00];
        let chp = run_chp(Some(&style), |_| Vec::new(), &bold);
        assert!(chp.bold && chp.italic);
        let chp = run_chp(Some(&style), |_| Vec::new(), &[bold, plain].concat());
        assert!(!chp.bold && chp.italic);
    }
}
