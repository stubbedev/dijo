//! What a document is and what Tika knows about it before its parser runs: how Tika's
//! `TikaInputStream` opened it, and what its detection found.

use std::borrow::Cow;

use crate::formats::embedded;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{java_file_name, InputFile, Opened, Parser};
use crate::mime::{self, Detection, Intent, Origin};

/// What a document is and what Tika knows about it before its parser runs: the metadata of the
/// input (its resource name and length) and of its detection.
#[derive(Debug, Clone)]
pub struct Detected {
    pub media_type: String,
    pub metadata: TikaMetadata,
    pub file: InputFile,
}

/// How Tika's `TikaInputStream` opens a document, which sets the metadata it starts with.
#[derive(Debug, Clone)]
pub enum OpenedFrom<'a> {
    /// The file at this path, as `TikaInputStream.get(Path, metadata)` opens it, or as
    /// `TikaInputStream.get(URL, metadata)` opens a `file:` URL of a regular file.
    File(Cow<'a, str>),
    /// Bytes without a name.
    Bytes,
    /// A document fetched from a URL, as `TikaInputStream.get(URL, metadata)` opens it, with
    /// what the server said of it. Its `Content-Length` is the server's, or where the server
    /// sends none, the length of what it sent: Tika's path records that only once a detector or
    /// parser happens to spool the stream to a file.
    Url(Headers),
}

/// What `TikaInputStream.get(URL, metadata)` records of a fetched document, each header's value
/// as `URLConnection` reads it: the last of its kind, its bytes as ISO-8859-1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    /// The last segment of the URL's path, still percent-encoded, unless the path ends in a
    /// slash.
    pub name: Option<String>,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    /// `URLConnection.getContentLength`: the `Content-Length`, where it is a whole number that
    /// fits in a Java `int`.
    pub content_length: Option<u32>,
}

impl Detected {
    /// Detects `input`, opened as `from` says, with the metadata Tika holds after its
    /// detection, alone as `TikaNativeMain.detect` runs it or before a parse as
    /// `AutoDetectParser` runs it, as `intent` says: what `TikaInputStream` writes as it opens
    /// the input (for a file its name and length, for a fetched document what the server said
    /// of it), then what the detectors write, in the order they write it.
    pub fn detect(
        from: &OpenedFrom<'_>,
        input: &[u8],
        intent: Intent,
    ) -> Result<Self, mime::DetectionError> {
        let mut metadata = TikaMetadata::new();
        let (origin, file) = match from {
            OpenedFrom::File(path) => {
                let name = java_file_name(path);
                metadata.set("tk:resource-name", name);
                metadata.set("Content-Length", input.len().to_string());
                (Origin::File { name }, InputFile::Original(path.to_string()))
            }
            OpenedFrom::Bytes => (Origin::Bytes, InputFile::Spooled),
            OpenedFrom::Url(headers) => {
                let hints = [
                    ("tk:resource-name", &headers.name),
                    ("Content-Type", &headers.content_type),
                    ("Content-Encoding", &headers.content_encoding),
                ];
                for (name, value) in hints {
                    if let Some(value) = value {
                        metadata.set(name, value.as_str());
                    }
                }
                let length = headers
                    .content_length
                    .map_or_else(|| input.len().to_string(), |length| length.to_string());
                metadata.set("Content-Length", length);
                let origin = Origin::Stream {
                    name: headers.name.as_deref(),
                    content_type: headers.content_type.as_deref(),
                    learns_length: false,
                };
                (origin, InputFile::Spooled)
            }
        };
        let Detection {
            media_type,
            metadata: entries,
        } = match intent {
            Intent::Detect => mime::detect(input, origin)?,
            Intent::Parse => mime::detect_to_parse(input, origin)?,
        };
        for entry in entries {
            metadata.set(entry.key.name(), entry.value);
        }
        Ok(Self {
            media_type: media_type.to_string(),
            metadata,
            file,
        })
    }

    /// Detects an embedded document `input`, its stream `opened` as given, with the `metadata`
    /// its embedding parser gives it, which hints at its type and which the detection's
    /// metadata is added to.
    pub fn detect_embedded(
        input: &[u8],
        metadata: &TikaMetadata,
        opened: Opened,
    ) -> Result<Self, mime::DetectionError> {
        let origin = Origin::Stream {
            name: metadata.get(embedded::RESOURCE_NAME),
            content_type: metadata.get("Content-Type"),
            learns_length: opened == Opened::WithMetadata,
        };
        let Detection {
            media_type,
            metadata: entries,
        } = mime::detect_to_parse(input, origin)?;
        let mut metadata = metadata.clone();
        for entry in entries {
            metadata.set(entry.key.name(), entry.value);
        }
        Ok(Self {
            media_type: media_type.to_string(),
            metadata,
            file: InputFile::Spooled,
        })
    }

    /// The metadata a parser starts from, as Tika's `AutoDetectParser` and `CompositeParser`
    /// leave it: the detected `Content-Type` and the parser in `tk:parsed-by`.
    pub fn metadata_for(&self, parser: &dyn Parser) -> TikaMetadata {
        let mut metadata = self.metadata.clone();
        metadata.set("Content-Type", self.media_type.clone());
        metadata.add("tk:parsed-by", parser.name());
        metadata
    }
}
