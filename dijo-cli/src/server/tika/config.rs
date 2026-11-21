//! The `config` part of tika-server's `/config` uploads: a parse context in Tika's JSON, of
//! which dijo applies the content handler and the tesseract languages, and decides the rest by
//! itself.

use serde_json::{Map, Value};

use super::output::Handler;

const HANDLER_FACTORY: &str = "basic-content-handler-factory";
const HANDLER_TYPE: &str = "type";
const TESSERACT: &str = "tesseract-ocr-parser";
const TESSERACT_LANGUAGE: &str = "language";
/// The components whose settings tune when and how Tika OCRs a PDF or an image and when it
/// gives up, which dijo decides by itself: it OCRs only the images and pages without usable
/// text, and stops at `--timeout`.
const DECIDED_BY_DIJO: &[&str] = &["pdf-parser", "timeout-limits"];

/// What a request's configuration asks of the extraction.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RequestConfig {
    pub handler: Option<Handler>,
    pub ocr_language: Option<String>,
}

/// The configuration a `config` part holds, or why it is refused.
pub fn parse(json: &[u8]) -> Result<RequestConfig, String> {
    let root: Value = serde_json::from_slice(json)
        .map_err(|error| format!("Could not resolve the request config: {error}"))?;
    let Value::Object(components) = root else {
        return Err("Could not resolve the request config: it is not a JSON object".to_string());
    };
    let mut config = RequestConfig::default();
    for (name, settings) in &components {
        let settings = settings.as_object().ok_or_else(|| {
            format!("Could not resolve the request config: {name} is not a JSON object")
        })?;
        match name.as_str() {
            HANDLER_FACTORY => config.handler = Some(handler(settings)?),
            TESSERACT => config.ocr_language = ocr_language(settings)?,
            name if DECIDED_BY_DIJO.contains(&name) => {}
            name => return Err(format!("The Tika config component {name} is not supported")),
        }
    }
    Ok(config)
}

fn handler(settings: &Map<String, Value>) -> Result<Handler, String> {
    if let Some(key) = settings.keys().find(|key| *key != HANDLER_TYPE) {
        return Err(format!(
            "The setting {key} of {HANDLER_FACTORY} is not supported"
        ));
    }
    let name = settings
        .get(HANDLER_TYPE)
        .ok_or_else(|| format!("{HANDLER_FACTORY} names no {HANDLER_TYPE}"))?
        .as_str()
        .ok_or_else(|| format!("The {HANDLER_TYPE} of {HANDLER_FACTORY} is not a string"))?;
    Handler::named(name).map_err(|unknown| unknown.to_string())
}

/// The tesseract languages `settings` name; the rest of tesseract's settings tune its OCR,
/// which dijo decides by itself.
fn ocr_language(settings: &Map<String, Value>) -> Result<Option<String>, String> {
    settings
        .get(TESSERACT_LANGUAGE)
        .map(|language| {
            language
                .as_str()
                .ok_or_else(|| format!("The {TESSERACT_LANGUAGE} of {TESSERACT} is not a string"))
                .and_then(crate::settings::ocr_language)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configurations_kontainer_sends_are_read() {
        assert_eq!(
            parse(
                br#"{"pdf-parser": {"pages": {"text": "EXTRACT"}},
                     "timeout-limits": {"totalTaskTimeoutMillis": 120000, "progressTimeoutMillis": 120000}}"#
            ),
            Ok(RequestConfig::default())
        );
        assert_eq!(
            parse(
                br#"{"tesseract-ocr-parser": {"language": "eng+dan"}, "pdf-parser": {"pages": {"render": {"dpi": 150}}}}"#
            ),
            Ok(RequestConfig {
                handler: None,
                ocr_language: Some("eng+dan".to_string()),
            })
        );
        assert_eq!(
            parse(br#"{"tesseract-ocr-parser": {"skipOcr": true}}"#),
            Ok(RequestConfig::default())
        );
        assert_eq!(
            parse(br#"{"tesseract-ocr-parser": {"language": "dan", "minFileSizeToOcr": 100000}}"#),
            Ok(RequestConfig {
                handler: None,
                ocr_language: Some("dan".to_string()),
            })
        );
    }

    #[test]
    fn the_content_handler_is_named_as_tika_names_it() {
        assert_eq!(
            parse(br#"{"basic-content-handler-factory": {"type": "MARKDOWN"}}"#),
            Ok(RequestConfig {
                handler: Some(Handler::Markdown),
                ocr_language: None,
            })
        );
        assert_eq!(
            parse(br#"{"basic-content-handler-factory": {"type": "body"}}"#)
                .map(|config| config.handler),
            Ok(Some(Handler::Body))
        );
        assert!(parse(br#"{"basic-content-handler-factory": {"type": "bogus"}}"#).is_err());
        assert!(parse(br#"{"basic-content-handler-factory": {"writeLimit": 5}}"#).is_err());
    }

    #[test]
    fn what_dijo_cannot_honour_is_refused() {
        assert_eq!(
            parse(br#"{"unpack-config": {}}"#),
            Err("The Tika config component unpack-config is not supported".to_string())
        );
        assert!(parse(br#"{"tesseract-ocr-parser": {"language": "klingon"}}"#).is_err());
        assert!(parse(br#"{"pdf-parser": 1}"#).is_err());
        assert!(parse(b"[]").is_err());
        assert!(parse(b"{").is_err());
    }
}
