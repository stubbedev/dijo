//! POI's `ListTables` and Tika's `ListManager`: the list definitions and overrides of the table
//! stream, and the numbers of list paragraphs as Tika counts and formats them.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use super::bytes::index;
use crate::formats::list_numbering::{LevelCounter, LevelTuple};
use crate::formats::poi::{i16_at, i32_at, u16_at, u8_at, Read, Thrown};

const LSTF_SIZE: i32 = 28;
const LVLF_SIZE: i32 = 28;
const LFO_SIZE: i32 = 16;
const LFOLVL_BASE_SIZE: i32 = 8;
const MAX_LFO_COUNT: i64 = 100_000;

/// A level of a list.
#[derive(Debug, Clone)]
pub(super) struct ListLevel {
    start_at: i32,
    number_format: i8,
    legal: bool,
    no_restart: bool,
    restart_limit: u8,
    placeholder_offsets: [i8; 9],
    pub(super) grpprl_papx: Vec<u8>,
    /// The number text, in UTF-16 units.
    number_text: Vec<u16>,
}

impl ListLevel {
    /// `ListLevel.read`: the level and its length.
    fn read(data: &[u8], offset: i32) -> Read<(ListLevel, i32)> {
        let at = index(offset)?;
        let start_at = i32_at(data, at)?;
        let number_format = u8_at(data, at + 4)? as i8;
        let info = u8_at(data, at + 5)?;
        if at + 6 > data.len() {
            return Err(Thrown(format!("Index {} out of bounds", at + 6)));
        }
        let mut placeholder_offsets = [0i8; 9];
        for (i, slot) in placeholder_offsets.iter_mut().enumerate() {
            *slot = data.get(at + 6 + i).map_or(0, |&byte| byte as i8);
        }
        u8_at(data, at + 0xF)?;
        i32_at(data, at + 0x10)?;
        i32_at(data, at + 0x14)?;
        let chpx_length = usize::from(u8_at(data, at + 0x18)?);
        let papx_length = usize::from(u8_at(data, at + 0x19)?);
        let restart_limit = u8_at(data, at + 0x1A)?;
        u8_at(data, at + 0x1B)?;
        let mut position = at + LVLF_SIZE as usize;
        let grpprl_papx = data
            .get(position..position + papx_length)
            .ok_or_else(|| Thrown::new("a list level's paragraph properties run past the table"))?
            .to_vec();
        position += papx_length;
        data.get(position..position + chpx_length)
            .ok_or_else(|| Thrown::new("a list level's character properties run past the table"))?;
        position += chpx_length;
        let count = usize::from(u16_at(data, position)?);
        position += 2;
        let mut number_text = Vec::with_capacity(count);
        for _ in 0..count {
            number_text.push(i16_at(data, position)? as u16);
            position += 2;
        }
        let level = ListLevel {
            start_at,
            number_format,
            legal: info & 0x04 != 0,
            no_restart: info & 0x08 != 0,
            restart_limit,
            placeholder_offsets,
            grpprl_papx,
            number_text,
        };
        Ok((level, (position - at) as i32))
    }

    /// `getRestart`.
    fn restart(&self) -> i32 {
        if self.no_restart {
            i32::from(self.restart_limit)
        } else {
            -1
        }
    }
}

/// A list definition: its levels, one for a simple list, nine otherwise.
#[derive(Debug, Clone)]
struct ListData {
    levels: Vec<ListLevel>,
}

/// An override of a list.
#[derive(Debug, Clone)]
struct Lfo {
    lsid: i32,
    levels: Vec<LevelOverride>,
}

/// A level override, `LFOLVL`.
#[derive(Debug, Clone)]
struct LevelOverride {
    /// The level it overrides, `iLvl`.
    level: usize,
    /// The number it starts at, where `fStartAt` sets one.
    start_at: Option<i32>,
    /// The level it defines in full, where `fFormatting` is set.
    formatting: Option<ListLevel>,
}

/// The list tables.
pub(super) struct ListTables {
    lists: HashMap<i32, ListData>,
    lfos: Vec<Lfo>,
}

impl ListTables {
    /// `new ListTables(tableStream, fcPlfLst, fcPlfLfo, lcbPlfLfo)`.
    pub(super) fn read(table: &[u8], lst_offset: i32, lfo_offset: i32) -> Read<ListTables> {
        let count = i32::from(i16_at(table, index(lst_offset)?)?);
        let mut offset = lst_offset.wrapping_add(2);
        let mut level_offset = offset.wrapping_add(count.wrapping_mul(LSTF_SIZE));
        let mut lists = HashMap::new();
        for _ in 0..count {
            let at = index(offset)?;
            let lsid = i32_at(table, at)?;
            i32_at(table, at + 4)?;
            for i in 0..9 {
                i16_at(table, at + 8 + i * 2)?;
            }
            let flags = u8_at(table, at + 26)?;
            u8_at(table, at + 27)?;
            let level_count = if flags & 0x01 != 0 { 1 } else { 9 };
            offset = offset.wrapping_add(LSTF_SIZE);
            let mut levels = Vec::with_capacity(level_count);
            for _ in 0..level_count {
                let (level, length) = ListLevel::read(table, level_offset)?;
                level_offset = level_offset.wrapping_add(length);
                levels.push(level);
            }
            lists.insert(lsid, ListData { levels });
        }
        let lfos = read_lfos(table, lfo_offset)?;
        Ok(ListTables { lists, lfos })
    }

    /// `getLfo(ilfo)`, `None` where POI throws `NoSuchElementException`.
    fn lfo(&self, ilfo: i32) -> Option<&Lfo> {
        if ilfo <= 0 || ilfo as usize > self.lfos.len() {
            return None;
        }
        Some(&self.lfos[ilfo as usize - 1])
    }

    /// `getLevel(getLfo(ilfo).getLsid(), level)` as `Paragraph.newParagraph` asks it: `None`
    /// for an unknown override or list or a level past its levels, failing for a negative level.
    pub(super) fn level(&self, ilfo: i32, level: i32) -> Read<Option<&ListLevel>> {
        let Some(lfo) = self.lfo(ilfo) else {
            return Ok(None);
        };
        let Some(list) = self.lists.get(&lfo.lsid) else {
            return Ok(None);
        };
        if level >= list.levels.len() as i32 {
            return Ok(None);
        }
        Ok(Some(&list.levels[index(level)?]))
    }
}

/// `PlfLfo`: the list overrides and the levels each overrides.
fn read_lfos(table: &[u8], offset: i32) -> Read<Vec<Lfo>> {
    let at = index(offset)?;
    let count = i64::from(i32_at(table, at)? as u32);
    if count > i64::from(i32::MAX) || count > MAX_LFO_COUNT {
        return Err(Thrown(format!(
            "Tried to allocate an array of length {count}"
        )));
    }
    let mut position = offset.wrapping_add(4);
    let mut headers = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let at = index(position)?;
        let lsid = i32_at(table, at)?;
        i32_at(table, at + 4)?;
        i32_at(table, at + 8)?;
        let levels = u8_at(table, at + 12)? as i8;
        u8_at(table, at + 15)?;
        headers.push((lsid, levels));
        position = position.wrapping_add(LFO_SIZE);
    }
    let mut lfos = Vec::with_capacity(headers.len());
    for (lsid, level_count) in headers {
        if level_count < 0 {
            return Err(Thrown::new("Cannot create LFOData with negative count"));
        }
        i32_at(table, index(position)?)?;
        position = position.wrapping_add(4);
        let mut levels = Vec::with_capacity(level_count as usize);
        for _ in 0..level_count {
            let at = index(position)?;
            let start_at = i32_at(table, at)?;
            let flags = i32_at(table, at + 4)?;
            position = position.wrapping_add(LFOLVL_BASE_SIZE);
            let formatting = if flags & 0x20 != 0 {
                let (level, length) = ListLevel::read(table, position)?;
                position = position.wrapping_add(length);
                Some(level)
            } else {
                None
            };
            levels.push(LevelOverride {
                level: (flags & 0x0F) as usize,
                start_at: (flags & 0x10 != 0).then_some(start_at),
                formatting,
            });
        }
        lfos.push(Lfo { lsid, levels });
    }
    Ok(lfos)
}

/// Tika's `ListManager` for a document.
pub(super) struct ListManager {
    counters: HashMap<i32, LevelCounter>,
    override_tuples: HashMap<i32, Option<Vec<LevelTuple>>>,
}

impl ListManager {
    pub(super) fn new() -> Self {
        ListManager {
            counters: HashMap::new(),
            override_tuples: HashMap::new(),
        }
    }

    /// `getFormattedNumber`: the number of the next paragraph of override `ilfo` at `ilvl`.
    pub(super) fn formatted_number(
        &mut self,
        tables: Option<&ListTables>,
        ilfo: i32,
        ilvl: i32,
    ) -> Read<String> {
        let Some(tables) = tables else {
            return Ok(String::new());
        };
        if !(0x0001..=0x07FE).contains(&ilfo) {
            return Ok(String::new());
        }
        let Some(lfo) = tables.lfo(ilfo) else {
            return Ok(String::new());
        };
        let lsid = lfo.lsid;
        let counter = match self.counters.entry(lsid) {
            Entry::Occupied(counter) => counter.into_mut(),
            Entry::Vacant(vacant) => {
                let Some(list) = tables.lists.get(&lsid) else {
                    return Ok(String::new());
                };
                vacant.insert(LevelCounter::new(list.levels.iter().map(tuple).collect()))
            }
        };
        let overrides = match self.override_tuples.get(&ilfo) {
            Some(Some(tuples)) => Some(tuples.clone()),
            _ => tables
                .lists
                .get(&lsid)
                .and_then(|list| override_tuples(lfo, &list.levels)),
        };
        let level =
            usize::try_from(ilvl).map_err(|_| Thrown(format!("Index {ilvl} out of bounds")))?;
        let formatted = counter.increment(level, overrides.as_deref());
        self.override_tuples.insert(ilfo, overrides);
        Ok(formatted)
    }
}

/// `buildOverrideTuples`: the list's levels with those the override overrides replaced, a level
/// defined in full by its definition and one restarted by its new start, if the override
/// overrides any. Tika numbers every level as the first level override defines it, and only
/// where that one defines its level in full (#111).
fn override_tuples(lfo: &Lfo, levels: &[ListLevel]) -> Option<Vec<LevelTuple>> {
    if lfo.levels.is_empty() {
        return None;
    }
    let mut tuples: Vec<LevelTuple> = levels.iter().map(tuple).collect();
    for level_override in &lfo.levels {
        let Some(slot) = tuples.get_mut(level_override.level) else {
            continue;
        };
        if let Some(level) = &level_override.formatting {
            *slot = tuple(level);
        } else if let Some(start_at) = level_override.start_at {
            slot.start = start_at;
        }
    }
    Some(tuples)
}

/// `buildTuple`.
fn tuple(level: &ListLevel) -> LevelTuple {
    LevelTuple {
        start: level.start_at,
        restart: level.restart(),
        text: number_text(&level.number_text, &level.placeholder_offsets),
        format: number_format(level.number_format).to_owned(),
        legal: level.legal,
    }
}

/// `convertToNewNumberText`: each placeholder of the level number text as `%n`.
fn number_text(text: &[u16], offsets: &[i8; 9]) -> String {
    let mut converted: Vec<u16> = Vec::new();
    let mut last = 0usize;
    for &offset in offsets {
        let Ok(offset) = usize::try_from(offset) else {
            break;
        };
        if offset == 0 || offset - 1 < last || offset > text.len() {
            break;
        }
        converted.extend_from_slice(&text[last..offset - 1]);
        let level = u32::from(text[offset - 1]) + 1;
        converted.extend(format!("%{level}").encode_utf16());
        last = offset;
    }
    if last < text.len() {
        converted.extend_from_slice(&text[last..]);
    }
    String::from_utf16_lossy(&converted)
}

/// `convertToNewNumFormat`.
fn number_format(code: i8) -> &'static str {
    match code {
        -1 | 47 => "none",
        1 => "upperRoman",
        2 => "lowerRoman",
        3 => "upperLetter",
        4 => "lowerLetter",
        5 => "ordinal",
        22 => "decimalZero",
        23 => "bullet",
        _ => "decimal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(start_at: i32) -> ListLevel {
        ListLevel {
            start_at,
            number_format: 0,
            legal: false,
            no_restart: false,
            restart_limit: 0,
            placeholder_offsets: [1, 0, 0, 0, 0, 0, 0, 0, 0],
            grpprl_papx: Vec::new(),
            number_text: vec![0],
        }
    }

    #[test]
    fn overrides_each_level_by_its_own_override() {
        let levels = [level(1), level(1), level(1)];
        let lfo = Lfo {
            lsid: 1,
            levels: vec![
                LevelOverride {
                    level: 2,
                    start_at: Some(5),
                    formatting: None,
                },
                LevelOverride {
                    level: 1,
                    start_at: None,
                    formatting: Some(level(9)),
                },
            ],
        };
        let tuples = override_tuples(&lfo, &levels).unwrap();
        let starts: Vec<i32> = tuples.iter().map(|tuple| tuple.start).collect();
        assert_eq!(starts, [1, 9, 5]);
        let none = Lfo {
            lsid: 1,
            levels: Vec::new(),
        };
        assert!(override_tuples(&none, &levels).is_none());
    }
}
