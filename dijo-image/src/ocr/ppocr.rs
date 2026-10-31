use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use image::GrayImage;
use rten::Model;
use rten_tensor::prelude::*;
use rten_tensor::NdTensor;

use super::prepare::resized;
use super::tesseract::{Area, OcrError, Reading};
use crate::{bundle, memory};

const DETECTOR: &str = "det.onnx";
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
const LINE_HEIGHT: u32 = 48;
const MIN_LINE_WIDTH: u32 = 320;
const MIN_LINE_CONFIDENCE: f32 = 0.5;
const CHARACTERS_KEY: &str = "  character_dict:";
const CHARACTER_ITEM: &str = "  - ";

pub(crate) struct PpOcr {
    dir: PathBuf,
    detector: OnceLock<Result<Model, OcrError>>,
    recognizers: [OnceLock<Result<LineReader, OcrError>>; RECOGNIZERS.len()],
}

struct LineReader {
    model: Model,
    labels: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineModel(usize);

impl LineModel {
    pub(crate) fn of_script(script: &str) -> Option<Self> {
        let (_, name) = RECOGNIZER_OF_SCRIPT
            .iter()
            .find(|(read, _)| *read == script)?;
        RECOGNIZERS.iter().position(|known| known == name).map(Self)
    }

    pub(crate) fn name(self) -> &'static str {
        RECOGNIZERS[self.0]
    }

    fn file(self) -> String {
        format!("{}.onnx", self.name())
    }

    fn configuration(self) -> String {
        format!("{}.yml", self.name())
    }
}

pub(crate) fn ppocr() -> Result<&'static PpOcr, OcrError> {
    static PPOCR: OnceLock<Result<PpOcr, OcrError>> = OnceLock::new();
    PPOCR
        .get_or_init(|| {
            bundle::bundled_recognizers()
                .map(PpOcr::new)
                .map_err(|reason| OcrError::Unavailable { reason })
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn failed(model: &str, reason: impl ToString) -> OcrError {
    OcrError::Rereading {
        model: model.to_owned(),
        reason: reason.to_string(),
    }
}

fn load(path: &Path) -> Result<Model, OcrError> {
    let name = path.display().to_string();
    std::thread::scope(|scope| {
        scope
            .spawn(|| Model::load_file(path))
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
    .map_err(|error| failed(&name, error))
}

impl PpOcr {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            detector: OnceLock::new(),
            recognizers: Default::default(),
        }
    }

    fn detector(&self) -> Result<&Model, OcrError> {
        self.detector
            .get_or_init(|| load(&self.dir.join(DETECTOR)))
            .as_ref()
            .map_err(Clone::clone)
    }

    fn line_reader(&self, model: LineModel) -> Result<&LineReader, OcrError> {
        self.recognizers[model.0]
            .get_or_init(|| {
                let configuration = self.dir.join(model.configuration());
                let model = load(&self.dir.join(model.file()))?;
                let name = configuration.display().to_string();
                let text = std::fs::read_to_string(&configuration).map_err(|e| failed(&name, e))?;
                let labels = labels(&text).map_err(|reason| failed(&name, reason))?;
                Ok(LineReader { model, labels })
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub(crate) fn read(
        &self,
        image: &GrayImage,
        model: LineModel,
        blocks: &[Area],
    ) -> Result<Reading, OcrError> {
        let reader = self.line_reader(model)?;
        let lines = self.lines(image)?;
        let mut read = Vec::with_capacity(lines.len());
        let (mut characters, mut confident) = (0f32, 0f32);
        for line in lines {
            let (text, confidence) = reader.read(&crop(image, line), model)?;
            let length = text.chars().count() as f32;
            characters += length;
            confident += length * confidence;
            if confidence >= MIN_LINE_CONFIDENCE && !text.trim().is_empty() {
                read.push((line, text));
            }
        }
        let confidence = match characters > 0.0 {
            true => (confident / characters * 100.0).round() as i32,
            false => 0,
        };
        let areas: Vec<Area> = read.iter().map(|&(line, _)| line).collect();
        let text = ordered(&areas, blocks)
            .into_iter()
            .map(|block| {
                block
                    .into_iter()
                    .map(|row| {
                        row.into_iter()
                            .map(|index| read[index].1.trim())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .map(|block| block + "\n\n")
            .collect();
        Ok(Reading {
            text,
            confidence,
            blocks: Vec::new(),
        })
    }

    fn lines(&self, image: &GrayImage) -> Result<Vec<Area>, OcrError> {
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
        Ok(text_areas(&probabilities.to_vec(), map_width, map_height)
            .into_iter()
            .filter_map(|area| scaled(area, map_width, map_height, width, height))
            .collect())
    }
}

impl LineReader {
    fn read(&self, line: &GrayImage, model: LineModel) -> Result<(String, f32), OcrError> {
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
        let name = model.file();
        let output = self
            .model
            .run_one(tensor.into(), None)
            .map_err(|error| failed(&name, error))?;
        let probabilities: NdTensor<f32, 3> =
            output.try_into().map_err(|error| failed(&name, error))?;
        let [_, steps, classes] = probabilities.shape();
        if classes != self.labels.len() {
            return Err(failed(
                &name,
                format!(
                    "it scores {classes} classes for {} labels",
                    self.labels.len()
                ),
            ));
        }
        let scores = probabilities.to_vec();
        Ok(decoded(&scores[..steps * classes], classes, &self.labels))
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

fn decoded(scores: &[f32], classes: usize, labels: &[String]) -> (String, f32) {
    let mut text = String::new();
    let mut confidences = 0f32;
    let mut emitted = 0u32;
    let mut previous = 0;
    for step in scores.chunks_exact(classes) {
        let (best, &score) = step
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .expect("a step scores every class");
        if best != 0 && best != previous {
            text.push_str(&labels[best]);
            confidences += score;
            emitted += 1;
        }
        previous = best;
    }
    match emitted {
        0 => (text, 0.0),
        _ => (text, confidences / emitted as f32),
    }
}

fn detection_size(width: u32, height: u32) -> (u32, u32) {
    let ratio = (f64::from(DETECTION_LONG_SIDE) / f64::from(width.max(height))).min(1.0);
    let rounded = |side: u32| {
        let scaled = f64::from(side) * ratio / f64::from(DETECTION_STRIDE);
        (scaled.round() as u32).max(1) * DETECTION_STRIDE
    };
    (rounded(width), rounded(height))
}

fn text_areas(map: &[f32], width: usize, height: usize) -> Vec<Area> {
    let mut seen = vec![false; width * height];
    let mut areas = Vec::new();
    let mut pending = Vec::new();
    for start in 0..width * height {
        if seen[start] || map[start] <= TEXT_PROBABILITY {
            continue;
        }
        seen[start] = true;
        pending.push(start);
        let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
        let (mut pixels, mut probability) = (0usize, 0f64);
        while let Some(index) = pending.pop() {
            let (x, y) = (index % width, index / width);
            left = left.min(x);
            right = right.max(x + 1);
            top = top.min(y);
            bottom = bottom.max(y + 1);
            pixels += 1;
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
        let length = (right - left).max(bottom - top) as f64;
        let thickness = pixels as f64 / length;
        if thickness < f64::from(MIN_BOX_SIDE)
            || probability / (pixels as f64) < f64::from(BOX_PROBABILITY)
        {
            continue;
        }
        let distance = length * thickness * f64::from(UNCLIP_RATIO) / (2.0 * (length + thickness));
        if thickness + 2.0 * distance < f64::from(MIN_UNCLIPPED_SIDE) {
            continue;
        }
        let distance = distance.round() as i32;
        areas.push(Area {
            left: left as i32 - distance,
            top: top as i32 - distance,
            right: right as i32 + distance,
            bottom: bottom as i32 + distance,
        });
    }
    areas
}

fn scaled(
    area: Area,
    map_width: usize,
    map_height: usize,
    width: u32,
    height: u32,
) -> Option<Area> {
    let x = |value: i32| {
        (f64::from(value) * f64::from(width) / map_width as f64)
            .round()
            .clamp(0.0, f64::from(width)) as i32
    };
    let y = |value: i32| {
        (f64::from(value) * f64::from(height) / map_height as f64)
            .round()
            .clamp(0.0, f64::from(height)) as i32
    };
    let area = Area {
        left: x(area.left),
        top: y(area.top),
        right: x(area.right),
        bottom: y(area.bottom),
    };
    (area.width() > 0 && area.height() > 0).then_some(area)
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

fn ordered(lines: &[Area], blocks: &[Area]) -> Vec<Vec<Vec<usize>>> {
    let mut grouped: Vec<Vec<usize>> = vec![Vec::new(); blocks.len() + 1];
    for (index, line) in lines.iter().enumerate() {
        let (x, y) = line.centre();
        let block = blocks
            .iter()
            .position(|block| block.contains(x, y))
            .unwrap_or(blocks.len());
        grouped[block].push(index);
    }
    grouped
        .into_iter()
        .filter(|group| !group.is_empty())
        .map(|group| rows(lines, group))
        .collect()
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
            0.1, 0.8, 0.05, 0.05, //
            0.1, 0.8, 0.05, 0.05, //
            0.9, 0.05, 0.03, 0.02, //
            0.1, 0.7, 0.1, 0.1, //
            0.1, 0.1, 0.1, 0.7, //
            0.2, 0.1, 0.6, 0.1,
        ];
        let (text, confidence) = decoded(&scores, 4, &labels);
        assert_eq!(text, "aa b");
        assert!((confidence - 0.7).abs() < 1e-6, "{confidence}");
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
        assert_eq!(text_areas(&map, width, height), [area(1, 2, 33, 13)]);
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
        let areas = text_areas(&map, width, height);
        assert_eq!(areas.len(), 1);
        assert!(areas[0].left < 10 && areas[0].right > 190, "{areas:?}");
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
}
