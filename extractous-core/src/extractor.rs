use crate::config::{ParserConfigs, Password};
use crate::errors::Error;
use crate::errors::ExtractResult;
use crate::formats::{self, NativeStream, Output};
use crate::isolate::isolate;
use crate::tika;
use crate::tika::JInputStream;
use crate::tika::{Source, StreamOptions};
use crate::{
    EmbeddedExtractResult, EmbeddedSpool, OfficeParserConfig, PdfParserConfig, SpooledEmbedded,
    TesseractOcrConfig,
};
use std::collections::HashMap;
use std::time::Duration;
use strum_macros::{Display, EnumString};

/// Metadata type alias
pub type Metadata = HashMap<String, Vec<String>>;

/// CharSet enum of all supported encodings
#[derive(Debug, Clone, Default, Copy, PartialEq, Eq, Hash, Display, EnumString)]
#[allow(non_camel_case_types)]
pub enum CharSet {
    #[default]
    #[strum(serialize = "UTF-8")]
    UTF_8,
    #[strum(serialize = "US-ASCII")]
    US_ASCII,
    #[strum(serialize = "UTF-16BE")]
    UTF_16BE,
}

/// A media type as detected by Apache Tika, such as `application/pdf`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MediaType(String);

impl MediaType {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// StreamReader implements std::io::Read
///
/// Can be used to perform buffered reading. For example:
/// ```rust
/// use extractous_core::{CharSet, Extractor};
/// use std::io::BufReader;
/// use std::io::prelude::*;
///
/// let extractor = Extractor::new();
/// let (reader, metadata) = extractor.extract_file("../README.md").unwrap();
///
/// let mut buf_reader = BufReader::new(reader);
/// let mut content = String::new();
/// buf_reader.read_to_string(&mut content).unwrap();
/// println!("{}", content);
/// ```
///
pub struct StreamReader {
    pub(crate) inner: Stream,
}

/// Where a stream's text comes from: Tika's parse in the isolate, or a native parser.
pub(crate) enum Stream {
    Tika(JInputStream),
    Native(NativeStream),
}

impl StreamReader {
    /// The document's metadata as the parser left it, which includes everything it found while
    /// producing the text. Available once the stream has been read to the end.
    pub fn metadata(&self) -> ExtractResult<Metadata> {
        match &self.inner {
            Stream::Tika(stream) => stream.metadata(),
            Stream::Native(stream) => stream.metadata(),
        }
    }

    /// The embedded documents copied to the spool given to
    /// [`Extractor::extract_file_spooling_embedded`], in the order they appear. Available once
    /// the stream has been read to the end.
    pub fn embedded(&self) -> ExtractResult<Vec<SpooledEmbedded>> {
        match &self.inner {
            Stream::Tika(stream) => stream.embedded(),
            Stream::Native(stream) => stream.metadata().map(|_| Vec::new()),
        }
    }
}

impl std::io::Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match &mut self.inner {
            Stream::Tika(stream) => stream.read(buf),
            Stream::Native(stream) => stream.read(buf),
        }
    }
}

/// Extractor for extracting text from different file formats
///
/// The Extractor uses the builder pattern to set configurations. This allows configuring and
/// extracting text in one line. For example
/// ```rust
/// use extractous_core::{CharSet, Extractor};
/// let (text, metadata) = Extractor::new()
///             .set_extract_string_max_length(1000)
///             .extract_file_to_string("../README.md")
///             .unwrap();
/// println!("{}", text);
/// ```
///
#[derive(Debug, Clone)]
pub struct Extractor {
    extract_string_max_length: i32,
    encoding: CharSet,
    configs: ParserConfigs,
    xml_output: bool,
    passwords: Vec<Password>,
    timeout: Option<Duration>,
}

impl Default for Extractor {
    fn default() -> Self {
        Self {
            extract_string_max_length: 500_000, // 500KB
            encoding: CharSet::UTF_8,
            configs: ParserConfigs::default(),
            xml_output: false,
            passwords: Vec::new(),
            timeout: None,
        }
    }
}

impl Extractor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum length of the extracted text. Used only for extract_to_string functions
    /// Default: 500_000
    pub fn set_extract_string_max_length(mut self, max_length: i32) -> Self {
        self.extract_string_max_length = max_length;
        self
    }

    /// Set the encoding to use for when extracting text to a stream.
    /// Not used for extract_to_string functions.
    /// Default: CharSet::UTF_8
    pub fn set_encoding(mut self, encoding: CharSet) -> Self {
        self.encoding = encoding;
        self
    }

    /// Set the configuration for the PDF parser
    pub fn set_pdf_config(mut self, config: PdfParserConfig) -> Self {
        self.configs.pdf = config;
        self
    }

    /// Set the configuration for the Office parser
    pub fn set_office_config(mut self, config: OfficeParserConfig) -> Self {
        self.configs.office = config;
        self
    }

    /// Set the configuration for the Tesseract OCR
    pub fn set_ocr_config(mut self, config: TesseractOcrConfig) -> Self {
        self.configs.ocr = config;
        self
    }

    /// Limits how long a streamed extraction may take, from the moment it starts: a read that
    /// would wait past it fails with [`Error::Timeout`] and the text read so far stands.
    pub fn set_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set the passwords tried, in order, on documents Tika reports as encrypted. Documents that
    /// are not encrypted never use them.
    pub fn set_passwords(mut self, passwords: impl IntoIterator<Item = Password>) -> Self {
        self.passwords = passwords.into_iter().collect();
        self
    }

    /// Set the configuration for the parse as xml
    pub fn set_xml_output(mut self, xml_output: bool) -> Self {
        self.xml_output = xml_output;
        self
    }

    /// Extracts text from a file path. Returns a tuple with stream of the extracted text and metadata.
    /// the stream is decoded using the extractor's `encoding`
    pub fn extract_file(&self, file_path: &str) -> ExtractResult<(StreamReader, Metadata)> {
        self.extract_to_stream(Source::File(file_path), None)
    }

    /// Like [`Extractor::extract_file`], but documents embedded in this one are copied to
    /// `spool` instead of contributing their text, for the caller to extract in their own right.
    /// The stream's [`StreamReader::embedded`] lists them once it has been read.
    pub fn extract_file_spooling_embedded(
        &self,
        file_path: &str,
        spool: &EmbeddedSpool,
    ) -> ExtractResult<(StreamReader, Metadata)> {
        self.extract_to_stream(Source::File(file_path), Some(spool))
    }

    /// Extracts text from a byte buffer. Returns a tuple with stream of the extracted text and metadata.
    /// the stream is decoded using the extractor's `encoding`
    pub fn extract_bytes(&self, buffer: &[u8]) -> ExtractResult<(StreamReader, Metadata)> {
        self.extract_to_stream(Source::Bytes(buffer), None)
    }

    /// Extracts text from an url. Returns a tuple with stream of the extracted text and metadata.
    /// the stream is decoded using the extractor's `encoding`
    pub fn extract_url(&self, url: &str) -> ExtractResult<(StreamReader, Metadata)> {
        self.extract_to_stream(Source::Url(url), None)
    }

    /// Extracts text from a file path. Returns a tuple with string that is of maximum length
    /// of the extractor's `extract_string_max_length` and metadata.
    pub fn extract_file_to_string(&self, file_path: &str) -> ExtractResult<(String, Metadata)> {
        self.extract_to_string(Source::File(file_path))
    }

    /// Extracts text from a byte buffer. Returns a tuple with string that is of maximum length
    /// of the extractor's `extract_string_max_length` and metadata.
    pub fn extract_bytes_to_string(&self, buffer: &[u8]) -> ExtractResult<(String, Metadata)> {
        self.extract_to_string(Source::Bytes(buffer))
    }

    /// Extracts text from a URL. Returns a tuple with string that is of maximum length
    /// of the extractor's `extract_string_max_length` and metadata.
    pub fn extract_url_to_string(&self, url: &str) -> ExtractResult<(String, Metadata)> {
        self.extract_to_string(Source::Url(url))
    }

    /// Extracts all embedded documents/resources (images, attachments, etc.) from a file.
    /// This is similar to Apache Tika Server's /unpack/all endpoint.
    pub fn extract_embedded_from_file(
        &self,
        file_path: &str,
    ) -> ExtractResult<EmbeddedExtractResult> {
        self.extract_embedded(Source::File(file_path))
    }

    /// Extracts all embedded documents/resources from a byte buffer.
    pub fn extract_embedded_from_bytes(
        &self,
        buffer: &[u8],
    ) -> ExtractResult<EmbeddedExtractResult> {
        self.extract_embedded(Source::Bytes(buffer))
    }

    /// Extracts all embedded documents/resources from the document at a URL.
    pub fn extract_embedded_from_url(&self, url: &str) -> ExtractResult<EmbeddedExtractResult> {
        self.extract_embedded(Source::Url(url))
    }

    /// Detects the media type of a file, such as `application/pdf`, from its content, its
    /// container structure and its name.
    pub fn detect_file(&self, file_path: &str) -> ExtractResult<MediaType> {
        tika::detect(isolate()?, Source::File(file_path)).map(MediaType)
    }

    /// Detects the media type of a document held in memory from its content and container
    /// structure.
    pub fn detect_bytes(&self, buffer: &[u8]) -> ExtractResult<MediaType> {
        tika::detect(isolate()?, Source::Bytes(buffer)).map(MediaType)
    }

    /// Detects the media type of the document at a URL.
    pub fn detect_url(&self, url: &str) -> ExtractResult<MediaType> {
        tika::detect(isolate()?, Source::Url(url)).map(MediaType)
    }

    /// Runs `attempt` without a password and, if Tika reports the document as encrypted, with
    /// each configured password in turn. A wrong password does not always surface as an
    /// encryption error (a 7z archive fails its checksum), so any failure with a password moves
    /// on to the next one; when none opens the document, the encryption error is returned.
    fn with_passwords<T>(
        &self,
        attempt: impl Fn(Option<&Password>) -> ExtractResult<T>,
    ) -> ExtractResult<T> {
        let encrypted = match attempt(None) {
            Err(error @ Error::Encrypted(_)) => error,
            result => return result,
        };
        self.passwords
            .iter()
            .find_map(|password| attempt(Some(password)).ok())
            .ok_or(encrypted)
    }

    fn output(&self) -> Output {
        if self.xml_output {
            Output::Xml
        } else {
            Output::Text
        }
    }

    fn extract_to_stream(
        &self,
        source: Source,
        embedded: Option<&EmbeddedSpool>,
    ) -> ExtractResult<(StreamReader, Metadata)> {
        if let Some(routed) = formats::route(source, embedded.is_some())? {
            let options = formats::StreamOptions {
                char_set: self.encoding,
                output: self.output(),
                timeout: self.timeout,
            };
            let (stream, metadata) = formats::parse_to_stream(
                routed.parser,
                routed.input.into_owned(),
                routed.detected,
                self.configs.clone(),
                options,
            )?;
            return Ok((
                StreamReader {
                    inner: Stream::Native(stream),
                },
                metadata,
            ));
        }
        let isolate = isolate()?;
        let options = StreamOptions {
            char_set: self.encoding,
            as_xml: self.xml_output,
            timeout: self.timeout,
            embedded,
        };
        self.with_passwords(|password| {
            tika::parse_to_stream(isolate, source, &self.configs, password, &options)
        })
    }

    fn extract_to_string(&self, source: Source) -> ExtractResult<(String, Metadata)> {
        if let Some(routed) = formats::route(source, false)? {
            let max_length = usize::try_from(self.extract_string_max_length).map_err(|_| {
                Error::ParseError(format!(
                    "The maximum string length must not be negative: {}",
                    self.extract_string_max_length
                ))
            })?;
            return formats::parse_to_string(
                routed.parser,
                &routed.input,
                &routed.detected,
                &self.configs,
                max_length,
                self.output(),
            );
        }
        let isolate = isolate()?;
        self.with_passwords(|password| {
            tika::parse_to_string(
                isolate,
                source,
                self.extract_string_max_length,
                &self.configs,
                password,
                self.xml_output,
            )
        })
    }

    fn extract_embedded(&self, source: Source) -> ExtractResult<EmbeddedExtractResult> {
        let isolate = isolate()?;
        self.with_passwords(|password| {
            tika::extract_embedded(isolate, source, &self.configs, password)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::StreamReader;
    use crate::Extractor;
    use std::fs::File;
    use std::io::BufReader;
    use std::io::{self, Read};
    use std::str;

    const TEST_FILE: &str = "../test_files/documents/mostly-ascii-utf8.txt";

    const TEST_URL: &str = "https://www.google.com/";

    fn expected_content() -> String {
        let mut file = File::open(TEST_FILE).unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).unwrap();
        content
    }

    #[test]
    fn extract_file_to_string_test() {
        // Prepare expected_content
        let expected_content = expected_content();

        let extractor = Extractor::new();
        let result = extractor.extract_file_to_string(TEST_FILE);
        let (content, metadata) = result.unwrap();
        assert_eq!(content.trim(), expected_content.trim());
        assert!(
            !metadata.is_empty(),
            "Metadata should contain at least one entry"
        );
    }

    fn read_content_from_stream(stream: StreamReader) -> String {
        let mut reader = BufReader::new(stream);
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer).unwrap();

        String::from_utf8(buffer).unwrap()
    }

    #[test]
    fn extract_file_test() {
        // Prepare expected_content
        let expected_content = expected_content();

        // Parse the files using extractous
        let extractor = Extractor::new();
        let result = extractor.extract_file(TEST_FILE);
        let (reader, metadata) = result.unwrap();
        let content = read_content_from_stream(reader);

        assert_eq!(content.trim(), expected_content.trim());
        assert!(
            !metadata.is_empty(),
            "Metadata should contain at least one entry"
        );
    }

    fn read_file_as_bytes(path: &str) -> io::Result<Vec<u8>> {
        let mut file = File::open(path)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        Ok(buffer)
    }

    #[test]
    fn extract_bytes_test() {
        // Prepare expected_content
        let expected_content = expected_content();

        // Parse the bytes using extractous
        let file_bytes = read_file_as_bytes(TEST_FILE).unwrap();
        let extractor = Extractor::new();
        let result = extractor.extract_bytes(&file_bytes);
        let (reader, metadata) = result.unwrap();
        let content = read_content_from_stream(reader);

        assert_eq!(content.trim(), expected_content.trim());
        assert!(
            !metadata.is_empty(),
            "Metadata should contain at least one entry"
        );
    }

    #[test]
    fn extract_url_test() {
        // Parse url by extractous
        let extractor = Extractor::new();
        let result = extractor.extract_url(TEST_URL);
        let (reader, metadata) = result.unwrap();
        let content = read_content_from_stream(reader);

        assert!(content.contains("Google"));
        assert!(
            !metadata.is_empty(),
            "Metadata should contain at least one entry"
        );
    }

    #[test]
    fn extract_file_to_xml_test() {
        // Parse the files using extractous
        let extractor = Extractor::new().set_xml_output(true);
        let result = extractor.extract_file_to_string(TEST_FILE);
        let (content, metadata) = result.unwrap();
        assert!(
            !content.is_empty(),
            "Metadata should contain at least one entry"
        );
        assert!(
            !metadata.is_empty(),
            "Metadata should contain at least one entry"
        );
    }
}
