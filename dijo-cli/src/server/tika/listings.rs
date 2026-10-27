//! tika-server's endpoints that answer without an upload: its greeting, its version and what it
//! detects and parses.

use std::fmt::Write as _;

use dijo_core::Declared;
use hyper::header::HeaderMap;
use serde_json::{json, Map, Value};

use super::media_ranges;

const PLAIN: &str = "text/plain; charset=utf-8";
const JSON: &str = "application/json";
const PARSER: &str = "dijo::DefaultParser";
const DETECTOR: &str = "dijo::DefaultDetector";
/// The detectors dijo's detection ports, in the order it runs them, named as its parsers are.
const DETECTORS: [&str; 8] = [
    "dijo::BPListDetector",
    "dijo::GZipSpecializationDetector",
    "dijo::RawTiffDetector",
    "dijo::POIFSContainerDetector",
    "dijo::MatroskaDetector",
    "dijo::OggDetector",
    "dijo::MiscOLEDetector",
    "dijo::DefaultZipContainerDetector",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Hello,
    Version,
    Parsers { details: bool },
    Detectors,
    MimeTypes,
    MimeType(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Json,
    Plain,
}

/// The status, media type and body tika-server answers a listing with.
pub fn answer(listing: &Listing, headers: &HeaderMap) -> (u16, &'static str, String) {
    match listing {
        Listing::Hello => (200, PLAIN, hello()),
        Listing::Version => (200, PLAIN, format!("dijo {}", env!("CARGO_PKG_VERSION"))),
        Listing::Parsers { details } => listed(headers, |form| parsers(form, *details)),
        Listing::Detectors => listed(headers, detectors),
        Listing::MimeTypes => listed(headers, mime_types),
        Listing::MimeType(name) => match dijo_core::declared_type(name) {
            Some(declared) => (200, JSON, mime_type(&declared).to_string()),
            None => (
                404,
                JSON,
                json!({ "error": format!("No such type {name}") }).to_string(),
            ),
        },
    }
}

/// What tika-server answers `GET /tika` with, which clients check it is up by.
pub fn hello() -> String {
    format!(
        "This is dijo {}, serving tika-server's API. Please PUT\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// A listing as JSON or plain text, whichever `Accept` names first; plain text when it names
/// neither, such as a browser's.
fn listed(headers: &HeaderMap, body: impl Fn(Form) -> String) -> (u16, &'static str, String) {
    let form = media_ranges(headers)
        .find_map(|range| match range.as_str() {
            "application/json" => Some(Form::Json),
            "text/plain" | "text/*" | "*/*" => Some(Form::Plain),
            _ => None,
        })
        .unwrap_or(Form::Plain);
    let media_type = match form {
        Form::Json => JSON,
        Form::Plain => PLAIN,
    };
    (200, media_type, body(form))
}

fn composite(name: &str, children: Vec<Value>, extra: &[(&str, Value)]) -> Value {
    let mut object = Map::from_iter([
        ("name".to_string(), Value::from(name)),
        ("composite".to_string(), Value::from(true)),
        ("children".to_string(), Value::from(children)),
    ]);
    object.extend(
        extra
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone())),
    );
    Value::Object(object)
}

fn parsers(form: Form, details: bool) -> String {
    let parsers: Vec<dijo_core::ListedParser> = dijo_core::parsers().collect();
    match form {
        Form::Json => composite(
            PARSER,
            parsers
                .iter()
                .map(|parser| {
                    let mut child = json!({
                        "name": parser.name,
                        "composite": false,
                        "decorated": false,
                    });
                    if details {
                        child["supportedTypes"] = Value::from(parser.media_types);
                    }
                    child
                })
                .collect(),
            &[("decorated", Value::from(false))],
        )
        .to_string(),
        Form::Plain => {
            let mut plain = format!("{PARSER} (Composite Parser):\n");
            for parser in &parsers {
                let _ = writeln!(plain, "  {}", parser.name);
                if details {
                    for media_type in parser.media_types {
                        let _ = writeln!(plain, "    Supports: {media_type}");
                    }
                }
            }
            plain
        }
    }
}

fn detectors(form: Form) -> String {
    match form {
        Form::Json => composite(
            DETECTOR,
            DETECTORS
                .iter()
                .map(|name| json!({ "name": name, "composite": false }))
                .collect(),
            &[],
        )
        .to_string(),
        Form::Plain => {
            let mut plain = format!("{DETECTOR} (Composite Detector):\n");
            for name in DETECTORS {
                let _ = writeln!(plain, "  {name}");
            }
            plain
        }
    }
}

fn mime_types(form: Form) -> String {
    let declared = dijo_core::declared_types();
    match form {
        Form::Json => Value::Object(
            declared
                .iter()
                .map(|declared| {
                    let mut entry = json!({ "alias": declared.aliases });
                    if let Some(supertype) = &declared.supertype {
                        entry["supertype"] = Value::from(supertype.as_str());
                    }
                    if let Some(parser) = dijo_core::parser_name(declared.name) {
                        entry["parser"] = Value::from(parser);
                    }
                    (declared.name.to_string(), entry)
                })
                .collect(),
        )
        .to_string(),
        Form::Plain => {
            let mut plain = String::new();
            for declared in &declared {
                let _ = writeln!(plain, "{}", declared.name);
                for alias in &declared.aliases {
                    let _ = writeln!(plain, "  alias:     {alias}");
                }
                if let Some(supertype) = &declared.supertype {
                    let _ = writeln!(plain, "  supertype: {}", supertype.as_str());
                }
                if let Some(parser) = dijo_core::parser_name(declared.name) {
                    let _ = writeln!(plain, "  parser:    {parser}");
                }
            }
            plain
        }
    }
}

fn mime_type(declared: &Declared) -> Value {
    let mut entry = json!({
        "type": declared.name,
        "alias": declared.aliases,
        "extensions": declared.extensions,
    });
    if let Some(supertype) = &declared.supertype {
        entry["supertype"] = Value::from(supertype.as_str());
    }
    if let Some(parser) = dijo_core::parser_name(declared.name) {
        entry["parser"] = Value::from(parser);
    }
    if let Some(extension) = declared.extensions.first() {
        entry["defaultExtension"] = Value::from(*extension);
    }
    entry
}
