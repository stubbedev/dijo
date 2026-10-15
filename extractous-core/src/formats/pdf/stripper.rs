//! `PDFTextStripper.writePage`: a page's glyphs, in content stream order, assembled into words,
//! lines and paragraphs with PDFBox's spacing heuristics, under Tika's settings (duplicate text
//! kept, not sorted by position, a space between words), and handed to the writer's callbacks,
//! which Tika's `PDF2XHTML` and `PDFMarkedContent2XHTML` override.

use std::collections::HashMap;
use std::sync::LazyLock;

use unicode_normalization::UnicodeNormalization;

use super::jfloat;
use super::position::TextPosition;
use super::Unported;
use crate::formats::xhtml::Stop;

const BIDI_MIRRORING: &str = include_str!("../../../models/pdf/BidiMirroring.txt");

/// `PDFParserConfig`'s settings of the stripper.
const SPACING_TOLERANCE: f32 = 0.5;
const AVERAGE_CHAR_TOLERANCE: f32 = 0.3;
const DROP_THRESHOLD: f32 = 2.5;
const INDENT_THRESHOLD: f32 = 2.0;
/// The word separator, which `enableAutoSpace` sets.
pub(super) const WORD_SEPARATOR: &str = " ";

/// Why writing stopped: the output stopped, or a word PDFBox writes in a way the port does not
/// follow.
#[derive(Debug)]
pub(super) enum WriteError {
    Stop(Stop),
    Unported(Unported),
}

impl From<Stop> for WriteError {
    fn from(stop: Stop) -> Self {
        Self::Stop(stop)
    }
}

impl From<Unported> for WriteError {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

/// The calls `writePage` makes, which Tika's writers override.
pub(super) trait Callbacks {
    /// `writePageStart`, where extractous orders a tagged page's lines.
    fn write_page_start(&mut self, articles: &mut [Vec<TextPosition>]);
    fn write_paragraph_start(&mut self) -> Result<(), Stop>;
    fn write_paragraph_end(&mut self) -> Result<(), Stop>;
    fn write_string(&mut self, text: &str, positions: &[&TextPosition]) -> Result<(), Stop>;
    fn write_word_separator(&mut self) -> Result<(), Stop>;
    fn write_line_separator(&mut self) -> Result<(), Stop>;
}

/// `PositionWrapper`: a glyph, by article and index, with what line assembly finds of it.
#[derive(Debug, Clone, Copy)]
struct Wrapper {
    article: usize,
    position: usize,
    line_start: bool,
    paragraph_start: bool,
    hanging_indent: bool,
    article_start: bool,
}

impl Wrapper {
    fn new(article: usize, position: usize) -> Self {
        Self {
            article,
            position,
            line_start: false,
            paragraph_start: false,
            hanging_indent: false,
            article_start: false,
        }
    }

    fn of<'a>(&self, articles: &'a [Vec<TextPosition>]) -> &'a TextPosition {
        &articles[self.article][self.position]
    }

    fn same_glyph(&self, other: &Wrapper) -> bool {
        self.article == other.article && self.position == other.position
    }
}

/// `LineItem`: a glyph, or a word separator.
#[derive(Debug, Clone, Copy)]
enum LineItem {
    Position(usize),
    WordSeparator,
}

/// `PDFTextStripper.writePage`.
pub(super) fn write_page(
    articles: &mut [Vec<TextPosition>],
    callbacks: &mut impl Callbacks,
) -> Result<(), WriteError> {
    if !articles.is_empty() {
        callbacks.write_page_start(articles);
    }
    let articles: &[Vec<TextPosition>] = articles;
    let mut max_y_for_line = -f32::MAX;
    let mut end_of_last_text_x = -1.0f32;
    let mut last_word_spacing = -1.0f32;
    let mut max_height_for_line = -1.0f32;
    let mut start_of_page = true;
    let mut last_position: Option<Wrapper> = None;
    let mut last_line_start: Option<Wrapper> = None;
    for (article, positions) in articles.iter().enumerate() {
        let mut start_of_article = true;
        let mut line: Vec<LineItem> = Vec::new();
        let mut previous_average_char_width = -1.0f32;
        for (index, position) in positions.iter().enumerate() {
            let mut current = Wrapper::new(article, index);
            if let Some(last) = &last_position {
                if font_or_size_changed(position, last.of(articles)) {
                    previous_average_char_width = -1.0;
                }
            }
            let position_x = position.x();
            let position_y = position.y();
            let position_width = position.width();
            let position_height = position.height();
            let word_char_count = position.individual_widths().len() as f32;
            let word_spacing = position.width_of_space();
            let delta_space = if jfloat::same(word_spacing, 0.0) || word_spacing.is_nan() {
                f32::MAX
            } else if last_word_spacing < 0.0 {
                word_spacing * SPACING_TOLERANCE
            } else {
                (word_spacing + last_word_spacing) / 2.0 * SPACING_TOLERANCE
            };
            let average_char_width = if previous_average_char_width < 0.0 {
                position_width / word_char_count
            } else {
                (previous_average_char_width + position_width / word_char_count) / 2.0
            };
            let delta_char_width = average_char_width * AVERAGE_CHAR_TOLERANCE;
            let mut expected_start_of_next_word_x = -f32::MAX;
            if !jfloat::same(end_of_last_text_x, -1.0) {
                expected_start_of_next_word_x =
                    end_of_last_text_x + jfloat::min(delta_space, delta_char_width);
            }
            if let Some(last) = last_position.as_mut() {
                if start_of_article {
                    last.article_start = true;
                    if let Some(start) = last_line_start.as_mut().filter(|s| s.same_glyph(last)) {
                        start.article_start = true;
                    }
                    start_of_article = false;
                }
                if !overlap(
                    position_y,
                    position_height,
                    max_y_for_line,
                    max_height_for_line,
                ) {
                    write_line(&normalize(positions, &line)?, callbacks)?;
                    line.clear();
                    handle_line_separation(
                        articles,
                        &mut current,
                        last,
                        last_line_start.as_ref(),
                        max_height_for_line,
                        callbacks,
                    )?;
                    last_line_start = Some(current);
                    expected_start_of_next_word_x = -f32::MAX;
                    max_y_for_line = -f32::MAX;
                    max_height_for_line = -1.0;
                }
                let last_glyph = last.of(articles);
                if !jfloat::same(expected_start_of_next_word_x, -f32::MAX)
                    && expected_start_of_next_word_x < position_x
                    && !last_glyph.unicode.ends_with(WORD_SEPARATOR)
                {
                    line.push(LineItem::WordSeparator);
                }
                if (position.x() - last_glyph.x()).abs() > word_spacing + delta_space {
                    max_y_for_line = -f32::MAX;
                    max_height_for_line = -1.0;
                }
            }
            if position_y >= max_y_for_line {
                max_y_for_line = position_y;
            }
            end_of_last_text_x = position_x + position_width;
            if start_of_page && last_position.is_none() {
                callbacks.write_paragraph_start()?;
            }
            line.push(LineItem::Position(index));
            max_height_for_line = jfloat::max(max_height_for_line, position_height);
            if start_of_page {
                current.paragraph_start = true;
                current.line_start = true;
                last_line_start = Some(current);
                start_of_page = false;
            }
            if let Some(start) = last_line_start.as_mut().filter(|s| s.same_glyph(&current)) {
                *start = current;
            }
            last_position = Some(current);
            last_word_spacing = word_spacing;
            previous_average_char_width = average_char_width;
        }
        if !line.is_empty() {
            write_line(&normalize(positions, &line)?, callbacks)?;
            callbacks.write_paragraph_end()?;
        }
    }
    Ok(())
}

/// `hasFontOrSizeChanged`.
fn font_or_size_changed(current: &TextPosition, last: &TextPosition) -> bool {
    if !jfloat::same(current.font_size(), last.font_size()) {
        return true;
    }
    if current.font.identity == last.font.identity {
        return false;
    }
    match (current.font.name(), last.font.name()) {
        (Some(current), last) => Some(current) != last,
        (None, _) => true,
    }
}

/// `within`.
fn within(first: f32, second: f32, variance: f32) -> bool {
    second < first + variance && second > first - variance
}

/// `overlap`.
fn overlap(y1: f32, height1: f32, y2: f32, height2: f32) -> bool {
    within(y1, y2, 0.1) || (y2 <= y1 && y2 >= y1 - height1) || (y1 <= y2 && y1 >= y2 - height2)
}

/// `multiplyFloat`: the product rounded to three decimals.
fn multiply_float(value1: f32, value2: f32) -> f32 {
    jfloat::round(value1 * value2 * 1000.0) as f32 / 1000.0
}

/// `handleLineSeparation`: the separation of a new line from the last, a paragraph break
/// where `isParagraphSeparation` sees one.
fn handle_line_separation(
    articles: &[Vec<TextPosition>],
    current: &mut Wrapper,
    last: &Wrapper,
    last_line_start: Option<&Wrapper>,
    max_height_for_line: f32,
    callbacks: &mut impl Callbacks,
) -> Result<(), Stop> {
    current.line_start = true;
    is_paragraph_separation(
        articles,
        current,
        last,
        last_line_start,
        max_height_for_line,
    );
    if current.paragraph_start {
        if last.article_start {
            if last.line_start {
                callbacks.write_line_separator()?;
            }
            callbacks.write_paragraph_start()?;
        } else {
            callbacks.write_line_separator()?;
            callbacks.write_paragraph_end()?;
            callbacks.write_paragraph_start()?;
        }
    } else {
        callbacks.write_line_separator()?;
    }
    Ok(())
}

/// `isParagraphSeparation`.
fn is_paragraph_separation(
    articles: &[Vec<TextPosition>],
    current: &mut Wrapper,
    last: &Wrapper,
    last_line_start: Option<&Wrapper>,
    max_height_for_line: f32,
) {
    let mut result = false;
    match last_line_start {
        None => result = true,
        Some(line_start) => {
            let position = current.of(articles);
            let last_position = last.of(articles);
            let line_start_position = line_start.of(articles);
            let y_gap = (position.y_dir_adj() - last_position.y_dir_adj()).abs();
            let new_y = multiply_float(DROP_THRESHOLD, max_height_for_line);
            let x_gap = position.x_dir_adj() - line_start_position.x_dir_adj();
            let new_x = multiply_float(INDENT_THRESHOLD, position.width_of_space());
            let position_width = multiply_float(0.25, position.width());
            if y_gap > new_y {
                result = true;
            } else if x_gap > new_x {
                if !line_start.paragraph_start {
                    result = true;
                } else {
                    current.hanging_indent = true;
                }
            } else if x_gap < -position.width_of_space() {
                if !line_start.paragraph_start {
                    result = true;
                }
            } else if x_gap.abs() < position_width {
                if line_start.hanging_indent {
                    current.hanging_indent = true;
                } else if line_start.paragraph_start {
                    if let Some(pattern) = list_item_pattern(&line_start_position.unicode) {
                        if list_item_pattern(&position.unicode) == Some(pattern) {
                            result = true;
                        }
                    }
                }
            }
        }
    }
    if result {
        current.paragraph_start = true;
    }
}

/// `matchListItemPattern`: the first of PDFBox's list item patterns the glyph's text matches.
fn list_item_pattern(text: &str) -> Option<usize> {
    fn digits(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
    }
    fn roman(s: &str, letters: &[u8]) -> bool {
        !s.is_empty() && s.bytes().all(|b| letters.contains(&b))
    }
    fn single(s: &str, f: fn(&u8) -> bool) -> bool {
        s.len() == 1 && f(&s.as_bytes()[0])
    }
    let patterns: [&dyn Fn(&str) -> bool; 10] = [
        &|s| s == ".",
        &|s| s.strip_suffix('.').is_some_and(digits),
        &|s| {
            s.strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .is_some_and(digits)
        },
        &|s| s.strip_suffix(')').is_some_and(digits),
        &|s| {
            s.strip_suffix('.')
                .is_some_and(|s| single(s, u8::is_ascii_uppercase))
        },
        &|s| {
            s.strip_suffix('.')
                .is_some_and(|s| single(s, u8::is_ascii_lowercase))
        },
        &|s| {
            s.strip_suffix(')')
                .is_some_and(|s| single(s, u8::is_ascii_uppercase))
        },
        &|s| {
            s.strip_suffix(')')
                .is_some_and(|s| single(s, u8::is_ascii_lowercase))
        },
        &|s| s.strip_suffix('.').is_some_and(|s| roman(s, b"IVXL")),
        &|s| s.strip_suffix('.').is_some_and(|s| roman(s, b"ivxl")),
    ];
    patterns.iter().position(|pattern| pattern(text))
}

/// A word and the glyphs it was made from.
struct Word<'a> {
    text: String,
    positions: Vec<&'a TextPosition>,
}

/// `writeLine`.
fn write_line(line: &[Word], callbacks: &mut impl Callbacks) -> Result<(), Stop> {
    for (index, word) in line.iter().enumerate() {
        callbacks.write_string(&word.text, &word.positions)?;
        if index + 1 < line.len() {
            callbacks.write_word_separator()?;
        }
    }
    Ok(())
}

/// `normalize`: a line's items made into words, split at the word separators.
fn normalize<'a>(
    positions: &'a [TextPosition],
    line: &[LineItem],
) -> Result<Vec<Word<'a>>, Unported> {
    let mut words = Vec::new();
    let mut builder = String::new();
    let mut word_positions: Vec<&TextPosition> = Vec::new();
    for item in line {
        match item {
            LineItem::WordSeparator => {
                words.push(Word {
                    text: normalize_word(&builder)?,
                    positions: std::mem::take(&mut word_positions),
                });
                builder.clear();
            }
            LineItem::Position(index) => {
                let position = &positions[*index];
                builder.push_str(&position.visually_ordered_unicode());
                word_positions.push(position);
            }
        }
    }
    if !builder.is_empty() {
        words.push(Word {
            text: normalize_word(&builder)?,
            positions: word_positions,
        });
    }
    Ok(words)
}

/// `normalizeWord`: Unicode presentation forms decomposed, then `handleDirection`.
fn normalize_word(word: &str) -> Result<String, Unported> {
    let mut out = String::with_capacity(word.len());
    let mut previous: Option<char> = None;
    for c in word.chars() {
        let code = u32::from(c);
        if (0xFB00..=0xFDFF).contains(&code) || (0xFE70..=0xFEFF).contains(&code) {
            if code == 0xFDF2 && matches!(previous, Some('\u{0627}' | '\u{FE8D}')) {
                out.push_str("\u{0644}\u{0644}\u{0647}");
            } else {
                let normalized: String = c.nfkc().collect();
                let normalized = crate::formats::java::trim(&normalized);
                if code >= 0xFB1D && normalized.encode_utf16().count() > 1 {
                    out.extend(normalized.chars().rev());
                } else {
                    out.push_str(normalized);
                }
            }
        } else {
            out.push(c);
        }
        previous = Some(c);
    }
    handle_direction(&out)
}

/// PDFBox's mirrored characters (`BidiMirroring.txt`).
static MIRRORING: LazyLock<HashMap<char, char>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for line in BIDI_MIRRORING.lines() {
        let line = line.split('#').next().unwrap_or_default();
        if line.len() < 2 {
            continue;
        }
        let fields: Vec<char> = line
            .split(';')
            .filter_map(|field| u32::from_str_radix(field.trim(), 16).ok())
            .filter_map(char::from_u32)
            .collect();
        if let [from, to] = fields[..] {
            map.insert(from, to);
        }
    }
    map
});

/// `handleDirection`: a word with right-to-left runs reordered visually and their mirrored
/// characters mirrored, as `java.text.Bidi` orders them.
fn handle_direction(word: &str) -> Result<String, Unported> {
    use unicode_bidi::{BidiInfo, Level};
    if word.is_ascii() {
        return Ok(word.to_owned());
    }
    let info = BidiInfo::new(word, None);
    if info.levels.iter().all(|level| level.is_ltr()) {
        return Ok(word.to_owned());
    }
    let [paragraph] = info.paragraphs.as_slice() else {
        return Err(Unported(
            "a word holding a paragraph separator, which the port does not order as \
             java.text.Bidi does"
                .to_owned(),
        ));
    };
    let base = paragraph.level;
    let line_levels = info.reordered_levels(paragraph, paragraph.range.clone());
    let char_levels: Vec<(char, Level)> = word
        .char_indices()
        .map(|(byte, c)| (c, line_levels[byte]))
        .collect();
    if base.is_ltr() && char_levels.iter().all(|(_, level)| level.is_ltr()) {
        return Ok(word.to_owned());
    }
    let mut runs: Vec<(usize, usize, Level)> = Vec::new();
    for (index, &(_, level)) in char_levels.iter().enumerate() {
        match runs.last_mut() {
            Some(run) if run.2 == level => run.1 = index + 1,
            _ => runs.push((index, index + 1, level)),
        }
    }
    let levels: Vec<Level> = runs.iter().map(|run| run.2).collect();
    let mut result = String::with_capacity(word.len());
    for run in BidiInfo::reorder_visual(&levels) {
        let (start, end, level) = runs[run];
        if level.is_rtl() {
            for &(c, _) in char_levels[start..end].iter().rev() {
                if u32::from(c) > 0xFFFF {
                    return Err(Unported(
                        "a right-to-left run holding a character beyond the Basic Multilingual \
                         Plane, which PDFBox reverses unit by unit"
                            .to_owned(),
                    ));
                }
                result.push(MIRRORING.get(&c).copied().unwrap_or(c));
            }
        } else {
            result.extend(char_levels[start..end].iter().map(|&(c, _)| c));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_list_item_patterns_as_pdfbox_does() {
        assert_eq!(list_item_pattern("."), Some(0));
        assert_eq!(list_item_pattern("12."), Some(1));
        assert_eq!(list_item_pattern("[3]"), Some(2));
        assert_eq!(list_item_pattern("4)"), Some(3));
        assert_eq!(list_item_pattern("A."), Some(4));
        assert_eq!(list_item_pattern("iv."), Some(9));
        assert_eq!(list_item_pattern("I."), Some(4));
        assert_eq!(list_item_pattern("word"), None);
    }

    #[test]
    fn keeps_left_to_right_words_across_paragraph_separators() {
        assert_eq!(
            handle_direction("þú\u{2029}ħ\rö").unwrap(),
            "þú\u{2029}ħ\rö"
        );
        assert!(handle_direction("\u{5D0}\u{2029}b").is_err());
    }

    #[test]
    fn rounds_products_to_three_decimals() {
        assert_eq!(multiply_float(2.5, 10.0), 25.0);
        assert_eq!(multiply_float(0.25, 4.0), 1.0);
    }

    #[test]
    fn decomposes_presentation_forms_and_orders_right_to_left_words() {
        assert_eq!(normalize_word("\u{FB01}ne").unwrap(), "fine");
        assert_eq!(normalize_word("plain").unwrap(), "plain");
        assert_eq!(
            handle_direction("\u{05D0}\u{05D1}").unwrap(),
            "\u{05D1}\u{05D0}"
        );
        assert_eq!(handle_direction("(\u{05D0})").unwrap(), "(\u{05D0})");
    }
}
