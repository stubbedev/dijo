//! Our Java `PreparingOcrParser`'s reading of an image: prepared first, turned upright by its
//! EXIF orientation and by the skew of its text lines, and resized so its text lines are of a
//! size tesseract reads well and fast, then read as it is: with the automatic language, as
//! upright text in the script and model the extraction assumes. When tesseract is neither
//! confident of that reading nor reads it as text in a language, its orientation and script
//! detection runs: the image is turned by the quarter turns it finds and, with the automatic
//! language, read with a model for the script it finds. With the automatic language, a reading
//! whose language is detected is then read again with that language's model, when one is
//! installed and it was not read with it, and the more confident reading kept; the next images
//! are first read with that model. A reading tesseract is not confident of and that is in no
//! language is taken for noise, such as a texture or blank paper, and dropped. In the reading
//! kept, the long s of early modern print, which tesseract reads as f, is restored by the
//! dictionary of the language's model. With the automatic language, a reading of 90 or less
//! confidence is read again with its page flattened by UVDoc, and the more confident reading kept;
//! then, in a script PP-OCRv5 reads but not in blackletter, it is read again with PP-OCRv5 and the
//! more confident reading kept.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use image::GrayImage;

use super::bitmap::{Bitmap, Density};
use super::flatten::flatten;
use super::ppocr::{ppocr, LineModel};
use super::prepare::{self, Orientation};
use super::report::Report;
use super::script::{
    installed_language, language, language_model, tagged_language, written_in, BLACKLETTER_SCRIPT,
};
use super::slots;
use super::tesseract::{OcrError, PageSegMode, Pixels, Reading, Tesseract, Thresholding};
use super::text::{corrected, lexicality, with_long_s};
use crate::config::TesseractOcrConfig;
use crate::formats::embedded::OcrLanguage;
use crate::language::{self as detection, DetectedLanguage};
use crate::unicode::is_letter;

/// The most pixels an image may have to be prepared; a larger one is read as it is.
pub(crate) const MAX_PIXELS: u64 = 60_000_000;
/// The scripts whose large text reads as well once reduced: the alphabets of Europe. The dots
/// and joins of Arabic, for one, need the resolution.
const REDUCIBLE_SCRIPTS: [&str; 3] = ["Latin", "Cyrillic", "Greek"];
/// The script the first image of an extraction is first read in, without detection.
const ASSUMED_SCRIPT: &str = "Latin";
/// The mean word confidence above which a first reading as upright Latin text stands: upright
/// pages of Latin script read at 90 or more, other scripts and turned pages at 60 or less.
const MIN_CONFIDENCE: i32 = 80;
/// The mean word confidence below which a reading is taken for noise when it is mostly digits
/// and marks, or in no language detected with confidence: tesseract reads a texture or blank
/// paper at 40 or so, and pages of text at 80 or more. From it, a first reading in a language
/// detected with confidence stands too: worn, old or faxed type reads at 55 to 85, while other
/// scripts and pages upside down read at 50 or less and in no language.
const MIN_TEXT_CONFIDENCE: i32 = 50;
const MAX_REREAD_CONFIDENCE: i32 = 90;
/// Scripts tesseract's detection takes for others (Armenian for upside-down Latin, Georgian for
/// Malayalam, Hebrew for Thai), tried in turn when no reading of an image with plenty of text is
/// confident.
const OVERLOOKED_SCRIPTS: [&str; 3] = ["Armenian", "Georgian", "Hebrew"];
/// Scripts detection reports for pages in another script, which is read first in its place:
/// Georgian pages are reported as Malayalam. The reported script is then the first of the
/// overlooked ones tried.
const REPORTED_FOR: [(&str, &str); 1] = [("Malayalam", "Georgian")];
const MIN_WORDS_TO_RETRY: usize = 20;
const LANGUAGE_STRIP_LINES: usize = 6;
/// Pages are segmented without tesseract's own orientation and script detection (mode 3, where
/// Tika's default is 1): an image is turned upright before it is read, and detection runs only
/// when a first reading needs it.
const SEGMENTATION: PageSegMode = PageSegMode(3);
/// `TesseractOCRConfig`'s defaults: the language, the time a run may take, and the largest image
/// file it reads.
const DEFAULT_LANGUAGE: &str = "eng";
const DEFAULT_TIMEOUT_SECONDS: i32 = 120;
const MAX_FILE_SIZE: u64 = i32::MAX as u64;

/// The language images are read in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Language {
    /// Each image's script is detected and read with a model for it.
    Auto,
    /// Tesseract's models, joined by `+`.
    Configured(String),
}

/// How the images of an extraction are read: our Java `OcrConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrSettings {
    language: Language,
    /// The time a run of tesseract may take, or none for no limit.
    timeout: Option<Duration>,
}

impl OcrSettings {
    pub fn of(config: &TesseractOcrConfig) -> Self {
        let language = match config.language() {
            Some(TesseractOcrConfig::AUTO_LANGUAGE) => Language::Auto,
            Some(language) => Language::Configured(language.to_owned()),
            None => Language::Configured(DEFAULT_LANGUAGE.to_owned()),
        };
        let seconds = config.timeout_seconds().unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        Self {
            language,
            timeout: u64::try_from(seconds)
                .ok()
                .filter(|&seconds| seconds > 0)
                .map(Duration::from_secs),
        }
    }

    /// The time a run of tesseract, or of a program decoding an image for it, may take.
    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Whether each image's script is detected, and its first script is learned.
    pub fn auto(&self) -> bool {
        self.language == Language::Auto
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageHint {
    code: &'static str,
    script: &'static str,
}

impl LanguageHint {
    pub fn tagged(tag: &str) -> Option<Self> {
        tagged_language(tag).map(|(code, script)| Self { code, script })
    }

    pub fn detected(text: &str) -> Option<Self> {
        detection::detect(text)
            .filter(|detected| detected.reliable)
            .map(|detected| Self {
                code: detected.code,
                script: detected.script,
            })
    }
}

/// An image to read.
pub struct Image<'a> {
    /// Its pixels, as Java decoded them.
    pub(crate) bitmap: &'a Bitmap,
    /// Its file, when tesseract reads the file itself while the image needs no preparation, as
    /// it does an image Java does not decode for it.
    pub(crate) file: Option<&'a [u8]>,
    /// Whether it is small enough to prepare: [`MAX_PIXELS`] or fewer, and decoded as it is.
    pub(crate) preparable: bool,
    /// The size of the image's file, for the limits it is read within.
    pub(crate) file_size: u64,
    /// The EXIF orientation recorded in it.
    pub(crate) orientation: Option<Orientation>,
    /// Its pixels to the inch, as its file records them or as it was rendered.
    pub(crate) density: Option<Density>,
    png: OnceLock<Vec<u8>>,
}

impl<'a> Image<'a> {
    /// An image of a file of `file_size` bytes, prepared unless it has more than [`MAX_PIXELS`]
    /// pixels.
    pub fn of(
        bitmap: &'a Bitmap,
        file: Option<&'a [u8]>,
        file_size: u64,
        orientation: Option<Orientation>,
        density: Option<Density>,
    ) -> Self {
        let (width, height) = bitmap.dimensions();
        Self {
            bitmap,
            file,
            preparable: u64::from(width) * u64::from(height) <= MAX_PIXELS,
            file_size,
            orientation,
            density,
            png: OnceLock::new(),
        }
    }
}

/// How an image was read, and with the automatic language the script and model of the reading
/// kept, which the extraction assumes for its next images: none when it is noise.
#[derive(Debug)]
pub struct Recognized {
    pub read: Attempt,
    pub language: Option<OcrLanguage>,
}

/// A way an image was prepared and what tesseract read in it that way.
#[derive(Debug, Clone)]
pub struct Attempt {
    prepared: Prepared,
    reading: Option<Reading>,
    /// Whether what tesseract read was taken for noise and dropped.
    noise: bool,
    recognizer: Option<&'static str>,
}

impl Attempt {
    fn read(prepared: Prepared, reading: Reading) -> Self {
        Self {
            prepared,
            reading: Some(reading),
            noise: false,
            recognizer: None,
        }
    }

    fn confidence(&self) -> i32 {
        self.reading
            .as_ref()
            .map_or(-1, |reading| reading.confidence)
    }

    fn confident(&self) -> bool {
        self.confidence() > MIN_CONFIDENCE
    }

    /// The words of the reading, as `text.split("\\s+").length` counts them.
    fn words(&self) -> usize {
        self.reading
            .as_ref()
            .map_or(0, |reading| word_count(&reading.text))
    }

    /// The more confident of this attempt and another, this one on a tie.
    fn or(self, other: Attempt) -> Attempt {
        if other.confidence() > self.confidence() {
            other
        } else {
            self
        }
    }

    /// The language of what was read, when it is detected with confidence.
    fn detected_language(&self) -> Option<DetectedLanguage> {
        let reading = self.reading.as_ref()?;
        detection::detect(&reading.text).filter(|detected| detected.reliable)
    }

    /// The language of what was read, when it is detected with confidence in the script it was
    /// read in, or in any script when it was read in the configured language.
    fn language_in_its_script(&self) -> Option<DetectedLanguage> {
        let script = self.prepared.script.as_deref();
        self.detected_language()
            .filter(|detected| script.is_none_or(|script| detected.script == script))
    }

    /// Whether what was read is text: in a language detected with confidence in its script, and
    /// read with a confidence of at least [`MIN_TEXT_CONFIDENCE`].
    fn in_a_language(&self) -> bool {
        self.confidence() >= MIN_TEXT_CONFIDENCE && self.language_in_its_script().is_some()
    }

    /// This attempt, its text dropped when it is noise: text tesseract read with less than
    /// [`MIN_TEXT_CONFIDENCE`] that is not mostly letters, such as the digits and marks it reads
    /// in a texture, or is in no language detected with confidence.
    fn without_noise(mut self) -> Self {
        let noise = self.reading.as_ref().is_some_and(|reading| {
            reading.confidence < MIN_TEXT_CONFIDENCE
                && !reading.text.trim().is_empty()
                && (!mostly_letters(&reading.text) || self.detected_language().is_none())
        });
        if let Some(reading) = self.reading.as_mut().filter(|_| noise) {
            reading.text.clear();
            self.noise = true;
        }
        self
    }

    /// The script and model it was read in, with the automatic language.
    fn language(&self) -> Option<OcrLanguage> {
        let prepared = &self.prepared;
        prepared.script.as_ref().map(|script| OcrLanguage {
            script: script.clone(),
            model: prepared.model.clone(),
        })
    }

    /// The report of how the image was read, on a page when it is one.
    pub fn report(&self, page: Option<u32>) -> String {
        let prepared = &self.prepared;
        Report::of(&prepared.model)
            .page(page)
            .script(prepared.script.as_deref())
            .exif_orientation(prepared.exif_orientation.map(Orientation::value))
            .prepared(
                prepared.decoded,
                prepared.flattened,
                prepared.inverted,
                prepared.despeckled,
                prepared.quarter_turns,
                prepared.skew_degrees,
                prepared.scale,
                prepared.uneven_lighting,
            )
            .noise(self.noise)
            .unwarped(prepared.unwarped)
            .ppocr(self.recognizer)
            .json()
    }

    /// The text read, unless the image was outside the limits it is read within.
    pub fn text(&self) -> Option<&str> {
        self.reading.as_ref().map(|reading| reading.text.as_str())
    }
}

/// Whether most of the characters of a text, but for whitespace, are letters.
fn mostly_letters(text: &str) -> bool {
    let (characters, letters) = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .fold((0u64, 0u64), |(characters, letters), c| {
            (characters + 1, letters + u64::from(is_letter(c)))
        });
    letters * 2 > characters
}

/// The words of `text`: the pieces between runs of whitespace. Our Java counted
/// `text.split("\\s+").length`, one for an empty text and one more for leading whitespace
/// (#117).
fn word_count(text: &str) -> usize {
    text.split(crate::java::is_regex_space)
        .filter(|piece| !piece.is_empty())
        .count()
}

/// How an image was prepared to be read, and with which model.
#[derive(Debug, Clone)]
struct Prepared {
    /// The prepared image, or none when the image reads as it is.
    image: Option<Arc<Bitmap>>,
    /// The model to read it with: the configured one, or with the automatic language the one
    /// for its script.
    model: String,
    uneven_lighting: bool,
    /// The script it is read in: the one detection found, the one assumed for a first reading,
    /// or none.
    script: Option<String>,
    /// Its pixels to the inch, as turned and scaled from the image's.
    density: Option<Density>,
    exif_orientation: Option<Orientation>,
    /// Whether it was decoded to prepare it.
    decoded: bool,
    flattened: bool,
    unwarped: bool,
    inverted: bool,
    despeckled: bool,
    quarter_turns: u8,
    skew_degrees: f64,
    scale: f64,
    png: OnceLock<Vec<u8>>,
}

impl Prepared {
    /// The image, read as it is.
    fn unprepared(script: Option<String>, model: String, density: Option<Density>) -> Self {
        Self {
            image: None,
            model,
            uneven_lighting: false,
            script,
            density,
            exif_orientation: None,
            decoded: false,
            flattened: false,
            unwarped: false,
            inverted: false,
            despeckled: false,
            quarter_turns: 0,
            skew_degrees: 0.0,
            scale: 1.0,
            png: OnceLock::new(),
        }
    }

    /// This preparation, to read in another script with another model.
    fn read_in(&self, script: Option<String>, model: String) -> Self {
        Self {
            script,
            model,
            ..self.clone()
        }
    }

    fn thresholding(&self) -> Thresholding {
        if self.uneven_lighting {
            Thresholding::Sauvola
        } else {
            Thresholding::Otsu
        }
    }
}

/// An image turned upright by its EXIF orientation and, when photographed at an angle, made
/// flat: the first steps of preparing it, which come before orientation detection. It keeps the
/// steps of preparing it that do not depend on the script it is read in, once made for a turn.
struct Oriented {
    image: Upright,
    exif_orientation: Option<Orientation>,
    flattened: bool,
    unscaled: [OnceCell<Unscaled>; 4],
}

/// The image as preparing it starts.
enum Upright {
    /// It is not prepared, but read as it is.
    Unprepared,
    /// It is upright as it is.
    AsItIs,
    /// It was turned or made flat into this.
    Made(Arc<Bitmap>),
}

impl Oriented {
    fn of(image: &Image<'_>) -> Self {
        if !image.preparable {
            return Self {
                image: Upright::Unprepared,
                exif_orientation: None,
                flattened: false,
                unscaled: Default::default(),
            };
        }
        let upright = image
            .orientation
            .map(|orientation| image.bitmap.oriented(orientation));
        let flat = flatten(upright.as_ref().unwrap_or(image.bitmap)).map(Bitmap::Gray);
        Self {
            exif_orientation: upright.as_ref().and(image.orientation),
            flattened: flat.is_some(),
            image: flat
                .or(upright)
                .map_or(Upright::AsItIs, |made| Upright::Made(Arc::new(made))),
            unscaled: Default::default(),
        }
    }
}

/// An upright image turned by the quarter turns detection found, its light text on a dark
/// ground inverted, its specks cleared, its background normalised when the lighting is uneven
/// and its faint marks whitened otherwise, and its skew corrected: prepared up to scaling it,
/// which depends on the script it is read in.
struct Unscaled {
    image: Arc<Bitmap>,
    inverted: bool,
    despeckled: bool,
    uneven_lighting: bool,
    skew_degrees: f64,
}

impl Unscaled {
    fn of(upright: &Bitmap, quarter_turns: u8) -> Self {
        let turned = upright.turned_clockwise(quarter_turns);
        let turned = turned.as_ref().unwrap_or(upright);
        let analysis = prepare::analysis(turned);
        let inverted = prepare::inverted(&analysis);
        let mut gray = turned.gray();
        if inverted {
            gray = Cow::Owned(prepare::invert(gray.into_owned()));
        }
        let despeckled = prepare::speckled(&gray);
        if despeckled {
            gray = Cow::Owned(prepare::despeckle(&gray));
        }
        let (analysis, uneven_lighting, gray) = if inverted || despeckled {
            let made = Bitmap::Gray(gray.into_owned());
            let analysis = prepare::analysis(&made);
            let uneven_lighting = prepare::uneven_lighting(&made, &analysis);
            (analysis, uneven_lighting, Cow::Owned(made.into_gray()))
        } else {
            let uneven_lighting = prepare::uneven_lighting(turned, &analysis);
            (analysis, uneven_lighting, gray)
        };
        let evened = Bitmap::Gray(if uneven_lighting {
            prepare::normalise_background(gray.into_owned())
        } else {
            prepare::whiten_faint_marks(&analysis, gray.into_owned())
        });
        let skew_degrees = prepare::skew_degrees(&prepare::analysis(&evened));
        let image = if skew_degrees != 0.0 {
            evened.rotated(skew_degrees)
        } else {
            evened
        };
        Self {
            image: Arc::new(image),
            inverted,
            despeckled,
            uneven_lighting,
            skew_degrees,
        }
    }
}

/// Reads images with a tesseract, as the settings of an extraction say.
pub struct Recognizer<'t> {
    tesseract: &'t Tesseract,
    settings: &'t OcrSettings,
}

impl<'t> Recognizer<'t> {
    pub fn new(tesseract: &'t Tesseract, settings: &'t OcrSettings) -> Self {
        Self {
            tesseract,
            settings,
        }
    }

    /// Prepares an image and reads it: what is read, how, and the script and model the
    /// extraction learns from it, if any. The image is first read as it is, upright text with
    /// the automatic language in the script and model assumed, or in the configured language
    /// otherwise. Only when tesseract is neither confident of that reading nor reads it as text
    /// in a language does its orientation and script detection run; the image is then turned by
    /// the quarter turns it finds and, with the automatic language, read with a model for the
    /// script it finds, unless that would read it as before. When no reading of an image with at
    /// least [`MIN_WORDS_TO_RETRY`] words is confident, the scripts detection overlooks are tried
    /// as well, and the most confident reading is kept. With the automatic language, the image is
    /// then read with the model of the language detected in the best reading, unless it was, and
    /// the more confident reading kept. A reading that is noise has its text dropped; in any
    /// other, long s read as f is restored by the dictionary of the language detected in it, or
    /// else of its model. Without a reading when the image is outside the configured limits.
    ///
    /// `assumed` is the script and model to read it in first with the automatic language: the
    /// ones an earlier image of this extraction settled on, or none for Latin, the most common
    /// kind, with its model. The image settles on the model of the language detected in it when
    /// one is installed, whichever reading is kept, so that the next images, read first with
    /// that model, need no second reading; else on the script and model of the reading kept.
    ///
    /// The images prepared to read it are freed once it is read: what was read keeps none. It is
    /// read once a slot of the process's readings is free, or on the one this thread holds.
    pub fn recognize(
        &self,
        image: &Image<'_>,
        assumed: Option<&OcrLanguage>,
    ) -> Result<Recognized, OcrError> {
        let _slot = slots::slot();
        let mut recognized = self.recognize_preparing(image, assumed)?;
        recognized.read.prepared.image = None;
        recognized.read.prepared.png = OnceLock::new();
        Ok(recognized)
    }

    /// [`Recognizer::recognize`], the reading kept with the image it was prepared into.
    fn recognize_preparing(
        &self,
        image: &Image<'_>,
        assumed: Option<&OcrLanguage>,
    ) -> Result<Recognized, OcrError> {
        let auto = self.settings.auto();
        let oriented = Oriented::of(image);
        let first = self.first_preparation(image, &oriented, assumed)?;
        if image.file_size > MAX_FILE_SIZE {
            return Ok(Recognized {
                read: Attempt {
                    prepared: first,
                    reading: None,
                    noise: false,
                    recognizer: None,
                },
                language: None,
            });
        }
        let seeded = match (auto, assumed) {
            (true, None) => self.in_language_of_strip(&first)?,
            _ => None,
        };
        let (optimistic, unconfident_seeded) = self.read_first(image, first, seeded)?;
        let best = match optimistic.confident() || optimistic.in_a_language() {
            true => optimistic,
            false => self.read_detected(optimistic, image, &oriented)?,
        };
        let (best, detected) = match auto {
            true => {
                self.read_in_detected_language(best, unconfident_seeded.as_ref(), image, &oriented)?
            }
            false => (best, None),
        };
        let best = best.without_noise();
        let language = match best.noise {
            true => None,
            false => detected.or_else(|| best.language()),
        };
        let dictionary = language
            .as_ref()
            .map_or(&best.prepared.model, |language| &language.model)
            .clone();
        let read_with_f = best.text().map(str::to_owned);
        let best = self.with_long_s(best, &dictionary)?;
        let best = match best.text() == read_with_f.as_deref() {
            true => best,
            false => self.in_blackletter(best, image, &dictionary)?,
        };
        let best = self.read_again(best, image, &dictionary)?;
        Ok(Recognized {
            read: best,
            language,
        })
    }

    pub fn assumed(&self, hint: LanguageHint) -> Result<Option<OcrLanguage>, OcrError> {
        match self.settings.auto() {
            true => self.installed_language_model(hint.code, hint.script),
            false => Ok(None),
        }
    }

    fn installed_language_model(
        &self,
        code: &str,
        script: &str,
    ) -> Result<Option<OcrLanguage>, OcrError> {
        let installed = self.tesseract.languages()?;
        Ok(
            language_model(&installed, code, script).map(|model| OcrLanguage {
                script: script.to_owned(),
                model: model.to_owned(),
            }),
        )
    }

    fn first_preparation(
        &self,
        image: &Image<'_>,
        oriented: &Oriented,
        assumed: Option<&OcrLanguage>,
    ) -> Result<Prepared, OcrError> {
        let first_language = match (self.settings.auto(), assumed) {
            (false, _) => None,
            (true, Some(assumed)) => Some(assumed.clone()),
            (true, None) => Some(OcrLanguage {
                script: ASSUMED_SCRIPT.to_owned(),
                model: self.model(Some(ASSUMED_SCRIPT))?,
            }),
        };
        Ok(match first_language {
            Some(OcrLanguage { script, model }) => finish(image, oriented, Some(script), model, 0),
            None => finish(image, oriented, None, self.model(None)?, 0),
        })
    }

    fn read_first(
        &self,
        image: &Image<'_>,
        first: Prepared,
        seeded: Option<Prepared>,
    ) -> Result<(Attempt, Option<Attempt>), OcrError> {
        let Some(seeded) = seeded else {
            return Ok((
                Attempt::read(first.clone(), self.read(image, &first)?),
                None,
            ));
        };
        let seeded = Attempt::read(seeded.clone(), self.read(image, &seeded)?);
        if seeded.confident() {
            return Ok((seeded, None));
        }
        let unseeded = Attempt::read(first.clone(), self.read(image, &first)?);
        Ok((unseeded.or(seeded.clone()), Some(seeded)))
    }

    fn in_language_of_strip(&self, first: &Prepared) -> Result<Option<Prepared>, OcrError> {
        let Some(language) = self.language_of_strip(first)? else {
            return Ok(None);
        };
        Ok((language.model != first.model)
            .then(|| first.read_in(Some(language.script), language.model)))
    }

    fn language_of_strip(&self, first: &Prepared) -> Result<Option<OcrLanguage>, OcrError> {
        let Some(Bitmap::Gray(prepared)) = first.image.as_deref() else {
            return Ok(None);
        };
        let Some(rows) = language_strip(prepared) else {
            return Ok(None);
        };
        let width = prepared.width() as usize;
        let strip = Pixels::Gray {
            data: &prepared.as_raw()[rows.start * width..rows.end * width],
            width: prepared.width(),
            height: rows.len() as u32,
        };
        let read = Attempt::read(first.clone(), self.read_pixels(strip, first)?);
        match read.in_a_language() {
            true => self.model_of_detected_language(&read),
            false => Ok(None),
        }
    }

    fn model_of_detected_language(&self, read: &Attempt) -> Result<Option<OcrLanguage>, OcrError> {
        let Some(script) = read.prepared.script.as_deref() else {
            return Ok(None);
        };
        let Some(detected) = read.language_in_its_script() else {
            return Ok(None);
        };
        self.installed_language_model(detected.code, script)
    }

    /// Runs orientation and script detection on an image tesseract did not read confidently
    /// first, and reads it turned upright by the quarter turns it finds and, with the automatic
    /// language, with a model for the script it finds, then in the scripts detection overlooks
    /// when no reading of plenty of words is confident: the most confident reading.
    fn read_detected(
        &self,
        optimistic: Attempt,
        image: &Image<'_>,
        oriented: &Oriented,
    ) -> Result<Attempt, OcrError> {
        let auto = self.settings.auto();
        let first = optimistic.prepared.clone();
        let detected = self
            .tesseract
            .detect(pixels_of(image, &first), first.density)?;
        let reported = detected.script;
        let script = reported.as_ref().filter(|_| auto).map(|reported| {
            REPORTED_FOR
                .iter()
                .find(|(name, _)| name == reported)
                .map_or_else(|| reported.clone(), |(_, read)| (*read).to_owned())
        });
        let model = self.model(script.as_deref())?;
        let mut best = optimistic;
        let mut fallback = first.clone();
        if detected.clockwise_quarter_turns != 0 || model != first.model {
            fallback = prepare_again(
                &first,
                image,
                oriented,
                script,
                model,
                detected.clockwise_quarter_turns,
            );
            let reading = self.read(image, &fallback)?;
            best = best.or(Attempt::read(fallback.clone(), reading));
        }
        if !auto || best.confident() || best.words() < MIN_WORDS_TO_RETRY {
            return Ok(best);
        }
        let tried: Vec<&str> = [&first, &fallback]
            .into_iter()
            .filter(|read| read.quarter_turns == 0)
            .filter_map(|read| read.script.as_deref())
            .collect();
        let scripts = overlooked(reported.as_deref(), &tried);
        self.read_overlooked(best, &scripts, image, oriented)
    }

    /// Reads an image again with the installed model of the language detected with confidence
    /// in its best reading so far, in the script it was read in, unless it was read with that
    /// model: the more confident reading, and that script and model, when such a model is
    /// installed.
    fn read_in_detected_language(
        &self,
        best: Attempt,
        read_before: Option<&Attempt>,
        image: &Image<'_>,
        oriented: &Oriented,
    ) -> Result<(Attempt, Option<OcrLanguage>), OcrError> {
        let Some(language) = self.model_of_detected_language(&best)? else {
            return Ok((best, None));
        };
        if language.model == best.prepared.model {
            return Ok((best, Some(language)));
        }
        if let Some(read_before) = read_before.filter(|read_before| {
            read_before.prepared.model == language.model
                && read_before.prepared.quarter_turns == best.prepared.quarter_turns
        }) {
            return Ok((best.or(read_before.clone()), Some(language)));
        }
        let retried = prepare_again(
            &best.prepared,
            image,
            oriented,
            Some(language.script.clone()),
            language.model.clone(),
            best.prepared.quarter_turns,
        );
        let reading = self.read(image, &retried)?;
        Ok((best.or(Attempt::read(retried, reading)), Some(language)))
    }

    /// Reads an image in the given scripts in turn, until one reads it confidently, passing over
    /// each reading whose letters are mostly in another script: the best reading, whose script
    /// the rest of the extraction then assumes.
    fn read_overlooked(
        &self,
        best: Attempt,
        scripts: &[String],
        image: &Image<'_>,
        oriented: &Oriented,
    ) -> Result<Attempt, OcrError> {
        let installed = self.tesseract.languages()?;
        let mut overlooked = best.clone();
        for script in scripts {
            let Some(model) = installed_language(&installed, script) else {
                continue;
            };
            if overlooked.confident() {
                continue;
            }
            let retried = prepare_again(
                &best.prepared,
                image,
                oriented,
                Some(script.clone()),
                model,
                0,
            );
            let reading = self.read(image, &retried)?;
            if written_in(&reading.text, script) {
                overlooked = overlooked.or(Attempt::read(retried, reading));
            }
        }
        Ok(overlooked)
    }

    /// The attempt with the long s restored in what it read, by the dictionary of `model`.
    fn with_long_s(&self, mut attempt: Attempt, model: &str) -> Result<Attempt, OcrError> {
        if let Some(reading) = attempt
            .reading
            .as_mut()
            .filter(|reading| reading.text.contains('f'))
        {
            reading.text = self
                .tesseract
                .with_dictionary(model, |is_word| with_long_s(&reading.text, is_word))?;
        }
        Ok(attempt)
    }

    fn in_blackletter(
        &self,
        modern: Attempt,
        image: &Image<'_>,
        dictionary: &str,
    ) -> Result<Attempt, OcrError> {
        if !self.settings.auto() {
            return Ok(modern);
        }
        let Some(model) = self.blackletter_model()? else {
            return Ok(modern);
        };
        if model == modern.prepared.model {
            return Ok(modern);
        }
        let detected = self
            .tesseract
            .detect(pixels_of(image, &modern.prepared), modern.prepared.density)?;
        if detected.script.as_deref() != Some(BLACKLETTER_SCRIPT) {
            return Ok(modern);
        }
        let prepared = modern
            .prepared
            .read_in(modern.prepared.script.clone(), model);
        let reading = self.read(image, &prepared)?;
        let blackletter = Attempt::read(prepared, reading);
        self.tesseract.with_dictionary(dictionary, |is_word| {
            more_lexical(modern, blackletter, is_word)
        })
    }

    fn read_again(
        &self,
        read: Attempt,
        image: &Image<'_>,
        dictionary: &str,
    ) -> Result<Attempt, OcrError> {
        if !self.settings.auto() || read.noise || read.confidence() > MAX_REREAD_CONFIDENCE {
            return Ok(read);
        }
        let read = self.read_unwarped(read, image)?;
        let Some(model) = read
            .prepared
            .script
            .as_deref()
            .and_then(LineModel::of_script)
        else {
            return Ok(read);
        };
        if self.blackletter_model()?.as_deref() == Some(read.prepared.model.as_str()) {
            return Ok(read);
        }
        let (Some(image), Some(reading)) = (read.prepared.image.as_deref(), read.reading.as_ref())
        else {
            return Ok(read);
        };
        if reading.text.trim().is_empty() {
            return Ok(read);
        }
        let reread = ppocr()?.read(&image.gray(), model, &reading.blocks)?;
        let again = Attempt {
            prepared: read.prepared.clone(),
            reading: Some(Reading {
                text: corrected(&reread.text),
                ..reread
            }),
            noise: false,
            recognizer: Some(model.name()),
        };
        Ok(read.or(self.with_long_s(again, dictionary)?))
    }

    fn read_unwarped(&self, read: Attempt, image: &Image<'_>) -> Result<Attempt, OcrError> {
        let Some(made) = read.prepared.image.as_deref() else {
            return Ok(read);
        };
        let unwarped = Prepared {
            image: Some(Arc::new(Bitmap::Gray(ppocr()?.unwarped(&made.gray())?))),
            unwarped: true,
            png: OnceLock::new(),
            ..read.prepared.clone()
        };
        let reading = self.read(image, &unwarped)?;
        Ok(read.or(Attempt::read(unwarped, reading)))
    }

    fn blackletter_model(&self) -> Result<Option<String>, OcrError> {
        let installed = self.tesseract.languages()?;
        Ok(installed_language(&installed, BLACKLETTER_SCRIPT))
    }

    /// The model to read a script with: with the automatic language, the one for the script,
    /// else the configured one, which must be installed.
    fn model(&self, script: Option<&str>) -> Result<String, OcrError> {
        Ok(match &self.settings.language {
            Language::Auto => language(self.tesseract.languages()?.as_ref(), script),
            Language::Configured(language) => {
                self.tesseract.check_installed(language)?;
                language.clone()
            }
        })
    }

    fn read(&self, image: &Image<'_>, prepared: &Prepared) -> Result<Reading, OcrError> {
        self.read_pixels(pixels_of(image, prepared), prepared)
    }

    fn read_pixels(&self, pixels: Pixels<'_>, prepared: &Prepared) -> Result<Reading, OcrError> {
        let reading = self.tesseract.read(
            pixels,
            prepared.density,
            &prepared.model,
            SEGMENTATION,
            prepared.thresholding(),
            self.settings.timeout,
        )?;
        Ok(Reading {
            text: corrected(&reading.text),
            ..reading
        })
    }
}

/// The image as tesseract reads it: prepared, or else its file, as Java hands tesseract the
/// file of an image it does not decode for it, or else its pixels.
fn pixels_of<'a>(image: &'a Image<'a>, prepared: &'a Prepared) -> Pixels<'a> {
    match (&prepared.image, image.file) {
        (Some(made), _) => made.pixels(&prepared.png),
        (None, Some(file)) => {
            let (width, height) = image.bitmap.dimensions();
            Pixels::Encoded {
                data: file.into(),
                width,
                height,
            }
        }
        (None, None) => image.bitmap.pixels(&image.png),
    }
}

fn more_lexical(read: Attempt, replacement: Attempt, is_word: &dyn Fn(&str) -> bool) -> Attempt {
    let lexicality_of = |attempt: &Attempt| lexicality(attempt.text().unwrap_or_default(), is_word);
    match lexicality_of(&replacement) > lexicality_of(&read) {
        true => replacement,
        false => read,
    }
}

/// The scripts to try when no reading is confident: the one detection reported, when another
/// was read in its place, then those detection overlooks, except the scripts already read
/// upright.
fn overlooked(reported: Option<&str>, tried: &[&str]) -> Vec<String> {
    let mut scripts: Vec<String> = Vec::new();
    for script in reported.into_iter().chain(OVERLOOKED_SCRIPTS) {
        if !tried.contains(&script) && !scripts.iter().any(|known| known == script) {
            scripts.push(script.to_owned());
        }
    }
    scripts
}

fn language_strip(image: &GrayImage) -> Option<Range<usize>> {
    let lines = prepare::ink_runs(image);
    if lines.len() < LANGUAGE_STRIP_LINES {
        return None;
    }
    let first = (lines.len() - LANGUAGE_STRIP_LINES) / 2;
    let last = first + LANGUAGE_STRIP_LINES - 1;
    let height = image.height() as usize;
    let top = match first.checked_sub(1) {
        Some(above) => lines[above].end.midpoint(lines[first].start),
        None => 0,
    };
    let bottom = lines
        .get(last + 1)
        .map_or(height, |below| lines[last].end.midpoint(below.start));
    (2 * (bottom - top) <= height).then_some(top..bottom)
}

fn reducible(script: Option<&str>) -> bool {
    script.is_some_and(|script| REDUCIBLE_SCRIPTS.contains(&script))
}

/// Prepares an image to read in a script with a model as [`finish`] does, reusing an earlier
/// preparation that turned it as far and reduces it as this script is reduced.
fn prepare_again(
    earlier: &Prepared,
    image: &Image<'_>,
    oriented: &Oriented,
    script: Option<String>,
    model: String,
    quarter_turns: u8,
) -> Prepared {
    if quarter_turns == earlier.quarter_turns
        && reducible(script.as_deref()) == reducible(earlier.script.as_deref())
    {
        return earlier.read_in(script, model);
    }
    finish(image, oriented, script, model, quarter_turns)
}

/// Prepares an oriented image turned by the quarter turns detection found to read in a script
/// with a model: scales its text to a readable size for the script once the image is
/// [`Unscaled`], which it is once for each turn.
fn finish(
    image: &Image<'_>,
    oriented: &Oriented,
    script: Option<String>,
    model: String,
    quarter_turns: u8,
) -> Prepared {
    let upright: &Bitmap = match &oriented.image {
        Upright::Unprepared => return Prepared::unprepared(script, model, image.density),
        Upright::AsItIs => image.bitmap,
        Upright::Made(upright) => upright,
    };
    let unscaled = oriented.unscaled[usize::from(quarter_turns % 4)]
        .get_or_init(|| Unscaled::of(upright, quarter_turns));
    let scale = prepare::scale_factor(&unscaled.image, MAX_PIXELS, reducible(script.as_deref()));
    let made = if scale != 1.0 {
        Arc::new(Bitmap::Gray(prepare::scale(&unscaled.image, scale)))
    } else {
        Arc::clone(&unscaled.image)
    };
    let sideways = oriented
        .exif_orientation
        .is_some_and(Orientation::swaps_sides)
        != (quarter_turns % 2 == 1);
    Prepared {
        image: Some(made),
        model,
        uneven_lighting: unscaled.uneven_lighting,
        script,
        density: image
            .density
            .map(|density| density.transformed(sideways, scale)),
        exif_orientation: oriented.exif_orientation,
        decoded: true,
        flattened: oriented.flattened,
        unwarped: false,
        inverted: unscaled.inverted,
        despeckled: unscaled.despeckled,
        quarter_turns,
        skew_degrees: unscaled.skew_degrees,
        scale,
        png: OnceLock::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_runs_as_the_configuration_says() {
        let timeout = |config: TesseractOcrConfig| OcrSettings::of(&config).timeout;
        assert_eq!(
            timeout(TesseractOcrConfig::new()),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            timeout(TesseractOcrConfig::new().set_timeout_seconds(5)),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            timeout(TesseractOcrConfig::new().set_timeout_seconds(0)),
            None
        );
        assert_eq!(
            timeout(TesseractOcrConfig::new().set_timeout_seconds(-1)),
            None
        );
    }

    #[test]
    fn counts_words_between_whitespace() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("   "), 0);
        assert_eq!(word_count("one two\n\nthree "), 3);
        assert_eq!(word_count(" one two"), 2);
    }

    fn attempt(text: &str, confidence: i32) -> Attempt {
        Attempt::read(
            Prepared::unprepared(Some("Latin".to_owned()), "script/Latin".to_owned(), None),
            Reading {
                text: text.to_owned(),
                confidence,
                blocks: Vec::new(),
            },
        )
    }

    const SPANISH: &str = "En un lugar de la Mancha, de cuyo nombre no quiero acordarme, no ha mucho \
                           tiempo que vivia un hidalgo de los de lanza en astillero, adarga antigua, \
                           rocin flaco y galgo corredor.";

    #[test]
    fn drops_the_digits_and_marks_read_in_a_texture() {
        let texture = attempt("0 0\nرن 0\nس6 0 7 4-00\n4« ور 00 .6 5_7 7 83\nقي 70 0", 40);
        let dropped = texture.without_noise();
        assert!(dropped.noise);
        assert_eq!(dropped.text(), Some(""));
        assert!(dropped.report(None).ends_with(",\"noise\":true}"));
    }

    #[test]
    fn drops_letters_read_in_a_texture_in_no_language() {
        let texture = attempt(
            "EEE NEON EEA IERS AER TOEN ERA NEEESE RE EEL KOSEGOL JORIE",
            26,
        );
        assert!(texture.without_noise().noise);
    }

    #[test]
    fn keeps_text_in_a_language_read_with_little_confidence() {
        let kept = attempt(SPANISH, 45).without_noise();
        assert!(!kept.noise);
        assert_eq!(kept.text(), Some(SPANISH));
    }

    #[test]
    fn keeps_digits_read_with_confidence() {
        let table = "2019 2020 2021\n1 204,50 1 310,00 1 422,75\n-3,2% 8,8% 8,5%";
        let kept = attempt(table, 91).without_noise();
        assert!(!kept.noise);
        assert_eq!(kept.text(), Some(table));
        assert!(!kept.report(None).contains("noise"));
    }

    #[test]
    fn tells_text_that_is_mostly_letters() {
        assert!(mostly_letters(SPANISH));
        assert!(!mostly_letters("0 0 7 4-00 .6 5_7 رن"));
        assert!(!mostly_letters(""));
    }

    #[test]
    fn tries_the_reported_script_then_the_overlooked_ones() {
        assert_eq!(
            overlooked(Some("Georgian"), &["Latin"]),
            ["Georgian", "Armenian", "Hebrew"]
        );
        assert_eq!(overlooked(None, &["Armenian"]), ["Georgian", "Hebrew"]);
        assert_eq!(
            overlooked(Some("Cyrillic"), &["Latin"]),
            ["Cyrillic", "Armenian", "Georgian", "Hebrew"]
        );
    }

    #[test]
    fn evens_out_the_lighting_of_a_page_in_shadow() {
        let page = prepare::tests::shadowed_page(800, 600);
        let image = Image::of(&page, None, 0, None, None);
        let oriented = Oriented {
            image: Upright::AsItIs,
            exif_orientation: None,
            flattened: false,
            unscaled: Default::default(),
        };
        let prepared = finish(&image, &oriented, None, "eng".to_owned(), 0);
        assert!(prepared.uneven_lighting);
        assert_eq!(prepared.thresholding(), Thresholding::Sauvola);
        let made = prepared.image.expect("a prepared image");
        assert!(!prepare::uneven_lighting(&made, &prepare::analysis(&made)));
    }

    /// An image prepared as it was before the steps that do not depend on the script were kept:
    /// every step from the image the one before made, each judging it afresh.
    fn prepared_step_by_step(upright: &Bitmap, quarter_turns: u8, reduce: bool) -> Prepared {
        let mut image = upright
            .turned_clockwise(quarter_turns)
            .unwrap_or_else(|| upright.clone());
        let inverted = prepare::inverted(&prepare::analysis(&image));
        if inverted {
            image = Bitmap::Gray(prepare::invert(image.gray().into_owned()));
        }
        let despeckled = prepare::speckled(&image.gray());
        if despeckled {
            image = Bitmap::Gray(prepare::despeckle(&image.gray()));
        }
        let uneven_lighting = prepare::uneven_lighting(&image, &prepare::analysis(&image));
        image = Bitmap::Gray(if uneven_lighting {
            prepare::normalise_background(image.gray().into_owned())
        } else {
            prepare::whiten_faint_marks(&prepare::analysis(&image), image.gray().into_owned())
        });
        let skew_degrees = prepare::skew_degrees(&prepare::analysis(&image));
        if skew_degrees != 0.0 {
            image = image.rotated(skew_degrees);
        }
        let scale = prepare::scale_factor(&image, MAX_PIXELS, reduce);
        if scale != 1.0 {
            image = Bitmap::Gray(prepare::scale(&image, scale));
        }
        Prepared {
            image: Some(Arc::new(image)),
            uneven_lighting,
            inverted,
            despeckled,
            quarter_turns,
            skew_degrees,
            scale,
            ..Prepared::unprepared(None, String::new(), None)
        }
    }

    #[test]
    fn prepares_each_turn_once_as_step_by_step() {
        let small_text = Bitmap::Rgb(image::RgbImage::from_fn(300, 200, |x, y| {
            let inked = y % 14 < 6 && (20..280).contains(&x) && x % 9 < 7;
            let speck = (x * 31 + y * 17) % 97 == 0;
            image::Rgb(match (inked, speck) {
                (true, _) => [20, 40, 160],
                (false, true) => [0, 0, 0],
                (false, false) => [250, 245, 230],
            })
        }));
        let negative = Bitmap::Gray(GrayImage::from_fn(240, 180, |x, y| {
            image::Luma([if y % 30 < 12 && x % 11 < 8 { 255 } else { 10 }])
        }));
        let large_text = Bitmap::Gray(lines(1000, (0..12).map(|line| 60 + line * 70)));
        let tiny = Bitmap::Gray(GrayImage::from_fn(6, 5, |x, y| {
            image::Luma([((x * 40 + y * 30) % 256) as u8])
        }));
        let pages = [
            prepare::tests::shadowed_page(400, 300),
            small_text,
            negative,
            large_text,
            tiny,
        ];
        let readings = [
            (Some("Latin"), 0),
            (Some("Arabic"), 0),
            (None, 1),
            (Some("Latin"), 1),
            (Some("Cyrillic"), 2),
            (Some("Han"), 3),
            (Some("Greek"), 0),
            (Some("Latin"), 3),
        ];
        for page in &pages {
            let image = Image::of(page, None, 0, None, None);
            let oriented = Oriented {
                image: Upright::AsItIs,
                exif_orientation: None,
                flattened: false,
                unscaled: Default::default(),
            };
            for (script, quarter_turns) in readings {
                let prepared = finish(
                    &image,
                    &oriented,
                    script.map(str::to_owned),
                    String::new(),
                    quarter_turns,
                );
                let expected = prepared_step_by_step(page, quarter_turns, reducible(script));
                let gray = |prepared: &Prepared| {
                    prepared
                        .image
                        .as_deref()
                        .map(|image| image.gray().into_owned())
                };
                let context = format!("{:?}, {script:?}, {quarter_turns}", page.dimensions());
                assert_eq!(gray(&prepared), gray(&expected), "{context}");
                assert_eq!(
                    (
                        prepared.inverted,
                        prepared.despeckled,
                        prepared.uneven_lighting,
                        prepared.skew_degrees,
                        prepared.scale,
                    ),
                    (
                        expected.inverted,
                        expected.despeckled,
                        expected.uneven_lighting,
                        expected.skew_degrees,
                        expected.scale,
                    ),
                    "{context}"
                );
            }
        }
    }

    fn german(word: &str) -> bool {
        ["die", "der", "Zulassung", "Frauen", "zur", "Sitzung"].contains(&word)
    }

    #[test]
    fn keeps_the_blackletter_reading_with_more_words_of_the_language() {
        let modern = attempt("Die Zulafsung der Srauen zur Situng", 66);
        let blackletter = attempt("Die Zulassung der Frauen zur Sitzung", 80);
        assert_eq!(
            more_lexical(modern, blackletter, &german).text(),
            Some("Die Zulassung der Frauen zur Sitzung")
        );
    }

    #[test]
    fn keeps_the_reading_it_would_replace_unless_the_other_has_more_words() {
        let modern = attempt("Die Zulassung der Frauen zur Sitzung", 80);
        let blackletter = attempt("Die Zulafsung der Frauen zur Sitzung", 90);
        assert_eq!(
            more_lexical(modern.clone(), blackletter, &german).text(),
            Some("Die Zulassung der Frauen zur Sitzung")
        );
        let same = attempt("Die Zulassung der Frauen zur Sitzung", 95);
        assert_eq!(more_lexical(modern, same, &german).confidence(), 80);
    }

    fn lines(height: u32, tops: impl IntoIterator<Item = u32>) -> GrayImage {
        let mut page = GrayImage::from_pixel(400, height, image::Luma([255]));
        for top in tops {
            for y in top..top + 10 {
                for x in 20..380 {
                    page.put_pixel(x, y, image::Luma([0]));
                }
            }
        }
        page
    }

    #[test]
    fn cuts_a_strip_of_lines_about_the_middle_between_lines() {
        let page = lines(1000, (0..20).map(|line| 100 + line * 20));
        assert_eq!(language_strip(&page), Some(235..355));
    }

    #[test]
    fn cuts_no_strip_of_a_page_of_few_lines_or_of_most_of_it() {
        assert_eq!(
            language_strip(&lines(1000, (0..5).map(|line| 100 + line * 20))),
            None
        );
        assert_eq!(
            language_strip(&lines(400, (0..8).map(|line| 20 + line * 45))),
            None
        );
    }

    fn auto_settings() -> OcrSettings {
        OcrSettings::of(&TesseractOcrConfig::new().set_language(TesseractOcrConfig::AUTO_LANGUAGE))
    }

    fn scan(name: &str) -> Bitmap {
        Bitmap::Gray(
            image::open(format!("../quality/corpus/{name}"))
                .unwrap()
                .to_luma8(),
        )
    }

    fn first_model(name: &str, assumed: Option<&OcrLanguage>) -> String {
        let tesseract = crate::ocr::tesseract().unwrap();
        let settings = auto_settings();
        let recognizer = Recognizer::new(tesseract, &settings);
        let bitmap = scan(name);
        let image = Image::of(&bitmap, None, 0, None, Density::per_inch(300.0, 300.0));
        let oriented = Oriented::of(&image);
        let first = recognizer
            .first_preparation(&image, &oriented, assumed)
            .unwrap();
        match assumed {
            Some(_) => first.model,
            None => recognizer
                .in_language_of_strip(&first)
                .unwrap()
                .map_or(first.model, |seeded| seeded.model),
        }
    }

    #[test]
    fn reads_a_first_image_first_with_the_model_of_the_language_of_a_strip() {
        assert_eq!(first_model("spanish-scan.png", None), "spa");
        assert_eq!(first_model("german-scan.png", None), "deu");
    }

    #[test]
    fn reads_a_first_image_in_no_language_first_with_the_script_model() {
        assert_eq!(
            first_model("prose-scan-upside-down.png", None),
            "script/Latin"
        );
        assert_eq!(first_model("russian-scan.png", None), "script/Latin");
    }

    #[test]
    fn reads_a_first_image_first_with_the_model_of_the_language_hinted() {
        let tesseract = crate::ocr::tesseract().unwrap();
        let settings = auto_settings();
        let recognizer = Recognizer::new(tesseract, &settings);
        let assumed = recognizer
            .assumed(LanguageHint::tagged("es-ES").unwrap())
            .unwrap();
        let spanish = OcrLanguage {
            script: "Latin".to_owned(),
            model: "spa".to_owned(),
        };
        assert_eq!(assumed.as_ref(), Some(&spanish));
        assert_eq!(first_model("spanish-scan.png", assumed.as_ref()), "spa");
        let configured = OcrSettings::of(&TesseractOcrConfig::new());
        let hint = LanguageHint::detected(SPANISH).unwrap();
        assert_eq!(
            Recognizer::new(tesseract, &configured)
                .assumed(hint)
                .unwrap(),
            None
        );
        assert_eq!(recognizer.assumed(hint).unwrap(), Some(spanish));
    }

    fn recognized(name: &str) -> Attempt {
        let tesseract = crate::ocr::tesseract().unwrap();
        let settings = auto_settings();
        let bitmap = scan(name);
        let image = Image::of(&bitmap, None, 0, None, Density::per_inch(300.0, 300.0));
        Recognizer::new(tesseract, &settings)
            .recognize(&image, None)
            .unwrap()
            .read
    }

    #[test]
    fn reads_worn_early_print_again_with_pp_ocr_and_keeps_its_more_confident_reading() {
        let read = recognized("more-epigrammata-1520.jpg");
        assert!(read.report(None).contains("\"ppocr\":\"latin\""));
        let text = read.text().unwrap();
        assert!(text.contains("picturæ lenocinio"), "{text}");
    }

    #[test]
    fn reads_a_curled_page_flattened() {
        let read = recognized("prose-photo-curled.jpg");
        assert!(read.report(None).contains("\"unwarped\":true"));
        let text = read.text().unwrap();
        assert!(
            text.contains("Every evening at dusk the keeper climbed the one hundred"),
            "{text}"
        );
    }

    #[test]
    fn leaves_a_page_tesseract_reads_confidently_to_tesseract() {
        assert!(!recognized("spanish-scan.png")
            .report(None)
            .contains("ppocr"));
    }

    #[test]
    fn reads_a_first_image_once_in_the_language_of_a_strip_as_it_read_it_before() {
        let tesseract = crate::ocr::tesseract().unwrap();
        let settings = auto_settings();
        let bitmap = scan("spanish-scan.png");
        let image = Image::of(&bitmap, None, 0, None, Density::per_inch(300.0, 300.0));
        let Recognized { read, language } = Recognizer::new(tesseract, &settings)
            .recognize(&image, None)
            .unwrap();
        assert_eq!(
            language,
            Some(OcrLanguage {
                script: "Latin".to_owned(),
                model: "spa".to_owned(),
            })
        );
        let text = read.text().unwrap();
        assert!(
            text.contains("Declaración Universal de Derechos Humanos"),
            "{text}"
        );
    }

    #[test]
    fn keeps_the_script_reading_of_a_first_image_its_languages_model_reads_unconfidently() {
        let tesseract = crate::ocr::tesseract().unwrap();
        let settings = auto_settings();
        let bitmap = scan("luxembourgish-scan.png");
        let image = Image::of(&bitmap, None, 0, None, Density::per_inch(300.0, 300.0));
        assert_eq!(first_model("luxembourgish-scan.png", None), "ltz");
        let Recognized { read, language } = Recognizer::new(tesseract, &settings)
            .recognize(&image, None)
            .unwrap();
        assert_eq!(read.prepared.model, "script/Latin");
        assert_eq!(
            language.map(|language| language.model).as_deref(),
            Some("ltz")
        );
        let text = read.text().unwrap();
        assert!(text.starts_with("UNIVERSAL - DEKLARATIOUN"), "{text}");
        assert!(text.contains("Dignitéit, déi all Mêmber"), "{text}");
    }
}
