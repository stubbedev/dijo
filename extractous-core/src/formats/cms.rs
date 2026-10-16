//! CMS (PKCS #7) data, as our Java `CmsParser` and the Tika `Pkcs7Parser` it hands the rest
//! read it: encrypted data (enveloped, authenticated-enveloped or encrypted) is reported as
//! encrypted, before the document starts; a detached signature, signed data typed
//! `application/pkcs7-signature` without content of its own, is an empty document; signed and
//! compressed data have their `Content-Type` refined and their content parsed, from fresh
//! metadata, by the parser it is routed to, into the same handler; digested data is labelled
//! and empty, and anything else empty.
//!
//! The content is read from the BER encoding as Bouncy Castle's streaming parsers read it:
//! the content info, the version and digest or compression algorithms, and the encapsulated
//! content info, its OCTET STRING primitive or constructed. Content no native parser takes,
//! signed data typed `application/pkcs7-mime` without content (whose kind Bouncy Castle tells
//! by its signers and certificates), and encodings the port does not read as Bouncy Castle
//! does stay on Tika's path.

use crate::compress::zlib;
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{self, Declined, Deferral, Failure, Opened, Parser, Prepared};
use crate::mime::MediaType;

/// The bytes `CmsParser` reads the content type from.
const HEADER_BYTES: usize = 64;
const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const OCTET_STRING: u8 = 0x04;
const CONSTRUCTED_OCTET_STRING: u8 = 0x24;
const OBJECT_IDENTIFIER: u8 = 0x06;
const EXPLICIT_0: u8 = 0xA0;
const SIGNATURE: &str = "application/pkcs7-signature";
/// The types `CmsClassifier` refines CMS data to.
const SIGNED: &str = "application/pkcs7-mime; smime-type=signed-data";
const COMPRESSED: &str = "application/pkcs7-mime; smime-type=compressed-data";
const DIGESTED: &str = "application/pkcs7-mime; smime-type=digested-data";
/// `Pkcs7Parser.MAX_DECOMPRESSED`.
const MAX_DECOMPRESSED: usize = 100 * 1024 * 1024;
/// 1.2.840.113549.1.7.2, `signedData`.
const SIGNED_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02];
/// 1.2.840.113549.1.7.5, `digestedData`.
const DIGESTED_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x05];
/// 1.2.840.113549.1.9.16.1.9, `id-ct-compressedData`.
const COMPRESSED_DATA: &[u8] = &[
    0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x10, 0x01, 0x09,
];
/// The encrypted content types `CmsParser` reports, by OID content, with their names.
const ENCRYPTED: [(&[u8], &str); 3] = [
    (
        &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x03],
        "EnvelopedData",
    ),
    (
        &[
            0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x10, 0x01, 0x17,
        ],
        "AuthEnvelopedData",
    ),
    (
        &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x06],
        "EncryptedData",
    ),
];

/// `CmsParser.contentType`: the content of the object identifier that opens the outer SEQUENCE
/// of the first 64 bytes, `None` when they do not hold one, or `Err` for one Bouncy Castle
/// rejects, which the port leaves to Tika's path.
fn header_content_type(input: &[u8]) -> Result<Option<&[u8]>, &'static str> {
    let header = &input[..input.len().min(HEADER_BYTES)];
    if header.len() < 2 || header[0] != SEQUENCE {
        return Ok(None);
    }
    let mut index = 2;
    let length = usize::from(header[1]);
    if length > 0x80 {
        index += length & 0x7F;
    }
    if index + 2 > header.len() || header[index] != OBJECT_IDENTIFIER {
        return Ok(None);
    }
    let oid_length = usize::from(header[index + 1]);
    let end = index + 2 + oid_length;
    if oid_length >= 0x80 || end > header.len() {
        return Ok(None);
    }
    let oid = &header[index + 2..end];
    if oid.last().is_none_or(|&last| last & 0x80 != 0) {
        return Err("an object identifier Bouncy Castle rejects");
    }
    Ok(Some(oid))
}

/// The name of the encrypted content type `input` opens with.
fn encryption(input: &[u8]) -> Option<&'static str> {
    let oid = header_content_type(input).ok().flatten()?;
    ENCRYPTED
        .iter()
        .find(|(encrypted, _)| *encrypted == oid)
        .map(|&(_, name)| name)
}

/// A BER length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    Definite(usize),
    Indefinite,
}

/// A reader of the BER encoding, the elements it reads limited to the forms the port knows
/// Bouncy Castle reads alike: one-byte tags, lengths of up to four bytes.
struct Ber<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Ber<'a> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    fn header(&mut self) -> Option<(u8, Length)> {
        let tag = self.byte()?;
        if tag & 0x1F == 0x1F {
            return None;
        }
        let first = self.byte()?;
        let length = match first {
            0x80 => return (tag & 0x20 != 0).then_some((tag, Length::Indefinite)),
            0x00..=0x7F => usize::from(first),
            0x81..=0x84 => (0..first & 0x7F).try_fold(0usize, |length, _| {
                Some(length << 8 | usize::from(self.byte()?))
            })?,
            _ => return None,
        };
        (self.bytes.len() - self.at >= length).then_some((tag, Length::Definite(length)))
    }

    fn take(&mut self, length: usize) -> &'a [u8] {
        let bytes = &self.bytes[self.at..self.at + length];
        self.at += length;
        bytes
    }

    /// Whether an end-of-contents marker comes next, which it reads.
    fn end_of_contents(&mut self) -> bool {
        let ends = self.bytes.get(self.at..self.at + 2) == Some(&[0, 0]);
        if ends {
            self.at += 2;
        }
        ends
    }

    /// Reads past the element whose header was `length`, nested indefinite lengths included.
    fn skip(&mut self, length: Length) -> Option<()> {
        match length {
            Length::Definite(length) => {
                self.take(length);
            }
            Length::Indefinite => {
                while !self.end_of_contents() {
                    let (_, length) = self.header()?;
                    self.skip(length)?;
                }
            }
        }
        Some(())
    }

    /// Whether the constructed element whose header was `length`, and which started at `start`,
    /// has more elements, an indefinite one's end-of-contents marker read.
    fn has_more(&mut self, start: usize, length: Length) -> bool {
        match length {
            Length::Definite(length) => self.at < start + length,
            Length::Indefinite => !self.end_of_contents(),
        }
    }
}

impl<'a> Ber<'a> {
    /// Reads the header of the next element, which must be `tag`.
    fn expect(&mut self, tag: u8) -> Option<Length> {
        let (found, length) = self.header()?;
        (found == tag).then_some(length)
    }

    /// Reads an object identifier, returning its content.
    fn object_identifier(&mut self) -> Option<&'a [u8]> {
        let Length::Definite(length) = self.expect(OBJECT_IDENTIFIER)? else {
            return None;
        };
        Some(self.take(length))
    }

    /// Reads an `AlgorithmIdentifier`: an object identifier and, at most, its parameters.
    fn algorithm_identifier(&mut self) -> Option<()> {
        let identifier = self.expect(SEQUENCE)?;
        let start = self.at;
        self.object_identifier()?;
        if self.has_more(start, identifier) {
            let (_, parameters) = self.header()?;
            self.skip(parameters)?;
            if self.has_more(start, identifier) {
                return None;
            }
        }
        Some(())
    }

    /// The content of an OCTET STRING, primitive or constructed of nested ones.
    fn octet_string(&mut self, content: &mut Vec<u8>) -> Option<()> {
        match self.header()? {
            (OCTET_STRING, Length::Definite(length)) => {
                content.extend_from_slice(self.take(length))
            }
            (CONSTRUCTED_OCTET_STRING, length) => {
                let start = self.at;
                while self.has_more(start, length) {
                    self.octet_string(content)?;
                }
            }
            _ => return None,
        }
        Some(())
    }

    /// Reads an encapsulated content info: its content type, then the content of its OCTET
    /// STRING, if it has one.
    fn encapsulated_content(&mut self) -> Option<Option<Vec<u8>>> {
        let encapsulated = self.expect(SEQUENCE)?;
        let start = self.at;
        self.object_identifier()?;
        if !self.has_more(start, encapsulated) {
            return Some(None);
        }
        self.expect(EXPLICIT_0)?;
        let mut content = Vec::new();
        self.octet_string(&mut content)?;
        Some(Some(content))
    }
}

/// The content type of `input` as `CmsClassifier.contentType` reads it, `None` when it is not a
/// SEQUENCE, or `Err` for one the port does not read as Bouncy Castle does.
fn classify(input: &[u8]) -> Result<Option<&[u8]>, &'static str> {
    if input.first() != Some(&SEQUENCE) {
        return Ok(None);
    }
    let mut ber = Ber {
        bytes: input,
        at: 0,
    };
    ber.expect(SEQUENCE)
        .and_then(|_| ber.object_identifier())
        .map(Some)
        .ok_or("CMS data the port does not read as Bouncy Castle does")
}

/// The content of signed data, as `CMSSignedDataParser.getSignedContent` reads it: the content
/// info, the version and digest algorithms of the signed data, then its encapsulated content
/// info; `None` where the port does not read it as Bouncy Castle does.
fn signed_content(input: &[u8]) -> Option<Option<Vec<u8>>> {
    let mut ber = Ber {
        bytes: input,
        at: 0,
    };
    ber.expect(SEQUENCE)?;
    if ber.object_identifier()? != SIGNED_DATA {
        return None;
    }
    ber.expect(EXPLICIT_0)?;
    ber.expect(SEQUENCE)?;
    let Length::Definite(length) = ber.expect(INTEGER)? else {
        return None;
    };
    ber.take(length);
    let algorithms = ber.expect(SET)?;
    let start = ber.at;
    while ber.has_more(start, algorithms) {
        ber.algorithm_identifier()?;
    }
    ber.encapsulated_content()
}

/// The content of compressed data, inflated as `CMSCompressedDataParser.getContent` with a
/// `ZlibExpanderProvider` inflates it, within `Pkcs7Parser`'s bound; `None` where the port does
/// not read it as Bouncy Castle does, the zlib stream does not end, or the content passes the
/// bound, past which Tika drops the rest and records a warning.
fn compressed_content(input: &[u8]) -> Option<Vec<u8>> {
    let mut ber = Ber {
        bytes: input,
        at: 0,
    };
    ber.expect(SEQUENCE)?;
    if ber.object_identifier()? != COMPRESSED_DATA {
        return None;
    }
    ber.expect(EXPLICIT_0)?;
    ber.expect(SEQUENCE)?;
    let Length::Definite(length) = ber.expect(INTEGER)? else {
        return None;
    };
    ber.take(length);
    ber.algorithm_identifier()?;
    let compressed = ber.encapsulated_content()??;
    zlib::decode(&compressed)
        .ok()
        .filter(|inflated| inflated.len() <= MAX_DECOMPRESSED)
}

/// Whether `metadata` types the document `application/pkcs7-signature`.
fn typed_as_signature(metadata: &TikaMetadata) -> bool {
    metadata
        .get("Content-Type")
        .and_then(MediaType::parse)
        .is_some_and(|kind| kind.base_type().as_str() == SIGNATURE)
}

/// What `CmsParser`, and the `Pkcs7Parser` it hands what it does not report itself, write for
/// `input`, typed by `metadata`, when it is not encrypted, or why Tika's path is left to read it.
fn prepare_unencrypted<'a>(
    input: &[u8],
    metadata: &TikaMetadata,
    configs: &'a ParserConfigs,
) -> Result<Prepared<'a>, Deferral> {
    let unread = || "CMS data the port does not read as Bouncy Castle does".to_owned();
    if typed_as_signature(metadata) {
        return match signed_content(input).ok_or_else(unread)? {
            None => Ok(empty(None)),
            Some(signed) => with_content(SIGNED, signed, configs),
        };
    }
    match classify(input).map_err(str::to_owned)? {
        Some(SIGNED_DATA) => match signed_content(input).ok_or_else(unread)? {
            Some(signed) => with_content(SIGNED, signed, configs),
            None => Err("signed data whose kind Bouncy Castle tells by its signers".to_owned()),
        },
        Some(COMPRESSED_DATA) => with_content(
            COMPRESSED,
            compressed_content(input).ok_or_else(unread)?,
            configs,
        ),
        Some(DIGESTED_DATA) => Ok(empty(Some(DIGESTED))),
        _ => Ok(empty(None)),
    }
}

/// An empty document, its `Content-Type` refined to `content_type`, if any.
fn empty<'a>(content_type: Option<&'static str>) -> Prepared<'a> {
    Box::new(move |xhtml| {
        if let Some(content_type) = content_type {
            xhtml.metadata_mut().set("Content-Type", content_type);
        }
        Ok(())
    })
}

/// The `Content-Type` refined to `content_type`, then `content` parsed by the parser Tika's
/// `AutoDetectParser` routes it to, from the metadata it starts from, into the same handler; or
/// why Tika's path is left to read it, when that parser is not native or declines it. The parser
/// prepares the content here, to tell whether it takes it, and again as the document is written,
/// as what it prepares borrows the content, which the document owns.
fn with_content<'a>(
    content_type: &'static str,
    content: Vec<u8>,
    configs: &'a ParserConfigs,
) -> Result<Prepared<'a>, Deferral> {
    let (parser, start) =
        formats::embedded_parser(&content, &TikaMetadata::new(), Opened::Alone)
            .map_err(|unrouted| format!("content Tika's path parses: {unrouted:?}"))?;
    if parser.prepare(&content, &start, configs).is_err() {
        return Err(format!("content {} declines", parser.name()));
    }
    Ok(Box::new(move |xhtml| {
        xhtml.metadata_mut().set("Content-Type", content_type);
        xhtml.record_parser(parser.name());
        let prepared = parser
            .prepare(&content, &start, configs)
            .map_err(|declined| {
                Failure::Extraction(Error::ParseError(format!(
                    "{} declined CMS content it had taken: {declined:?}",
                    parser.name()
                )))
            })?;
        xhtml.with_metadata(start, prepared)
    }))
}

/// Our Java `CmsParser`.
pub(crate) struct CmsParser;

impl Parser for CmsParser {
    fn name(&self) -> &'static str {
        "extractous::CmsParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "ai.yobix.CmsParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/pkcs7-mime", SIGNATURE]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        header_content_type(input).map_err(str::to_owned)?;
        if let Some(name) = encryption(input) {
            return Err(Declined::Fails(Error::Encrypted(format!(
                "S/MIME (CMS {name}) data; decrypting it needs the recipient's private key"
            ))));
        }
        Ok(prepare_unencrypted(input, metadata, configs)?)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

    const TIKA_NAME: &str = "ai.yobix.CmsParser";
    /// The detached signatures and certificate lists of Tika's tests, which Tika's path writes
    /// as empty documents.
    const DETACHED: [&str; 5] = [
        "testDetached.p7s",
        "testPKCS7_certs_only_def.p7c",
        "testPKCS7_certs_only_ind.p7c",
        "testPKCS7_signature_def.p7s",
        "testPKCS7_signature_ind.p7s",
    ];
    /// Enveloped data, which both paths report as encrypted.
    const ENVELOPED: [&str; 2] = ["testPKCS7_enveloped_def.p7m", "testPKCS7_enveloped_ind.p7m"];

    #[test]
    fn reads_the_content_type_as_cms_parser_does() {
        let enveloped = [&[0x30, 0x80, 0x06, 0x09][..], ENCRYPTED[0].0, &[0xA0, 0x80]].concat();
        assert_eq!(encryption(&enveloped), Some("EnvelopedData"));
        let long_form = [&[0x30, 0x82, 0x01, 0x00, 0x06, 0x09][..], ENCRYPTED[2].0].concat();
        assert_eq!(encryption(&long_form), Some("EncryptedData"));
        assert_eq!(header_content_type(b"-----BEGIN PKCS7-----"), Ok(None));
        assert!(header_content_type(&[0x30, 0x03, 0x06, 0x01, 0x81]).is_err());
    }

    /// A DER element of `tag` holding `content`.
    fn der(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut element = vec![tag];
        match content.len() {
            length @ 0..=0x7F => element.push(length as u8),
            length @ 0x80..=0xFF => element.extend([0x81, length as u8]),
            length => element.extend([0x82, (length >> 8) as u8, length as u8]),
        }
        element.extend_from_slice(content);
        element
    }

    /// A BER element of `tag` holding `elements`, of indefinite length.
    fn ber(tag: u8, elements: &[Vec<u8>]) -> Vec<u8> {
        [vec![tag, 0x80], elements.concat(), vec![0, 0]].concat()
    }

    const DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x01];
    const SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
    const ZLIB: &[u8] = &[
        0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x10, 0x03, 0x08,
    ];

    /// Signed data of `content`, as `encapsulated` encodes it, without signers.
    fn signed(encapsulated: Vec<u8>) -> Vec<u8> {
        let algorithm = der(
            SEQUENCE,
            &[der(OBJECT_IDENTIFIER, SHA256), der(0x05, &[])].concat(),
        );
        let signed_data = der(
            SEQUENCE,
            &[
                der(INTEGER, &[1]),
                der(SET, &algorithm),
                encapsulated,
                der(SET, &[]),
            ]
            .concat(),
        );
        der(
            SEQUENCE,
            &[
                der(OBJECT_IDENTIFIER, SIGNED_DATA),
                der(EXPLICIT_0, &signed_data),
            ]
            .concat(),
        )
    }

    fn encapsulated(content: &[u8]) -> Vec<u8> {
        der(
            SEQUENCE,
            &[
                der(OBJECT_IDENTIFIER, DATA),
                der(EXPLICIT_0, &der(OCTET_STRING, content)),
            ]
            .concat(),
        )
    }

    /// Compressed data of `content`.
    fn compressed(content: &[u8]) -> Vec<u8> {
        let compressed_data = der(
            SEQUENCE,
            &[
                der(INTEGER, &[0]),
                der(SEQUENCE, &der(OBJECT_IDENTIFIER, ZLIB)),
                encapsulated(&zlib::tests::zlib(content)),
            ]
            .concat(),
        );
        der(
            SEQUENCE,
            &[
                der(OBJECT_IDENTIFIER, COMPRESSED_DATA),
                der(EXPLICIT_0, &compressed_data),
            ]
            .concat(),
        )
    }

    const XML: &[u8] = br#"<?xml version="1.0"?><r xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Signed</dc:title>Some signed text.</r>"#;

    #[test]
    fn reads_content_as_tikas_path_does() {
        let scratch = Scratch::new("cms");
        let split = ber(
            SEQUENCE,
            &[
                der(OBJECT_IDENTIFIER, DATA),
                ber(
                    EXPLICIT_0,
                    &[ber(
                        CONSTRUCTED_OCTET_STRING,
                        &[der(OCTET_STRING, &XML[..20]), der(OCTET_STRING, &XML[20..])],
                    )],
                ),
            ],
        );
        let read = [
            scratch.write("signed.p7m", &signed(encapsulated(XML))),
            scratch.write("split.p7m", &signed(split)),
            scratch.write(
                "signed-text.p7s",
                &signed(encapsulated(b"Plain signed text.")),
            ),
            scratch.write(
                "compressed.p7z",
                &compressed(b"Compressed text, compressed text."),
            ),
            scratch.write(
                "detached.p7s",
                &signed(der(SEQUENCE, &der(OBJECT_IDENTIFIER, DATA))),
            ),
        ];
        let (parsed, deferred) = documents(&CmsParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
    }

    #[test]
    fn reads_the_signed_local_document_as_tikas_path_does() {
        let (parsed, deferred) = documents(&CmsParser, &[Path::new("../test_files/documents")]);
        assert!(deferred.is_empty(), "{deferred:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    /// Tika's CMS test documents the native path leaves to Tika's: compressed PDF content, which
    /// no native parser takes (the document is in two of the corpus's modules), digested data
    /// typed as a signature, which Bouncy Castle fails to read as signed data, and signed data
    /// in PEM, which it cannot read at all.
    const LEFT_TO_TIKA: [&str; 4] = [
        "testPKCS7_compressed_def_long.p7z",
        "testPKCS7_compressed_def_long.p7z",
        "testPKCS7_digested.p7",
        "testPKCS7_signed_data_def.pem",
    ];

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn reads_tikas_test_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&CmsParser, &[&conformance_corpus()]);
        let mut left: Vec<String> = deferred
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, LEFT_TO_TIKA);
        let (enveloped, rest): (Vec<&Path>, Vec<&Path>) = parsed
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| ENVELOPED.iter().any(|name| path.ends_with(name)));
        assert_eq!(enveloped.len(), ENVELOPED.len());
        assert!(DETACHED
            .iter()
            .all(|name| rest.iter().any(|path| path.ends_with(name))));
        assert_same_as_tika(&rest, TIKA_NAME);
        assert_fails_as_tika(&enveloped);
        let error = crate::Extractor::new()
            .extract_file_to_string(enveloped[0].to_str().unwrap())
            .unwrap_err();
        assert!(matches!(error, Error::Encrypted(_)), "{error:?}");
    }
}
