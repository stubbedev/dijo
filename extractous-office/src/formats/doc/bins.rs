//! POI's `CHPBinTable` and `PAPBinTable`: the character and paragraph property runs of the
//! document from their formatted disk pages, mapped to character positions through the text
//! pieces, joined with the properties the pieces' modifiers add, and rebuilt as POI rebuilds
//! them: runs split at every boundary and paragraphs at every paragraph, cell and page mark.

use std::collections::{BTreeSet, HashMap};

use super::bytes::{index, safely_clone};
use super::sprm::{has_kind, SprmOperation, TYPE_CHP, TYPE_PAP};
use super::text::{ComplexFileTable, Plex};
use crate::formats::poi::{i16_at, i32_at, u8_at, Read, Thrown};

const MAX_GRPPRL_LENGTH: i64 = 500_000_000;
const PAGE_SIZE: i32 = 512;
/// The characters that end a paragraph: a paragraph mark, a cell mark and a page break.
pub(super) const PARAGRAPH_MARKS: [u8; 3] = [13, 7, 12];

/// POI's `SprmBuffer`: bytes a grpprl is written to, with the room past its end that POI's
/// buffers keep, which its comparisons and readers see.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
            return Err(Thrown::new(format!(
                "charStart ({start}) > charEnd ({end})"
            )));
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
}

/// `PAPX.getIstd` of a grpprl: its first short.
fn istd_of(grpprl: &[u8]) -> Read<i16> {
    Ok(match grpprl.len() {
        0 => 0,
        1 => i16::from(grpprl[0]),
        _ => i16_at(grpprl, 0)?,
    })
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

/// `new PAPBinTable(...)`: each paragraph run of every page, mapped to characters, with the
/// huge grpprls the data stream holds, handed to `visit` in the order they are read; one empty
/// run where the pages hold none.
fn visit_paragraph_runs(
    main: &[u8],
    data: &[u8],
    pages: &[i32],
    clx: &ComplexFileTable,
    visit: &mut dyn FnMut(PropertyRun) -> Read<()>,
) -> Read<()> {
    const BX_SIZE: i32 = 13;
    let mut visited = false;
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
                visited = true;
                visit(PropertyRun::new(
                    first,
                    last,
                    Some(SprmBuffer::from_bytes(grpprl.clone())),
                )?)?;
            }
        }
    }
    if !visited {
        visit(PropertyRun::new(0, 0, Some(SprmBuffer::empty(2)))?)?;
    }
    Ok(())
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
        .map_err(|_| Thrown::new(format!("a huge grpprl of negative size {size}")))?;
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
        // The insertion point of a start not found; POI takes `Math.abs` of the result, one
        // past it, which the steps back below make up for (#111).
        let mut position = if found < 0 { -(found + 1) } else { found } as usize;
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

/// The paragraphs `PAPBinTable.rebuild` makes, one for each paragraph, cell or page mark of the
/// text, partitioning it from its start, in about four bytes each: where each ends, one past
/// its mark, and its grpprl, each kept once, by the paragraphs from which on it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Paragraphs {
    ends: Vec<i32>,
    /// From which paragraph on each grpprl holds, ascending: an index into `grpprls`, `None`
    /// for a paragraph whose merged runs held no properties, as POI leaves it.
    changes: Vec<(usize, Option<u32>)>,
    grpprls: Vec<SprmBuffer>,
    /// The first paragraph whose mark lies past every run, if any: it and those after it have
    /// no properties, where POI gives each the last run's again (#185).
    #[cfg(any(test, feature = "testing"))]
    pub(super) past_runs: Option<usize>,
}

impl Paragraphs {
    pub(super) fn len(&self) -> usize {
        self.ends.len()
    }

    pub(super) fn start(&self, index: usize) -> i32 {
        match index {
            0 => 0,
            _ => self.ends[index - 1],
        }
    }

    pub(super) fn end(&self, index: usize) -> i32 {
        self.ends[index]
    }

    /// The paragraph's grpprl, `None` where its merged runs held no properties.
    pub(super) fn sprms(&self, index: usize) -> Option<&SprmBuffer> {
        let change = self
            .changes
            .partition_point(|&(first, _)| first <= index)
            .checked_sub(1)
            .expect("a grpprl from the first paragraph on");
        self.changes[change]
            .1
            .map(|grpprl| &self.grpprls[grpprl as usize])
    }

    /// `getGrpprl`.
    pub(super) fn grpprl(&self, index: usize) -> &[u8] {
        self.sprms(index).map_or(&[], SprmBuffer::bytes)
    }

    /// `PAPX.getIstd`.
    pub(super) fn istd(&self, index: usize) -> Read<i16> {
        istd_of(self.grpprl(index))
    }
}

/// The paragraphs as they are made, a grpprl at a time.
struct ParagraphsBuilder {
    paragraphs: Paragraphs,
    made: usize,
    known: HashMap<SprmBuffer, u32>,
}

impl ParagraphsBuilder {
    fn new(marks: Vec<i32>) -> Self {
        let mut ends = marks;
        for end in &mut ends {
            *end += 1;
        }
        ParagraphsBuilder {
            paragraphs: Paragraphs {
                ends,
                changes: Vec::new(),
                grpprls: Vec::new(),
                #[cfg(any(test, feature = "testing"))]
                past_runs: None,
            },
            made: 0,
            known: HashMap::new(),
        }
    }

    /// The next paragraph's grpprl.
    fn push(&mut self, sprms: Option<SprmBuffer>) {
        let id = sprms.map(|sprms| match self.known.get(&sprms) {
            Some(&id) => id,
            None => {
                let id = u32::try_from(self.paragraphs.grpprls.len())
                    .expect("fewer grpprls than paragraphs");
                self.paragraphs.grpprls.push(sprms.clone());
                self.known.insert(sprms, id);
                id
            }
        });
        if self.paragraphs.changes.last().map(|&(_, last)| last) != Some(id) {
            self.paragraphs.changes.push((self.made, id));
        }
        self.made += 1;
    }

    /// The paragraphs left to make lie past every run: each has no properties.
    fn push_past_runs(&mut self) {
        #[cfg(any(test, feature = "testing"))]
        if self.made < self.paragraphs.len() {
            self.paragraphs.past_runs = Some(self.made);
        }
        while self.made < self.paragraphs.len() {
            self.push(Some(SprmBuffer::empty(2)));
        }
    }

    fn finish(self) -> Paragraphs {
        debug_assert_eq!(self.made, self.paragraphs.ends.len());
        self.paragraphs
    }
}

/// The grpprl `PAPBinTable.rebuild` gives the paragraph `[start, end)` of the runs `ending` in
/// it, in the order they were read: the one run that is the paragraph as it is, the runs'
/// grpprls joined, or none where none has properties; an empty one where no run ends in it.
fn rebuilt_sprms(start: i32, end: i32, ending: &[&PropertyRun]) -> Option<SprmBuffer> {
    match ending {
        [] => return Some(SprmBuffer::empty(2)),
        [only] if only.start == start && only.end == end => return only.sprms.clone(),
        _ => {}
    }
    let mut sprms: Option<SprmBuffer> = None;
    for run in ending {
        let grpprl = run.grpprl();
        if grpprl.len() <= 2 {
            continue;
        }
        match &mut sprms {
            None => sprms = run.sprms.clone(),
            Some(buffer) => buffer.append(grpprl, 2),
        }
    }
    sprms
}

/// `new PAPBinTable(...)` and `PAPBinTable.rebuild`: the paragraphs of the text, one for each
/// of its paragraph, cell and page marks, at the positions `marks` gives in order.
///
/// Where the pieces add no paragraph properties and the runs of the pages end in the order
/// they are read, as they do but in a document of reordered pieces, the runs are read from the
/// pages as the marks are walked, so that only the paragraphs are held; otherwise all the runs
/// are read first, as POI reads them.
pub(super) fn read_paragraphs(
    main: &[u8],
    data: &[u8],
    pages: &[i32],
    marks: Vec<i32>,
    clx: &ComplexFileTable,
) -> Read<Paragraphs> {
    let modifiers = paragraph_modifiers(clx)?;
    let mut in_order = modifiers.is_empty();
    if in_order {
        let mut last_end = i32::MIN;
        visit_paragraph_runs(main, data, pages, clx, &mut |run| {
            in_order &= run.end >= last_end;
            last_end = run.end;
            Ok(())
        })?;
    }
    if in_order {
        rebuild_paragraphs_in_order(main, data, pages, marks, clx)
    } else {
        let mut runs = Vec::new();
        visit_paragraph_runs(main, data, pages, clx, &mut |run| {
            runs.push(run);
            Ok(())
        })?;
        runs.extend(modifiers);
        Ok(rebuild_paragraphs(runs, marks))
    }
}

/// The runs of the pieces' modifiers of paragraph properties.
fn paragraph_modifiers(clx: &ComplexFileTable) -> Read<Vec<PropertyRun>> {
    let mut runs = Vec::new();
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
    Ok(runs)
}

/// `PAPBinTable.rebuild` of runs read in the order they end: each mark takes the runs that
/// end in it as they are read. The marks after the one the last run ends in lie past every run
/// and have no properties, as Word lays the paragraphs out, where POI's index stays at the last
/// run and gives each of them the last run again (#185).
fn rebuild_paragraphs_in_order(
    main: &[u8],
    data: &[u8],
    pages: &[i32],
    marks: Vec<i32>,
    clx: &ComplexFileTable,
) -> Read<Paragraphs> {
    let mut builder = ParagraphsBuilder::new(marks);
    let mut mark = 0;
    let mut ending: Vec<PropertyRun> = Vec::new();
    visit_paragraph_runs(main, data, pages, clx, &mut |run| {
        while mark < builder.paragraphs.len() && run.end > builder.paragraphs.end(mark) {
            let start = builder.paragraphs.start(mark);
            let end = builder.paragraphs.end(mark);
            let sprms = rebuilt_sprms(start, end, &ending.iter().collect::<Vec<_>>());
            builder.push(sprms);
            ending.clear();
            mark += 1;
        }
        if mark < builder.paragraphs.len() {
            ending.push(run);
        }
        Ok(())
    })?;
    if mark < builder.paragraphs.len() {
        let start = builder.paragraphs.start(mark);
        let end = builder.paragraphs.end(mark);
        builder.push(rebuilt_sprms(
            start,
            end,
            &ending.iter().collect::<Vec<_>>(),
        ));
    }
    builder.push_past_runs();
    Ok(builder.finish())
}

/// `PAPBinTable.rebuild`: a paragraph for each paragraph, cell or page mark of the text, at the
/// positions `marks` gives in order, with the grpprls of the runs ending in it joined in the
/// order they were read. The marks after the one the last run ends in lie past every run and
/// have no properties, as Word lays the paragraphs out, where POI's index stays at the last run
/// and gives each of them the last run again (#185).
fn rebuild_paragraphs(runs: Vec<PropertyRun>, marks: Vec<i32>) -> Paragraphs {
    let mut by_end: Vec<usize> = (0..runs.len()).collect();
    by_end.sort_by_key(|&i| runs[i].end);
    let mut builder = ParagraphsBuilder::new(marks);
    let mut next = 0;
    for mark in 0..builder.paragraphs.len() {
        if next == by_end.len() {
            break;
        }
        let start_inclusive = builder.paragraphs.start(mark);
        let end_exclusive = builder.paragraphs.end(mark);
        let ending_here = by_end[next..]
            .iter()
            .take_while(|&&i| runs[i].end <= end_exclusive)
            .count();
        let mut ending = by_end[next..next + ending_here].to_vec();
        next += ending_here;
        ending.sort_unstable();
        let ending: Vec<&PropertyRun> = ending.iter().map(|&i| &runs[i]).collect();
        builder.push(rebuilt_sprms(start_inclusive, end_exclusive, &ending));
    }
    builder.push_past_runs();
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs read out of the order they end, as the pieces' modifiers add them, give the
    /// paragraphs they end in their properties, and the paragraphs past every run none (#185).
    #[test]
    fn gives_paragraphs_past_every_run_no_properties_whatever_the_order_of_the_runs() {
        let run = |start, end, istd: u8| {
            PropertyRun::new(start, end, Some(SprmBuffer::from_bytes(vec![istd, 0]))).unwrap()
        };
        let paragraphs = rebuild_paragraphs(vec![run(5, 10, 2), run(0, 5, 1)], vec![4, 9, 14, 19]);
        assert_eq!(paragraphs.len(), 4);
        assert_eq!(paragraphs.past_runs, Some(2));
        let styles: Vec<i16> = (0..4)
            .map(|index| paragraphs.istd(index).unwrap())
            .collect();
        assert_eq!(styles, [1, 2, 0, 0]);
        for index in [2, 3] {
            assert_eq!(
                paragraphs.sprms(index),
                Some(&SprmBuffer::empty(2)),
                "{index}"
            );
        }
    }
}
