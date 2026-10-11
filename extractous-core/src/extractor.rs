use crate::config::ParserConfigs;
use crate::errors::ExtractResult;
use crate::tika;
use crate::tika::JInputStream;
use crate::tika::Source;
use crate::{EmbeddedExtractResult, OfficeParserConfig, PdfParserConfig, TesseractOcrConfig};
use std::collections::HashMap;
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
    pub(crate) inner: JInputStream,
}

impl StreamReader {
    /// The document's metadata as Tika left it after parsing, which includes everything the
    /// parser found while producing the text. Available once the stream has been read to the end.
    pub fn metadata(&self) -> ExtractResult<Metadata> {
        self.inner.metadata()
    }
}

impl std::io::Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
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
}

impl Default for Extractor {
    fn default() -> Self {
        Self {
            extract_string_max_length: 500_000, // 500KB
            encoding: CharSet::UTF_8,
            configs: ParserConfigs::default(),
            xml_output: false,
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

    /// Set the configuration for the parse as xml
    pub fn set_xml_output(mut self, xml_output: bool) -> Self {
        self.xml_output = xml_output;
        self
    }

    /// Extracts text from a file path. Returns a tuple with stream of the extracted text and metadata.
    /// the stream is decoded using the extractor's `encoding`
    pub fn extract_file(&self, file_path: &str) -> ExtractResult<(StreamReader, Metadata)> {
        self.extract_to_stream(Source::File(file_path))
    }

    /// Extracts text from a byte buffer. Returns a tuple with stream of the extracted text and metadata.
    /// the stream is decoded using the extractor's `encoding`
    pub fn extract_bytes(&self, buffer: &[u8]) -> ExtractResult<(StreamReader, Metadata)> {
        self.extract_to_stream(Source::Bytes(buffer))
    }

    /// Extracts text from an url. Returns a tuple with stream of the extracted text and metadata.
    /// the stream is decoded using the extractor's `encoding`
    pub fn extract_url(&self, url: &str) -> ExtractResult<(StreamReader, Metadata)> {
        self.extract_to_stream(Source::Url(url))
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
        tika::extract_embedded(Source::File(file_path), &self.configs)
    }

    /// Extracts all embedded documents/resources from a byte buffer.
    pub fn extract_embedded_from_bytes(
        &self,
        buffer: &[u8],
    ) -> ExtractResult<EmbeddedExtractResult> {
        tika::extract_embedded(Source::Bytes(buffer), &self.configs)
    }

    /// Extracts all embedded documents/resources from the document at a URL.
    pub fn extract_embedded_from_url(&self, url: &str) -> ExtractResult<EmbeddedExtractResult> {
        tika::extract_embedded(Source::Url(url), &self.configs)
    }

    /// Detects the media type of a file, such as `application/pdf`, from its content, its
    /// container structure and its name.
    pub fn detect_file(&self, file_path: &str) -> ExtractResult<MediaType> {
        tika::detect(Source::File(file_path)).map(MediaType)
    }

    /// Detects the media type of a document held in memory from its content and container
    /// structure.
    pub fn detect_bytes(&self, buffer: &[u8]) -> ExtractResult<MediaType> {
        tika::detect(Source::Bytes(buffer)).map(MediaType)
    }

    /// Detects the media type of the document at a URL.
    pub fn detect_url(&self, url: &str) -> ExtractResult<MediaType> {
        tika::detect(Source::Url(url)).map(MediaType)
    }

    fn extract_to_stream(&self, source: Source) -> ExtractResult<(StreamReader, Metadata)> {
        tika::parse_to_stream(source, &self.encoding, &self.configs, self.xml_output)
    }

    fn extract_to_string(&self, source: Source) -> ExtractResult<(String, Metadata)> {
        tika::parse_to_string(
            source,
            self.extract_string_max_length,
            &self.configs,
            self.xml_output,
        )
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

    const TEST_FILE: &str = "../README.md";

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
