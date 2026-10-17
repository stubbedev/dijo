//! extractous's `ReadingOrder`: a page's glyphs put into reading order before PDFBox assembles
//! them into lines, for pages whose content stream draws their text in another order. Blocks
//! of glyphs fall into bands, ordered within a band as T. M. Breuel orders text lines; a
//! vertically written page is turned a quarter first; and a line drawn in runs of different
//! fonts has its runs merged from the left.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::ops::Range;

use super::jfloat;
use super::matrix::Matrix;
use super::position::{is_left_to_right, is_right_to_left, TextPosition};
use super::structure::Label;

const SAME_LINE: f32 = 0.5;
const NEXT_LINE: f32 = 3.0;
const MAX_BLOCKS: usize = 300;
const SAME_COLUMN: f32 = 0.3;
const MIN_GLYPH_STEP: f32 = 0.6;
const MAX_GLYPH_STEP: f32 = 1.6;
const MIN_VERTICAL_STEPS: i32 = 8;
const VERTICAL_MAJORITY: i32 = 2;

/// `ReadingOrder.arrange`: the glyphs of an article reordered into reading order.
pub(super) fn arrange(glyphs: &mut Vec<TextPosition>) {
    if glyphs.len() < 2 || has_mixed_directions(glyphs) {
        return;
    }
    if is_written_vertically(glyphs) {
        turn_columns_into_lines(glyphs);
    }
    let mut blocks = blocks(std::mem::take(glyphs));
    let right_to_left = is_right_to_left_page(&blocks);
    let ordered: Vec<usize> = if blocks.len() > MAX_BLOCKS {
        (0..blocks.len()).collect()
    } else {
        let mut ordered = Vec::with_capacity(blocks.len());
        for band in bands(&blocks) {
            ordered.extend(order(&blocks, &band, right_to_left));
        }
        ordered
    };
    for index in ordered {
        let block = &mut blocks[index];
        if !right_to_left {
            order_lines_from_the_left(&mut block.glyphs);
        }
        glyphs.append(&mut block.glyphs);
    }
}

/// `ReadingOrder.orderLines`: the font runs of each line merged from the left.
pub(super) fn order_lines(glyphs: &mut [TextPosition]) {
    if glyphs.len() < 2 || has_mixed_directions(glyphs) || is_right_to_left_glyphs(glyphs) {
        return;
    }
    order_lines_from_the_left(glyphs);
}

fn order_lines_from_the_left(glyphs: &mut [TextPosition]) {
    let mut start = 0;
    for end in 1..=glyphs.len() {
        if end == glyphs.len() || !on_one_line(&glyphs[start], &glyphs[end]) {
            merge_font_runs(&mut glyphs[start..end]);
            start = end;
        }
    }
}

/// `mergeFontRuns`: the runs of glyphs in one font, each in its order, merged by taking next
/// the glyph furthest left among the runs' next glyphs, from the earlier run on a tie.
fn merge_font_runs(line: &mut [TextPosition]) {
    let mut runs: Vec<Range<usize>> = Vec::new();
    for (index, glyph) in line.iter().enumerate() {
        if index == 0 || glyph.font.identity != line[index - 1].font.identity {
            runs.push(index..index);
        }
        if let Some(run) = runs.last_mut() {
            run.end = index + 1;
        }
    }
    if runs.len() < 2 {
        return;
    }
    let mut merged = Vec::with_capacity(line.len());
    for _ in 0..line.len() {
        let mut leftmost: Option<usize> = None;
        for run in 0..runs.len() {
            if runs[run].is_empty() {
                continue;
            }
            let better = leftmost.is_none_or(|best| {
                line[runs[run].start].x_dir_adj() < line[runs[best].start].x_dir_adj()
            });
            if better {
                leftmost = Some(run);
            }
        }
        let run = leftmost.unwrap_or_else(|| unreachable!("a glyph is left in some run"));
        merged.push(runs[run].start);
        runs[run].start += 1;
    }
    permute(line, merged);
}

/// Puts `items[order[i]]` at `i` for every `i`, `order` being a permutation of the indices.
fn permute<T>(items: &mut [T], mut order: Vec<usize>) {
    for start in 0..order.len() {
        let mut current = start;
        loop {
            let next = order[current];
            order[current] = current;
            if next == start {
                break;
            }
            items.swap(current, next);
            current = next;
        }
    }
}

fn on_one_line(one: &TextPosition, other: &TextPosition) -> bool {
    (other.y_dir_adj() - one.y_dir_adj()).abs() <= SAME_LINE * jfloat::max(em(one), em(other))
}

fn em(glyph: &TextPosition) -> f32 {
    jfloat::max(glyph.font_size_in_pt(), glyph.height_dir())
}

fn has_mixed_directions(glyphs: &[TextPosition]) -> bool {
    let direction = glyphs[0].dir();
    glyphs.iter().any(|glyph| glyph.dir() != direction)
}

/// `isRightToLeft`: more glyphs starting with a right-to-left character than with a
/// left-to-right one.
fn is_right_to_left_glyphs(glyphs: &[TextPosition]) -> bool {
    let mut balance = 0i64;
    for glyph in glyphs {
        let Some(first) = glyph.unicode.chars().next() else {
            continue;
        };
        if is_right_to_left(first) {
            balance += 1;
        } else if is_left_to_right(first) {
            balance -= 1;
        }
    }
    balance > 0
}

fn is_right_to_left_page(blocks: &[Block]) -> bool {
    let mut balance = 0i64;
    for glyph in blocks.iter().flat_map(|block| &block.glyphs) {
        let Some(first) = glyph.unicode.chars().next() else {
            continue;
        };
        if is_right_to_left(first) {
            balance += 1;
        } else if is_left_to_right(first) {
            balance -= 1;
        }
    }
    balance > 0
}

/// `isWrittenVertically`.
fn is_written_vertically(glyphs: &[TextPosition]) -> bool {
    let mut down = 0;
    let mut along = 0;
    let mut last: Option<&TextPosition> = None;
    for glyph in glyphs {
        if glyph.unicode.chars().all(crate::unicode::is_whitespace) {
            continue;
        }
        if let Some(last) = last {
            let em = jfloat::max(em(last), em(glyph));
            let dx = glyph.x_dir_adj() - last.x_dir_adj();
            let dy = glyph.y_dir_adj() - last.y_dir_adj();
            if dx.abs() <= SAME_COLUMN * em
                && dy >= MIN_GLYPH_STEP * em
                && dy <= MAX_GLYPH_STEP * em
            {
                down += 1;
            } else if dy.abs() <= SAME_COLUMN * em && dx > 0.0 && dx <= MAX_GLYPH_STEP * em {
                along += 1;
            }
        }
        last = Some(glyph);
    }
    down >= MIN_VERTICAL_STEPS && down > VERTICAL_MAJORITY * along
}

/// `turnColumnsIntoLines`: each glyph replaced by one on a page turned a quarter clockwise.
fn turn_columns_into_lines(glyphs: &mut [TextPosition]) {
    for glyph in glyphs.iter_mut() {
        let matrix = glyph.text_matrix();
        let x = glyph.y_dir_adj();
        let y = glyph.x_dir_adj();
        let width = glyph.width_dir_adj();
        *glyph = TextPosition::new(
            0,
            glyph.page_height(),
            glyph.page_width(),
            Matrix::new(matrix.scale_x(), 0.0, 0.0, matrix.scale_y(), x, y),
            x + width,
            y,
            glyph.height_dir(),
            width,
            glyph.width_of_space(),
            glyph.unicode.clone(),
            std::rc::Rc::clone(&glyph.font),
            glyph.font_size(),
            glyph.font_size_in_pt() as i32,
            Label::Untagged,
        );
    }
}

/// A run of glyphs in content stream order, with the box they cover.
struct Block {
    order: usize,
    glyphs: Vec<TextPosition>,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    baseline: f32,
    em: f32,
}

impl Block {
    fn new(order: usize) -> Self {
        Self {
            order,
            glyphs: Vec::new(),
            left: f32::MAX,
            top: f32::MAX,
            right: -f32::MAX,
            bottom: -f32::MAX,
            baseline: 0.0,
            em: 0.0,
        }
    }

    fn continued_by(&self, glyph: &TextPosition) -> bool {
        let x = glyph.x_dir_adj();
        let y = glyph.y_dir_adj();
        let line_em = jfloat::max(self.em, em(glyph));
        if (y - self.baseline).abs() <= SAME_LINE * line_em {
            return true;
        }
        y > self.baseline
            && y - self.baseline <= NEXT_LINE * line_em
            && x >= self.left - line_em
            && x <= self.right + line_em
    }

    fn add(&mut self, glyph: TextPosition) {
        let x = glyph.x_dir_adj();
        let y = glyph.y_dir_adj();
        self.left = jfloat::min(self.left, x);
        self.right = jfloat::max(self.right, x + glyph.width_dir_adj());
        self.top = jfloat::min(self.top, y - glyph.height_dir());
        self.bottom = jfloat::max(self.bottom, y);
        self.baseline = y;
        self.em = em(&glyph);
        self.glyphs.push(glyph);
    }

    fn overlaps_horizontally(&self, other: &Block) -> bool {
        self.left < other.right && other.left < self.right
    }

    fn is_above(&self, other: &Block) -> bool {
        self.bottom <= other.top
    }

    fn is_between_vertically(&self, one: &Block, other: &Block) -> bool {
        self.top >= jfloat::min(one.bottom, other.bottom)
            && self.bottom <= jfloat::max(one.top, other.top)
    }
}

fn blocks(glyphs: Vec<TextPosition>) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for glyph in glyphs {
        let continues = blocks
            .last()
            .is_some_and(|block| block.continued_by(&glyph));
        if !continues {
            blocks.push(Block::new(blocks.len()));
        }
        if let Some(block) = blocks.last_mut() {
            block.add(glyph);
        }
    }
    blocks
}

/// `Double.compare` of two `float`s widened.
fn compare_double(a: f32, b: f32) -> Ordering {
    let (a, b) = (f64::from(a), f64::from(b));
    match a.partial_cmp(&b) {
        Some(Ordering::Equal) if a == 0.0 => a.is_sign_positive().cmp(&b.is_sign_positive()),
        Some(ordering) => ordering,
        None => a.is_nan().cmp(&b.is_nan()),
    }
}

/// `bands`: the blocks in bands from the top, each in content stream order.
fn bands(blocks: &[Block]) -> Vec<Vec<usize>> {
    let mut from_top: Vec<usize> = (0..blocks.len()).collect();
    from_top.sort_by(|&a, &b| compare_double(blocks[a].top, blocks[b].top));
    let mut bands: Vec<Vec<usize>> = Vec::new();
    let mut band_bottom = 0.0f32;
    for index in from_top {
        let block = &blocks[index];
        if bands.is_empty() || block.top >= band_bottom {
            bands.push(Vec::new());
            band_bottom = block.bottom;
        }
        if let Some(band) = bands.last_mut() {
            band.push(index);
        }
        band_bottom = jfloat::max(band_bottom, block.bottom);
    }
    for band in &mut bands {
        band.sort_by_key(|&index| blocks[index].order);
    }
    bands
}

/// A block in a priority queue by its content stream order, least first.
#[derive(PartialEq, Eq)]
struct ByOrder(usize, usize);

impl Ord for ByOrder {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.cmp(&self.0).then(other.1.cmp(&self.1))
    }
}

impl PartialOrd for ByOrder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `order`: a band's blocks in Breuel's partial order, ties by content stream order.
fn order(blocks: &[Block], band: &[usize], right_to_left: bool) -> Vec<usize> {
    let count = band.len();
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); count];
    let mut predecessors = vec![0usize; count];
    for (one, successors_of_one) in successors.iter_mut().enumerate() {
        for (other, predecessors_of_other) in predecessors.iter_mut().enumerate() {
            if one != other && precedes(blocks, band, one, other, right_to_left) {
                successors_of_one.push(other);
                *predecessors_of_other += 1;
            }
        }
    }
    let mut ready: BinaryHeap<ByOrder> = BinaryHeap::new();
    let mut waiting: BinaryHeap<ByOrder> = BinaryHeap::new();
    for (position, &index) in band.iter().enumerate() {
        let entry = ByOrder(blocks[index].order, position);
        if predecessors[position] == 0 {
            ready.push(entry);
        } else {
            waiting.push(entry);
        }
    }
    let mut placed = vec![false; count];
    let mut ordered = Vec::with_capacity(count);
    while ordered.len() < count {
        let next = match ready.pop() {
            Some(entry) => entry,
            None => loop {
                let entry = waiting
                    .pop()
                    .unwrap_or_else(|| unreachable!("a block is left to place"));
                if !placed[entry.1] && predecessors[entry.1] > 0 {
                    break entry;
                }
            },
        };
        let position = next.1;
        placed[position] = true;
        ordered.push(band[position]);
        for &successor in &successors[position] {
            if !placed[successor] {
                predecessors[successor] -= 1;
                if predecessors[successor] == 0 {
                    ready.push(ByOrder(blocks[band[successor]].order, successor));
                }
            }
        }
    }
    ordered
}

fn precedes(blocks: &[Block], band: &[usize], one: usize, other: usize, rtl: bool) -> bool {
    let (a, b) = (&blocks[band[one]], &blocks[band[other]]);
    if a.overlaps_horizontally(b) {
        return a.is_above(b);
    }
    let before = if rtl {
        a.left >= b.right
    } else {
        a.right <= b.left
    };
    if !before || b.is_above(a) {
        return false;
    }
    !band.iter().enumerate().any(|(position, &index)| {
        let between = &blocks[index];
        position != one
            && position != other
            && between.overlaps_horizontally(a)
            && between.overlaps_horizontally(b)
            && between.is_between_vertically(a, b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permutes_in_place() {
        let mut items = ['a', 'b', 'c', 'd', 'e'];
        permute(&mut items, vec![2, 0, 1, 4, 3]);
        assert_eq!(items, ['c', 'a', 'b', 'e', 'd']);
        let mut same = ['x', 'y'];
        permute(&mut same, vec![0, 1]);
        assert_eq!(same, ['x', 'y']);
    }
}
