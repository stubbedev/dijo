//! tika-server's API, answered by dijo's extraction, so that a Tika client can be pointed at
//! dijo unchanged: the endpoints of tika-server 4.1, and the paths, `Accept` types and
//! metadata keys of tika-server 3 they replaced.

mod iso639;
pub mod keys;
pub mod listings;
pub mod output;

use hyper::header::{HeaderMap, ACCEPT};
use hyper::Method;

use keys::Keys;
use listings::Listing;
use output::{FieldForm, Handler, MetaForm, Output};

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
            Endpoint::Upload(Output::Unpack { .. }) => "tika_unpack",
            Endpoint::Upload(Output::Detect) => "tika_detect",
            Endpoint::Upload(Output::Language) => "tika_language",
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
    if method == Method::GET {
        let listing = match segments.as_slice() {
            ["tika"] => Listing::Hello,
            ["version"] => Listing::Version,
            ["parsers"] => Listing::Parsers { details: false },
            ["parsers", "details"] => Listing::Parsers { details: true },
            ["detectors"] => Listing::Detectors,
            ["mime-types"] => Listing::MimeTypes,
            ["mime-types", kind, subtype] => Listing::MimeType(format!("{kind}/{subtype}")),
            _ => return None,
        };
        return Some(Endpoint::Listing(listing));
    }
    if method == Method::POST {
        return match segments.as_slice() {
            ["language"] | ["language", "stream" | "string"] => upload(Output::Language),
            _ => None,
        };
    }
    if method != Method::PUT {
        return None;
    }
    match segments.as_slice() {
        ["tika"] => Some(Endpoint::Negotiated),
        ["tika", "json"] => upload(Output::Json(Handler::Markdown, Keys::Tika4)),
        ["tika", "json", handler] => upload(Output::Json(Handler::named(handler)?, Keys::Tika4)),
        ["tika", handler] => match Handler::named(handler)? {
            Handler::Ignore => None,
            handler => Some(Endpoint::Content(handler)),
        },
        ["rmeta"] => upload(Output::Rmeta(Handler::Markdown)),
        ["rmeta", handler] => upload(Output::Rmeta(Handler::named(handler)?)),
        ["meta"] => Some(Endpoint::Meta),
        ["meta", field] => Some(Endpoint::MetaField(field.to_string())),
        ["detect"] | ["detect", "stream"] => upload(Output::Detect),
        ["language"] | ["language", "stream" | "string"] => upload(Output::Language),
        ["unpack"] => upload(Output::Unpack { all: false }),
        ["unpack", "all"] => upload(Output::Unpack { all: true }),
        _ => None,
    }
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
        Handler::Text | Handler::Ignore => &["text/plain"],
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

pub fn meta_form(headers: &HeaderMap) -> Option<MetaForm> {
    negotiate(
        headers,
        &[
            (&["application/json", "*/*"], MetaForm::Json),
            (&["text/csv"], MetaForm::Csv),
        ],
    )
}

pub fn field_form(headers: &HeaderMap) -> Option<FieldForm> {
    negotiate(
        headers,
        &[
            (&["application/json", "*/*"], FieldForm::Json),
            (&["text/csv"], FieldForm::Csv),
            (&["text/plain"], FieldForm::Plain),
        ],
    )
}

/// The media type tika-server labels content of `handler` with.
pub fn content_media_type(handler: Handler) -> &'static str {
    match handler {
        Handler::Text | Handler::Markdown | Handler::Ignore => "text/plain; charset=utf-8",
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
            Some(Endpoint::Upload(Output::Unpack { all: false }))
        );
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
            meta_form(&headers(&[("accept", "text/csv")])),
            Some(MetaForm::Csv)
        );
        assert_eq!(meta_form(&headers(&[("accept", "text/plain")])), None);
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
