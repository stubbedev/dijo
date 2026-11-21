//! tika-server's API, answered by dijo's extraction, so that a Tika client can be pointed at
//! dijo unchanged: the endpoints of tika-server 4.2, and the paths, `Accept` types and
//! metadata keys of tika-server 3 they replaced.

pub mod config;
mod iso639;
pub mod keys;
pub mod listings;
pub mod output;
mod xmp;

use hyper::header::{HeaderMap, ACCEPT};
use hyper::Method;

use crate::server::handlers::Crash;
use crate::server::upload::Parts;
use config::RequestConfig;
use keys::Keys;
use listings::Listing;
use output::{FieldForm, Handler, MetaForm, Output, Unpacked};

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
const JSON_RANGES: &[&str] = &["application/json"];
const XMP_RANGES: &[&str] = &["application/rdf+xml"];
/// The ranges that accept any content handler's answer.
const ANY_TEXT_RANGES: &[&str] = &["text/*", "*/*"];

/// An endpoint of tika-server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// Answered at once, without an upload.
    Listing(Listing),
    /// `PUT /tika`: the content as the request's `Accept` asks for it.
    Negotiated,
    /// `PUT /tika/{text,html,xml,md}`: the content, or tika-server 3's JSON object by `Accept`.
    Content(Handler),
    /// `PUT /meta`, in the form the request's `Accept` asks for.
    Meta,
    /// `PUT /meta/{field}`, in the form the request's `Accept` asks for.
    MetaField(String),
    /// Answered from an upload in a worker.
    Upload(Output),
    /// `GET /status`: what the server is doing.
    Status,
    /// An endpoint of a named preset. tika-server has presets only when configured with them,
    /// and dijo has none.
    Preset(String),
    /// Answered from the parts of a `multipart/form-data` upload.
    Multipart(Multipart),
}

/// A `multipart/form-data` endpoint: which parts it reads, and what it answers from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Multipart {
    pub parts: Parts,
    pub target: Target,
}

/// What a `multipart/form-data` endpoint answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `/tika/config[/{format}]`: the content.
    Content(Choice),
    /// `/tika/config/json[/{handler}]`: as `PUT /tika/json`.
    Json(Choice),
    /// `/rmeta/{form,config}[/{handler}]`: as `PUT /rmeta`.
    Rmeta(Choice),
    /// `/meta/{form,config}`: as `PUT /meta`; tika-server-standard answers XMP on `/meta/form`
    /// alone.
    Meta { xmp: bool },
    /// `/unpack`: as `PUT /unpack`.
    Unpack,
    /// `/unpack/all[/config][/{handler}]`: as `PUT /unpack/all`.
    UnpackAll(Choice),
}

/// The content handler of a `multipart/form-data` endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Named by the path, which a configuration may not name too.
    Named(Handler),
    /// Named by the configuration, if at all.
    Configured,
}

impl Choice {
    /// The handler, named by the path or the configuration, or the refusal of naming it in both.
    pub fn resolve(self, configured: Option<Handler>) -> Result<Option<Handler>, String> {
        match (self, configured) {
            (Choice::Named(_), Some(_)) => Err("The path names a content handler and so does the \
                 config part. Use one or the other."
                .to_string()),
            (Choice::Named(handler), None) => Ok(Some(handler)),
            (Choice::Configured, configured) => Ok(configured),
        }
    }
}

impl Endpoint {
    /// The name the endpoint is counted under in the metrics.
    pub fn label(&self) -> &'static str {
        match self {
            Endpoint::Listing(Listing::Hello) => "tika_hello",
            Endpoint::Listing(_) => "tika_listing",
            Endpoint::Negotiated | Endpoint::Content(_) => "tika",
            Endpoint::Meta | Endpoint::MetaField(_) => "tika_meta",
            Endpoint::Upload(Output::Json(..)) => "tika_json",
            Endpoint::Upload(Output::Rmeta(_)) => "tika_rmeta",
            Endpoint::Upload(Output::Meta(_) | Output::MetaField { .. }) => "tika_meta",
            Endpoint::Upload(Output::Unpack(_)) => "tika_unpack",
            Endpoint::Upload(Output::Detect) => "tika_detect",
            Endpoint::Upload(Output::Language) => "tika_language",
            Endpoint::Status => "tika_status",
            Endpoint::Preset(_) => "tika_preset",
            Endpoint::Multipart(multipart) => match multipart.target {
                Target::Content(_) => "tika",
                Target::Json(_) => "tika_json",
                Target::Rmeta(_) => "tika_rmeta",
                Target::Meta { .. } => "tika_meta",
                Target::Unpack | Target::UnpackAll(_) => "tika_unpack",
            },
        }
    }
}

/// The tika-server endpoint a request is for; None for a method and path tika-server does not
/// serve, or that dijo does not.
pub fn route(method: &Method, path: &str) -> Option<Endpoint> {
    let segments: Vec<&str> = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .split('/')
        .collect();
    let upload = |output| Some(Endpoint::Upload(output));
    let multipart = |parts, target| Some(Endpoint::Multipart(Multipart { parts, target }));
    let named = |name: &str| Some(Choice::Named(Handler::named(name)?));
    if method == Method::GET {
        let listing = match segments.as_slice() {
            [""] => Listing::Welcome,
            ["tika"] => Listing::Hello,
            ["version"] => Listing::Version,
            ["parsers"] => Listing::Parsers { details: false },
            ["parsers", "details"] => Listing::Parsers { details: true },
            ["detectors"] => Listing::Detectors,
            ["mime-types"] => Listing::MimeTypes,
            ["mime-types", kind, subtype] => Listing::MimeType(format!("{kind}/{subtype}")),
            ["status"] => return Some(Endpoint::Status),
            _ => return None,
        };
        return Some(Endpoint::Listing(listing));
    }
    if method == Method::POST {
        use Parts::{Config, Form};
        return match segments.as_slice() {
            ["language"] | ["language", "stream" | "string"] => upload(Output::Language),
            ["tika", "config"] => multipart(Config, Target::Content(Choice::Configured)),
            ["tika", "config", "text"] => {
                multipart(Config, Target::Content(Choice::Named(Handler::Body)))
            }
            ["tika", "config", format @ ("html" | "xml" | "md")] => {
                multipart(Config, Target::Content(named(format)?))
            }
            ["tika", "config", "json"] => multipart(Config, Target::Json(Choice::Configured)),
            ["tika", "config", "json", handler] => multipart(Config, Target::Json(named(handler)?)),
            ["rmeta", "form"] => multipart(Form, Target::Rmeta(Choice::Configured)),
            ["rmeta", "form", handler] => multipart(Form, Target::Rmeta(named(handler)?)),
            ["rmeta", "config"] => multipart(Config, Target::Rmeta(Choice::Configured)),
            ["rmeta", "config", handler] => multipart(Config, Target::Rmeta(named(handler)?)),
            ["meta", "form"] => multipart(Form, Target::Meta { xmp: true }),
            ["meta", "config"] => multipart(Config, Target::Meta { xmp: false }),
            ["unpack"] => multipart(Config, Target::Unpack),
            ["unpack", "all"] | ["unpack", "all", "config"] => {
                multipart(Config, Target::UnpackAll(Choice::Configured))
            }
            ["unpack", "all", handler] | ["unpack", "all", "config", handler] => {
                multipart(Config, Target::UnpackAll(named(handler)?))
            }
            _ => None,
        };
    }
    if method != Method::PUT {
        return None;
    }
    match segments.as_slice() {
        ["tika"] => Some(Endpoint::Negotiated),
        ["tika", "text"] => Some(Endpoint::Content(Handler::Text)),
        ["tika", "html"] => Some(Endpoint::Content(Handler::Html)),
        ["tika", "xml"] => Some(Endpoint::Content(Handler::Xml)),
        ["tika", "md"] => Some(Endpoint::Content(Handler::Markdown)),
        ["tika", "json"] => upload(Output::Json(Handler::Markdown, Keys::Tika4)),
        ["tika", "json", handler] => upload(Output::Json(Handler::named(handler)?, Keys::Tika4)),
        ["tika", "preset", name, ..] | ["rmeta" | "unpack", "preset", name, ..] => {
            Some(Endpoint::Preset(name.to_string()))
        }
        ["rmeta"] => upload(Output::Rmeta(Handler::Markdown)),
        ["rmeta", handler] => upload(Output::Rmeta(Handler::named(handler)?)),
        ["meta"] => Some(Endpoint::Meta),
        ["meta", field] => Some(Endpoint::MetaField(field.to_string())),
        ["detect"] | ["detect", "stream"] => upload(Output::Detect),
        ["language"] | ["language", "stream" | "string"] => upload(Output::Language),
        ["unpack"] => upload(Output::Unpack(Unpacked::Embedded)),
        ["unpack", "all"] => upload(Output::Unpack(Unpacked::All(None))),
        ["unpack", "all", handler] => upload(Output::Unpack(Unpacked::All(Some(Handler::named(
            handler,
        )?)))),
        _ => None,
    }
}

/// What a `multipart/form-data` endpoint answers, once its configuration is read.
pub enum Resolved {
    /// The content as the handler writes it, labelled with the media type.
    Content(Handler, &'static str),
    Output(Output),
}

/// What `target` answers given the request's configuration, or why it is refused.
pub fn resolve(
    target: Target,
    config: &RequestConfig,
    headers: &HeaderMap,
) -> Result<Resolved, (u16, String)> {
    let handler = |choice: Choice| {
        choice
            .resolve(config.handler)
            .map_err(|message| (400, message))
    };
    Ok(match target {
        Target::Content(choice) => {
            let label = match choice {
                Choice::Named(handler) => content_media_type(handler),
                Choice::Configured => content_media_type(Handler::Text),
            };
            Resolved::Content(handler(choice)?.unwrap_or(Handler::Markdown), label)
        }
        Target::Json(choice) => Resolved::Output(Output::Json(
            handler(choice)?.unwrap_or(Handler::Markdown),
            Keys::Tika4,
        )),
        Target::Rmeta(choice) => {
            Resolved::Output(Output::Rmeta(handler(choice)?.unwrap_or(Handler::Markdown)))
        }
        Target::Meta { xmp } => Resolved::Output(Output::Meta(
            meta_form(headers, xmp)
                .ok_or((406, "No form of this endpoint is acceptable".to_string()))?,
        )),
        Target::Unpack => Resolved::Output(Output::Unpack(Unpacked::Embedded)),
        Target::UnpackAll(choice) => {
            Resolved::Output(Output::Unpack(Unpacked::All(handler(choice)?)))
        }
    })
}

/// The body tika-server answers with when its forked parser dies, which clients read the
/// cause from.
pub fn crash_body(crash: Crash) -> String {
    let status = match crash {
        Crash::Timeout => "TIMEOUT",
        Crash::OutOfMemory => "OOM",
        Crash::Unspecified => "UNSPECIFIED_CRASH",
    };
    serde_json::json!({ "status": status }).to_string()
}

/// The media ranges of the request's `Accept`, in the order it lists them, lowercase and
/// without their parameters.
fn media_ranges(headers: &HeaderMap) -> impl Iterator<Item = String> + '_ {
    headers
        .get_all(ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|range| {
            range
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
}

/// What `Accept` picks out of `forms`, by the first range it lists that one of them matches;
/// the first form when there is no `Accept`.
fn negotiate<T: Copy>(headers: &HeaderMap, forms: &[(&[&str], T)]) -> Option<T> {
    if headers.get(ACCEPT).is_none() {
        return forms.first().map(|(_, form)| *form);
    }
    media_ranges(headers).find_map(|range| {
        forms
            .iter()
            .find(|(ranges, _)| ranges.contains(&range.as_str()))
            .map(|(_, form)| *form)
    })
}

/// What `PUT /tika` and `PUT /tika/{handler}` answer with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The content as the handler writes it.
    Content(Handler),
    /// One JSON object of the metadata in tika-server 3's keys and the content as the handler
    /// writes it, as tika-server 3 answered `application/json`.
    Json(Handler),
}

/// The media ranges that name the content `handler` writes.
fn handler_ranges(handler: Handler) -> &'static [&'static str] {
    match handler {
        Handler::Text | Handler::Body | Handler::Ignore => &["text/plain"],
        Handler::Markdown => &["text/plain", "text/markdown"],
        Handler::Html => &["text/html", "application/xhtml+xml"],
        Handler::Xml => &["text/xml", "application/xml"],
    }
}

/// What `PUT /tika` answers with: plain text, as tika-server 4 answers alone, or what
/// tika-server 3 answered `text/html`, `text/xml` and `application/json` with: the XHTML, the
/// XML, or the JSON object of its default XML handler.
pub fn negotiated_answer(headers: &HeaderMap) -> Option<Answer> {
    let text = [handler_ranges(Handler::Text), ANY_TEXT_RANGES].concat();
    negotiate(
        headers,
        &[
            (text.as_slice(), Answer::Content(Handler::Text)),
            (
                handler_ranges(Handler::Html),
                Answer::Content(Handler::Html),
            ),
            (handler_ranges(Handler::Xml), Answer::Content(Handler::Xml)),
            (JSON_RANGES, Answer::Json(Handler::Xml)),
        ],
    )
}

/// What `PUT /tika/{handler}` answers with: the content, as tika-server 4 answers alone, or
/// the JSON object tika-server 3 answered `application/json` with.
pub fn content_answer(headers: &HeaderMap, handler: Handler) -> Option<Answer> {
    let content = [handler_ranges(handler), ANY_TEXT_RANGES].concat();
    negotiate(
        headers,
        &[
            (content.as_slice(), Answer::Content(handler)),
            (JSON_RANGES, Answer::Json(handler)),
        ],
    )
}

/// The form `/meta` answers in by `Accept`; XMP where tika-server-standard answers it.
pub fn meta_form(headers: &HeaderMap, xmp: bool) -> Option<MetaForm> {
    let forms: &[(&[&str], MetaForm)] = &[
        (&["application/json", "*/*"], MetaForm::Json),
        (&["text/csv"], MetaForm::Csv),
        (XMP_RANGES, MetaForm::Xmp),
    ];
    negotiate(headers, if xmp { forms } else { &forms[..2] })
}

pub fn field_form(headers: &HeaderMap) -> Option<FieldForm> {
    negotiate(
        headers,
        &[
            (&["application/json", "*/*"], FieldForm::Json),
            (&["text/csv"], FieldForm::Csv),
            (&["text/plain"], FieldForm::Plain),
            (XMP_RANGES, FieldForm::Xmp),
        ],
    )
}

/// The media type tika-server labels content of `handler` with.
pub fn content_media_type(handler: Handler) -> &'static str {
    match handler {
        Handler::Text | Handler::Body | Handler::Markdown | Handler::Ignore => {
            "text/plain; charset=utf-8"
        }
        Handler::Html => "text/html; charset=utf-8",
        Handler::Xml => "text/xml; charset=utf-8",
    }
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
    fn paths_route_to_tika_servers_endpoints() {
        let put = |path| route(&Method::PUT, path);
        assert_eq!(put("/tika"), Some(Endpoint::Negotiated));
        assert_eq!(put("/tika/md"), Some(Endpoint::Content(Handler::Markdown)));
        assert_eq!(put("/tika/ignore"), None);
        assert_eq!(
            put("/tika/json"),
            Some(Endpoint::Upload(Output::Json(
                Handler::Markdown,
                Keys::Tika4
            )))
        );
        assert_eq!(
            put("/tika/json/xml"),
            Some(Endpoint::Upload(Output::Json(Handler::Xml, Keys::Tika4)))
        );
        assert_eq!(
            put("/rmeta/text"),
            Some(Endpoint::Upload(Output::Rmeta(Handler::Text)))
        );
        assert_eq!(put("/rmeta/bogus"), None);
        assert_eq!(
            put("/meta/Content-Type"),
            Some(Endpoint::MetaField("Content-Type".to_string()))
        );
        assert_eq!(
            put("/detect/stream"),
            Some(Endpoint::Upload(Output::Detect))
        );
        assert_eq!(
            put("/unpack/"),
            Some(Endpoint::Upload(Output::Unpack(Unpacked::Embedded)))
        );
        assert_eq!(
            put("/unpack/all/TEXT"),
            Some(Endpoint::Upload(Output::Unpack(Unpacked::All(Some(
                Handler::Text
            )))))
        );
        assert_eq!(
            put("/rmeta/body"),
            Some(Endpoint::Upload(Output::Rmeta(Handler::Body)))
        );
        assert_eq!(
            put("/tika/preset/fast/json/text"),
            Some(Endpoint::Preset("fast".to_string()))
        );
        assert_eq!(
            put("/unpack/preset/fast/all"),
            Some(Endpoint::Preset("fast".to_string()))
        );
        assert_eq!(
            route(&Method::GET, "/"),
            Some(Endpoint::Listing(Listing::Welcome))
        );
        assert_eq!(route(&Method::GET, "/status"), Some(Endpoint::Status));
        assert_eq!(route(&Method::POST, "/pipes"), None);
        assert_eq!(route(&Method::POST, "/async"), None);
        assert_eq!(
            route(&Method::POST, "/language/string"),
            Some(Endpoint::Upload(Output::Language))
        );
        assert_eq!(route(&Method::POST, "/tika"), None);
        assert_eq!(
            route(&Method::GET, "/mime-types/application/pdf"),
            Some(Endpoint::Listing(Listing::MimeType(
                "application/pdf".to_string()
            )))
        );
        assert_eq!(route(&Method::GET, "/extract"), None);
    }

    #[test]
    fn multipart_paths_route_to_their_parts_and_targets() {
        let post = |path| match route(&Method::POST, path) {
            Some(Endpoint::Multipart(Multipart { parts, target })) => Some((parts, target)),
            _ => None,
        };
        assert_eq!(
            post("/tika/config"),
            Some((Parts::Config, Target::Content(Choice::Configured)))
        );
        assert_eq!(
            post("/tika/config/text"),
            Some((Parts::Config, Target::Content(Choice::Named(Handler::Body))))
        );
        assert_eq!(
            post("/tika/config/json/text"),
            Some((Parts::Config, Target::Json(Choice::Named(Handler::Text))))
        );
        assert_eq!(post("/tika/config/body"), None);
        assert_eq!(
            post("/rmeta/form/ignore"),
            Some((Parts::Form, Target::Rmeta(Choice::Named(Handler::Ignore))))
        );
        assert_eq!(
            post("/meta/form"),
            Some((Parts::Form, Target::Meta { xmp: true }))
        );
        assert_eq!(
            post("/meta/config"),
            Some((Parts::Config, Target::Meta { xmp: false }))
        );
        assert_eq!(post("/unpack"), Some((Parts::Config, Target::Unpack)));
        assert_eq!(
            post("/unpack/all/config"),
            Some((Parts::Config, Target::UnpackAll(Choice::Configured)))
        );
        assert_eq!(
            post("/unpack/all/config/md"),
            Some((
                Parts::Config,
                Target::UnpackAll(Choice::Named(Handler::Markdown))
            ))
        );
        assert_eq!(post("/rmeta/config/bogus"), None);
    }

    #[test]
    fn a_handler_is_named_by_the_path_or_the_config_but_not_both() {
        let config = |handler| RequestConfig {
            handler,
            ocr_language: None,
        };
        let resolved = |target, handler| match resolve(target, &config(handler), &headers(&[])) {
            Ok(Resolved::Content(handler, label)) => Ok(format!("{handler:?} {label}")),
            Ok(Resolved::Output(output)) => Ok(format!("{output:?}")),
            Err((status, _)) => Err(status),
        };
        assert_eq!(
            resolved(Target::Content(Choice::Configured), None),
            Ok("Markdown text/plain; charset=utf-8".to_string())
        );
        assert_eq!(
            resolved(Target::Content(Choice::Configured), Some(Handler::Xml)),
            Ok("Xml text/plain; charset=utf-8".to_string())
        );
        assert_eq!(
            resolved(Target::Content(Choice::Named(Handler::Html)), None),
            Ok("Html text/html; charset=utf-8".to_string())
        );
        assert_eq!(
            resolved(
                Target::Json(Choice::Named(Handler::Text)),
                Some(Handler::Xml)
            ),
            Err(400)
        );
        assert_eq!(
            resolved(Target::UnpackAll(Choice::Configured), None),
            Ok("Unpack(All(None))".to_string())
        );
    }

    #[test]
    fn a_dead_parser_is_reported_as_tika_server_reports_it() {
        assert_eq!(crash_body(Crash::Timeout), r#"{"status":"TIMEOUT"}"#);
        assert_eq!(crash_body(Crash::OutOfMemory), r#"{"status":"OOM"}"#);
        assert_eq!(
            crash_body(Crash::Unspecified),
            r#"{"status":"UNSPECIFIED_CRASH"}"#
        );
    }

    #[test]
    fn accept_picks_the_first_form_it_lists() {
        assert_eq!(
            negotiated_answer(&headers(&[])),
            Some(Answer::Content(Handler::Text))
        );
        assert_eq!(
            negotiated_answer(&headers(&[("accept", "text/plain")])),
            Some(Answer::Content(Handler::Text))
        );
        assert_eq!(
            negotiated_answer(&headers(&[("accept", "image/png, Text/HTML; q=0.5")])),
            Some(Answer::Content(Handler::Html))
        );
        assert_eq!(
            negotiated_answer(&headers(&[("accept", "text/xml, */*")])),
            Some(Answer::Content(Handler::Xml))
        );
        assert_eq!(
            negotiated_answer(&headers(&[("accept", "application/json")])),
            Some(Answer::Json(Handler::Xml))
        );
        assert_eq!(
            negotiated_answer(&headers(&[("accept", "image/png")])),
            None
        );
        assert_eq!(
            meta_form(&headers(&[("accept", "text/csv")]), false),
            Some(MetaForm::Csv)
        );
        assert_eq!(meta_form(&headers(&[("accept", "text/plain")]), true), None);
        assert_eq!(
            meta_form(&headers(&[("accept", "application/rdf+xml")]), true),
            Some(MetaForm::Xmp)
        );
        assert_eq!(
            meta_form(&headers(&[("accept", "application/rdf+xml")]), false),
            None
        );
        assert_eq!(
            field_form(&headers(&[("accept", "text/plain")])),
            Some(FieldForm::Plain)
        );
    }

    #[test]
    fn content_endpoints_answer_tika_3s_json_object_by_accept() {
        for handler in [
            Handler::Text,
            Handler::Html,
            Handler::Xml,
            Handler::Markdown,
        ] {
            assert_eq!(
                content_answer(&headers(&[]), handler),
                Some(Answer::Content(handler))
            );
            assert_eq!(
                content_answer(&headers(&[("accept", "application/json")]), handler),
                Some(Answer::Json(handler))
            );
            assert_eq!(
                content_answer(&headers(&[("accept", "*/*, application/json")]), handler),
                Some(Answer::Content(handler))
            );
            assert_eq!(
                content_answer(&headers(&[("accept", "image/png")]), handler),
                None
            );
        }
        assert_eq!(
            content_answer(
                &headers(&[("accept", "application/xhtml+xml")]),
                Handler::Html
            ),
            Some(Answer::Content(Handler::Html))
        );
        assert_eq!(
            content_answer(&headers(&[("accept", "text/html")]), Handler::Text),
            None
        );
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
