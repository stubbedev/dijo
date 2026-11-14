use std::cmp::Reverse;
use std::sync::OnceLock;

use image::GrayImage;
use rten::{Model, ModelOptions};
use rten_tensor::prelude::*;
use rten_tensor::NdTensor;

use super::flatten::{warp, Point};
use super::prepare::resized;
use super::tesseract::{Area, OcrError, Reading};
use crate::unicode::{is_letter, is_mark};
use crate::{bundle, memory};

const DETECTOR: &str = "det";
const UNWARPER: &str = "uvdoc";
const UNWARPING_BYTES_PER_PIXEL: u64 = 48;
const RECOGNIZERS: [&str; 3] = ["latin", "devanagari", "cjk"];
const RECOGNIZER_OF_SCRIPT: [(&str, &str); 4] = [
    ("Latin", "latin"),
    ("Devanagari", "devanagari"),
    ("Han", "cjk"),
    ("Japanese", "cjk"),
];
const DETECTION_LONG_SIDE: u32 = 960;
const DETECTION_STRIDE: u32 = 32;
const DETECTION_BYTES_PER_PIXEL: u64 = 128;
const DETECTION_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const DETECTION_DEVIATION: [f32; 3] = [0.229, 0.224, 0.225];
const TEXT_PROBABILITY: f32 = 0.3;
const BOX_PROBABILITY: f32 = 0.6;
const UNCLIP_RATIO: f32 = 1.5;
const MIN_BOX_SIDE: u32 = 3;
const MIN_UNCLIPPED_SIDE: u32 = MIN_BOX_SIDE + 2;
/// How far a line must rise or fall along its length, as a share of its thickness, to be read
/// along its slant rather than cut out upright: from its thickness, an upright cut is at least
/// twice as tall as the line, its neighbours filling the rest.
const MIN_SLANT_DRIFT: f64 = 1.0;
/// How many times as long as it is thick a region must be to run along a slant: as long as a
/// short word. A character or a mark alone slants only by its shape.
const MIN_SLANT_ELONGATION: f64 = 3.0;
const LINE_HEIGHT: u32 = 48;
const MIN_LINE_WIDTH: u32 = 320;
const MIN_LINE_CONFIDENCE: f32 = 0.5;
/// The score from which a recognizer is sure of a character, and its other likely labels are not
/// kept to try against the word list.
const MIN_SURE_SCORE: f32 = 0.9;
/// How likely another label of a character must be, as a share of the score of the label read,
/// to be tried against the word list: a tenth, so that a word list never turns a character the
/// recognizer all but ruled out into one that makes a word.
const MIN_OTHER_SHARE: f32 = 0.1;
/// The most characters of a word tried as their other likely labels, so that a word takes at most
/// fifteen lookups.
const MAX_UNSURE: usize = 4;
/// How many times taller than wide a line's area must be to run down the page, as PaddleOCR turns
/// the areas at least 1.5 times as tall as they are wide: as a ratio of height to width.
const MIN_VERTICAL_ASPECT: (i32, i32) = (3, 2);
const CHARACTERS_KEY: &str = "  character_dict:";
const CHARACTER_ITEM: &str = "  - ";

pub(crate) struct PpOcr {
    detector: OnceLock<Result<Model, OcrError>>,
    unwarper: OnceLock<Result<Model, OcrError>>,
    recognizers: [OnceLock<Result<LineReader, OcrError>>; RECOGNIZERS.len()],
}

struct LineReader {
    model: Model,
    labels: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineModel(usize);

/// The text lines DBNet finds in an image, and whether they run down it, as the columns of
/// Chinese and Japanese set vertically do: whether lines that do cover most of the lines' area.
/// The lines running down an image are its [`columns`], each read whole, since DBNet breaks a
/// column where its punctuation leaves a gap.
#[derive(Debug)]
pub(crate) struct Lines {
    lines: Vec<Line>,
    vertical: bool,
}

/// A line DBNet found: the area that bounds it and, when it runs aslant, the corners of the
/// rectangle it fills, top left, top right, bottom right and bottom left, along which it is read,
/// as PaddleOCR reads every line.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Line {
    area: Area,
    slant: Option<[Point; 4]>,
}

impl Line {
    fn upright(area: Area) -> Self {
        Self { area, slant: None }
    }

    /// The line cut out of an image: its area, or its rectangle turned upright.
    fn cropped(&self, image: &GrayImage) -> GrayImage {
        match &self.slant {
            Some(corners) => warp(image, corners),
            None => crop(image, self.area),
        }
    }
}

impl Lines {
    fn of(lines: Vec<Line>) -> Self {
        let area = |line: &Line| i64::from(line.area.width()) * i64::from(line.area.height());
        let all: i64 = lines.iter().map(area).sum();
        let upright: i64 = lines
            .iter()
            .filter(|line| upright(&line.area))
            .map(area)
            .sum();
        match 2 * upright > all {
            true => {
                let areas: Vec<Area> = lines.iter().map(|line| line.area).collect();
                Self {
                    lines: columns(&areas).into_iter().map(Line::upright).collect(),
                    vertical: true,
                }
            }
            false => Self {
                lines,
                vertical: false,
            },
        }
    }

    pub(crate) fn vertical(&self) -> bool {
        self.vertical
    }
}

/// Whether a line's area is tall enough to run down the page.
fn upright(line: &Area) -> bool {
    let (height, width) = MIN_VERTICAL_ASPECT;
    line.height() * width >= line.width() * height
}

impl LineModel {
    /// The recognizer of vertical text: PP-OCRv5's of Chinese and Japanese, the scripts set in
    /// columns, which reads each column turned to run across.
    pub(crate) const VERTICAL: Self = Self(2);

    pub(crate) fn of_script(script: &str) -> Option<Self> {
        let (_, name) = RECOGNIZER_OF_SCRIPT
            .iter()
            .find(|(read, _)| *read == script)?;
        RECOGNIZERS.iter().position(|known| known == name).map(Self)
    }

    pub(crate) fn name(self) -> &'static str {
        RECOGNIZERS[self.0]
    }
}

pub(crate) fn ppocr() -> &'static PpOcr {
    static PPOCR: PpOcr = PpOcr {
        detector: OnceLock::new(),
        unwarper: OnceLock::new(),
        recognizers: [const { OnceLock::new() }; RECOGNIZERS.len()],
    };
    &PPOCR
}

fn failed(model: &str, reason: impl ToString) -> OcrError {
    OcrError::Rereading {
        model: model.to_owned(),
        reason: reason.to_string(),
    }
}

/// The embedded model of this name, its weights used where the program holds them.
fn load(name: &str) -> Result<Model, OcrError> {
    let model = bundle::ppocr_model(name)
        .ok_or_else(|| failed(name, "the build embedded no such model"))?;
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                ModelOptions::with_all_ops()
                    .external_data_static(bundle::PPOCR_WEIGHTS, model.weights)
                    .load_static_slice(model.graph)
            })
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
    .map_err(|error| failed(name, error))
}

#[cfg(test)]
thread_local! {
    /// The pages UVDoc flattened on this thread.
    pub(crate) static UNWARPINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl PpOcr {
    fn unwarper(&self) -> Result<&Model, OcrError> {
        self.unwarper
            .get_or_init(|| load(UNWARPER))
            .as_ref()
            .map_err(Clone::clone)
    }

    pub(crate) fn unwarped(&self, image: &GrayImage) -> Result<GrayImage, OcrError> {
        #[cfg(test)]
        UNWARPINGS.with(|unwarpings| unwarpings.set(unwarpings.get() + 1));
        let (width, height) = image.dimensions();
        let plane = width as usize * height as usize;
        let channel: Vec<f32> = image
            .as_raw()
            .iter()
            .map(|&level| f32::from(level) / 255.0)
            .collect();
        let mut data = Vec::with_capacity(3 * plane);
        for _ in 0..3 {
            data.extend_from_slice(&channel);
        }
        drop(channel);
        let tensor = NdTensor::from_data([1, 3, height as usize, width as usize], data);
        let unwarper = self.unwarper()?;
        let _reserved =
            memory::reserve(plane as u64 * UNWARPING_BYTES_PER_PIXEL).map_err(|limit| {
                OcrError::MemoryLimit {
                    image: format!("a {width}x{height} image"),
                    limit,
                }
            })?;
        let output = unwarper
            .run_one(tensor.into(), None)
            .map_err(|error| failed(UNWARPER, error))?;
        let unwarped: NdTensor<f32, 4> =
            output.try_into().map_err(|error| failed(UNWARPER, error))?;
        let [_, _, unwarped_height, unwarped_width] = unwarped.shape();
        let levels: Vec<u8> = unwarped
            .to_vec()
            .into_iter()
            .take(unwarped_height * unwarped_width)
            .map(|value| (value * 255.0).round().clamp(0.0, 255.0) as u8)
            .collect();
        GrayImage::from_raw(unwarped_width as u32, unwarped_height as u32, levels).ok_or_else(
            || {
                failed(
                    UNWARPER,
                    format!("it made no image of {width}x{height} pixels"),
                )
            },
        )
    }

    fn detector(&self) -> Result<&Model, OcrError> {
        self.detector
            .get_or_init(|| load(DETECTOR))
            .as_ref()
            .map_err(Clone::clone)
    }

    fn line_reader(&self, model: LineModel) -> Result<&LineReader, OcrError> {
        self.recognizers[model.0]
            .get_or_init(|| {
                let name = model.name();
                let configuration = bundle::ppocr_configuration(name)
                    .ok_or_else(|| failed(name, "the build embedded no configuration of it"))?;
                let labels = labels(configuration).map_err(|reason| failed(name, reason))?;
                let model = load(name)?;
                Ok(LineReader { model, labels })
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// What a recognizer reads in the lines of an image, ordered by the blocks tesseract found in
    /// it, or, when its lines run down it, by its columns from the right: a column is read turned
    /// a quarter counter-clockwise, its top to the left, as PaddleOCR reads it.
    pub(crate) fn read(
        &self,
        image: &GrayImage,
        model: LineModel,
        lines: &Lines,
        blocks: &[Area],
    ) -> Result<Reread<'_>, OcrError> {
        let reader = self.line_reader(model)?;
        let mut read = Vec::with_capacity(lines.lines.len());
        let (mut characters, mut confident) = (0f32, 0f32);
        for line in &lines.lines {
            let cropped = line.cropped(image);
            let cropped = match lines.vertical && upright(&line.area) {
                true => image::imageops::rotate270(&cropped),
                false => cropped,
            };
            let glyphs = reader.read(&cropped, model)?;
            let text = spelled(&glyphs, &reader.labels);
            let confidence = confidence(&glyphs);
            let length = text.chars().count() as f32;
            characters += length;
            confident += length * confidence;
            if confidence >= MIN_LINE_CONFIDENCE && !text.trim().is_empty() {
                read.push((line.area, glyphs));
            }
        }
        let confidence = match characters > 0.0 {
            true => (confident / characters * 100.0).round() as i32,
            false => 0,
        };
        Ok(Reread {
            labels: &reader.labels,
            lines: read,
            blocks: blocks.to_vec(),
            vertical: lines.vertical,
            confidence,
        })
    }

    pub(crate) fn lines(&self, image: &GrayImage) -> Result<Lines, OcrError> {
        let (width, height) = image.dimensions();
        let (input_width, input_height) = detection_size(width, height);
        let input = resized(image, input_width, input_height);
        let plane = input_width as usize * input_height as usize;
        let mut data = vec![0f32; 3 * plane];
        for (channel, values) in data.chunks_exact_mut(plane).enumerate() {
            for (value, &level) in values.iter_mut().zip(input.as_raw()) {
                *value = (f32::from(level) / 255.0 - DETECTION_MEAN[channel])
                    / DETECTION_DEVIATION[channel];
            }
        }
        let tensor = NdTensor::from_data([1, 3, input_height as usize, input_width as usize], data);
        let detector = self.detector()?;
        let _reserved =
            memory::reserve(plane as u64 * DETECTION_BYTES_PER_PIXEL).map_err(|limit| {
                OcrError::MemoryLimit {
                    image: format!("a {width}x{height} image"),
                    limit,
                }
            })?;
        let output = detector
            .run_one(tensor.into(), None)
            .map_err(|error| failed(DETECTOR, error))?;
        let probabilities: NdTensor<f32, 4> =
            output.try_into().map_err(|error| failed(DETECTOR, error))?;
        let [_, _, map_height, map_width] = probabilities.shape();
        Ok(Lines::of(
            text_areas(&probabilities.to_vec(), map_width, map_height)
                .into_iter()
                .filter_map(|line| scaled(line, map_width, map_height, width, height))
                .collect(),
        ))
    }
}

impl LineReader {
    fn read(&self, line: &GrayImage, model: LineModel) -> Result<Vec<Glyph>, OcrError> {
        let (width, height) = line.dimensions();
        let scaled_width =
            ((f64::from(width) * f64::from(LINE_HEIGHT) / f64::from(height)).ceil() as u32).max(1);
        let input_width = scaled_width.max(MIN_LINE_WIDTH) as usize;
        let input = resized(line, scaled_width, LINE_HEIGHT);
        let mut channel = vec![0f32; input_width * LINE_HEIGHT as usize];
        for (row, levels) in channel
            .chunks_exact_mut(input_width)
            .zip(input.as_raw().chunks_exact(scaled_width as usize))
        {
            for (value, &level) in row.iter_mut().zip(levels) {
                *value = f32::from(level) / 127.5 - 1.0;
            }
        }
        let mut data = Vec::with_capacity(3 * channel.len());
        for _ in 0..3 {
            data.extend_from_slice(&channel);
        }
        let tensor = NdTensor::from_data([1, 3, LINE_HEIGHT as usize, input_width], data);
        let name = model.name();
        let output = self
            .model
            .run_one(tensor.into(), None)
            .map_err(|error| failed(name, error))?;
        let probabilities: NdTensor<f32, 3> =
            output.try_into().map_err(|error| failed(name, error))?;
        let [_, steps, classes] = probabilities.shape();
        if classes != self.labels.len() {
            return Err(failed(
                name,
                format!(
                    "it scores {classes} classes for {} labels",
                    self.labels.len()
                ),
            ));
        }
        let scores = probabilities.to_vec();
        Ok(decoded(&scores[..steps * classes], classes))
    }
}

fn labels(configuration: &str) -> Result<Vec<String>, String> {
    let mut lines = configuration.lines();
    lines
        .by_ref()
        .find(|line| *line == CHARACTERS_KEY)
        .ok_or_else(|| format!("it has no {:?}", CHARACTERS_KEY.trim()))?;
    let mut labels = vec![String::new()];
    for item in lines.map_while(|line| line.strip_prefix(CHARACTER_ITEM)) {
        labels.push(label(item).ok_or_else(|| format!("{item:?} is no label"))?);
    }
    if labels.len() == 1 {
        return Err(format!("its {:?} is empty", CHARACTERS_KEY.trim()));
    }
    labels.push(" ".to_owned());
    Ok(labels)
}

fn label(item: &str) -> Option<String> {
    let character = match item.strip_prefix('\'') {
        Some(quoted) => quoted.strip_suffix('\'')?.replace("''", "'"),
        None if item.starts_with('"') => return None,
        None => item.to_owned(),
    };
    (!character.is_empty()).then_some(character)
}

/// What a recognizer read in the lines of an image: each line's characters, and where it was
/// unsure of one, the likeliest other label, for a word list to choose between.
pub(crate) struct Reread<'r> {
    labels: &'r [String],
    lines: Vec<(Area, Vec<Glyph>)>,
    blocks: Vec<Area>,
    vertical: bool,
    confidence: i32,
}

impl Reread<'_> {
    /// The text of the likeliest label of every character.
    pub(crate) fn reading(&self) -> Reading {
        self.reading_by(|glyphs| spelled(glyphs, self.labels))
    }

    /// The text with each word that `is_word` does not hold as read made the likeliest word it
    /// holds with some of its unsure characters read as their other likely labels.
    pub(crate) fn reading_in(&self, is_word: &dyn Fn(&str) -> bool) -> Reading {
        self.reading_by(|glyphs| worded(glyphs, self.labels, is_word))
    }

    fn reading_by(&self, spell: impl Fn(&[Glyph]) -> String) -> Reading {
        let texts: Vec<String> = self.lines.iter().map(|(_, glyphs)| spell(glyphs)).collect();
        let text = match self.vertical {
            true => in_columns(texts.iter().map(|text| text.trim())),
            false => {
                let areas: Vec<Area> = self.lines.iter().map(|&(area, _)| area).collect();
                in_blocks(&areas, &self.blocks, |index| texts[index].trim())
            }
        };
        Reading {
            text,
            confidence: self.confidence,
            blocks: Vec::new(),
        }
    }
}

/// A character a recognizer read: its label, the score it gave it, and, when that score is below
/// [`MIN_SURE_SCORE`], the likeliest other label but the blank and its score.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Glyph {
    label: usize,
    score: f32,
    other: Option<(usize, f32)>,
}

fn decoded(scores: &[f32], classes: usize) -> Vec<Glyph> {
    let mut glyphs: Vec<Glyph> = Vec::new();
    let mut previous = 0;
    for step in scores.chunks_exact(classes) {
        let (best, &score) = step
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .expect("a step scores every class");
        if best != 0 && best != previous {
            glyphs.push(Glyph {
                label: best,
                score,
                other: None,
            });
        }
        previous = best;
        if best == 0 || score >= MIN_SURE_SCORE {
            continue;
        }
        let Some(glyph) = glyphs.last_mut() else {
            continue;
        };
        let runner_up = step
            .iter()
            .enumerate()
            .skip(1)
            .filter(|&(label, _)| label != best)
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(label, &score)| (label, score));
        if runner_up.is_some_and(|(_, score)| glyph.other.is_none_or(|(_, kept)| score > kept)) {
            glyph.other = runner_up;
        }
    }
    glyphs
}

fn spelled(glyphs: &[Glyph], labels: &[String]) -> String {
    glyphs
        .iter()
        .map(|glyph| labels[glyph.label].as_str())
        .collect()
}

fn confidence(glyphs: &[Glyph]) -> f32 {
    match glyphs.len() {
        0 => 0.0,
        emitted => glyphs.iter().map(|glyph| glyph.score).sum::<f32>() / emitted as f32,
    }
}

/// The text of a line with each of its words that `is_word` does not hold as read, but holds with
/// some of its [`MAX_UNSURE`] least sure characters read as other labels at least
/// [`MIN_OTHER_SHARE`] as likely, written as the likeliest such word: the one whose changed
/// characters' other labels were scored highest against what was read.
fn worded(glyphs: &[Glyph], labels: &[String], is_word: &dyn Fn(&str) -> bool) -> String {
    let in_word = |label: usize| labels[label].chars().all(|c| is_letter(c) || is_mark(c));
    let mut text = String::new();
    let mut rest = glyphs;
    while let Some(first) = rest.first() {
        let length = match in_word(first.label) {
            true => rest
                .iter()
                .position(|glyph| !in_word(glyph.label))
                .unwrap_or(rest.len()),
            false => 1,
        };
        let (word, after) = rest.split_at(length);
        rest = after;
        let read = spelled(word, labels);
        if !in_word(first.label) {
            text.push_str(&read);
            continue;
        }
        let mut unsure: Vec<(usize, usize, f32)> = word
            .iter()
            .enumerate()
            .filter_map(|(at, glyph)| {
                let (label, score) = glyph.other.filter(|&(label, score)| {
                    in_word(label) && score >= MIN_OTHER_SHARE * glyph.score
                })?;
                Some((at, label, score.ln() - glyph.score.ln()))
            })
            .collect();
        if unsure.is_empty() || is_word(&read) {
            text.push_str(&read);
            continue;
        }
        unsure.sort_by(|a, b| b.2.total_cmp(&a.2));
        unsure.truncate(MAX_UNSURE);
        let mut choices: Vec<(f32, u32)> = (1..1u32 << unsure.len())
            .map(|chosen| {
                let likelihood = unsure
                    .iter()
                    .enumerate()
                    .filter(|&(bit, _)| chosen >> bit & 1 == 1)
                    .map(|(_, &(_, _, odds))| odds)
                    .sum();
                (likelihood, chosen)
            })
            .collect();
        choices.sort_by(|a, b| b.0.total_cmp(&a.0));
        let found = choices.into_iter().find_map(|(_, chosen)| {
            let mut labelled: Vec<usize> = word.iter().map(|glyph| glyph.label).collect();
            for (bit, &(at, label, _)) in unsure.iter().enumerate() {
                if chosen >> bit & 1 == 1 {
                    labelled[at] = label;
                }
            }
            let candidate: String = labelled
                .iter()
                .map(|&label| labels[label].as_str())
                .collect();
            is_word(&candidate).then_some(candidate)
        });
        text.push_str(found.as_deref().unwrap_or(&read));
    }
    text
}

fn detection_size(width: u32, height: u32) -> (u32, u32) {
    let ratio = (f64::from(DETECTION_LONG_SIDE) / f64::from(width.max(height))).min(1.0);
    let rounded = |side: u32| {
        let scaled = f64::from(side) * ratio / f64::from(DETECTION_STRIDE);
        (scaled.round() as u32).max(1) * DETECTION_STRIDE
    };
    (rounded(width), rounded(height))
}

fn text_areas(map: &[f32], width: usize, height: usize) -> Vec<Line> {
    let mut seen = vec![false; width * height];
    let mut lines = Vec::new();
    let mut pending = Vec::new();
    let mut members = Vec::new();
    for start in 0..width * height {
        if seen[start] || map[start] <= TEXT_PROBABILITY {
            continue;
        }
        seen[start] = true;
        pending.push(start);
        members.clear();
        let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
        let mut probability = 0f64;
        while let Some(index) = pending.pop() {
            let (x, y) = (index % width, index / width);
            left = left.min(x);
            right = right.max(x + 1);
            top = top.min(y);
            bottom = bottom.max(y + 1);
            members.push(index);
            probability += f64::from(map[index]);
            for ny in y.saturating_sub(1)..(y + 2).min(height) {
                for nx in x.saturating_sub(1)..(x + 2).min(width) {
                    let neighbour = ny * width + nx;
                    if !seen[neighbour] && map[neighbour] > TEXT_PROBABILITY {
                        seen[neighbour] = true;
                        pending.push(neighbour);
                    }
                }
            }
        }
        let pixels = members.len();
        let length = (right - left).max(bottom - top) as f64;
        let thickness = pixels as f64 / length;
        if thickness < f64::from(MIN_BOX_SIDE)
            || probability / (pixels as f64) < f64::from(BOX_PROBABILITY)
        {
            continue;
        }
        let distance = unclipping(length, thickness);
        if thickness + 2.0 * distance < f64::from(MIN_UNCLIPPED_SIDE) {
            continue;
        }
        let margin = distance.round() as i32;
        lines.push(Line {
            area: Area {
                left: left as i32 - margin,
                top: top as i32 - margin,
                right: right as i32 + margin,
                bottom: bottom as i32 + margin,
            },
            slant: slant(&members, width, distance),
        });
    }
    lines
}

/// How far PaddleOCR widens a text region on every side: by its area times the unclip ratio over
/// its perimeter.
fn unclipping(length: f64, thickness: f64) -> f64 {
    length * thickness * f64::from(UNCLIP_RATIO) / (2.0 * (length + thickness))
}

/// The corners of the rectangle the pixels of a region of DBNet's map fill along the axis they
/// spread along most, widened by `distance` on every side, when that axis runs nearer across than
/// down, the region is at least [`MIN_SLANT_ELONGATION`] times as long along it as it is thick and
/// rises or falls along it by at least [`MIN_SLANT_DRIFT`] of its thickness; else none, the line
/// cut out upright.
fn slant(members: &[usize], width: usize, distance: f64) -> Option<[Point; 4]> {
    let count = members.len() as f64;
    let pixel = |index: usize| ((index % width) as f64, (index / width) as f64);
    let (sum_x, sum_y) = members
        .iter()
        .map(|&index| pixel(index))
        .fold((0.0, 0.0), |(sum_x, sum_y), (x, y)| (sum_x + x, sum_y + y));
    let mean = (sum_x / count + 0.5, sum_y / count + 0.5);
    let (mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0);
    for &index in members {
        let (x, y) = pixel(index);
        let (dx, dy) = (x + 0.5 - mean.0, y + 0.5 - mean.1);
        xx += dx * dx;
        yy += dy * dy;
        xy += dx * dy;
    }
    let angle = 0.5 * (2.0 * xy).atan2(xx - yy);
    let along = (angle.cos(), angle.sin());
    if along.1.abs() >= along.0 {
        return None;
    }
    let across = (-along.1, along.0);
    let mut bounds = [f64::MAX, f64::MIN, f64::MAX, f64::MIN];
    for &index in members {
        let (x, y) = pixel(index);
        for (corner_x, corner_y) in [(x, y), (x + 1.0, y), (x, y + 1.0), (x + 1.0, y + 1.0)] {
            let (dx, dy) = (corner_x - mean.0, corner_y - mean.1);
            let (a, n) = (dx * along.0 + dy * along.1, dx * across.0 + dy * across.1);
            bounds = [
                bounds[0].min(a),
                bounds[1].max(a),
                bounds[2].min(n),
                bounds[3].max(n),
            ];
        }
    }
    let [first, last, low, high] = bounds;
    let length = last - first;
    let thickness = count / length;
    if length < MIN_SLANT_ELONGATION * thickness
        || length * along.1.abs() < MIN_SLANT_DRIFT * thickness
    {
        return None;
    }
    let (middle_along, middle_across) = ((first + last) / 2.0, (low + high) / 2.0);
    let (half_long, half_short) = (length / 2.0 + distance, (high - low) / 2.0 + distance);
    let corner = |a: f64, n: f64| {
        let (a, n) = (middle_along + a * half_long, middle_across + n * half_short);
        Point {
            x: mean.0 + a * along.0 + n * across.0,
            y: mean.1 + a * along.1 + n * across.1,
        }
    };
    Some([
        corner(-1.0, -1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ])
}

fn scaled(
    line: Line,
    map_width: usize,
    map_height: usize,
    width: u32,
    height: u32,
) -> Option<Line> {
    let x = |value: f64| value * f64::from(width) / map_width as f64;
    let y = |value: f64| value * f64::from(height) / map_height as f64;
    let slant = line.slant.map(|corners| {
        corners.map(|corner| Point {
            x: x(corner.x),
            y: y(corner.y),
        })
    });
    let within = |value: f64, side: u32| value.round().clamp(0.0, f64::from(side)) as i32;
    let area = Area {
        left: within(x(f64::from(line.area.left)), width),
        top: within(y(f64::from(line.area.top)), height),
        right: within(x(f64::from(line.area.right)), width),
        bottom: within(y(f64::from(line.area.bottom)), height),
    };
    (area.width() > 0 && area.height() > 0).then_some(Line { area, slant })
}

fn crop(image: &GrayImage, area: Area) -> GrayImage {
    image::imageops::crop_imm(
        image,
        area.left as u32,
        area.top as u32,
        area.width() as u32,
        area.height() as u32,
    )
    .to_image()
}

/// The text of lines read across a page, in the [`ordered`] groups of the blocks they lie in and
/// then by rows: the lines of a row joined by spaces, its rows by line breaks, and each group
/// ended by an empty line.
fn in_blocks<'t>(lines: &[Area], blocks: &[Area], text: impl Fn(usize) -> &'t str) -> String {
    ordered(lines, blocks)
        .into_iter()
        .map(|block| {
            block
                .into_iter()
                .map(|row| row.into_iter().map(&text).collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .map(|block| block + "\n\n")
        .collect()
}

/// The text of the columns of a page, in the order they are read: each on a line of its own, and
/// the page ended by an empty line.
fn in_columns<'t>(columns: impl Iterator<Item = &'t str>) -> String {
    let mut text: String = columns.flat_map(|column| [column, "\n"]).collect();
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// The columns lines running down a page form, as Chinese and Japanese set vertically do, in the
/// order they are read, from the right: the pages of a spread, the right one first. A line belongs
/// to the column its centre lies within, which spans every line of it.
fn columns(lines: &[Area]) -> Vec<Area> {
    let mut order: Vec<Area> = lines.to_vec();
    order.sort_by_key(|line| (Reverse(line.left + line.right), line.top));
    let mut columns: Vec<Area> = Vec::new();
    for line in order {
        let (centre, _) = line.centre();
        match columns
            .last_mut()
            .filter(|column| column.left <= centre && centre < column.right)
        {
            Some(column) => {
                column.left = column.left.min(line.left);
                column.top = column.top.min(line.top);
                column.right = column.right.max(line.right);
                column.bottom = column.bottom.max(line.bottom);
            }
            None => columns.push(line),
        }
    }
    columns
}

/// The lines of a page grouped by the blocks, in their order, whose areas hold their centres, and
/// each group's lines by rows. The lines that lie in no block, whose areas depend on what
/// tesseract recognized in them, form groups of their own, each read where its [`slot`] puts it
/// among the blocks.
fn ordered(lines: &[Area], blocks: &[Area]) -> Vec<Vec<Vec<usize>>> {
    let mut within: Vec<Vec<usize>> = vec![Vec::new(); blocks.len()];
    let mut between: Vec<Vec<usize>> = vec![Vec::new(); blocks.len() + 1];
    for (index, line) in lines.iter().enumerate() {
        let (x, y) = line.centre();
        match blocks.iter().position(|block| block.contains(x, y)) {
            Some(block) => within[block].push(index),
            None => between[slot(line, blocks)].push(index),
        }
    }
    let mut within = within.into_iter();
    let mut groups = Vec::with_capacity(2 * blocks.len() + 1);
    for loose in between {
        groups.push(loose);
        groups.extend(within.next());
    }
    groups
        .into_iter()
        .filter(|group| !group.is_empty())
        .map(|group| rows(lines, group))
        .collect()
}

/// The place among blocks, by the index of the block it is read before, of a line that lies in
/// none: before the first block under it in its column, the blocks that share some of its width;
/// else after the last block of its column, which all lie above or beside it; else, for a line in
/// a margin, which shares no block's width, before the first block that starts under it, or
/// after every block.
fn slot(line: &Area, blocks: &[Area]) -> usize {
    let (_, centre) = line.centre();
    let under = |block: &Area| centre < block.top;
    let in_column = |block: &Area| line.shares_width(block);
    blocks
        .iter()
        .position(|block| in_column(block) && under(block))
        .or_else(|| blocks.iter().rposition(in_column).map(|block| block + 1))
        .or_else(|| blocks.iter().position(under))
        .unwrap_or(blocks.len())
}

fn rows(lines: &[Area], mut group: Vec<usize>) -> Vec<Vec<usize>> {
    group.sort_by_key(|&index| (lines[index].top + lines[index].bottom, lines[index].left));
    let mut rows: Vec<(Area, Vec<usize>)> = Vec::new();
    for index in group {
        let line = lines[index];
        let (_, centre) = line.centre();
        match rows
            .last_mut()
            .filter(|(span, _)| span.top <= centre && centre < span.bottom)
        {
            Some((span, row)) => {
                span.top = span.top.min(line.top);
                span.bottom = span.bottom.max(line.bottom);
                row.push(index);
            }
            None => rows.push((line, vec![index])),
        }
    }
    rows.into_iter()
        .map(|(_, mut row)| {
            row.sort_by_key(|&index| lines[index].left);
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(left: i32, top: i32, right: i32, bottom: i32) -> Area {
        Area {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn loads_every_embedded_model_with_its_weights_apart_from_its_graph() {
        for name in [DETECTOR, UNWARPER].into_iter().chain(RECOGNIZERS) {
            load(name).unwrap_or_else(|error| panic!("{name}: {error}"));
            let graph = bundle::ppocr_model(name).expect("an embedded model").graph;
            assert!(
                Model::load_static_slice(graph).is_err(),
                "{name} holds its weights in its graph"
            );
        }
    }

    #[test]
    fn detects_at_a_long_side_of_960_in_multiples_of_32() {
        assert_eq!(detection_size(2480, 3508), (672, 960));
        assert_eq!(detection_size(500, 300), (512, 288));
        assert_eq!(detection_size(10, 10), (32, 32));
    }

    #[test]
    fn reads_the_labels_of_paddleocrs_configuration() {
        let configuration = "PostProcess:\n  name: CTCLabelDecode\n  character_dict:\n  - '0'\n  \
                             - A\n  - ''''\n  - ſ\nother: 1\n";
        assert_eq!(
            labels(configuration).unwrap(),
            ["", "0", "A", "'", "ſ", " "]
        );
        assert!(labels("  character_dict:\n  - \"a\"\n").is_err());
        assert!(labels("  character_dict:\n  - ''\n").is_err());
        assert!(labels("name: CTCLabelDecode\n").is_err());
    }

    #[test]
    fn decodes_the_most_likely_labels_without_repeats_or_blanks() {
        let labels: Vec<String> = ["", "a", "b", " "].map(str::to_owned).to_vec();
        let scores = [
            0.1, 0.8, 0.06, 0.04, //
            0.1, 0.8, 0.05, 0.05, //
            0.9, 0.05, 0.03, 0.02, //
            0.1, 0.7, 0.1, 0.1, //
            0.1, 0.1, 0.1, 0.7, //
            0.2, 0.1, 0.6, 0.1,
        ];
        let glyphs = decoded(&scores, 4);
        assert_eq!(spelled(&glyphs, &labels), "aa b");
        assert!((confidence(&glyphs) - 0.7).abs() < 1e-6);
        assert_eq!(glyphs[0].other, Some((2, 0.06)));
        assert_eq!(glyphs[3].other, Some((3, 0.1)));
    }

    #[test]
    fn keeps_no_other_label_of_a_character_it_is_sure_of() {
        let scores = [0.02, 0.95, 0.03, 0.0];
        assert_eq!(decoded(&scores, 4)[0].other, None);
    }

    fn glyph(label: usize, score: f32, other: Option<(usize, f32)>) -> Glyph {
        Glyph {
            label,
            score,
            other,
        }
    }

    #[test]
    fn reads_an_unknown_word_as_the_likeliest_known_one_its_unsure_characters_spell() {
        let labels: Vec<String> = ["", "c", "a", "f", "s", "e", ",", " "]
            .map(str::to_owned)
            .to_vec();
        let words = ["casa", "case"];
        let is_word = |word: &str| words.contains(&word);
        let read = [
            glyph(1, 0.99, None),
            glyph(2, 0.99, None),
            glyph(3, 0.6, Some((4, 0.35))),
            glyph(5, 0.5, Some((2, 0.45))),
            glyph(6, 0.99, None),
            glyph(7, 0.99, None),
            glyph(3, 0.7, Some((4, 0.25))),
            glyph(5, 0.99, None),
        ];
        assert_eq!(worded(&read, &labels, &is_word), "case, fe");
        assert_eq!(spelled(&read, &labels), "cafe, fe");
    }

    #[test]
    fn keeps_a_known_word_and_one_no_unsure_character_makes_known() {
        let labels: Vec<String> = ["", "c", "a", "f", "e", "x"].map(str::to_owned).to_vec();
        let is_word = |word: &str| ["cafe"].contains(&word);
        let known = [
            glyph(1, 0.99, None),
            glyph(2, 0.99, None),
            glyph(3, 0.6, Some((5, 0.35))),
            glyph(4, 0.99, None),
        ];
        assert_eq!(worded(&known, &labels, &is_word), "cafe");
        let unknown = [
            glyph(1, 0.6, Some((4, 0.3))),
            glyph(3, 0.99, None),
            glyph(4, 0.99, None),
        ];
        assert_eq!(worded(&unknown, &labels, &is_word), "cfe");
        let ruled_out = [
            glyph(1, 0.99, None),
            glyph(2, 0.99, None),
            glyph(5, 0.8, Some((3, 0.07))),
            glyph(4, 0.99, None),
        ];
        assert_eq!(worded(&ruled_out, &labels, &is_word), "caxe");
    }

    #[test]
    fn finds_text_areas_as_paddleocr_does_and_widens_them() {
        let (width, height) = (40, 20);
        let mut map = vec![0f32; width * height];
        for y in 5..10 {
            for x in 4..30 {
                map[y * width + x] = 0.9;
            }
        }
        map[15 * width + 35] = 0.9;
        assert_eq!(
            text_areas(&map, width, height),
            [Line::upright(area(1, 2, 33, 13))]
        );
    }

    #[test]
    fn keeps_a_slanted_line_whose_box_is_mostly_background() {
        let (width, height) = (200, 60);
        let mut map = vec![0f32; width * height];
        for x in 10..190 {
            let middle = 10 + x * 40 / 200;
            for y in middle..middle + 5 {
                map[y * width + x] = 0.9;
            }
        }
        let lines = text_areas(&map, width, height);
        assert_eq!(lines.len(), 1);
        let area = lines[0].area;
        assert!(area.left < 10 && area.right > 190, "{lines:?}");
    }

    #[test]
    fn reads_a_slanted_line_along_the_rectangle_it_fills() {
        let (width, height) = (200, 60);
        let mut map = vec![0f32; width * height];
        for x in 10..190 {
            let middle = 10 + x * 40 / 200;
            for y in middle..middle + 5 {
                map[y * width + x] = 0.9;
            }
        }
        let lines = text_areas(&map, width, height);
        let [top_left, top_right, bottom_right, bottom_left] =
            lines[0].slant.expect("a slanted line");
        let slope = |from: Point, to: Point| (to.y - from.y) / (to.x - from.x);
        assert!((slope(top_left, top_right) - 0.2).abs() < 0.02, "{lines:?}");
        assert!(
            (slope(bottom_left, bottom_right) - 0.2).abs() < 0.02,
            "{lines:?}"
        );
        let thickness = (bottom_left.y - top_left.y).hypot(bottom_left.x - top_left.x);
        assert!((11.0..15.0).contains(&thickness), "{lines:?}");
    }

    #[test]
    fn cuts_out_a_level_line_upright() {
        let (width, height) = (200, 60);
        let mut map = vec![0f32; width * height];
        for y in 20..26 {
            for x in 10..190 {
                map[y * width + x] = 0.9;
            }
        }
        map[19 * width + 12] = 0.9;
        map[26 * width + 180] = 0.9;
        let lines = text_areas(&map, width, height);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].slant, None, "{lines:?}");
    }

    #[test]
    fn cuts_out_a_slanted_mark_upright() {
        let (width, height) = (60, 60);
        let mut map = vec![0f32; width * height];
        for x in 20..26 {
            let middle = 20 + (x - 20) / 2;
            for y in middle..middle + 5 {
                map[y * width + x] = 0.9;
            }
        }
        let lines = text_areas(&map, width, height);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].slant, None, "{lines:?}");
    }

    #[test]
    fn turns_a_slanted_line_upright_to_read_it() {
        let band = |x: u32, y: u32| {
            let middle = 30.0 + f64::from(x) * 0.2;
            (f64::from(y) - middle).abs() < 6.0
        };
        let image = GrayImage::from_fn(300, 120, |x, y| {
            image::Luma([if band(x, y) { 0 } else { 255 }])
        });
        let corner = |x: f64, offset: f64| Point {
            x,
            y: 30.0 + x * 0.2 + offset,
        };
        let line = Line {
            area: area(20, 20, 280, 100),
            slant: Some([
                corner(20.0, -12.0),
                corner(280.0, -12.0),
                corner(280.0, 12.0),
                corner(20.0, 12.0),
            ]),
        };
        let strip = line.cropped(&image);
        let (strip_width, strip_height) = strip.dimensions();
        assert_eq!(strip_height, 24);
        let dark = |row: u32| {
            (0..strip_width)
                .filter(|&x| strip.get_pixel(x, row).0[0] < 128)
                .count() as u32
        };
        assert!(dark(strip_height / 2) > strip_width * 9 / 10);
        assert_eq!(dark(1), 0);
        assert_eq!(dark(strip_height - 2), 0);
    }

    #[test]
    fn reads_the_vertical_text_recognizer_of_chinese_and_japanese() {
        assert_eq!(LineModel::VERTICAL.name(), "cjk");
        assert_eq!(LineModel::of_script("Japanese"), Some(LineModel::VERTICAL));
        assert_eq!(LineModel::of_script("Han"), Some(LineModel::VERTICAL));
    }

    #[test]
    fn reads_lines_down_a_page_when_they_cover_most_of_its_text() {
        let across = [
            area(0, 0, 400, 30),
            area(0, 40, 380, 70),
            area(0, 80, 20, 120),
        ];
        let upright_lines = |areas: &[Area]| areas.iter().copied().map(Line::upright).collect();
        assert!(!Lines::of(upright_lines(&across)).vertical());
        let down = [
            area(300, 0, 330, 400),
            area(250, 0, 280, 380),
            area(0, 0, 60, 30),
        ];
        let lines = Lines::of(upright_lines(&down));
        assert!(lines.vertical());
        assert_eq!(lines.lines, upright_lines(&columns(&down)) as Vec<Line>);
        assert!(!Lines::of(Vec::new()).vertical());
    }

    #[test]
    fn reads_columns_from_the_right_and_the_right_page_of_a_spread_first() {
        let lines = [
            area(100, 0, 130, 300),
            area(500, 20, 530, 200),
            area(500, 220, 532, 400),
            area(640, 0, 670, 400),
            area(140, 0, 170, 350),
            area(506, 410, 524, 430),
        ];
        assert_eq!(
            columns(&lines),
            [
                area(640, 0, 670, 400),
                area(500, 20, 532, 430),
                area(140, 0, 170, 350),
                area(100, 0, 130, 300),
            ]
        );
    }

    #[test]
    fn puts_each_column_on_a_line_of_its_own() {
        assert_eq!(
            in_columns(["之に對して", "と云ふかも"].into_iter()),
            "之に對して\nと云ふかも\n\n"
        );
        assert_eq!(in_columns(std::iter::empty()), "");
    }

    #[test]
    fn orders_lines_by_the_blocks_they_lie_in_then_by_rows() {
        let lines = [
            area(510, 0, 900, 20),
            area(0, 30, 400, 50),
            area(0, 0, 200, 22),
            area(210, 2, 400, 20),
            area(510, 30, 900, 50),
            area(0, 900, 300, 920),
        ];
        let blocks = [area(500, 0, 1000, 60), area(0, 0, 450, 60)];
        assert_eq!(
            ordered(&lines, &blocks),
            [
                vec![vec![0], vec![4]],
                vec![vec![2, 3], vec![1]],
                vec![vec![5]]
            ]
        );
    }

    #[test]
    fn reads_a_line_above_every_block_first() {
        let lines = [
            area(10, 110, 400, 130),
            area(500, 110, 900, 130),
            area(300, 20, 600, 60),
        ];
        let blocks = [area(0, 100, 450, 200), area(480, 100, 950, 200)];
        assert_eq!(
            ordered(&lines, &blocks),
            [vec![vec![2]], vec![vec![0]], vec![vec![1]]]
        );
    }

    #[test]
    fn reads_a_line_between_two_stacked_blocks_between_them() {
        let lines = [
            area(10, 310, 800, 330),
            area(300, 220, 500, 260),
            area(10, 110, 800, 130),
        ];
        let blocks = [area(0, 100, 900, 200), area(0, 300, 900, 400)];
        assert_eq!(
            ordered(&lines, &blocks),
            [vec![vec![2]], vec![vec![1]], vec![vec![0]]]
        );
    }

    #[test]
    fn reads_a_line_in_a_column_below_its_blocks_after_them_and_before_the_next_column() {
        let lines = [
            area(10, 110, 400, 130),
            area(500, 610, 900, 630),
            area(10, 520, 400, 540),
        ];
        let blocks = [area(0, 100, 450, 500), area(480, 100, 950, 700)];
        assert_eq!(
            ordered(&lines, &blocks),
            [vec![vec![0]], vec![vec![2]], vec![vec![1]]]
        );
    }

    #[test]
    fn reads_a_fragment_in_the_margin_beside_a_block_on_its_own_after_it() {
        let lines = [
            area(10, 110, 700, 130),
            area(760, 112, 800, 128),
            area(10, 310, 700, 330),
        ];
        let blocks = [area(0, 100, 720, 200), area(0, 300, 720, 400)];
        assert_eq!(
            ordered(&lines, &blocks),
            [vec![vec![0]], vec![vec![1]], vec![vec![2]]]
        );
        assert_eq!(
            ordered(&lines[..2], &blocks[..1]),
            [vec![vec![0]], vec![vec![1]]]
        );
    }
}
