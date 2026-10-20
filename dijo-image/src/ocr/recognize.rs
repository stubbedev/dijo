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
//! dictionary of the language's model.

use std::sync::Arc;
use std::time::Duration;

use super::bitmap::{Bitmap, Density};
use super::flatten::flatten;
use super::prepare::{self, Orientation};
use super::report::Report;
use super::script::{installed_language, language, language_model, written_in};
use super::tesseract::{OcrError, PageSegMode, Pixels, Reading, Tesseract, Thresholding};
use super::text::{corrected, with_long_s};
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
/// Scripts tesseract's detection takes for others (Armenian for upside-down Latin, Georgian for
/// Malayalam, Hebrew for Thai), tried in turn when no reading of an image with plenty of text is
/// confident.
const OVERLOOKED_SCRIPTS: [&str; 3] = ["Armenian", "Georgian", "Hebrew"];
/// Scripts detection reports for pages in another script, which is read first in its place:
/// Georgian pages are reported as Malayalam. The reported script is then the first of the
/// overlooked ones tried.
const REPORTED_FOR: [(&str, &str); 1] = [("Malayalam", "Georgian")];
const MIN_WORDS_TO_RETRY: usize = 20;
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
}

impl Attempt {
    fn read(prepared: Prepared, reading: Reading) -> Self {
        Self {
            prepared,
            reading: Some(reading),
            noise: false,
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
    inverted: bool,
    despeckled: bool,
    quarter_turns: u8,
    skew_degrees: f64,
    scale: f64,
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
            inverted: false,
            despeckled: false,
            quarter_turns: 0,
            skew_degrees: 0.0,
            scale: 1.0,
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
/// flat: the first steps of preparing it, which come before orientation detection.
struct Oriented {
    image: Upright,
    exif_orientation: Option<Orientation>,
    flattened: bool,
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
    /// The images prepared to read it are freed once it is read: what was read keeps none.
    pub fn recognize(
        &self,
        image: &Image<'_>,
        assumed: Option<&OcrLanguage>,
    ) -> Result<Recognized, OcrError> {
        let mut recognized = self.recognize_preparing(image, assumed)?;
        recognized.read.prepared.image = None;
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
        let first_language = match (auto, assumed) {
            (false, _) => None,
            (true, Some(assumed)) => Some(assumed.clone()),
            (true, None) => Some(OcrLanguage {
                script: ASSUMED_SCRIPT.to_owned(),
                model: self.model(Some(ASSUMED_SCRIPT))?,
            }),
        };
        let first = match &first_language {
            Some(OcrLanguage { script, model }) => {
                finish(image, &oriented, Some(script.clone()), model.clone(), 0)
            }
            None => finish(image, &oriented, None, self.model(None)?, 0),
        };
        if image.file_size > MAX_FILE_SIZE {
            return Ok(Recognized {
                read: Attempt {
                    prepared: first,
                    reading: None,
                    noise: false,
                },
                language: None,
            });
        }
        let optimistic = Attempt::read(first.clone(), self.read(image, &first)?);
        let best = match optimistic.confident() || optimistic.in_a_language() {
            true => optimistic,
            false => self.read_detected(optimistic, image, &oriented)?,
        };
        let (best, detected) = match auto {
            true => self.read_in_detected_language(best, image, &oriented)?,
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
        let best = self.with_long_s(best, &dictionary)?;
        Ok(Recognized {
            read: best,
            language,
        })
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
        image: &Image<'_>,
        oriented: &Oriented,
    ) -> Result<(Attempt, Option<OcrLanguage>), OcrError> {
        let Some(script) = best.prepared.script.clone() else {
            return Ok((best, None));
        };
        let Some(detected) = best.language_in_its_script() else {
            return Ok((best, None));
        };
        let installed = self.tesseract.languages()?;
        let Some(model) = language_model(&installed, detected.code, &script) else {
            return Ok((best, None));
        };
        let language = OcrLanguage {
            script: script.clone(),
            model: model.to_owned(),
        };
        if model == best.prepared.model {
            return Ok((best, Some(language)));
        }
        let retried = prepare_again(
            &best.prepared,
            image,
            oriented,
            Some(script),
            model.to_owned(),
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
        let reading = self.tesseract.read(
            pixels_of(image, prepared),
            prepared.density,
            &prepared.model,
            SEGMENTATION,
            prepared.thresholding(),
            self.settings.timeout,
        )?;
        Ok(Reading {
            text: corrected(&reading.text),
            confidence: reading.confidence,
        })
    }
}

/// The image as tesseract reads it: prepared, or else its file, as Java hands tesseract the
/// file of an image it does not decode for it, or else its pixels.
fn pixels_of<'a>(image: &Image<'a>, prepared: &'a Prepared) -> Pixels<'a> {
    match (&prepared.image, image.file) {
        (Some(prepared), _) => prepared.pixels(),
        (None, Some(file)) => {
            let (width, height) = image.bitmap.dimensions();
            Pixels::Encoded {
                data: file.into(),
                width,
                height,
            }
        }
        (None, None) => image.bitmap.pixels(),
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

/// The image as preparing it has made it so far, or the `original` while it has made none.
fn prepared<'b>(made: &'b Option<Arc<Bitmap>>, original: &'b Bitmap) -> &'b Bitmap {
    made.as_deref().unwrap_or(original)
}

/// Turns an oriented image by the quarter turns detection found, inverts light text on a dark
/// ground, clears specks, whitens faint marks unless the lighting is uneven, corrects its skew
/// and scales its text to a readable size.
fn finish(
    image: &Image<'_>,
    oriented: &Oriented,
    script: Option<String>,
    model: String,
    quarter_turns: u8,
) -> Prepared {
    let mut made = match &oriented.image {
        Upright::Unprepared => return Prepared::unprepared(script, model, image.density),
        Upright::AsItIs => None,
        Upright::Made(upright) => Some(Arc::clone(upright)),
    };
    let original = image.bitmap;
    if let Some(turned) = prepared(&made, original).turned_clockwise(quarter_turns) {
        made = Some(Arc::new(turned));
    }
    let inverted = prepare::inverted(prepared(&made, original));
    if inverted {
        made = Some(Arc::new(Bitmap::Gray(prepare::invert(prepared(
            &made, original,
        )))));
    }
    let despeckled = prepare::speckled(prepared(&made, original));
    if despeckled {
        made = Some(Arc::new(Bitmap::Gray(prepare::despeckle(prepared(
            &made, original,
        )))));
    }
    let uneven_lighting = prepare::uneven_lighting(prepared(&made, original));
    if !uneven_lighting {
        made = Some(Arc::new(Bitmap::Gray(prepare::whiten_faint_marks(
            prepared(&made, original),
        ))));
    }
    let skew_degrees = prepare::skew_degrees(prepared(&made, original));
    if skew_degrees != 0.0 {
        made = Some(Arc::new(prepared(&made, original).rotated(skew_degrees)));
    }
    let scale = prepare::scale_factor(
        prepared(&made, original),
        MAX_PIXELS,
        reducible(script.as_deref()),
    );
    if scale != 1.0 {
        made = Some(Arc::new(Bitmap::Gray(prepare::scale(
            prepared(&made, original),
            scale,
        ))));
    }
    let sideways = oriented
        .exif_orientation
        .is_some_and(Orientation::swaps_sides)
        != (quarter_turns % 2 == 1);
    Prepared {
        image: made,
        model,
        uneven_lighting,
        script,
        density: image
            .density
            .map(|density| density.transformed(sideways, scale)),
        exif_orientation: oriented.exif_orientation,
        decoded: true,
        flattened: oriented.flattened,
        inverted,
        despeckled,
        quarter_turns,
        skew_degrees,
        scale,
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
}
