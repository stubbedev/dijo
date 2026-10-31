//! Our Java `OcrReport`: what was done to an image before tesseract read it, and with which
//! model, as one JSON object per image or page under [`KEY`]: the page, the model, the script
//! orientation and script detection found, the EXIF orientation applied, whether a page
//! photographed at an angle was made flat, whether light text on a dark ground was inverted,
//! whether specks were removed, the quarter turns and deskewing that made it upright, the
//! enlargement, whether it was thresholded for uneven lighting, and whether the image could be
//! decoded to prepare it at all.

use std::fmt::Write as _;

use crate::java::printf;

/// The metadata key the reports are added under.
pub const KEY: &str = "dijo:ocr";

/// A report being written.
pub(crate) struct Report {
    json: String,
}

impl Report {
    pub(crate) fn of(language: &str) -> Self {
        Self {
            json: String::from("{"),
        }
        .string("language", language)
    }

    pub(crate) fn page(self, page: Option<u32>) -> Self {
        match page {
            Some(page) => self.number("page", i64::from(page)),
            None => self,
        }
    }

    pub(crate) fn script(self, script: Option<&str>) -> Self {
        match script {
            Some(script) => self.string("script", script),
            None => self,
        }
    }

    pub(crate) fn exif_orientation(self, orientation: Option<u8>) -> Self {
        match orientation {
            Some(orientation) => self.number("exif_orientation", i64::from(orientation)),
            None => self,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepared(
        self,
        decoded: bool,
        flattened: bool,
        inverted: bool,
        despeckled: bool,
        quarter_turns: u8,
        skew_degrees: f64,
        scale: f64,
        thresholded: bool,
    ) -> Self {
        self.boolean("prepared", decoded)
            .boolean("flattened", flattened)
            .boolean("inverted", inverted)
            .boolean("despeckled", despeckled)
            .number("turned_degrees", i64::from(quarter_turns) * 90)
            .decimal("deskewed_degrees", skew_degrees)
            .decimal("scale", scale)
            .boolean("thresholded", thresholded)
    }

    pub(crate) fn json(mut self) -> String {
        self.json.push('}');
        self.json
    }

    /// Notes that what was read was taken for noise and dropped.
    pub(crate) fn noise(self, noise: bool) -> Self {
        match noise {
            true => self.boolean("noise", true),
            false => self,
        }
    }

    pub(crate) fn unwarped(self, unwarped: bool) -> Self {
        match unwarped {
            true => self.boolean("unwarped", true),
            false => self,
        }
    }

    pub(crate) fn ppocr(self, recognizer: Option<&str>) -> Self {
        match recognizer {
            Some(recognizer) => self.string("ppocr", recognizer),
            None => self,
        }
    }

    fn key(mut self, name: &str) -> Self {
        if self.json.len() > 1 {
            self.json.push(',');
        }
        let _ = write!(self.json, "\"{name}\":");
        self
    }

    fn string(self, name: &str, value: &str) -> Self {
        let mut report = self.key(name);
        report.json.push('"');
        for c in value.chars() {
            match c {
                '"' => report.json.push_str("\\\""),
                '\\' => report.json.push_str("\\\\"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(report.json, "\\u{:04x}", c as u32);
                }
                c => report.json.push(c),
            }
        }
        report.json.push('"');
        report
    }

    fn number(self, name: &str, value: i64) -> Self {
        let mut report = self.key(name);
        let _ = write!(report.json, "{value}");
        report
    }

    fn decimal(self, name: &str, value: f64) -> Self {
        let mut report = self.key(name);
        report.json.push_str(&printf::fixed(value, 0, 3, false));
        report
    }

    fn boolean(self, name: &str, value: bool) -> Self {
        let mut report = self.key(name);
        let _ = write!(report.json, "{value}");
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_report_as_ocr_report_does() {
        let json = Report::of("script/Latin")
            .page(Some(2))
            .script(Some("Latin"))
            .exif_orientation(Some(6))
            .prepared(true, false, false, false, 1, -0.5, 0.6955, true)
            .json();
        assert_eq!(
            json,
            "{\"language\":\"script/Latin\",\"page\":2,\"script\":\"Latin\",\
             \"exif_orientation\":6,\"prepared\":true,\"flattened\":false,\"inverted\":false,\
             \"despeckled\":false,\"turned_degrees\":90,\"deskewed_degrees\":-0.500,\
             \"scale\":0.696,\"thresholded\":true}"
        );
    }

    #[test]
    fn notes_a_reading_taken_for_noise() {
        let report = |noise| {
            Report::of("script/Latin")
                .prepared(true, false, false, false, 0, 0.0, 1.0, false)
                .noise(noise)
                .json()
        };
        assert!(report(true).ends_with(",\"thresholded\":false,\"noise\":true}"));
        assert!(!report(false).contains("noise"));
    }
}
