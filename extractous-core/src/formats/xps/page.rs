//! Tika's `XPSPageContentHandler`: the glyph runs of a fixed page grouped by the canvases they
//! are drawn in, laid out in rows by their origins, each row split where a glyph advances far
//! and sorted by direction, written as paragraphs with a space between runs far apart; the
//! links of its paths; and the images its paths are filled with, which the document parses
//! once its pages are written.

use std::rc::Rc;

use crate::formats::xml::{Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java;

/// `ESTIMATE_GLYPH_WIDTH`: the width of a glyph whose advance is not given, before scaling.
const ESTIMATE_GLYPH_WIDTH: f32 = 0.5;
/// `WHITESPACE_THRESHOLD`: the gap, relative to the font size, a space stands for.
const WHITESPACE_THRESHOLD: f32 = 0.3;
/// `SPLIT_THRESHOLD`: the advance beyond which a run is split.
const SPLIT_THRESHOLD: f32 = 1.0;
/// `ROW_COMBINE_THRESHOLD`: the difference in height, relative to the font size, within which
/// runs share a row.
const ROW_COMBINE_THRESHOLD: f32 = 0.5;
/// What the canvas stack holds for a canvas without a clip or a brush without a transform.
const NULL_CLIP: &str = "NULL_CLIP";

/// What a fixed page writes, in order.
#[derive(Debug, PartialEq)]
pub(super) enum Block {
    /// A canvas of rows.
    Canvas(Vec<Row>),
    /// The links of the page's paths.
    Links(Vec<String>),
}

/// A paragraph of runs, each with whether a space comes before it.
pub(super) type Row = Vec<(bool, Vec<u16>)>;

/// An image a page's path is filled with, by its path in the archive as the page gives it, with
/// the original location of the first path that names one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Image {
    pub(super) original_location: Option<String>,
}

/// The images of the document's pages, in the order they were first filled with.
pub(super) type Images = Vec<(String, Image)>;

/// `GlyphRun.DIRECTION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    LeftToRight,
    RightToLeft,
}

/// `GlyphRun`: its advances are a range of a list a split shares with the runs split off it,
/// as Java's `subList` views share theirs.
#[derive(Debug, Clone)]
struct GlyphRun {
    origin_y: f32,
    origin_x: f32,
    text: Vec<u16>,
    advances: Rc<Vec<f32>>,
    start: usize,
    end: usize,
    direction: Direction,
    font_size: f32,
}

impl GlyphRun {
    fn advances(&self) -> &[f32] {
        &self.advances[self.start..self.end]
    }

    /// `width()`: the advances summed, a glyph without one as wide as the average before it,
    /// scaled by the font size.
    fn width(&self) -> f32 {
        let mut width = 0.0f32;
        for (i, &advance) in self.advances().iter().enumerate() {
            width += estimated(advance, width, i);
        }
        width * self.font_size
    }

    fn left(&self) -> f32 {
        match self.direction {
            Direction::LeftToRight => self.origin_x,
            Direction::RightToLeft => self.origin_x - self.width(),
        }
    }

    fn right(&self) -> f32 {
        match self.direction {
            Direction::LeftToRight => self.origin_x + self.width(),
            Direction::RightToLeft => self.origin_x,
        }
    }
}

/// What a glyph adds to a run's width: its advance, or for one without, half a unit at the
/// start and the average so far elsewhere.
fn estimated(advance: f32, width: f32, index: usize) -> f32 {
    if advance != 0.0 {
        advance
    } else if index == 0 {
        ESTIMATE_GLYPH_WIDTH
    } else {
        width / index as f32
    }
}

/// `XPSPageContentHandler`, reading a fixed page.
#[derive(Debug, Default)]
pub(super) struct PageHandler {
    image_source: Option<String>,
    original_location: Option<String>,
    /// The runs of each canvas, by the canvas stack they were drawn in, in the order the stacks
    /// were first drawn in.
    canvases: Vec<(String, Vec<GlyphRun>)>,
    urls: Vec<String>,
    canvas_stack: Vec<String>,
    /// The images the page's paths are filled with, merged into the document's at its end.
    images: Images,
}

/// The first value of an attribute with `local_name`, in any namespace, as `getVal` finds it.
fn value<'a>(attributes: &'a [Attribute], local_name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.local_name == local_name)
        .map(|attribute| attribute.value.as_ref())
}

/// `Float.parseFloat`, which fails the page's parse where it throws.
fn parse_float(text: &str) -> Result<f32, Failure> {
    java::parse_float(text).ok_or_else(|| {
        Failure::Malformed(format!(
            "NumberFormatException: For input string: \"{text}\""
        ))
    })
}

/// `parseIndicesString`: the advance of each glyph, in hundredths of the font size.
fn parse_indices(indices: &str) -> Result<Vec<f32>, Failure> {
    indices
        .split(';')
        .map(|index| {
            let parts: Vec<&str> = index.split(',').collect();
            match parts.get(1) {
                Some(advance) if !java::is_blank(advance) => Ok(parse_float(advance)? / 100.0),
                _ => Ok(0.0),
            }
        })
        .collect()
}

impl PageHandler {
    /// `startElement` of a `Glyphs` element: a run, when it has a string.
    fn glyphs(&mut self, attributes: &[Attribute]) -> Result<(), Failure> {
        let mut origin_x = None;
        let mut origin_y = None;
        let mut text: Option<&str> = None;
        let mut bidi_level = None;
        let mut indices = None;
        let mut font_size = 0.0f32;
        for attribute in attributes {
            let value = java::trim(&attribute.value);
            match attribute.local_name.as_ref() {
                "OriginX" if !value.is_empty() => origin_x = Some(parse_float(value)?),
                "OriginY" if !value.is_empty() => origin_y = Some(parse_float(value)?),
                "UnicodeString" => text = Some(attribute.value.as_ref()),
                "BidiLevel" if !value.is_empty() => {
                    bidi_level = Some(java::parse_int(value).ok_or_else(|| {
                        Failure::Malformed(format!(
                            "NumberFormatException: For input string: \"{value}\""
                        ))
                    })?);
                }
                "Indices" => indices = Some(parse_indices(value)?),
                "FontRenderingEmSize" => font_size = parse_float(value)?,
                _ => {}
            }
        }
        let Some(text) = text else {
            return Ok(());
        };
        let canvas: String = self
            .canvas_stack
            .iter()
            .flat_map(|entry| [entry.as_str(), ";"])
            .collect();
        let advances = Rc::new(indices.unwrap_or_default());
        let run = GlyphRun {
            origin_y: origin_y.unwrap_or(i32::MAX as f32),
            origin_x: origin_x.unwrap_or(i32::MIN as f32),
            text: text.encode_utf16().collect(),
            end: advances.len(),
            advances,
            start: 0,
            direction: match bidi_level {
                Some(level) if level % 2 != 0 => Direction::RightToLeft,
                _ => Direction::LeftToRight,
            },
            font_size,
        };
        match self.canvases.iter_mut().find(|(key, _)| *key == canvas) {
            Some((_, runs)) => runs.push(run),
            None => self.canvases.push((canvas, vec![run])),
        }
        Ok(())
    }

    /// `endDocument`'s `writePage`: what the page writes inside its `page` div, and the images
    /// of its paths.
    pub(super) fn finish(self) -> Result<(Vec<Block>, Images), Failure> {
        let mut blocks = Vec::new();
        for (_, runs) in self.canvases {
            if runs.is_empty() {
                continue;
            }
            let rows = build_rows(runs)?;
            let mut written = Vec::with_capacity(rows.len());
            for row in rows {
                written.push(write_row(row)?);
            }
            blocks.push(Block::Canvas(written));
        }
        if !self.urls.is_empty() {
            blocks.push(Block::Links(self.urls));
        }
        Ok((blocks, self.images))
    }
}

impl SaxHandler for PageHandler {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        match local_name {
            "Canvas" => {
                let clip = value(attributes, "Clip").unwrap_or(NULL_CLIP);
                self.canvas_stack.push(clip.to_owned());
                return Ok(());
            }
            "VisualBrush" => {
                let transform = value(attributes, "Transform").unwrap_or(NULL_CLIP);
                self.canvas_stack.push(transform.to_owned());
                return Ok(());
            }
            "Path" => {
                if let Some(url) = value(attributes, "FixedPage.NavigateUri") {
                    if !self.urls.iter().any(|known| known == url) {
                        self.urls.push(url.to_owned());
                    }
                }
                self.original_location =
                    value(attributes, "AutomationProperties.HelpText").map(str::to_owned);
            }
            "ImageBrush" => {
                self.image_source = value(attributes, "ImageSource").map(str::to_owned);
            }
            _ => {}
        }
        if local_name == "Glyphs" {
            self.glyphs(attributes)?;
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        match local_name {
            "Canvas" => {
                self.canvas_stack.pop();
            }
            "Path" => {
                if let Some(source) = self.image_source.take() {
                    let location = self.original_location.take();
                    match self.images.iter_mut().find(|(known, _)| *known == source) {
                        Some((_, image)) => {
                            if image.original_location.is_none() {
                                image.original_location = location;
                            }
                        }
                        None => self.images.push((
                            source,
                            Image {
                                original_location: location,
                            },
                        )),
                    }
                }
                self.image_source = None;
                self.original_location = None;
            }
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// Adds the images of a page to the document's: a path new to it in order, and for one it has,
/// the original location where it has none.
pub(super) fn merge_images(document: &mut Images, page: Images) {
    for (source, image) in page {
        match document.iter_mut().find(|(known, _)| *known == source) {
            Some((_, known)) => {
                if known.original_location.is_none() {
                    known.original_location = image.original_location;
                }
            }
            None => document.push((source, image)),
        }
    }
}

/// `Float.compare`.
fn compare_floats(a: f32, b: f32) -> std::cmp::Ordering {
    let bits = |value: f32| {
        if value.is_nan() {
            0x7fc0_0000_i32
        } else {
            value.to_bits() as i32
        }
    };
    if a < b {
        std::cmp::Ordering::Less
    } else if a > b {
        std::cmp::Ordering::Greater
    } else {
        bits(a).cmp(&bits(b))
    }
}

/// `buildRows`: each run in the row whose last run is closest in height, unless its first run
/// is too far, and then in a row of its own, the rows sorted by height when a new row starts
/// above the lowest yet.
fn build_rows(runs: Vec<GlyphRun>) -> Result<Vec<Vec<GlyphRun>>, Failure> {
    let mut rows: Vec<Vec<GlyphRun>> = Vec::new();
    let mut max_y = -1.0f32;
    for run in runs {
        if rows.is_empty() {
            rows.push(vec![run]);
            continue;
        }
        let closest = closest_row(&rows, run.origin_y).ok_or_else(|| {
            Failure::Malformed(
                "NullPointerException: no row is closest to a run of no height".to_owned(),
            )
        })?;
        let first = &rows[closest][0];
        let average = (run.font_size + first.font_size) / 2.0;
        let mut added_row = false;
        let origin_y = run.origin_y;
        if (run.origin_y - first.origin_y).abs() < average * ROW_COMBINE_THRESHOLD {
            rows[closest].push(run);
        } else {
            rows.push(vec![run]);
            added_row = true;
        }
        if max_y > -1.0 && added_row && origin_y < max_y {
            if rows.iter().any(|row| row[0].origin_y.is_nan()) {
                return Err(Failure::Malformed(
                    "rows of no height, which Java's sort orders as it finds them".to_owned(),
                ));
            }
            rows.sort_by(|a, b| {
                a[0].origin_y
                    .partial_cmp(&b[0].origin_y)
                    .expect("heights that are numbers")
            });
        }
        if origin_y > max_y {
            max_y = origin_y;
        }
    }
    Ok(rows)
}

/// `findClosestRowVertically`: the row whose last run is nearest in height, the last such row
/// on a tie, or none where every distance is not a number.
fn closest_row(rows: &[Vec<GlyphRun>], y: f32) -> Option<usize> {
    let mut best = None;
    let mut best_distance = f32::INFINITY;
    for (index, row) in rows.iter().enumerate().rev() {
        let Some(last) = row.last() else {
            continue;
        };
        let distance = (last.origin_y - y).abs();
        if distance == 0.0 {
            return Some(index);
        }
        if distance < best_distance {
            best = Some(index);
            best_distance = distance;
        }
    }
    best
}

/// `writeRow`: the row split and sorted, each run after a space where it starts far enough
/// from the one before.
fn write_row(row: Vec<GlyphRun>) -> Result<Row, Failure> {
    let mut row = split_row(row)?;
    sort_row(&mut row);
    let mut written = Vec::with_capacity(row.len());
    let mut previous: Option<&GlyphRun> = None;
    for run in &row {
        let space = previous.is_some_and(|previous| {
            let distance = run.left() - previous.right();
            let average = (run.font_size + previous.font_size) / 2.0;
            distance > average * WHITESPACE_THRESHOLD
        });
        written.push((space, run.text.clone()));
        previous = Some(run);
    }
    Ok(written)
}

/// `substring`, in UTF-16 units, which fails beyond the end.
fn substring(text: &[u16], start: usize, end: usize) -> Result<Vec<u16>, Failure> {
    text.get(start..end).map(<[u16]>::to_vec).ok_or_else(|| {
        Failure::Malformed(format!(
            "StringIndexOutOfBoundsException: begin {start}, end {end}, length {}",
            text.len()
        ))
    })
}

/// `splitRow`: each left-to-right run split after each glyph that advances more than the split
/// threshold. The glyph a run is split after keeps no advance in the part before, which shares
/// the advances with what follows; the scan goes on from the second glyph of what follows.
fn split_row(row: Vec<GlyphRun>) -> Result<Vec<GlyphRun>, Failure> {
    let mut split = Vec::with_capacity(row.len());
    for run in row {
        if run.direction != Direction::LeftToRight {
            split.push(run);
            continue;
        }
        let mut advances: Vec<f32> = run.advances().to_vec();
        let mut pieces: Vec<(usize, usize, f32, Vec<u16>)> = Vec::new();
        let (mut start, mut origin_x, mut text) = (0usize, run.origin_x, run.text);
        let mut width = 0.0f32;
        let mut i = 0usize;
        while i + 1 < advances.len() - start {
            let advance = advances[start + i];
            width += estimated(advance, width, i);
            if advance > SPLIT_THRESHOLD {
                let before = substring(&text, 0, i + 1)?;
                pieces.push((start, start + i + 1, origin_x, before));
                advances[start + i] = 0.0;
                let after = substring(&text, i + 1, text.len())?;
                origin_x += width * run.font_size;
                start += i + 1;
                text = after;
                i = 0;
                width = 0.0;
            }
            i += 1;
        }
        let shared = Rc::new(advances);
        let end = shared.len();
        let piece = |(start, end, origin_x, text): (usize, usize, f32, Vec<u16>)| GlyphRun {
            origin_y: run.origin_y,
            origin_x,
            text,
            advances: Rc::clone(&shared),
            start,
            end,
            direction: Direction::LeftToRight,
            font_size: run.font_size,
        };
        split.extend(pieces.into_iter().map(&piece));
        split.push(piece((start, end, origin_x, text)));
    }
    Ok(split)
}

/// `sortRow`: right to left where every run with text is, else left to right, by the runs'
/// left edges.
fn sort_row(row: &mut [GlyphRun]) {
    let right_to_left = row
        .iter()
        .filter(|run| !java::is_blank(&String::from_utf16_lossy(&run.text)))
        .all(|run| run.direction != Direction::LeftToRight);
    if right_to_left {
        row.sort_by(|a, b| compare_floats(b.left(), a.left()));
    } else {
        row.sort_by(|a, b| compare_floats(a.left(), b.left()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, x: f32, y: f32, advances: &[f32]) -> GlyphRun {
        let advances = Rc::new(advances.to_vec());
        GlyphRun {
            origin_y: y,
            origin_x: x,
            text: text.encode_utf16().collect(),
            end: advances.len(),
            advances,
            start: 0,
            direction: Direction::LeftToRight,
            font_size: 10.0,
        }
    }

    fn texts(row: &[GlyphRun]) -> Vec<String> {
        row.iter()
            .map(|run| String::from_utf16_lossy(&run.text))
            .collect()
    }

    #[test]
    fn splits_a_run_after_a_wide_advance_and_zeroes_it_in_the_part_before() {
        let split = split_row(vec![run("ab cd", 0.0, 0.0, &[0.5, 0.5, 3.0, 0.5, 0.5])]).unwrap();
        assert_eq!(texts(&split), ["ab ", "cd"]);
        assert_eq!(split[0].advances(), [0.5, 0.5, 0.0]);
        assert_eq!(split[0].width(), (0.5 + 0.5 + 0.5) * 10.0);
        assert_eq!(split[1].origin_x, 40.0);
    }

    #[test]
    fn fails_where_the_advances_outrun_the_text() {
        assert!(split_row(vec![run("a", 0.0, 0.0, &[0.5, 2.0, 0.5, 0.5])]).is_err());
    }

    #[test]
    fn rows_runs_by_height_and_sorts_rows_started_above_the_lowest_after_the_first() {
        let rows = |runs: Vec<GlyphRun>| -> Vec<Vec<String>> {
            build_rows(runs)
                .unwrap()
                .iter()
                .map(|row| texts(row))
                .collect()
        };
        let first_unsorted = rows(vec![
            run("low", 0.0, 100.0, &[]),
            run("high", 0.0, 10.0, &[]),
            run("low too", 50.0, 102.0, &[]),
        ]);
        assert_eq!(first_unsorted, [vec!["low", "low too"], vec!["high"]]);
        let sorted = rows(vec![
            run("middle", 0.0, 50.0, &[]),
            run("low", 0.0, 100.0, &[]),
            run("high", 0.0, 10.0, &[]),
        ]);
        assert_eq!(sorted, [vec!["high"], vec!["middle"], vec!["low"]]);
    }

    #[test]
    fn compares_floats_as_java_does() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_floats(-0.0, 0.0), Less);
        assert_eq!(compare_floats(f32::NAN, f32::INFINITY), Greater);
        assert_eq!(compare_floats(f32::NAN, f32::NAN), Equal);
        assert_eq!(compare_floats(1.0, 2.0), Less);
    }

    #[test]
    fn reads_indices_as_advances() {
        assert_eq!(
            parse_indices("1,50;2;3,,7;4, 20").unwrap(),
            [0.5, 0.0, 0.0, 0.2]
        );
        assert!(parse_indices("1,x").is_err());
    }
}
