//! tika-server's `PUT /tika`, answered by the extraction of `/extract`, so that a Tika client
//! can be pointed at dijo unchanged.

use hyper::header::{HeaderMap, ACCEPT};

pub const PASSWORD_HEADER: &str = "password";
const OCR_LANGUAGE_HEADER: &str = "x-tika-ocrlanguage";
const OPTION_PREFIX: &str = "x-tika-";
/// Tika's tuning of when to OCR, at what resolution and for how long, which dijo decides by
/// itself: it OCRs only the images and pages without usable text, and stops at `--timeout`.
const IGNORED_OPTIONS: &[&str] = &[
    "x-tika-ocrskipocr",
    "x-tika-pdfocrstrategy",
    "x-tika-pdfocrdpi",
    "x-tika-ocrminfilesizetoocr",
    "x-tika-timeout-millis",
];
const ACCEPTED_MEDIA_RANGES: &[&str] = &["text/plain", "text/*", "*/*"];

/// What tika-server answers `GET /tika` with, which clients check it is up by.
pub fn hello() -> String {
    format!(
        "This is dijo {}, serving tika-server's PUT /tika. Please PUT\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// Whether the request accepts plain text, which is all `PUT /tika` answers with; no `Accept`
/// accepts anything.
pub fn accepts_text(headers: &HeaderMap) -> bool {
    let mut accepted = headers.get_all(ACCEPT).iter().peekable();
    accepted.peek().is_none()
        || accepted
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|range| {
                let media = range.split(';').next().unwrap_or_default().trim();
                ACCEPTED_MEDIA_RANGES
                    .iter()
                    .any(|accepted| media.eq_ignore_ascii_case(accepted))
            })
}

/// Refuses a Tika option dijo neither applies nor decides by itself, rather than extracting as
/// if it had not been asked for.
pub fn check_headers(headers: &HeaderMap) -> Result<(), String> {
    match headers.keys().map(|name| name.as_str()).find(|name| {
        name.starts_with(OPTION_PREFIX)
            && *name != OCR_LANGUAGE_HEADER
            && !IGNORED_OPTIONS.contains(name)
    }) {
        Some(name) => Err(format!("The Tika option {name} is not supported")),
        None => Ok(()),
    }
}

/// The tesseract languages of `X-Tika-OCRLanguage`, such as `eng+deu`; without it, the language
/// is detected.
pub fn ocr_language(headers: &HeaderMap) -> Result<Option<String>, String> {
    headers
        .get(OCR_LANGUAGE_HEADER)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| format!("{OCR_LANGUAGE_HEADER} must be visible ASCII"))
                .and_then(crate::settings::ocr_language)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(name, value)| {
                (
                    hyper::header::HeaderName::from_static(name),
                    HeaderValue::from_static(value),
                )
            })
            .collect()
    }

    #[test]
    fn plain_text_is_accepted_however_it_is_asked_for() {
        assert!(accepts_text(&headers(&[])));
        assert!(accepts_text(&headers(&[("accept", "text/plain")])));
        assert!(accepts_text(&headers(&[(
            "accept",
            "application/json, Text/Plain; q=0.5"
        )])));
        assert!(accepts_text(&headers(&[("accept", "*/*")])));
        assert!(!accepts_text(&headers(&[("accept", "application/json")])));
        assert!(!accepts_text(&headers(&[("accept", "text/html")])));
    }

    #[test]
    fn options_dijo_decides_are_ignored_and_unknown_ones_refused() {
        assert_eq!(
            check_headers(&headers(&[
                ("x-tika-ocrskipocr", "true"),
                ("x-tika-pdfocrstrategy", "no_ocr"),
                ("x-tika-pdfocrdpi", "150"),
                ("x-tika-ocrminfilesizetoocr", "100000"),
                ("x-tika-timeout-millis", "120000"),
                ("x-tika-ocrlanguage", "eng"),
                ("content-type", "application/pdf"),
            ])),
            Ok(())
        );
        assert_eq!(
            check_headers(&headers(&[("x-tika-pdfextractinlineimages", "true")])),
            Err("The Tika option x-tika-pdfextractinlineimages is not supported".to_string())
        );
    }

    #[test]
    fn the_ocr_language_is_read_and_checked() {
        assert_eq!(ocr_language(&headers(&[])), Ok(None));
        assert_eq!(
            ocr_language(&headers(&[("x-tika-ocrlanguage", "dan+eng")])),
            Ok(Some("dan+eng".to_string()))
        );
        assert!(ocr_language(&headers(&[("x-tika-ocrlanguage", "klingon")])).is_err());
    }
}
