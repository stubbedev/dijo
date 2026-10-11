use std::str::FromStr;

use extractous_core::Metadata;
use serde::Serialize;

pub const SCHEMA_VERSION: u32 = 1;

/// Which Tika metadata keys feed each normalised field, in order of preference: the first key
/// present wins.
struct TikaKeys {
    title: &'static [&'static str],
    authors: &'static [&'static str],
    created: &'static [&'static str],
    modified: &'static [&'static str],
    language: &'static [&'static str],
    page_count: &'static [&'static str],
    encrypted: &'static [&'static str],
    password_used: &'static [&'static str],
    content_type: &'static [&'static str],
}

const TIKA_KEYS: TikaKeys = TikaKeys {
    title: &["dc:title"],
    authors: &["dc:creator", "meta:author", "Author"],
    created: &[
        "dcterms:created",
        "meta:creation-date",
        "pdf:docinfo:created",
        "Creation-Date",
    ],
    modified: &[
        "dcterms:modified",
        "Last-Modified",
        "meta:save-date",
        "pdf:docinfo:modified",
    ],
    language: &["dc:language"],
    page_count: &["xmpTPg:NPages", "meta:page-count", "Page-Count"],
    encrypted: &["pdf:encrypted"],
    password_used: &["extractous:password-used"],
    content_type: &["Content-Type"],
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Complete,
    Partial,
    Encrypted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCategory {
    Unreadable,
    Corrupt,
    Encrypted,
    Unsupported,
    LimitExceeded,
    Timeout,
    Internal,
}

impl ErrorCategory {
    pub fn name(self) -> &'static str {
        match self {
            ErrorCategory::Unreadable => "unreadable",
            ErrorCategory::Corrupt => "corrupt",
            ErrorCategory::Encrypted => "encrypted",
            ErrorCategory::Unsupported => "unsupported",
            ErrorCategory::LimitExceeded => "limit-exceeded",
            ErrorCategory::Timeout => "timeout",
            ErrorCategory::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtractionError {
    pub category: ErrorCategory,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

const RECORDED_EXCEPTION_PREFIX: &str = "X-TIKA:EXCEPTION:";
const EMBEDDED_EXCEPTION_KEYS: [&str; 2] = ["embedded_exception", "embedded_stream_exception"];

/// Failures Tika recovered from and recorded in the metadata instead of failing the parse, such
/// as an embedded document that could not be read or a damaged PDF page.
pub fn recorded_failures(metadata: &Metadata) -> Vec<ExtractionError> {
    let mut keys: Vec<&String> = metadata
        .keys()
        .filter(|key| key.starts_with(RECORDED_EXCEPTION_PREFIX))
        .collect();
    keys.sort();
    keys.into_iter()
        .flat_map(|key| {
            let kind = &key[RECORDED_EXCEPTION_PREFIX.len()..];
            let location = EMBEDDED_EXCEPTION_KEYS
                .contains(&kind)
                .then(|| "embedded document".to_string());
            metadata[key].iter().map(move |trace| ExtractionError {
                location: location.clone(),
                ..ExtractionError::from_message(summarize_stack_trace(trace))
            })
        })
        .collect()
}

/// Turns a Java stack trace into the `class: message <- caused by class: message` form the rest
/// of the errors use.
fn summarize_stack_trace(trace: &str) -> String {
    trace
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with(char::is_whitespace))
        .map(|line| line.strip_prefix("Caused by: ").unwrap_or(line).trim_end())
        .collect::<Vec<_>>()
        .join(" <- caused by ")
}

impl ExtractionError {
    /// Classifies an error from the extraction library, by its kind where the library knows it
    /// and by its cause chain otherwise.
    pub fn from_error(error: &extractous_core::Error) -> Self {
        let message = error.to_string();
        let category = match error {
            extractous_core::Error::IoError(_) => ErrorCategory::Unreadable,
            extractous_core::Error::Encrypted(_) => ErrorCategory::Encrypted,
            extractous_core::Error::Timeout => ErrorCategory::Timeout,
            _ => return Self::from_message(message),
        };
        Self {
            category,
            message,
            location: None,
        }
    }

    /// Classifies a failure to read the extracted text, which wraps the library's error.
    pub fn from_io(error: &std::io::Error) -> Self {
        match error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<extractous_core::Error>())
        {
            Some(inner) => Self::from_error(inner),
            None => Self::from_message(error.to_string()),
        }
    }

    /// Classifies an error by the Java exception classes in its cause chain.
    pub fn from_message(message: String) -> Self {
        let category = if message.contains("NoSuchFileException")
            || message.contains("AccessDeniedException")
        {
            ErrorCategory::Unreadable
        } else if message.contains("EncryptedDocumentException") {
            ErrorCategory::Encrypted
        } else if message.contains("UnsupportedFormatException") || message.contains("No parser") {
            ErrorCategory::Unsupported
        } else if message.contains("WriteLimitReachedException") {
            ErrorCategory::LimitExceeded
        } else if message.contains("TimeoutException") || message.contains("timeout") {
            ErrorCategory::Timeout
        } else if message.contains("ZipException")
            || message.contains("Truncated")
            || message.contains("EOFException")
            || message.contains("CorruptedFileException")
            || message.contains("InvalidFormatException")
            || message.contains("TIKA-198")
        {
            ErrorCategory::Corrupt
        } else {
            ErrorCategory::Internal
        };
        Self {
            category,
            message,
            location: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Source {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
}

/// A date as RFC 3339. A value with an offset is converted to UTC; one without is reported as
/// given, never assumed to be in any particular zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Date {
    Utc(String),
    UnknownOffset(String),
}

impl Date {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if let Ok(timestamp) = jiff::Timestamp::from_str(value) {
            return Some(Date::Utc(timestamp.to_string()));
        }
        jiff::civil::DateTime::from_str(value)
            .ok()
            .map(|date_time| Date::UnknownOffset(date_time.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Language {
    pub code: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NormalizedMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<Date>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<Date>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub language: Vec<Language>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decrypted: Option<bool>,
}

fn values<'a>(metadata: &'a Metadata, keys: &[&str]) -> &'a [String] {
    keys.iter()
        .find_map(|key| metadata.get(*key).filter(|values| !values.is_empty()))
        .map_or(&[], Vec::as_slice)
}

fn first<'a>(metadata: &'a Metadata, keys: &[&str]) -> Option<&'a str> {
    values(metadata, keys).first().map(String::as_str)
}

fn distinct(values: &[String]) -> Vec<String> {
    let mut distinct: Vec<String> = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim();
        if !value.is_empty() && !distinct.iter().any(|seen| seen == value) {
            distinct.push(value.to_string());
        }
    }
    distinct
}

pub fn normalize(metadata: &Metadata) -> NormalizedMetadata {
    let password_used = first(metadata, TIKA_KEYS.password_used) == Some("true");
    NormalizedMetadata {
        title: first(metadata, TIKA_KEYS.title)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string),
        authors: distinct(values(metadata, TIKA_KEYS.authors)),
        created: first(metadata, TIKA_KEYS.created).and_then(Date::parse),
        modified: first(metadata, TIKA_KEYS.modified).and_then(Date::parse),
        language: distinct(values(metadata, TIKA_KEYS.language))
            .into_iter()
            .map(|code| Language { code })
            .collect(),
        page_count: first(metadata, TIKA_KEYS.page_count)
            .and_then(|count| count.trim().parse().ok()),
        encrypted: password_used.then_some(true).or_else(|| {
            first(metadata, TIKA_KEYS.encrypted).and_then(|flag| flag.trim().parse().ok())
        }),
        decrypted: password_used.then_some(true),
    }
}

/// The detected media type without parameters, such as `text/html` for
/// `text/html; charset=UTF-8`.
pub fn mime_type(metadata: &Metadata) -> Option<String> {
    first(metadata, TIKA_KEYS.content_type)
        .and_then(|value| value.split(';').next())
        .map(|essence| essence.trim().to_string())
        .filter(|essence| !essence.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(entries: &[(&str, &[&str])]) -> Metadata {
        entries
            .iter()
            .map(|(key, values)| {
                (
                    key.to_string(),
                    values.iter().map(|value| value.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn dates_with_an_offset_become_utc() {
        assert_eq!(
            Date::parse("2023-02-26T09:19:44.326437+02:00"),
            Some(Date::Utc("2023-02-26T07:19:44.326437Z".to_string()))
        );
        assert_eq!(
            Date::parse("2022-07-29T10:03:21Z"),
            Some(Date::Utc("2022-07-29T10:03:21Z".to_string()))
        );
    }

    #[test]
    fn dates_without_an_offset_are_not_assumed_local() {
        assert_eq!(
            Date::parse("2024-05-01T10:00:00"),
            Some(Date::UnknownOffset("2024-05-01T10:00:00".to_string()))
        );
        assert_eq!(Date::parse("yesterday"), None);
    }

    #[test]
    fn earlier_keys_win_and_multi_values_stay_arrays() {
        let normalized = normalize(&metadata(&[
            ("dc:creator", &["Ada", "Ada", "Charles"]),
            ("meta:author", &["Someone else"]),
            ("pdf:docinfo:created", &["2020-01-01T00:00:00Z"]),
            ("dcterms:created", &["2021-01-01T00:00:00Z"]),
            ("xmpTPg:NPages", &["28"]),
            ("pdf:encrypted", &["true"]),
            ("dc:language", &["en-US"]),
        ]));
        assert_eq!(normalized.authors, ["Ada", "Charles"]);
        assert_eq!(
            normalized.created,
            Some(Date::Utc("2021-01-01T00:00:00Z".to_string()))
        );
        assert_eq!(normalized.page_count, Some(28));
        assert_eq!(normalized.encrypted, Some(true));
        assert_eq!(
            normalized.language,
            [Language {
                code: "en-US".to_string()
            }]
        );
        assert_eq!(normalized.title, None);
    }

    #[test]
    fn recorded_failures_condense_stack_traces() {
        let trace = "org.apache.tika.exception.TikaException: Error creating OOXML extractor\n\
                     \tat org.apache.tika.parser.microsoft.ooxml.OOXMLParser.parse(OOXMLParser.java:117)\n\
                     Caused by: java.io.IOException: Truncated ZIP file\n\
                     \t... 12 more\n";
        let failures = recorded_failures(&metadata(&[
            ("X-TIKA:EXCEPTION:embedded_exception", &[trace]),
            (
                "X-TIKA:Parsed-By",
                &["org.apache.tika.parser.DefaultParser"],
            ),
        ]));
        assert_eq!(
            failures,
            [ExtractionError {
                category: ErrorCategory::Corrupt,
                message: "org.apache.tika.exception.TikaException: Error creating OOXML extractor \
                          <- caused by java.io.IOException: Truncated ZIP file"
                    .to_string(),
                location: Some("embedded document".to_string()),
            }]
        );
    }

    #[test]
    fn mime_type_drops_parameters() {
        assert_eq!(
            mime_type(&metadata(&[(
                "Content-Type",
                &["text/html; charset=UTF-8"]
            )])),
            Some("text/html".to_string())
        );
    }

    #[test]
    fn category_names_match_their_serialized_form() {
        for category in [
            ErrorCategory::Unreadable,
            ErrorCategory::Corrupt,
            ErrorCategory::Encrypted,
            ErrorCategory::Unsupported,
            ErrorCategory::LimitExceeded,
            ErrorCategory::Timeout,
            ErrorCategory::Internal,
        ] {
            assert_eq!(
                serde_json::to_value(category).unwrap(),
                category.name(),
                "{category:?}"
            );
        }
    }

    #[test]
    fn errors_are_classified_by_their_cause_chain() {
        let classify = |message: &str| ExtractionError::from_message(message.to_string()).category;
        assert_eq!(
            classify("Parse error occurred : org.apache.tika.exception.EncryptedDocumentException: Unable to process: document is encrypted"),
            ErrorCategory::Encrypted
        );
        assert_eq!(
            classify("TIKA-198: Illegal IOException <- caused by java.io.IOException: Truncated ZIP file"),
            ErrorCategory::Corrupt
        );
        assert_eq!(
            classify("java.io.IOException: <- caused by org.apache.tika.exception.TikaException: TesseractOCRParser timeout"),
            ErrorCategory::Timeout
        );
        assert_eq!(
            classify("Could not open the input: java.nio.file.NoSuchFileException: a.pdf"),
            ErrorCategory::Unreadable
        );
        assert_eq!(classify("something else"), ErrorCategory::Internal);
    }
}
