//! POI's `CHPBinTable` and `PAPBinTable`: the character and paragraph property runs of the
//! document from their formatted disk pages, mapped to character positions through the text
//! pieces, joined with the properties the pieces' modifiers add, and rebuilt as POI rebuilds
//! them: runs split at every boundary and paragraphs at every paragraph, cell and page mark.

use std::collections::BTreeSet;

use super::bytes::{index, safely_clone};
use super::sprm::{has_kind, SprmOperation, TYPE_CHP, TYPE_PAP};
use super::text::{ComplexFileTable, Plex};
use crate::formats::poi::{i16_at, i32_at, u8_at, Read, Thrown};

const MAX_GRPPRL_LENGTH: i64 = 500_000_000;
const PAGE_SIZE: i32 = 512;

/// POI's `SprmBuffer`: bytes a grpprl is written to, with the room past its end that POI's
/// buffers keep, which its comparisons and readers see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SprmBuffer {
    buf: Vec<u8>,
    offset: usize,
}

impl SprmBuffer {
    /// `new SprmBuffer(buf, start)`: the bytes as they are.
    pub(super) fn from_bytes(buf: Vec<u8>) -> Self {
        let offset = buf.len();
        SprmBuffer { buf, offset }
    }

    /// `new SprmBuffer(start)`: four bytes of room after `start` zero bytes.
    pub(super) fn empty(start: usize) -> Self {
        SprmBuffer {
            buf: vec![0; start + 4],
            offset: start,
        }
    }

    /// `append(grpprl, from)`.
    pub(super) fn append(&mut self, grpprl: &[u8], from: usize) {
        let addition = grpprl.len() - from;
        if self.offset + addition >= self.buf.len() {
            self.buf.resize(self.offset + addition, 0);
        }
        self.buf[self.offset..self.offset + addition].copy_from_slice(&grpprl[from..]);
        self.offset += addition;
    }

    /// `toByteArray`: the whole buffer.
    pub(super) fn bytes(&self) -> &[u8] {
        &self.buf
    }
}

/// A run of character or paragraph properties over a range of characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PropertyRun {
    pub(super) start: i32,
    pub(super) end: i32,
    /// `None` for a paragraph whose merged runs held no properties, as POI leaves it.
    pub(super) sprms: Option<SprmBuffer>,
}

impl PropertyRun {
    /// `BytePropertyNode`: a range ending before it starts fails; one starting before zero
    /// starts at zero.
    fn new(start: i32, end: i32, sprms: Option<SprmBuffer>) -> Read<Self> {
        if start > end {
            return Err(Thrown(format!("charStart ({start}) > charEnd ({end})")));
        }
        let clamped = start.max(0);
        Ok(PropertyRun {
            start: clamped,
            end: end.max(clamped),
            sprms,
        })
    }

    /// `getGrpprl`.
    pub(super) fn grpprl(&self) -> &[u8] {
        self.sprms.as_ref().map_or(&[], SprmBuffer::bytes)
    }

    /// `PAPX.getIstd`.
    pub(super) fn istd(&self) -> Read<i16> {
        let grpprl = self.grpprl();
        Ok(match grpprl.len() {
            0 => 0,
            1 => i16::from(grpprl[0]),
            _ => i16_at(grpprl, 0)?,
        })
    }
}

/// How a bin table names its formatted disk pages: by an int, or by an unsigned short in the
/// documents before Word 97.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageNumber {
    Int,
    Short,
}

/// The page numbers of a bin table's formatted disk pages.
pub(super) fn bin_pages(
    stream: &[u8],
    offset: i32,
    size: i32,
    number: PageNumber,
) -> Read<Vec<i32>> {
    let struct_size = match number {
        PageNumber::Int => 4,
        PageNumber::Short => 2,
    };
    Plex::read(stream, offset, size, struct_size)?
        .entries
        .iter()
        .map(|(_, _, page)| match number {
            PageNumber::Int => i32_at(page, 0),
            PageNumber::Short => i16_at(page, 0).map(|page| i32::from(page as u16)),
        })
        .collect()
}

/// `new CHPBinTable(...)`: the character runs of every page, mapped to characters.
pub(super) fn character_runs(
    main: &[u8],
    pages: &[i32],
    clx: &ComplexFileTable,
) -> Read<Vec<PropertyRun>> {
    let mut runs = Vec::new();
    for &page in pages {
        let at = PAGE_SIZE.wrapping_mul(page);
        let count = i32::from(u8_at(main, index(at.wrapping_add(511))?)?);
        for x in 0..count {
            let start = i32_at(main, index(at.wrapping_add(x * 4))?)?;
            let end = i32_at(main, index(at.wrapping_add((x + 1) * 4))?)?;
            let grpprl_at = 2 * i32::from(u8_at(main, index(at + (count + 1) * 4 + x)?)?);
            let grpprl = if grpprl_at == 0 {
                Vec::new()
            } else {
                let length = i32::from(u8_at(main, index(at + grpprl_at)?)?);
                safely_clone(main, at + grpprl_at + 1, length, MAX_GRPPRL_LENGTH)?
            };
            for (first, last) in clx.char_ranges(start, end) {
                runs.push(PropertyRun::new(
                    first,
                    last,
                    Some(SprmBuffer::from_bytes(grpprl.clone())),
                )?);
            }
        }
    }
    if runs.is_empty() {
        runs.push(PropertyRun::new(0, 0, Some(SprmBuffer::empty(0)))?);
    }
    Ok(runs)
}

/// `new PAPBinTable(...)`: the paragraph runs of every page, mapped to characters, with the
/// huge grpprls the data stream holds.
pub(super) fn paragraph_runs(
    main: &[u8],
    data: &[u8],
    pages: &[i32],
    clx: &ComplexFileTable,
) -> Read<Vec<PropertyRun>> {
    const BX_SIZE: i32 = 13;
    let mut runs = Vec::new();
    for &page in pages {
        let at = PAGE_SIZE.wrapping_mul(page);
        let count = i32::from(u8_at(main, index(at.wrapping_add(511))?)?);
        for x in 0..count {
            let start = i32_at(main, index(at.wrapping_add(x * 4))?)?;
            let end = i32_at(main, index(at.wrapping_add((x + 1) * 4))?)?;
            let mut papx_at =
                2 * i32::from(u8_at(main, index(at + (count + 1) * 4 + x * BX_SIZE)?)?);
            let mut length = 2 * i32::from(u8_at(main, index(at + papx_at)?)?);
            if length == 0 {
                papx_at += 1;
                length = 2 * i32::from(u8_at(main, index(at + papx_at)?)?);
            } else {
                length -= 1;
            }
            let grpprl = safely_clone(main, at + papx_at + 1, length, 512)?;
            let grpprl = huge(grpprl, data)?;
            for (first, last) in clx.char_ranges(start, end) {
                runs.push(PropertyRun::new(
                    first,
                    last,
                    Some(SprmBuffer::from_bytes(grpprl.clone())),
                )?);
            }
        }
    }
    if runs.is_empty() {
        runs.push(PropertyRun::new(0, 0, Some(SprmBuffer::empty(2)))?);
    }
    Ok(runs)
}

/// `PAPX.findHuge`: the grpprl a `sprmPHugePapx` points to in the data stream, else the one
/// given.
fn huge(grpprl: Vec<u8>, data: &[u8]) -> Read<Vec<u8>> {
    if grpprl.len() != 8 {
        return Ok(grpprl);
    }
    let operation = SprmOperation::new(&grpprl, 2)?;
    if !matches!(operation.operation(), 0x45 | 0x46) || operation.size_code() != 3 {
        return Ok(grpprl);
    }
    let offset = operation.operand()?;
    if offset.wrapping_add(1) as i64 >= data.len() as i64 {
        return Ok(grpprl);
    }
    let size = i32::from(i16_at(data, index(offset)?)?);
    if offset.wrapping_add(size) as i64 >= data.len() as i64 {
        return Ok(grpprl);
    }
    let size = usize::try_from(size)
        .map_err(|_| Thrown(format!("a huge grpprl of negative size {size}")))?;
    let start = index(offset.wrapping_add(2))?;
    let source = start
        .checked_add(size)
        .and_then(|end| data.get(start..end))
        .ok_or_else(|| Thrown::new("a huge grpprl runs past the data stream"))?;
    let mut bytes = Vec::with_capacity(size + 2);
    bytes.extend_from_slice(&grpprl[..2]);
    bytes.extend_from_slice(source);
    Ok(bytes)
}

/// `CHPBinTable.rebuild`: the runs of the pieces' modifiers added, then the runs split at
/// every boundary of every run, overlapping runs' grpprls joined in the order they were read,
/// and neighbours with equal grpprls merged.
pub(super) fn rebuild_character_runs(
    mut runs: Vec<PropertyRun>,
    clx: &ComplexFileTable,
) -> Read<Vec<PropertyRun>> {
    for piece in &clx.pieces {
        if !piece.prm_is_complex() {
            continue;
        }
        let Some(grpprl) = usize::try_from(piece.prm_igrpprl())
            .ok()
            .and_then(|igrpprl| clx.grpprls.get(igrpprl))
        else {
            continue;
        };
        if has_kind(grpprl, 0, TYPE_CHP)? {
            runs.push(PropertyRun::new(
                piece.start,
                piece.end,
                Some(SprmBuffer::from_bytes(grpprl.clone())),
            )?);
        }
    }
    let mut by_start: Vec<usize> = (0..runs.len()).collect();
    by_start.sort_by_key(|&i| runs[i].start);
    let mut boundaries = BTreeSet::new();
    for run in &runs {
        boundaries.insert(run.start);
        boundaries.insert(run.end);
    }
    boundaries.remove(&0);
    let mut rebuilt: Vec<PropertyRun> = Vec::new();
    let mut last_start = 0;
    for &boundary in &boundaries {
        let start_inclusive = last_start;
        last_start = boundary;
        let found = binary_search_start(&runs, &by_start, boundary);
        let mut position = found.unsigned_abs() as usize;
        while position >= by_start.len() {
            position -= 1;
        }
        while position > 0 && runs[by_start[position]].start >= boundary {
            position -= 1;
        }
        let mut overlapping: Vec<usize> = Vec::new();
        for &i in &by_start[position..] {
            let run = &runs[i];
            if boundary < run.start {
                break;
            }
            if start_inclusive.max(run.start) < boundary.min(run.end) {
                overlapping.push(i);
            }
        }
        if overlapping.is_empty() {
            rebuilt.push(PropertyRun::new(
                start_inclusive,
                boundary,
                Some(SprmBuffer::empty(0)),
            )?);
            continue;
        }
        if let [only] = overlapping[..] {
            let existing = &runs[only];
            if existing.start == start_inclusive && existing.end == boundary {
                rebuilt.push(existing.clone());
                continue;
            }
        }
        overlapping.sort_unstable();
        let mut sprms = SprmBuffer::empty(0);
        for &i in &overlapping {
            sprms.append(runs[i].grpprl(), 0);
        }
        rebuilt.push(PropertyRun::new(start_inclusive, boundary, Some(sprms))?);
    }
    let mut compacted: Vec<PropertyRun> = Vec::with_capacity(rebuilt.len());
    for run in rebuilt {
        if let Some(previous) = compacted.last_mut() {
            if previous.end == run.start && previous.grpprl() == run.grpprl() {
                previous.end = run.end;
                continue;
            }
        }
        compacted.push(run);
    }
    Ok(compacted)
}

/// `CHPBinTable.binarySearch` over the runs in the order of their starts.
fn binary_search_start(runs: &[PropertyRun], by_start: &[usize], start: i32) -> i64 {
    let mut low: i64 = 0;
    let mut high: i64 = by_start.len() as i64 - 1;
    while low <= high {
        let mid = (low + high) / 2;
        let value = runs[by_start[mid as usize]].start;
        if value < start {
            low = mid + 1;
        } else if value > start {
            high = mid - 1;
        } else {
            return mid;
        }
    }
    -(low + 1)
}

/// `PAPBinTable.rebuild`: the runs of the pieces' modifiers added, then a paragraph for each
/// paragraph, cell or page mark of the text, with the grpprls of the runs ending in it joined in
/// the order they were read.
pub(super) fn rebuild_paragraph_runs(
    mut runs: Vec<PropertyRun>,
    text: &[u16],
    clx: &ComplexFileTable,
) -> Read<Vec<PropertyRun>> {
    for piece in &clx.pieces {
        if !piece.prm_is_complex() {
            continue;
        }
        let Some(grpprl) = usize::try_from(piece.prm_igrpprl())
            .ok()
            .and_then(|igrpprl| clx.grpprls.get(igrpprl))
        else {
            continue;
        };
        if has_kind(grpprl, 0, TYPE_PAP)? {
            let mut sprms = SprmBuffer::empty(2);
            sprms.append(grpprl, 0);
            runs.push(PropertyRun::new(piece.start, piece.end, Some(sprms))?);
        }
    }
    let mut by_end: Vec<usize> = (0..runs.len()).collect();
    by_end.sort_by_key(|&i| runs[i].end);
    let mut rebuilt = Vec::new();
    let mut last_start = 0;
    let mut last_index = 0;
    for (char_index, &unit) in text.iter().enumerate() {
        if !matches!(unit, 13 | 7 | 12) {
            continue;
        }
        let char_index = char_index as i32;
        let start_inclusive = last_start;
        let end_exclusive = char_index + 1;
        let mut broken = false;
        let mut ending: Vec<usize> = Vec::new();
        for (position, &i) in by_end.iter().enumerate().skip(last_index) {
            broken = false;
            if runs[i].end - 1 > char_index {
                last_index = position;
                broken = true;
                break;
            }
            ending.push(i);
        }
        if !broken {
            last_index = by_end.len().saturating_sub(1);
        }
        if ending.is_empty() {
            rebuilt.push(PropertyRun::new(
                start_inclusive,
                end_exclusive,
                Some(SprmBuffer::empty(2)),
            )?);
            last_start = end_exclusive;
            continue;
        }
        if let [only] = ending[..] {
            let existing = &runs[only];
            if existing.start == start_inclusive && existing.end == end_exclusive {
                rebuilt.push(existing.clone());
                last_start = end_exclusive;
                continue;
            }
        }
        ending.sort_unstable();
        let mut sprms: Option<SprmBuffer> = None;
        for &i in &ending {
            let grpprl = runs[i].grpprl();
            if grpprl.len() <= 2 {
                continue;
            }
            match &mut sprms {
                None => sprms = runs[i].sprms.clone(),
                Some(buffer) => buffer.append(grpprl, 2),
            }
        }
        rebuilt.push(PropertyRun::new(start_inclusive, end_exclusive, sprms)?);
        last_start = end_exclusive;
    }
    Ok(rebuilt)
}
