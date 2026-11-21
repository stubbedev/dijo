//! tika-server's endpoints that answer without an upload: its welcome and greeting, its version
//! and what it detects and parses.

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

const HTML: &str = "text/html; charset=utf-8";
const DOCS: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "#tika-servers-api");
const ZIP: &[&str] = &["application/zip"];
const JSON_ONLY: &[&str] = &["application/json"];
const TEXT_ONLY: &[&str] = &["text/plain"];
const META: &[&str] = &["application/json", "text/csv", "application/rdf+xml"];
const LISTED: &[&str] = &["application/json", "text/plain"];
/// The endpoints of tika-server's API dijo serves: method, path and the media types answered.
const ENDPOINTS: &[(&str, &str, &[&str])] = &[
    ("GET", "/", &["text/html", "text/plain"]),
    ("GET", "/tika", TEXT_ONLY),
    (
        "PUT",
        "/tika",
        &["text/plain", "text/html", "text/xml", "application/json"],
    ),
    ("PUT", "/tika/text", &["text/plain", "application/json"]),
    ("PUT", "/tika/html", &["text/html", "application/json"]),
    ("PUT", "/tika/xml", &["text/xml", "application/json"]),
    ("PUT", "/tika/md", &["text/plain", "application/json"]),
    ("PUT", "/tika/json", JSON_ONLY),
    ("PUT", "/tika/json/{handler}", JSON_ONLY),
    ("POST", "/tika/config", TEXT_ONLY),
    ("POST", "/tika/config/text", TEXT_ONLY),
    ("POST", "/tika/config/html", &["text/html"]),
    ("POST", "/tika/config/xml", &["text/xml"]),
    ("POST", "/tika/config/md", TEXT_ONLY),
    ("POST", "/tika/config/json", JSON_ONLY),
    ("POST", "/tika/config/json/{handler}", JSON_ONLY),
    ("PUT", "/rmeta", JSON_ONLY),
    ("PUT", "/rmeta/{handler}", JSON_ONLY),
    ("POST", "/rmeta/form", JSON_ONLY),
    ("POST", "/rmeta/form/{handler}", JSON_ONLY),
    ("POST", "/rmeta/config", JSON_ONLY),
    ("POST", "/rmeta/config/{handler}", JSON_ONLY),
    ("PUT", "/meta", META),
    (
        "PUT",
        "/meta/{field}",
        &[
            "application/json",
            "text/csv",
            "text/plain",
            "application/rdf+xml",
        ],
    ),
    ("POST", "/meta/form", META),
    ("POST", "/meta/config", &["application/json", "text/csv"]),
    ("PUT", "/unpack", ZIP),
    ("POST", "/unpack", ZIP),
    ("PUT", "/unpack/all", ZIP),
    ("PUT", "/unpack/all/{handler}", ZIP),
    ("POST", "/unpack/all", ZIP),
    ("POST", "/unpack/all/{handler}", ZIP),
    ("POST", "/unpack/all/config", ZIP),
    ("POST", "/unpack/all/config/{handler}", ZIP),
    ("PUT", "/detect/stream", TEXT_ONLY),
    ("PUT", "/language/stream", TEXT_ONLY),
    ("POST", "/language/stream", TEXT_ONLY),
    ("PUT", "/language/string", TEXT_ONLY),
    ("POST", "/language/string", TEXT_ONLY),
    ("GET", "/version", TEXT_ONLY),
    ("GET", "/parsers", LISTED),
    ("GET", "/parsers/details", LISTED),
    ("GET", "/detectors", LISTED),
    ("GET", "/mime-types", LISTED),
    ("GET", "/mime-types/{type}/{subtype}", JSON_ONLY),
    ("GET", "/status", JSON_ONLY),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Welcome,
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
        Listing::Welcome => welcome(headers),
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

/// What tika-server answers `GET /` with: the endpoints it serves, as HTML or, when `Accept`
/// names plain text before HTML, as plain text.
fn welcome(headers: &HeaderMap) -> (u16, &'static str, String) {
    let plain = media_ranges(headers)
        .find_map(|range| match range.as_str() {
            "text/plain" => Some(true),
            "text/html" | "text/*" | "*/*" => Some(false),
            _ => None,
        })
        .unwrap_or(false);
    let title = format!("dijo {}", env!("CARGO_PKG_VERSION"));
    if plain {
        let mut text = format!("{title}\nFor endpoints, please see {DOCS}\n\n");
        for (method, path, produces) in ENDPOINTS {
            let _ = writeln!(text, "{method} {path}");
            for media_type in *produces {
                let _ = writeln!(text, " => {media_type}");
            }
        }
        return (200, PLAIN, text);
    }
    let mut html = format!(
        "<html><head><title>Welcome to the {title} Server</title></head><body>\
         <h1>Welcome to the {title} Server</h1>\
         <p>For endpoints, please see <a href=\"{DOCS}\">{DOCS}</a></p>\n<ul>\n"
    );
    for (method, path, produces) in ENDPOINTS {
        let shown = if path.contains('{') {
            path.to_string()
        } else {
            format!("<a href=\"{path}\">{path}</a>")
        };
        let _ = write!(html, "<li><b>{method}</b> <i>{shown}</i>");
        for media_type in *produces {
            let _ = write!(html, "<br />Produces: {media_type}");
        }
        html.push_str("</li>\n");
    }
    html.push_str("</ul>\n</body></html>\n");
    (200, HTML, html)
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
