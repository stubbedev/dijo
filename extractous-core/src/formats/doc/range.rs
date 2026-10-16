//! POI's `Range` and the ranges Tika's extractor walks: paragraphs with their properties as
//! `Paragraph.newParagraph` makes them, character runs with theirs, and tables of rows of cells
//! as `Range.getTable`, `Table` and `TableRow` find them, with POI's quirks.

use super::bins::PropertyRun;
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

    /// `isInList`: an `ilfo` of 0xF801, sign-extended as POI reads it, never equals the
    /// unsigned value POI compares it to.
    pub(super) fn is_in_list(&self) -> bool {
        self.pap.ilfo != 0 && self.pap.ilfo != 0xF801
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

impl Document {
    /// `text`: the text of a range, failing where it runs past the text.
    pub(super) fn text_of(&self, span: Span) -> Read<&[u16]> {
        self.text
            .get(span.start as usize..span.end as usize)
            .ok_or_else(|| {
                Thrown(format!(
                    "begin {}, end {}, length {}",
                    span.start,
                    span.end,
                    self.text.len()
                ))
            })
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
            return Err(Thrown(format!(
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
        let run = &self.paragraphs[papx];
        let istd = run.istd()?;
        let style = self.styles.papx(i32::from(istd))?;
        let mut pap = Pap {
            istd: i32::from(istd),
            ..Pap::default()
        };
        pap = uncompress_pap(&pap, &style, 2)?;
        pap = uncompress_pap(&pap, run.grpprl(), 2)?;
        if pap.ilfo != 0 {
            if let Some(lists) = &self.lists {
                if let Some(level) = lists.level(pap.ilfo, i32::from(pap.ilvl))? {
                    pap = uncompress_pap(&pap, &level.grpprl_papx, 0)?;
                    pap = uncompress_pap(&pap, &style, 2)?;
                    pap = uncompress_pap(&pap, run.grpprl(), 2)?;
                }
            }
        }
        let span = self.subrange(parent.start.max(run.start), parent.end.min(run.end))?;
        Ok(Paragraph {
            span,
            papx,
            istd,
            pap,
        })
    }

    /// `initCharacterRuns`.
    fn character_bounds(&self, span: Span) -> Read<(usize, usize)> {
        find_range(&self.characters, span)
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
        let istd = self.paragraphs[first].istd()?;
        self.character_run(span, chpx, istd).map(Some)
    }

    fn character_at(&self, span: Span, index: usize) -> Read<usize> {
        let (first, last) = self.character_bounds(span)?;
        if first + index >= last {
            return Err(Thrown(format!(
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
        let section_start = self.section_start(paragraph.span);
        if start_index != 0 {
            let previous = self.new_paragraph(range, start_index - 1)?;
            if previous.is_in_table() && previous.table_level() == level && section_start == 0 {
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
        let span = self.subrange(paragraph.span.start, self.paragraphs[end_index].end)?;
        let rows = self.rows(span, level)?;
        Ok(Table { span, rows })
    }

    /// `initSections` of a paragraph, whose first section index starts the search at 0: the
    /// index of the first section of the range. `Range.getTable` compares it to the last section
    /// index of the previous paragraph, which is never found and stays 0.
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
        let mut row_start = 0;
        for row_end in 0..count {
            let first = self.paragraph(table, row_start)?;
            let last = self.paragraph(table, row_end)?;
            if last.is_table_row_end() && last.table_level() == level {
                let row = self.subrange(first.span.start, last.span.end)?;
                rows.push(self.cells(row, level)?);
                row_start = row_end + 1;
            }
        }
        Ok(rows)
    }

    /// `new TableRow(...)` and its `initCells`: a cell ends at each paragraph ending in a cell
    /// mark at the row's level. Paragraphs after the last cell mark make a cell whose bounds are
    /// paragraph indexes taken for character positions, as POI makes it; where the cells are
    /// not as many as the row's properties say, a last cell of a single row end paragraph goes.
    fn cells(&self, row: Span, level: i32) -> Read<Vec<Span>> {
        let count = self.paragraph_count(row)?;
        let last = self.paragraph(row, count - 1)?;
        let sprms = self.paragraphs[last.papx].sprms.as_ref().ok_or_else(|| {
            Thrown::new("Cannot process TableProperties with an empty SprmBuffer")
        })?;
        let expected = i64::from(table_cell_count(sprms.bytes())?);
        let mut cells = Vec::new();
        let mut cell_start = 0;
        for index in 0..count {
            let paragraph = self.paragraph(row, index)?;
            let text = self.text_of(paragraph.span)?;
            let ends_cell = text.last() == Some(&CELL_MARK) || paragraph.pap.inner_table_cell;
            if ends_cell && paragraph.table_level() == level {
                let first = self.paragraph(row, cell_start)?;
                cells.push(self.subrange(first.span.start, paragraph.span.end)?);
                cell_start = index + 1;
            }
        }
        if cell_start + 1 < count {
            cells.push(self.subrange(cell_start as i32, count as i32 - 1)?);
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
        return Err(Thrown(format!(
            "Range start must not be negative. Given {start}"
        )));
    }
    if end < start {
        return Err(Thrown(format!(
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

/// `Range.findRange(rpl, start, end)`: the runs a range covers, `[first, last)`.
pub(super) fn find_range(runs: &[PropertyRun], span: Span) -> Read<(usize, usize)> {
    let start_index = binary_search_start(runs, span.start);
    let mut start_index = start_index;
    while start_index > 0 && runs[(start_index - 1) as usize].start >= span.start {
        start_index -= 1;
    }
    let mut end_index = binary_search_end(runs, start_index, span.end)?;
    while end_index < runs.len() as i64 - 1 && runs[(end_index + 1) as usize].end <= span.end {
        end_index += 1;
    }
    if start_index < 0
        || start_index >= runs.len() as i64
        || start_index > end_index
        || end_index < 0
        || end_index >= runs.len() as i64
    {
        return Err(Thrown::new("problem finding range"));
    }
    Ok((start_index as usize, end_index as usize + 1))
}

/// `Range.binarySearchStart`.
fn binary_search_start(runs: &[PropertyRun], start: i32) -> i64 {
    if runs.is_empty() {
        return -1;
    }
    if runs[0].start >= start {
        return 0;
    }
    let (mut low, mut high) = (0i64, runs.len() as i64 - 1);
    while low <= high {
        let mid = (low + high) >> 1;
        let at = runs[mid as usize].start;
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
fn binary_search_end(runs: &[PropertyRun], found_start: i64, end: i32) -> Read<i64> {
    let Some(last) = runs.last() else {
        return Err(Thrown::new("Index -1 out of bounds for length 0"));
    };
    if last.end <= end {
        return Ok(runs.len() as i64 - 1);
    }
    let (mut low, mut high) = (found_start, runs.len() as i64 - 1);
    while low <= high {
        let mid = (low + high) >> 1;
        let at = usize::try_from(mid)
            .ok()
            .and_then(|mid| runs.get(mid))
            .ok_or_else(|| Thrown(format!("Index {mid} out of bounds")))?
            .end;
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
