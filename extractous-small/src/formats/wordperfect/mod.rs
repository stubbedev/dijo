//! WordPerfect documents and Quattro Pro workbooks, as Tika's `WordPerfectParser` and
//! `QuattroProParser` ([`quattro`]) read them through their `WPInputStream`. A WordPerfect 5 or 6
//! document's prefix area becomes metadata, then its document area is read byte by byte into
//! paragraphs of text, its functions skipped and its extended characters looked up in
//! WordPerfect's character sets ([`charsets`]). The spreadsheets of Quattro Pro 1 to 6, which
//! Tika does not read, are read natively ([`old_quattro`]).

mod charsets;
mod old_quattro;
mod quattro;

pub use old_quattro::OldQuattroProParser;
pub use quattro::QuattroProParser;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::java;

/// `WPPrefixArea.WP5_MAJOR_VERSION`.
const WP5_MAJOR_VERSION: u8 = 0;
/// `WPPrefixArea.WP6_MAJOR_VERSION`.
const WP6_MAJOR_VERSION: u8 = 2;
/// The text `WPDocumentAreaExtractor` gathers before it writes it, in UTF-16 units.
const CHUNK: usize = 4096;

/// The bytes `WPInputStream` reads, failing as its reads fail where the input ends.
struct Stream<'a> {
    data: &'a [u8],
    at: usize,
}

/// The `EOFException` `WPInputStream` throws where the input ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EndOfFile;

impl EndOfFile {
    const REASON: &'static str = "java.io.EOFException: the document ends inside a record";
}

impl From<EndOfFile> for Failure {
    fn from(_: EndOfFile) -> Self {
        Failure::Malformed(EndOfFile::REASON.to_owned())
    }
}

impl From<EndOfFile> for Decline<'_> {
    fn from(_: EndOfFile) -> Self {
        Decline::Fails(Error::IoError(EndOfFile::REASON.to_owned()))
    }
}

impl<'a> Stream<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    /// `read`: the next byte, if any.
    fn read(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    /// `readWP`, `readWPChar` and `readWPByte`: the next byte, which must be there.
    fn byte(&mut self) -> Result<u8, EndOfFile> {
        self.read().ok_or(EndOfFile)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], EndOfFile> {
        let bytes = self.data.get(self.at..self.at + N).ok_or(EndOfFile)?;
        self.at += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    /// `readWPShort`.
    fn short(&mut self) -> Result<u16, EndOfFile> {
        self.take().map(u16::from_le_bytes)
    }

    /// `readWPLong`, which adds its bytes up as a Java `int`.
    fn long(&mut self) -> Result<i32, EndOfFile> {
        self.take().map(i32::from_le_bytes)
    }

    /// `skipWPByte`: `count` bytes, which must be there.
    fn skip(&mut self, count: usize) -> Result<(), EndOfFile> {
        if self.data.len() - self.at < count {
            self.at = self.data.len();
            return Err(EndOfFile);
        }
        self.at += count;
        Ok(())
    }

    /// `skipUntilChar`: past the next `target`, or to the end.
    fn skip_until(&mut self, target: u8) {
        match self.data[self.at..].iter().position(|&byte| byte == target) {
            Some(offset) => self.at += offset + 1,
            None => self.at = self.data.len(),
        }
    }

    /// `readWPString`: `length` bytes as characters.
    fn string(&mut self, length: usize) -> Result<String, EndOfFile> {
        let bytes = self.data.get(self.at..self.at + length).ok_or(EndOfFile)?;
        self.at += length;
        Ok(java::latin1(bytes))
    }
}

/// `WPPrefixArea`, as `WPPrefixAreaExtractor` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrefixArea {
    file_id: String,
    document_area: i32,
    product_type: u8,
    file_type: u8,
    major_version: u8,
    minor_version: u8,
    encrypted: bool,
    /// The file size a WordPerfect 6 document records, if it is long enough to.
    file_size: Option<i32>,
}

impl PrefixArea {
    fn read(input: &[u8]) -> Result<Self, EndOfFile> {
        let mut stream = Stream::new(input);
        let file_id = stream.string(4)?;
        let document_area = stream.long()?;
        let product_type = stream.byte()?;
        let file_type = stream.byte()?;
        let major_version = stream.byte()?;
        let minor_version = stream.byte()?;
        let encrypted = stream.short()? != 0;
        stream.short()?;
        let file_size = if major_version == WP6_MAJOR_VERSION {
            input
                .get(20..24)
                .map(|bytes| i32::from_le_bytes(bytes.try_into().expect("four bytes")))
        } else {
            None
        };
        Ok(Self {
            file_id,
            document_area,
            product_type,
            file_type,
            major_version,
            minor_version,
            encrypted,
            file_size,
        })
    }

    /// `applyMetadata`, but for the content type, which Tika's detection has set already.
    fn write(&self, metadata: &mut TikaMetadata) {
        metadata.set("wordperfect:file-id", self.file_id.as_str());
        metadata.set("wordperfect:product-type", self.product_type.to_string());
        metadata.set("wordperfect:file-type", self.file_type.to_string());
        metadata.set("wordperfect:major-version", self.major_version.to_string());
        metadata.set("wordperfect:minor-version", self.minor_version.to_string());
        metadata.set("wordperfect:encrypted", self.encrypted.to_string());
        if let Some(size) = self.file_size.filter(|&size| size > -1) {
            metadata.set("wordperfect:file-size", size.to_string());
        }
    }
}

/// Tika's `WordPerfectParser`, which takes WordPerfect 5.0, 5.1 and 6 documents.
pub struct WordPerfectParser;

impl Parser for WordPerfectParser {
    fn name(&self) -> &'static str {
        "extractous::WordPerfectParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.wordperfect.WordPerfectParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.wordperfect; version=5.0",
            "application/vnd.wordperfect; version=5.1",
            "application/vnd.wordperfect; version=6.x",
        ]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let prefix = PrefixArea::read(input)?;
        if prefix.major_version != WP5_MAJOR_VERSION && prefix.major_version != WP6_MAJOR_VERSION {
            return Err(Decline::Fails(Error::ParseError(format!(
                "UnsupportedFormatException: Parser doesn't recognize this major version: {}",
                prefix.major_version
            ))));
        }
        if prefix.encrypted {
            return Err(Decline::Fails(Error::Encrypted(
                "EncryptedDocumentException: the WordPerfect document is encrypted".to_owned(),
            )));
        }
        let start = usize::try_from(prefix.document_area).unwrap_or(0);
        let area = input.get(start..).ok_or(EndOfFile)?;
        Ok(Box::new(move |xhtml| {
            prefix.write(xhtml.metadata_mut());
            let version = if prefix.major_version == WP6_MAJOR_VERSION {
                Version::Six
            } else {
                Version::Five
            };
            DocumentArea {
                xhtml,
                out: String::with_capacity(CHUNK),
                out_units: 0,
            }
            .extract(Stream::new(area), version)
        }))
    }
}

/// The WordPerfect version whose document area extractor reads the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    /// `WP5DocumentAreaExtractor`.
    Five,
    /// `WP6DocumentAreaExtractor`, which includes deleted content.
    Six,
}

/// `WP5DocumentAreaExtractor.FIXED_LENGTH_FUNCTION_SIZES`, from 193 on.
const WP5_FUNCTION_SIZES: [usize; 15] = [9, 11, 3, 3, 5, 6, 7, 4, 5, 6, 6, 8, 10, 10, 12];
/// `WP6DocumentAreaExtractor.FIXED_LENGTH_FUNCTION_SIZES`, from 241 on.
const WP6_FUNCTION_SIZES: [usize; 14] = [5, 3, 3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 8, 8];

/// `WPDocumentAreaExtractor`: the text gathered for the paragraph, written whenever it reaches
/// [`CHUNK`] and at each paragraph's end.
struct DocumentArea<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    out: String,
    out_units: usize,
}

impl DocumentArea<'_, '_> {
    fn extract(mut self, mut stream: Stream<'_>, version: Version) -> Result<(), Failure> {
        while let Some(c) = stream.read() {
            match version {
                Version::Five => self.wp5(c, &mut stream)?,
                Version::Six => self.wp6(c, &mut stream)?,
            }
            if self.out_units >= CHUNK {
                self.flush()?;
            }
        }
        self.end_paragraph()
    }

    fn push(&mut self, character: char) {
        self.out.push(character);
        self.out_units += character.len_utf16();
    }

    fn flush(&mut self) -> Result<(), Failure> {
        self.xhtml.characters(&self.out)?;
        self.out.clear();
        self.out_units = 0;
        Ok(())
    }

    /// `endParagraph`: the text gathered, in a paragraph of its own.
    fn end_paragraph(&mut self) -> Result<(), Failure> {
        self.xhtml.start_element("p")?;
        self.flush()?;
        self.xhtml.end_element("p")?;
        Ok(())
    }

    /// An extended character of `charsets`, as `WP5Charsets.append` and `WP6Charsets.append`
    /// look it up: a space for one they do not know.
    fn extended(&mut self, charsets: &[&[char]], stream: &mut Stream<'_>) -> Result<(), Failure> {
        let value = stream.byte()?;
        let charset = stream.byte()?;
        stream.byte()?;
        let character = charsets
            .get(usize::from(charset))
            .and_then(|set| set.get(usize::from(value)))
            .copied()
            .unwrap_or(' ');
        self.push(character);
        Ok(())
    }

    /// `WP5DocumentAreaExtractor.extract` of one byte.
    fn wp5(&mut self, c: u8, stream: &mut Stream<'_>) -> Result<(), Failure> {
        match c {
            10 | 12 | 140 | 153 => self.end_paragraph()?,
            11 | 13 | 144..=149 => self.push(' '),
            32..=126 => self.push(char::from(c)),
            160 => self.push('\u{A0}'),
            169..=171 => self.push('-'),
            192 => self.extended(&charsets::WP5, stream)?,
            193..=207 => stream.skip(WP5_FUNCTION_SIZES[usize::from(c - 193)] - 1)?,
            208..=255 => {
                stream.byte()?;
                let size = stream.short()?;
                stream.skip(usize::from(size))?;
            }
            _ => {}
        }
        Ok(())
    }

    /// `WP6DocumentAreaExtractor.extract` of one byte.
    fn wp6(&mut self, c: u8, stream: &mut Stream<'_>) -> Result<(), Failure> {
        match c {
            241 => {
                stream.byte()?;
                stream.skip(WP6_FUNCTION_SIZES[0] - 2)?;
            }
            1..=32 => self.push(charsets::INTERNATIONAL[usize::from(c)]),
            33..=126 => self.push(char::from(c)),
            128 => self.push(' '),
            129 => self.push('\u{A0}'),
            132 => self.push('-'),
            135 | 137 => self.end_paragraph()?,
            138 => stream.skip_until(139),
            198 => self.push('\t'),
            180..=207 => self.end_paragraph()?,
            208..=239 => {
                let subgroup = stream.byte()?;
                let size = stream.short()?;
                stream.skip(usize::from(size).saturating_sub(4))?;
                match (c, subgroup) {
                    (208, 1..=3 | 20..=22) | (213, _) => self.push(' '),
                    (208, 10) | (224, _) => self.push('\t'),
                    (208, 4..=19 | 23..=28) => self.end_paragraph()?,
                    _ => {}
                }
            }
            240 => self.extended(&charsets::WP6, stream)?,
            242..=254 => stream.skip(WP6_FUNCTION_SIZES[usize::from(c - 241)] - 1)?,
            255 => stream.skip_until(255),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
