//! Our Java `PreparingOcrParser`'s reading of an image: prepared first, turned upright by its
//! EXIF orientation and by the skew of its text lines, and resized so its text lines are of a
//! size tesseract reads well and fast, then read as it is: with the automatic language, as
//! upright text in the script the extraction assumes. When tesseract is not confident of that
//! reading, its orientation and script detection runs: the image is turned by the quarter turns
//! it finds and, with the automatic language, read with a model for the script it finds.

use std::sync::Arc;
use std::time::Duration;

use image::GrayImage;

use super::flatten::flatten;
use super::prepare::{self, Orientation};
use super::report::Report;
use super::script::{installed_language, language, written_in};
use super::tesseract::{OcrError, PageSegMode, Pixels, Poor, Reading, Tesseract, Thresholding};
use super::text::corrected;
use crate::config::TesseractOcrConfig;

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
/// Scripts tesseract's detection takes for others (Armenian for upside-down Latin, Georgian for
/// Malayalam), tried in turn when no reading of an image with plenty of text is confident.
const OVERLOOKED_SCRIPTS: [&str; 2] = ["Armenian", "Georgian"];
/// Scripts detection reports for pages in another script, which is read first in its place:
/// Georgian pages are reported as Malayalam. The reported script is then the first of the
/// overlooked ones tried.
const REPORTED_FOR: [(&str, &str); 1] = [("Malayalam", "Georgian")];
const MIN_WORDS_TO_RETRY: usize = 20;
/// The first words of a reading in the assumed script that show the page is in another one:
/// the Latin model reads the first twenty words of a Cyrillic, Greek, Armenian or Georgian page
/// with a mean confidence of at most about 55, and of a Latin page with at least about 88.
const POOR: Poor = Poor {
    after_words: 20,
    below_confidence: 70,
};
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
pub(crate) struct OcrSettings {
    language: Language,
    timeout: Duration,
}

impl OcrSettings {
    pub(crate) fn of(config: &TesseractOcrConfig) -> Self {
        let language = match config.language() {
            Some(TesseractOcrConfig::AUTO_LANGUAGE) => Language::Auto,
            Some(language) => Language::Configured(language.to_owned()),
            None => Language::Configured(DEFAULT_LANGUAGE.to_owned()),
        };
        let seconds = config.timeout_seconds().unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        Self {
            language,
            timeout: Duration::from_millis(u64::try_from(seconds).unwrap_or(0) * 1000),
        }
    }

    /// Whether each image's script is detected, and its first script is learned.
    pub(crate) fn auto(&self) -> bool {
        self.language == Language::Auto
    }
}

/// An image to read.
pub(crate) struct Image<'a> {
    /// Its grey levels.
    pub(crate) gray: &'a GrayImage,
    /// Whether it is small enough to prepare: [`MAX_PIXELS`] or fewer, and decoded as it is.
    pub(crate) preparable: bool,
    /// The size of the image's file, for the limits it is read within.
    pub(crate) file_size: u64,
    /// The EXIF orientation recorded in it.
    pub(crate) orientation: Option<Orientation>,
}

impl<'a> Image<'a> {
    /// An image of a file of `file_size` bytes, prepared unless it has more than [`MAX_PIXELS`]
    /// pixels.
    pub(crate) fn of(
        gray: &'a GrayImage,
        file_size: u64,
        orientation: Option<Orientation>,
    ) -> Self {
        Self {
            gray,
            preparable: u64::from(gray.width()) * u64::from(gray.height()) <= MAX_PIXELS,
            file_size,
            orientation,
        }
    }
}

/// How an image was read, and the script it taught the extraction, if any.
#[derive(Debug)]
pub(crate) struct Recognized {
    pub(crate) read: Attempt,
    pub(crate) learned: Option<String>,
}

/// A way an image was prepared and what tesseract read in it that way.
#[derive(Debug, Clone)]
pub(crate) struct Attempt {
    prepared: Prepared,
    reading: Option<Reading>,
}

impl Attempt {
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
            .map_or(0, |reading| java_word_count(&reading.text))
    }

    /// The more confident of this attempt and another, this one on a tie.
    fn or(self, other: Attempt) -> Attempt {
        if other.confidence() > self.confidence() {
            other
        } else {
            self
        }
    }

    /// The report of how the image was read, on a page when it is one.
    pub(crate) fn report(&self, page: Option<u32>) -> String {
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
            .json()
    }

    /// The text read, unless the image was outside the limits it is read within.
    pub(crate) fn text(&self) -> Option<&str> {
        self.reading.as_ref().map(|reading| reading.text.as_str())
    }
}

/// `text.split("\\s+").length`: the pieces between runs of whitespace, a leading empty one
/// counted and trailing ones not, and one for an empty text.
fn java_word_count(text: &str) -> usize {
    if text.is_empty() {
        return 1;
    }
    let runs = text
        .split(crate::java::is_regex_space)
        .filter(|piece| !piece.is_empty())
        .count();
    if runs == 0 {
        return 0;
    }
    runs + usize::from(text.starts_with(crate::java::is_regex_space))
}

/// How an image was prepared to be read, and with which model.
#[derive(Debug, Clone)]
struct Prepared {
    /// The prepared image, or none when the image reads as it is.
    image: Option<Arc<GrayImage>>,
    /// The model to read it with: the configured one, or with the automatic language the one
    /// for its script.
    model: String,
    uneven_lighting: bool,
    /// The script it is read in: the one detection found, the one assumed for a first reading,
    /// or none.
    script: Option<String>,
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
    fn unprepared(script: Option<String>, model: String) -> Self {
        Self {
            image: None,
            model,
            uneven_lighting: false,
            script,
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
    Made(GrayImage),
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
            .map(|orientation| prepare::apply_orientation(image.gray, orientation));
        let flat = flatten(upright.as_ref().unwrap_or(image.gray));
        Self {
            exif_orientation: upright.as_ref().and(image.orientation),
            flattened: flat.is_some(),
            image: flat.or(upright).map_or(Upright::AsItIs, Upright::Made),
        }
    }
}

/// Reads images with a tesseract, as the settings of an extraction say.
pub(crate) struct Recognizer<'t> {
    tesseract: &'t Tesseract,
    settings: &'t OcrSettings,
}

impl<'t> Recognizer<'t> {
    pub(crate) fn new(tesseract: &'t Tesseract, settings: &'t OcrSettings) -> Self {
        Self {
            tesseract,
            settings,
        }
    }

    /// Prepares an image and reads it: what is read, how, and the script the extraction learns
    /// from it, if any. The image is first read as it is, upright text with the automatic
    /// language in the script assumed, or in the configured language otherwise. Only when
    /// tesseract is not confident of that reading does its orientation and script detection
    /// run; the image is then turned by the quarter turns it finds and, with the automatic
    /// language, read with a model for the script it finds, unless that would read it as
    /// before. When no reading of an image with at least [`MIN_WORDS_TO_RETRY`] words is
    /// confident, the scripts detection overlooks are tried as well, and the most confident
    /// reading is kept. With the automatic language, each of these readings is a guess, given up
    /// once it goes [`POOR`]ly, as a page in another script or turned does; when every one is
    /// given up, the image is read to the end as detection found it, in the first reading's
    /// script when detection found none. Without a reading when the image is outside the
    /// configured limits.
    ///
    /// `assumed` is the script to read it in first with the automatic language: the one
    /// detection last found in this extraction, or none for Latin, the most common kind.
    pub(crate) fn recognize(
        &self,
        image: &Image<'_>,
        assumed: Option<&str>,
    ) -> Result<Recognized, OcrError> {
        let auto = self.settings.auto();
        let oriented = Oriented::of(image);
        let assumed_script = auto.then(|| assumed.unwrap_or(ASSUMED_SCRIPT).to_owned());
        let first_model = self.model(assumed_script.as_deref())?;
        let first = finish(image, &oriented, assumed_script, first_model, 0);
        if image.file_size > MAX_FILE_SIZE {
            return Ok(Recognized {
                read: Attempt {
                    prepared: first,
                    reading: None,
                },
                learned: None,
            });
        }
        let optimistic = self.guess(image, &first)?;
        if optimistic
            .as_ref()
            .is_some_and(|reading| reading.confidence > MIN_CONFIDENCE)
        {
            return Ok(Recognized {
                read: Attempt {
                    prepared: first,
                    reading: optimistic,
                },
                learned: None,
            });
        }
        let detected = self.tesseract.detect(pixels_of(image, &first))?;
        let reported = detected.script;
        let script = match &reported {
            _ if !auto => None,
            None if optimistic.is_none() => first.script.clone(),
            None => None,
            Some(reported) => Some(
                REPORTED_FOR
                    .iter()
                    .find(|(name, _)| name == reported)
                    .map_or_else(|| reported.clone(), |(_, read)| (*read).to_owned()),
            ),
        };
        let model = self.model(script.as_deref())?;
        let mut learned = script.clone();
        let mut best = Attempt {
            prepared: first.clone(),
            reading: optimistic,
        };
        let mut fallback = first.clone();
        if detected.clockwise_quarter_turns != 0 || model != first.model {
            fallback = prepare_again(
                &first,
                image,
                &oriented,
                script,
                model,
                detected.clockwise_quarter_turns,
            );
            if let Some(reading) = self.guess(image, &fallback)? {
                best = best.or(Attempt {
                    prepared: fallback.clone(),
                    reading: Some(reading),
                });
            }
        }
        if auto
            && !best.confident()
            && (best.reading.is_none() || best.words() >= MIN_WORDS_TO_RETRY)
        {
            let tried: Vec<&str> = [&first, &fallback]
                .into_iter()
                .filter(|read| read.quarter_turns == 0)
                .filter_map(|read| read.script.as_deref())
                .collect();
            let scripts = overlooked(reported.as_deref(), &tried);
            best = self.read_overlooked(best, &scripts, image, &oriented)?;
            if best.prepared.script.is_some() {
                learned = best.prepared.script.clone();
            }
        }
        if best.reading.is_none() {
            let reading = self.read(image, &fallback)?;
            best = Attempt {
                prepared: fallback,
                reading: Some(reading),
            };
        }
        Ok(Recognized {
            read: best,
            learned,
        })
    }

    /// Reads an image in the given scripts in turn, until one reads it confidently, giving up
    /// each reading that goes poorly and passing over each whose letters are mostly in another
    /// script: the best reading, whose script the rest of the extraction then assumes.
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
            if let Some(reading) = self.read_unless_poor(image, &retried)? {
                if written_in(&reading.text, script) {
                    overlooked = overlooked.or(Attempt {
                        prepared: retried,
                        reading: Some(reading),
                    });
                }
            }
        }
        Ok(overlooked)
    }

    /// The model to read a script with: with the automatic language, the one for the script,
    /// else the configured one.
    fn model(&self, script: Option<&str>) -> Result<String, OcrError> {
        Ok(match &self.settings.language {
            Language::Auto => language(&self.tesseract.languages()?, script),
            Language::Configured(language) => language.clone(),
        })
    }

    /// A reading that may turn out a guess: with the automatic language given up when it goes
    /// poorly, else read to the end.
    fn guess(&self, image: &Image<'_>, prepared: &Prepared) -> Result<Option<Reading>, OcrError> {
        if self.settings.auto() {
            self.read_unless_poor(image, prepared)
        } else {
            self.read(image, prepared).map(Some)
        }
    }

    fn read(&self, image: &Image<'_>, prepared: &Prepared) -> Result<Reading, OcrError> {
        Ok(self
            .run(image, prepared, None)?
            .expect("a reading without a judge is never given up"))
    }

    fn read_unless_poor(
        &self,
        image: &Image<'_>,
        prepared: &Prepared,
    ) -> Result<Option<Reading>, OcrError> {
        self.run(image, prepared, Some(POOR))
    }

    fn run(
        &self,
        image: &Image<'_>,
        prepared: &Prepared,
        poor: Option<Poor>,
    ) -> Result<Option<Reading>, OcrError> {
        let reading = self.tesseract.read(
            pixels_of(image, prepared),
            &prepared.model,
            SEGMENTATION,
            prepared.thresholding(),
            poor,
            self.settings.timeout,
        )?;
        Ok(reading.map(|reading| Reading {
            text: corrected(&reading.text),
            confidence: reading.confidence,
        }))
    }
}

/// The image as tesseract reads it prepared, or as it is.
fn pixels_of<'a>(image: &Image<'a>, prepared: &'a Prepared) -> Pixels<'a> {
    gray_pixels(prepared.image.as_deref().unwrap_or(image.gray))
}

/// A grey image as tesseract reads it.
pub(crate) fn gray_pixels(image: &GrayImage) -> Pixels<'_> {
    Pixels {
        data: image.as_raw(),
        width: image.width(),
        height: image.height(),
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
    let (oriented_image, mut changed) = match &oriented.image {
        Upright::Unprepared => return Prepared::unprepared(script, model),
        Upright::AsItIs => (image.gray, false),
        Upright::Made(upright) => (upright, true),
    };
    let mut prepared = match prepare::turn_clockwise(oriented_image, quarter_turns) {
        Some(turned) => {
            changed = true;
            turned
        }
        None => oriented_image.clone(),
    };
    let inverted = prepare::inverted(&prepared);
    if inverted {
        prepared = prepare::invert(prepared);
        changed = true;
    }
    let despeckled = prepare::speckled(&prepared);
    if despeckled {
        prepared = prepare::despeckle(&prepared);
        changed = true;
    }
    let uneven_lighting = prepare::uneven_lighting(&prepared);
    if !uneven_lighting {
        prepared = prepare::whiten_faint_marks(prepared);
        changed = true;
    }
    let skew_degrees = prepare::skew_degrees(&prepared);
    if skew_degrees != 0.0 {
        prepared = prepare::rotate(&prepared, skew_degrees);
        changed = true;
    }
    let scale = prepare::scale_factor(&prepared, MAX_PIXELS, reducible(script.as_deref()));
    if scale != 1.0 {
        prepared = prepare::scale(&prepared, scale);
        changed = true;
    }
    Prepared {
        image: changed.then(|| Arc::new(prepared)),
        model,
        uneven_lighting,
        script,
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
    fn counts_words_as_java_splits_them() {
        assert_eq!(java_word_count(""), 1);
        assert_eq!(java_word_count("   "), 0);
        assert_eq!(java_word_count("one two\n\nthree "), 3);
        assert_eq!(java_word_count(" one two"), 3);
    }

    #[test]
    fn tries_the_reported_script_then_the_overlooked_ones() {
        assert_eq!(
            overlooked(Some("Georgian"), &["Latin"]),
            ["Georgian", "Armenian"]
        );
        assert_eq!(overlooked(None, &["Armenian"]), ["Georgian"]);
        assert_eq!(
            overlooked(Some("Cyrillic"), &["Latin"]),
            ["Cyrillic", "Armenian", "Georgian"]
        );
    }
}
