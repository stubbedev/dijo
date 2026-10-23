//! POI's `Range` and the ranges Tika's extractor walks: paragraphs with their properties as
//! `Paragraph.newParagraph` makes them, character runs with theirs, and tables of rows of cells
//! as `Range.getTable`, `Table` and `TableRow` find them, with POI's quirks.

use super::bins::{Paragraphs, PropertyRun};
use super::document::Document;
use super::props::{run_chp, table_cell_count, uncompress_pap, Chp, Pap};
use crate::formats::poi::{Read, Thrown};

/// The cell mark ending the last paragraph of a table cell.
const CELL_MARK: u16 = 0x07;

/// A range of the text, `[start, end)` in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) start: i32,
    pub(super) end: i32,
}

/// A paragraph, as `Paragraph.newParagraph` makes it.
#[derive(Debug, Clone)]
pub(super) struct Paragraph {
    pub(super) span: Span,
    /// The index of its run in the document's paragraph runs.
    papx: usize,
    /// `getStyleIndex`.
    pub(super) istd: i16,
    pap: Pap,
}

impl Paragraph {
    pub(super) fn is_in_table(&self) -> bool {
        self.pap.in_table
    }

    /// `getTableLevel`.
    pub(super) fn table_level(&self) -> i32 {
        self.pap.itap
    }

    /// `isTableRowEnd`.
    fn is_table_row_end(&self) -> bool {
        self.pap.ttp || self.pap.ttp_embedded
    }

    /// `isInList`: an `ilfo` of 0xF801 marks no list. POI reads it sign-extended, which never
    /// equals the unsigned value it compares it to (#111).
    pub(super) fn is_in_list(&self) -> bool {
        self.pap.ilfo != 0 && self.pap.ilfo as u16 != 0xF801
    }

    pub(super) fn ilfo(&self) -> i32 {
        self.pap.ilfo
    }

    pub(super) fn ilvl(&self) -> i32 {
        i32::from(self.pap.ilvl)
    }
}

/// A character run, as `Range.getCharacterRun` makes it.
#[derive(Debug, Clone)]
pub(super) struct CharacterRun {
    pub(super) span: Span,
    pub(super) chp: Chp,
}

/// A table, as `Range.getTable` finds it.
#[derive(Debug, Clone)]
pub(super) struct Table {
    pub(super) span: Span,
    pub(super) rows: Vec<Vec<Span>>,
}

impl Document<'_> {
    /// `text`: the text of a range, read from the main stream, failing where it runs past the
    /// text.
    pub(super) fn text_of(&self, span: Span) -> Read<Vec<u16>> {
        let mut text = Vec::new();
        self.text_into(span, &mut text)?;
        Ok(text)
    }

    /// The text of a range, read from the main stream onto the end of `text`, failing where it
    /// runs past the text.
    pub(super) fn text_into(&self, span: Span, text: &mut Vec<u16>) -> Read<()> {
        self.text
            .read_into(self.main, span.start as usize..span.end as usize, text)
            .then_some(())
            .ok_or_else(|| self.past_the_text(span))
    }

    /// The text of a range, read from the main stream and handed to `each` a character at a
    /// time, failing where it runs past the text.
    fn decode(&self, span: Span, each: impl FnMut(u16)) -> Read<()> {
        self.text
            .decode(self.main, span.start as usize..span.end as usize, each)
            .then_some(())
            .ok_or_else(|| self.past_the_text(span))
    }

    /// The character at `at` of the text, failing where it lies past the text.
    pub(super) fn unit_at(&self, at: i32) -> Read<u16> {
        let mut found = None;
        self.decode(
            Span {
                start: at,
                end: at + 1,
            },
            |unit| found = Some(unit),
        )?;
        Ok(found.expect("a character of the text lies in a piece"))
    }

    /// Whether each character of a range is one of `units`, ASCII characters, read without
    /// decoding, failing where it runs past the text.
    pub(super) fn consists_of(&self, span: Span, units: &[u8]) -> Read<bool> {
        self.check_span(span)?;
        Ok(self
            .text
            .consists_of(self.main, span.start as usize..span.end as usize, units))
    }

    /// Fails where a range runs past the text, as reading its text does.
    pub(super) fn check_span(&self, span: Span) -> Read<()> {
        if span.end as usize > self.text.len() {
            return Err(self.past_the_text(span));
        }
        Ok(())
    }

    fn past_the_text(&self, span: Span) -> Thrown {
        Thrown::new(format!(
            "begin {}, end {}, length {}",
            span.start,
            span.end,
            self.text.len()
        ))
    }

    /// Whether the text of a range is `units`, read only where it is as long.
    pub(super) fn text_is(&self, span: Span, units: &[u16]) -> Read<bool> {
        self.check_span(span)?;
        if (span.end - span.start) as usize != units.len() {
            return Ok(false);
        }
        let mut expected = units.iter();
        let mut equal = true;
        self.decode(span, |unit| equal &= expected.next() == Some(&unit))?;
        Ok(equal)
    }

    /// The text of a range a piece at a time, each at most `CHUNK` characters, a surrogate
    /// pair never cut, failing where it runs past the text.
    pub(super) fn chunks_of(
        &self,
        span: Span,
        mut each: impl FnMut(&[u16]) -> Read<bool>,
    ) -> Read<()> {
        const CHUNK: i32 = 1 << 16;
        self.check_span(span)?;
        let mut units = Vec::new();
        let mut at = span.start;
        while at < span.end {
            let mut to = at.saturating_add(CHUNK).min(span.end);
            units.clear();
            self.text_into(Span { start: at, end: to }, &mut units)?;
            if to < span.end
                && units
                    .last()
                    .is_some_and(|&unit| (0xD800..0xDC00).contains(&unit))
            {
                units.pop();
                to -= 1;
            }
            if !each(&units)? {
                return Ok(());
            }
            at = to;
        }
        Ok(())
    }

    /// `new Range(start, end, document)`.
    pub(super) fn range(&self, start: i32, end: i32) -> Read<Span> {
        subrange(start, end, None)
    }

    /// `new Range(start, end, parent)`, which also checks the range lies within the text.
    pub(super) fn subrange(&self, start: i32, end: i32) -> Read<Span> {
        subrange(start, end, Some(self.text.len()))
    }

    /// `initParagraphs`: the paragraph runs the range covers, `[first, last)`.
    fn paragraph_bounds(&self, span: Span) -> Read<(usize, usize)> {
        find_range(&self.paragraphs, span)
    }

    /// `numParagraphs`.
    pub(super) fn paragraph_count(&self, span: Span) -> Read<usize> {
        let (first, last) = self.paragraph_bounds(span)?;
        Ok(last - first)
    }

    /// `getParagraph(index)`.
    pub(super) fn paragraph(&self, span: Span, index: usize) -> Read<Paragraph> {
        let (first, last) = self.paragraph_bounds(span)?;
        if first + index >= last {
            return Err(Thrown::new(format!(
                "Paragraph #{index} ({}) not in range [{first}; {last})",
                first + index
            )));
        }
        self.new_paragraph(span, first + index)
    }

    /// `Paragraph.newParagraph(parent, papx)`: the paragraph's style's properties, then its
    /// own; where it is in a list whose level has properties, those, then the style's and its
    /// own again.
    fn new_paragraph(&self, parent: Span, papx: usize) -> Read<Paragraph> {
        let istd = self.paragraphs.istd(papx)?;
        let style = self.styles.papx(i32::from(istd))?;
        let mut pap = Pap {
            istd: i32::from(istd),
            ..Pap::default()
        };
        pap = uncompress_pap(&pap, &style, 2)?;
        pap = uncompress_pap(&pap, self.paragraphs.grpprl(papx), 2)?;
        if pap.ilfo != 0 {
            if let Some(lists) = &self.lists {
                if let Some(level) = lists.level(pap.ilfo, i32::from(pap.ilvl))? {
                    pap = uncompress_pap(&pap, &level.grpprl_papx, 0)?;
                    pap = uncompress_pap(&pap, &style, 2)?;
                    pap = uncompress_pap(&pap, self.paragraphs.grpprl(papx), 2)?;
                }
            }
        }
        let span = self.subrange(
            parent.start.max(self.paragraphs.start(papx)),
            parent.end.min(self.paragraphs.end(papx)),
        )?;
        Ok(Paragraph {
            span,
            papx,
            istd,
            pap,
        })
    }

    /// `initCharacterRuns`.
    fn character_bounds(&self, span: Span) -> Read<(usize, usize)> {
        find_range(self.characters.as_slice(), span)
    }

    /// `numCharacterRuns`.
    pub(super) fn run_count(&self, span: Span) -> Read<usize> {
        let (first, last) = self.character_bounds(span)?;
        Ok(last - first)
    }

    /// `getCharacterRun(index)` of a paragraph: its runs take the paragraph's style.
    pub(super) fn paragraph_run(&self, paragraph: &Paragraph, index: usize) -> Read<CharacterRun> {
        let chpx = self.character_at(paragraph.span, index)?;
        self.character_run(paragraph.span, chpx, paragraph.istd)
    }

    /// `getCharacterRun(index)` of any other range: its runs take the style of the paragraph
    /// they start in, and none is made where that lies past the paragraphs.
    pub(super) fn range_run(&self, span: Span, index: usize) -> Read<Option<CharacterRun>> {
        let chpx = self.character_at(span, index)?;
        let run = &self.characters[chpx];
        let (first, _) = find_range(
            &self.paragraphs,
            Span {
                start: run.start.max(span.start),
                end: run.end.min(span.end),
            },
        )?;
        let (paragraph_start, _) = self.paragraph_bounds(span)?;
        if first.max(paragraph_start) >= self.paragraphs.len() {
            return Ok(None);
        }
        let istd = self.paragraphs.istd(first)?;
        self.character_run(span, chpx, istd).map(Some)
    }

    fn character_at(&self, span: Span, index: usize) -> Read<usize> {
        let (first, last) = self.character_bounds(span)?;
        if first + index >= last {
            return Err(Thrown::new(format!(
                "CHPX #{index} ({}) not in range [{first}; {last})",
                first + index
            )));
        }
        Ok(first + index)
    }

    /// `new CharacterRun(chpx, styleSheet, istd, parent)`.
    fn character_run(&self, parent: Span, chpx: usize, istd: i16) -> Read<CharacterRun> {
        let run = &self.characters[chpx];
        let span = self.subrange(parent.start.max(run.start), parent.end.min(run.end))?;
        let paragraph_style = self.styles.character_style(i32::from(istd))?;
        let chp = run_chp(
            paragraph_style.as_ref(),
            |style| self.styles.chpx(style),
            run.grpprl(),
        );
        Ok(CharacterRun { span, chp })
    }

    /// `Range.getTable(paragraph)` of the range a paragraph is in: the paragraphs from it on
    /// that are in a table at its level or deeper, in rows ending at each row end mark of its
    /// level.
    pub(super) fn table(&self, range: Span, paragraph: &Paragraph) -> Read<Table> {
        let level = paragraph.table_level();
        let (start_index, _) = self.paragraph_bounds(paragraph.span)?;
        self.character_bounds(paragraph.span)?;
        if start_index != 0 {
            let previous = self.new_paragraph(range, start_index - 1)?;
            // Tika compares the section with one it never sets, which is 0 (#111).
            if previous.is_in_table()
                && previous.table_level() == level
                && self.section_start(previous.span) == self.section_start(paragraph.span)
            {
                return Err(Thrown::new(
                    "This paragraph is not the first one in the table",
                ));
            }
        }
        let overall = self.range(0, self.text.len() as i32)?;
        let mut end_index = start_index;
        while end_index + 1 < self.paragraphs.len() {
            let next = self.new_paragraph(overall, end_index + 1)?;
            if !next.is_in_table() || next.table_level() < level {
                break;
            }
            end_index += 1;
        }
        self.paragraph_bounds(range)?;
        self.character_bounds(range)?;
        let span = self.subrange(paragraph.span.start, self.paragraphs.end(end_index))?;
        let rows = self.rows(span, level)?;
        Ok(Table { span, rows })
    }

    /// `initSections` of a paragraph, whose first section index starts the search at 0: the
    /// index of the first section of the range.
    fn section_start(&self, span: Span) -> usize {
        let sections = &self.sections;
        if sections.is_empty() {
            return 0;
        }
        let mut x = 0;
        while sections[x].1 <= span.start && x < sections.len() - 1 {
            x += 1;
        }
        let (start, end) = sections[x];
        if start > span.end {
            return 0;
        }
        if end <= span.start {
            return sections.len();
        }
        x
    }

    /// `Table.initRows`: a row ends at each paragraph ending a row at the table's level.
    fn rows(&self, table: Span, level: i32) -> Read<Vec<Vec<Span>>> {
        let count = self.paragraph_count(table)?;
        let mut rows = Vec::new();
        let mut row_start = None;
        for row_end in 0..count {
            let last = self.paragraph(table, row_end)?;
            let start = *row_start.get_or_insert(last.span.start);
            if last.is_table_row_end() && last.table_level() == level {
                let row = self.subrange(start, last.span.end)?;
                rows.push(self.cells(row, level)?);
                row_start = None;
            }
        }
        Ok(rows)
    }

    /// `new TableRow(...)` and its `initCells`: a cell ends at each paragraph ending in a cell
    /// mark at the row's level. Paragraphs after the last cell mark but the row's end make a
    /// cell, where POI takes their paragraph indexes for character positions (#111); where the
    /// cells are not as many as the row's properties say, a last cell of a single row end
    /// paragraph goes.
    fn cells(&self, row: Span, level: i32) -> Read<Vec<Span>> {
        let count = self.paragraph_count(row)?;
        let last = self.paragraph(row, count - 1)?;
        let sprms = self.paragraphs.sprms(last.papx).ok_or_else(|| {
            Thrown::new("Cannot process TableProperties with an empty SprmBuffer")
        })?;
        let expected = i64::from(table_cell_count(sprms.bytes())?);
        let mut cells = Vec::new();
        let mut cell_start = 0;
        for index in 0..count {
            let paragraph = self.paragraph(row, index)?;
            self.check_span(paragraph.span)?;
            let span = paragraph.span;
            let ends_with_mark = span.end > span.start
                && self.text_is(
                    Span {
                        start: span.end - 1,
                        end: span.end,
                    },
                    &[CELL_MARK],
                )?;
            let ends_cell = ends_with_mark || paragraph.pap.inner_table_cell;
            if ends_cell && paragraph.table_level() == level {
                let first = self.paragraph(row, cell_start)?;
                cells.push(self.subrange(first.span.start, paragraph.span.end)?);
                cell_start = index + 1;
            }
        }
        if cell_start + 1 < count {
            let first = self.paragraph(row, cell_start)?;
            let last = self.paragraph(row, count - 2)?;
            cells.push(self.subrange(first.span.start, last.span.end)?);
        }
        if !cells.is_empty() && cells.len() as i64 != expected {
            let last = cells[cells.len() - 1];
            if self.paragraph_count(last)? == 1 && self.paragraph(last, 0)?.is_table_row_end() {
                cells.pop();
            }
        }
        Ok(cells)
    }
}

/// `new Range(start, end, document)`, and, given the length of the text, `new Range(start,
/// end, parent)`, which also checks the range lies within the text.
pub(super) fn subrange(start: i32, end: i32, text_length: Option<usize>) -> Read<Span> {
    if start < 0 {
        return Err(Thrown::new(format!(
            "Range start must not be negative. Given {start}"
        )));
    }
    if end < start {
        return Err(Thrown::new(format!(
            "The end ({end}) must not be before the start ({start})"
        )));
    }
    if let Some(length) = text_length {
        let length = length as i64;
        if i64::from(start) > length || i64::from(end) > length {
            return Err(Thrown::new("start or end can't be > text length"));
        }
    }
    Ok(Span { start, end })
}

/// Runs over ranges of characters, in order, as `Range` searches them.
pub(super) trait Runs {
    fn count(&self) -> usize;
    fn start_of(&self, index: usize) -> i32;
    fn end_of(&self, index: usize) -> i32;
}

impl Runs for [PropertyRun] {
    fn count(&self) -> usize {
        self.len()
    }

    fn start_of(&self, index: usize) -> i32 {
        self[index].start
    }

    fn end_of(&self, index: usize) -> i32 {
        self[index].end
    }
}

impl Runs for Paragraphs {
    fn count(&self) -> usize {
        self.len()
    }

    fn start_of(&self, index: usize) -> i32 {
        self.start(index)
    }

    fn end_of(&self, index: usize) -> i32 {
        self.end(index)
    }
}

/// `Range.findRange(rpl, start, end)`: the runs a range covers, `[first, last)`.
pub(super) fn find_range<R: Runs + ?Sized>(runs: &R, span: Span) -> Read<(usize, usize)> {
    let count = runs.count() as i64;
    let start_index = binary_search_start(runs, span.start);
    let mut start_index = start_index;
    while start_index > 0 && runs.start_of((start_index - 1) as usize) >= span.start {
        start_index -= 1;
    }
    let mut end_index = binary_search_end(runs, start_index, span.end)?;
    while end_index < count - 1 && runs.end_of((end_index + 1) as usize) <= span.end {
        end_index += 1;
    }
    if start_index < 0
        || start_index >= count
        || start_index > end_index
        || end_index < 0
        || end_index >= count
    {
        return Err(Thrown::new("problem finding range"));
    }
    Ok((start_index as usize, end_index as usize + 1))
}

/// `Range.binarySearchStart`.
fn binary_search_start<R: Runs + ?Sized>(runs: &R, start: i32) -> i64 {
    if runs.count() == 0 {
        return -1;
    }
    if runs.start_of(0) >= start {
        return 0;
    }
    let (mut low, mut high) = (0i64, runs.count() as i64 - 1);
    while low <= high {
        let mid = (low + high) >> 1;
        let at = runs.start_of(mid as usize);
        if at < start {
            low = mid + 1;
        } else if at > start {
            high = mid - 1;
        } else {
            return mid;
        }
    }
    low - 1
}

/// `Range.binarySearchEnd`, failing where POI indexes out of the runs.
fn binary_search_end<R: Runs + ?Sized>(runs: &R, found_start: i64, end: i32) -> Read<i64> {
    let count = runs.count();
    if count == 0 {
        return Err(Thrown::new("Index -1 out of bounds for length 0"));
    }
    if runs.end_of(count - 1) <= end {
        return Ok(count as i64 - 1);
    }
    let (mut low, mut high) = (found_start, count as i64 - 1);
    while low <= high {
        let mid = (low + high) >> 1;
        let at = usize::try_from(mid)
            .ok()
            .filter(|&mid| mid < count)
            .map(|mid| runs.end_of(mid))
            .ok_or_else(|| Thrown::new(format!("Index {mid} out of bounds")))?;
        if at < end {
            low = mid + 1;
        } else if at > end {
            high = mid - 1;
        } else {
            return Ok(mid);
        }
    }
    Ok(low)
}
