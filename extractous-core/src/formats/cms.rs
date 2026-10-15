//! CMS (PKCS #7) data, as our Java `CmsParser` reports it: encrypted data (enveloped,
//! authenticated-enveloped or encrypted) as encrypted, before the document starts; a detached
//! signature, signed data typed `application/pkcs7-signature` without content of its own, as an
//! empty document.
//!
//! Whether signed data carries its content is read from the start of the BER encoding Bouncy
//! Castle's streaming `CMSSignedDataParser` reads before it answers: the content info, the
//! version and digest algorithms of the signed data, and its encapsulated content info. Signed
//! data with content, which Tika's `Pkcs7Parser` parses with the whole of Tika, and everything
//! else stay on Tika's path.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};
use crate::mime::MediaType;

/// The bytes `CmsParser` reads the content type from.
const HEADER_BYTES: usize = 64;
const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const OBJECT_IDENTIFIER: u8 = 0x06;
const EXPLICIT_0: u8 = 0xA0;
const SIGNATURE: &str = "application/pkcs7-signature";
/// 1.2.840.113549.1.7.2, `signedData`.
const SIGNED_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02];
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

/// Whether signed data `input` encapsulates its content, as `CMSSignedDataParser` finds it,
/// `None` where the port does not read it as Bouncy Castle does.
fn encapsulates_content(input: &[u8]) -> Option<bool> {
    let mut ber = Ber {
        bytes: input,
        at: 0,
    };
    let (SEQUENCE, _) = ber.header()? else {
        return None;
    };
    let (OBJECT_IDENTIFIER, Length::Definite(length)) = ber.header()? else {
        return None;
    };
    if ber.take(length) != SIGNED_DATA {
        return None;
    }
    let (EXPLICIT_0, _) = ber.header()? else {
        return None;
    };
    let (SEQUENCE, _) = ber.header()? else {
        return None;
    };
    let (INTEGER, Length::Definite(length)) = ber.header()? else {
        return None;
    };
    ber.take(length);
    let (SET, algorithms) = ber.header()? else {
        return None;
    };
    let start = ber.at;
    while ber.has_more(start, algorithms) {
        let (SEQUENCE, identifier) = ber.header()? else {
            return None;
        };
        let identifier_start = ber.at;
        let (OBJECT_IDENTIFIER, Length::Definite(length)) = ber.header()? else {
            return None;
        };
        ber.take(length);
        if ber.has_more(identifier_start, identifier) {
            let (_, parameters) = ber.header()?;
            ber.skip(parameters)?;
            if ber.has_more(identifier_start, identifier) {
                return None;
            }
        }
    }
    let (SEQUENCE, encapsulated) = ber.header()? else {
        return None;
    };
    let start = ber.at;
    let (OBJECT_IDENTIFIER, Length::Definite(length)) = ber.header()? else {
        return None;
    };
    ber.take(length);
    if !ber.has_more(start, encapsulated) {
        return Some(false);
    }
    let (EXPLICIT_0, _) = ber.header()? else {
        return None;
    };
    Some(true)
}

/// Whether `metadata` types the document `application/pkcs7-signature`.
fn typed_as_signature(metadata: &TikaMetadata) -> bool {
    metadata
        .get("Content-Type")
        .and_then(MediaType::parse)
        .is_some_and(|kind| kind.base_type().as_str() == SIGNATURE)
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

    fn defers(
        &self,
        input: &[u8],
        metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<String> {
        if let Err(why) = header_content_type(input) {
            return Some(why.to_owned());
        }
        if encryption(input).is_some() {
            return None;
        }
        if !typed_as_signature(metadata) {
            return Some("CMS data Tika's Pkcs7Parser reads".to_owned());
        }
        match encapsulates_content(input) {
            Some(false) => None,
            Some(true) => Some("signed data that encapsulates its content".to_owned()),
            None => Some("CMS data the port does not read as Bouncy Castle does".to_owned()),
        }
    }

    fn fails_before_starting(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<Error> {
        encryption(input).map(|name| {
            Error::Encrypted(format!(
                "S/MIME (CMS {name}) data; decrypting it needs the recipient's private key"
            ))
        })
    }

    /// A detached signature: an empty document.
    fn parse(
        &self,
        _input: &[u8],
        _configs: &ParserConfigs,
        _xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents,
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

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn reads_tikas_test_signatures_as_tikas_path_does() {
        let (parsed, _) = documents(&CmsParser, &[&conformance_corpus()]);
        let named = |names: &[&str]| -> Vec<PathBuf> {
            parsed
                .iter()
                .filter(|path| names.iter().any(|name| path.ends_with(name)))
                .cloned()
                .collect()
        };
        let detached = named(&DETACHED);
        assert_eq!(detached.len(), DETACHED.len());
        assert_same_as_tika(
            &detached.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
        let enveloped = named(&ENVELOPED);
        assert_eq!(enveloped.len(), ENVELOPED.len());
        let paths: Vec<&Path> = enveloped.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&paths);
        let error = crate::Extractor::new()
            .extract_file_to_string(paths[0].to_str().unwrap())
            .unwrap_err();
        assert!(matches!(error, Error::Encrypted(_)), "{error:?}");
    }
}
