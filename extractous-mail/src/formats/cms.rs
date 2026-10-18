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
//! content info, its OCTET STRING primitive or constructed; tags of any number, lengths of any
//! form Bouncy Castle takes. Content no native parser takes, and data laid out otherwise than
//! the port reads as Bouncy Castle does, stay on Tika's path. Data Bouncy Castle fails to read
//! as signed or compressed data, such as digested data or PEM text typed as a signature, or a
//! malformed encoding, fails as it fails on Tika's path; data whose content type it fails to
//! read is no CMS data to Tika, an empty document.

use crate::compress::{zlib, Limited};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{self, Decline, Deferral, Failure, Footprint, Opened, Parser, Prepared};
use crate::input::{Input, Pages};
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
const CERTS_ONLY: &str = "application/pkcs7-mime; smime-type=certs-only";
/// The tag of the certificates of signed data, `[0] IMPLICIT`, and of its CRLs, `[1] IMPLICIT`.
const CERTIFICATES: u8 = 0xA0;
const CRLS: u8 = 0xA1;
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
    Definite(u64),
    Indefinite,
}

/// Why the port reads no further in CMS data.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Unread {
    /// Bouncy Castle throws this exception reading the encoding.
    Thrown(String),
    /// The data is laid out otherwise than the port reads as Bouncy Castle does.
    Unported,
}

type Read<T> = Result<T, Unread>;

/// The `IOException` Bouncy Castle throws on a malformed encoding.
fn corrupt(message: &str) -> Unread {
    Unread::Thrown(format!("java.io.IOException: {message}"))
}

/// The `EOFException` Bouncy Castle throws where the encoding ends early.
fn eof(message: &str) -> Unread {
    Unread::Thrown(format!("java.io.EOFException: {message}"))
}

/// The tag Bouncy Castle reads of an element: its first byte, which holds the class and
/// whether it is constructed, and its number, from the bytes after it where the first's low
/// five bits are all set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tag {
    byte: u8,
    number: u32,
}

/// A reader of the BER encoding as Bouncy Castle's `ASN1StreamParser` reads it: tags of any
/// number, definite lengths of up to 63 bits however many bytes they take, indefinite lengths
/// of constructed elements, and the content of an element read only as far as the input holds
/// it.
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

    /// `readTagNumber` and `readLongLength`: the next element's tag and length.
    fn tagged_header(&mut self) -> Read<(Tag, Length)> {
        let byte = self
            .byte()
            .ok_or_else(|| eof("EOF found when tag expected"))?;
        let mut number = u32::from(byte & 0x1F);
        if number == 0x1F {
            let mut next = self
                .byte()
                .ok_or_else(|| eof("EOF found inside tag value."))?;
            if next < 31 {
                return Err(corrupt("corrupted stream - high tag number < 31 found"));
            }
            number = u32::from(next & 0x7F);
            if number == 0 {
                return Err(corrupt("corrupted stream - invalid high tag number found"));
            }
            while next & 0x80 != 0 {
                if number >> 24 != 0 {
                    return Err(corrupt("Tag number more than 31 bits"));
                }
                next = self
                    .byte()
                    .ok_or_else(|| eof("EOF found inside tag value."))?;
                number = number << 7 | u32::from(next & 0x7F);
            }
        }
        let first = self
            .byte()
            .ok_or_else(|| eof("EOF found when length expected"))?;
        let length = match first {
            0x00..=0x7F => Length::Definite(u64::from(first)),
            0x80 if byte & 0x20 == 0 => {
                return Err(corrupt("indefinite-length primitive encoding encountered"))
            }
            0x80 => Length::Indefinite,
            0xFF => return Err(corrupt("invalid long form definite-length 0xFF")),
            _ => {
                let mut length = 0u64;
                for _ in 0..first & 0x7F {
                    let octet = self.byte().ok_or_else(|| eof("EOF found reading length"))?;
                    if length >> 55 != 0 {
                        return Err(corrupt("long form definite-length more than 63 bits"));
                    }
                    length = length << 8 | u64::from(octet);
                }
                Length::Definite(length)
            }
        };
        Ok((Tag { byte, number }, length))
    }

    /// The next element's tag byte and length.
    fn header(&mut self) -> Read<(u8, Length)> {
        self.tagged_header().map(|(tag, length)| (tag.byte, length))
    }

    /// The `length` bytes of a definite-length element's content, which the input must hold.
    fn take(&mut self, length: u64) -> Read<&'a [u8]> {
        let left = (self.bytes.len() - self.at) as u64;
        if length > left {
            return Err(eof(&format!(
                "DEF length {length} object truncated by {}",
                length - left
            )));
        }
        let bytes = &self.bytes[self.at..self.at + length as usize];
        self.at += bytes.len();
        Ok(bytes)
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
    fn skip(&mut self, length: Length) -> Read<()> {
        match length {
            Length::Definite(length) => {
                self.take(length)?;
            }
            Length::Indefinite => {
                while !self.end_of_contents() {
                    let (_, length) = self.header()?;
                    self.skip(length)?;
                }
            }
        }
        Ok(())
    }

    /// Whether the constructed element whose header was `length`, and which started at `start`,
    /// has more elements, an indefinite one's end-of-contents marker read.
    fn has_more(&mut self, start: usize, length: Length) -> bool {
        match length {
            Length::Definite(length) => (self.at as u64) < start as u64 + length,
            Length::Indefinite => !self.end_of_contents(),
        }
    }
}

impl<'a> Ber<'a> {
    /// Reads the header of the next element, which must be `tag`.
    fn expect(&mut self, tag: u8) -> Read<Length> {
        let (found, length) = self.header()?;
        if found == tag {
            Ok(length)
        } else {
            Err(Unread::Unported)
        }
    }

    /// Reads an object identifier, returning its content.
    fn object_identifier(&mut self) -> Read<&'a [u8]> {
        let Length::Definite(length) = self.expect(OBJECT_IDENTIFIER)? else {
            return Err(Unread::Unported);
        };
        self.take(length)
    }

    /// Reads an `AlgorithmIdentifier`: an object identifier and, at most, its parameters.
    fn algorithm_identifier(&mut self) -> Read<()> {
        let identifier = self.expect(SEQUENCE)?;
        let start = self.at;
        self.object_identifier()?;
        if self.has_more(start, identifier) {
            let (_, parameters) = self.header()?;
            self.skip(parameters)?;
            if self.has_more(start, identifier) {
                return Err(Unread::Unported);
            }
        }
        Ok(())
    }

    /// The content of an OCTET STRING, primitive or constructed of nested ones: the pieces of
    /// the input it is made of.
    fn octet_string(&mut self, content: &mut Vec<&'a [u8]>) -> Read<()> {
        match self.header()? {
            (OCTET_STRING, Length::Definite(length)) => content.push(self.take(length)?),
            (CONSTRUCTED_OCTET_STRING, length) => {
                let start = self.at;
                while self.has_more(start, length) {
                    self.octet_string(content)?;
                }
            }
            _ => return Err(Unread::Unported),
        }
        Ok(())
    }

    /// Reads the elements of a SET or tagged collection of `length`, returning how many of them
    /// are SEQUENCEs.
    fn sequences(&mut self, length: Length) -> Read<usize> {
        let start = self.at;
        let mut sequences = 0;
        while self.has_more(start, length) {
            let (tag, element) = self.header()?;
            self.skip(element)?;
            sequences += usize::from(tag == SEQUENCE);
        }
        Ok(sequences)
    }

    /// `refinedSignedType` of signed data without encapsulated content, read on from after its
    /// encapsulated content info: certificates and no signers make it certs-only, anything
    /// else a detached signature. Only X.509 certificates, SEQUENCEs, count as certificates.
    fn detached_type(&mut self) -> Read<&'static str> {
        let (mut tag, mut length) = self.header()?;
        let mut certificates = 0;
        if tag == CERTIFICATES {
            certificates = self.sequences(length)?;
            (tag, length) = self.header()?;
        }
        if tag == CRLS {
            self.sequences(length)?;
            (tag, length) = self.header()?;
        }
        if tag != SET {
            return Err(Unread::Unported);
        }
        let signers = self.sequences(length)?;
        Ok(if certificates > 0 && signers == 0 {
            CERTS_ONLY
        } else {
            SIGNATURE
        })
    }

    /// Reads an encapsulated content info: its content type, then the content of its OCTET
    /// STRING, if it has one.
    fn encapsulated_content(&mut self) -> Read<Option<Vec<&'a [u8]>>> {
        let encapsulated = self.expect(SEQUENCE)?;
        let start = self.at;
        self.object_identifier()?;
        if !self.has_more(start, encapsulated) {
            return Ok(None);
        }
        self.expect(EXPLICIT_0)?;
        let mut content = Vec::new();
        self.octet_string(&mut content)?;
        Ok(Some(content))
    }
}

/// The content type of `input` as `CmsClassifier.contentType` reads it: the object identifier
/// that opens its outer SEQUENCE, none where it is not one, which Tika takes for data that is
/// not CMS, whatever Bouncy Castle throws reading it.
fn classify(input: &[u8]) -> Option<&[u8]> {
    if input.first() != Some(&SEQUENCE) {
        return None;
    }
    let mut ber = Ber {
        bytes: input,
        at: 0,
    };
    ber.expect(SEQUENCE).ok()?;
    ber.object_identifier().ok()
}

/// What `CMSSignedDataParser` makes of data it is given as signed data.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Signed<'a> {
    /// The encapsulated content: the pieces of the input it is made of.
    Read(Vec<&'a [u8]>),
    /// No encapsulated content, and the type `refinedSignedType` gives the data, reading its
    /// certificates and signers, or why it gives none.
    Detached(Read<&'static str>),
    /// Bouncy Castle fails on it, with this exception.
    Fails(String),
}

/// Data read as signed data, as `CMSSignedDataParser.getSignedContent` reads it whatever its
/// content type: the content info, the version and digest algorithms of the signed data, then
/// its encapsulated content info; `None` where the port does not read it as Bouncy Castle does.
fn signed_content(input: &[u8]) -> Option<Signed<'_>> {
    match read_signed(input) {
        Ok(signed) => Some(signed),
        Err(Unread::Thrown(exception)) => Some(Signed::Fails(exception)),
        Err(Unread::Unported) => None,
    }
}

/// The universal types `ASN1StreamParser.readObject` has a parser for when constructed.
const PARSED: [u32; 5] = [0x03, 0x04, 0x08, 0x10, 0x11];

/// [`signed_content`], failing where the port does not read the data on.
fn read_signed(input: &[u8]) -> Read<Signed<'_>> {
    let mut ber = Ber {
        bytes: input,
        at: 0,
    };
    let (tag, length) = ber.tagged_header()?;
    if tag.byte & 0xE0 == 0x20 && !PARSED.contains(&tag.number) {
        let encoding = match length {
            Length::Indefinite => "BER",
            Length::Definite(_) => "DL",
        };
        return Ok(Signed::Fails(format!(
            "org.bouncycastle.asn1.ASN1Exception: unknown {encoding} object encountered: 0x{:x}",
            tag.number
        )));
    }
    if tag.byte != SEQUENCE {
        return Err(Unread::Unported);
    }
    ber.object_identifier()?;
    ber.expect(EXPLICIT_0)?;
    ber.expect(SEQUENCE)?;
    let Length::Definite(length) = ber.expect(INTEGER)? else {
        return Err(Unread::Unported);
    };
    ber.take(length)?;
    let algorithms = match ber.header()? {
        (SET, algorithms) => algorithms,
        (SEQUENCE, _) => {
            return Ok(Signed::Fails(
                "java.lang.ClassCastException: the digest algorithms of the signed data are a \
                 SEQUENCE, where an ASN1SetParser is expected"
                    .to_owned(),
            ))
        }
        _ => return Err(Unread::Unported),
    };
    let start = ber.at;
    while ber.has_more(start, algorithms) {
        ber.algorithm_identifier()?;
    }
    Ok(match ber.encapsulated_content()? {
        Some(content) => Signed::Read(content),
        None => Signed::Detached(ber.detached_type()),
    })
}

/// The failure of a document Bouncy Castle fails to read as signed data with `exception`.
fn unreadable_signed_data(exception: String) -> Decline<'static> {
    Decline::Fails(Error::ParseError(format!(
        "Unable to read the CMS signed data: {exception}"
    )))
}

/// The content of compressed data, inflated as `CMSCompressedDataParser.getContent` with a
/// `ZlibExpanderProvider` inflates it, to a spool, within `Pkcs7Parser`'s bound; `None` where
/// the port does not read it as Bouncy Castle does, the zlib stream does not end, or the content
/// passes the bound, past which Tika drops the rest and records a warning; or why Bouncy Castle
/// fails to read it, or the compressed content, in pieces, cannot be spooled whole.
fn compressed_content(input: &[u8]) -> Result<Option<Input<'static>>, Error> {
    let pieces = match compressed_pieces(input) {
        Ok(Some(pieces)) => pieces,
        Ok(None) | Err(Unread::Unported) => return Ok(None),
        Err(Unread::Thrown(exception)) => {
            return Err(Error::ParseError(format!(
                "Unable to parse pkcs7 compressed data: {exception}"
            )))
        }
    };
    let compressed = Input::joined(&pieces, Pages::IN_MEMORY).map_err(|error| {
        Error::IoError(format!(
            "spooling the compressed CMS content fails: {error}"
        ))
    })?;
    Ok(zlib::decode(&compressed)
        .and_then(Limited::whole)
        .ok()
        .filter(|inflated| inflated.len() <= MAX_DECOMPRESSED))
}

/// The pieces of the input the compressed content of compressed data is made of, none where it
/// has none.
fn compressed_pieces(input: &[u8]) -> Read<Option<Vec<&[u8]>>> {
    let mut ber = Ber {
        bytes: input,
        at: 0,
    };
    ber.expect(SEQUENCE)?;
    if ber.object_identifier()? != COMPRESSED_DATA {
        return Err(Unread::Unported);
    }
    ber.expect(EXPLICIT_0)?;
    ber.expect(SEQUENCE)?;
    let Length::Definite(length) = ber.expect(INTEGER)? else {
        return Err(Unread::Unported);
    };
    ber.take(length)?;
    ber.algorithm_identifier()?;
    ber.encapsulated_content()
}

/// Whether `metadata` types the document `application/pkcs7-signature`.
fn typed_as_signature(metadata: &TikaMetadata) -> bool {
    metadata
        .get("Content-Type")
        .and_then(MediaType::parse)
        .is_some_and(|kind| kind.base_type().as_str() == SIGNATURE)
}

/// What `CmsParser`, and the `Pkcs7Parser` it hands what it does not report itself, write for
/// `input`, typed by `metadata`, when it is not encrypted; how it fails, as Tika's path fails it;
/// or why Tika's path is left to read it.
fn prepare_unencrypted<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    metadata: &TikaMetadata,
    configs: &'a ParserConfigs,
) -> Result<Prepared<'a>, Decline<'a>> {
    let unread = || "CMS data the port does not read as Bouncy Castle does".to_owned();
    let signed = |pieces: Vec<&'a [u8]>| -> Result<Prepared<'a>, Decline<'a>> {
        let content = Input::joined(&pieces, pages).map_err(|error| {
            Decline::Fails(Error::IoError(format!(
                "spooling the signed CMS content fails: {error}"
            )))
        })?;
        Ok(with_content(SIGNED, content, configs)?)
    };
    if typed_as_signature(metadata) {
        return match signed_content(input).ok_or_else(unread)? {
            Signed::Detached(_) => Ok(empty(None)),
            Signed::Read(pieces) => signed(pieces),
            Signed::Fails(exception) => Err(unreadable_signed_data(exception)),
        };
    }
    match classify(input) {
        Some(SIGNED_DATA) => match signed_content(input).ok_or_else(unread)? {
            Signed::Read(pieces) => signed(pieces),
            Signed::Detached(Ok(refined)) => Ok(empty(Some(refined))),
            Signed::Detached(Err(Unread::Unported)) => Err(Decline::Deferred(unread())),
            Signed::Detached(Err(Unread::Thrown(exception))) | Signed::Fails(exception) => {
                Err(unreadable_signed_data(exception))
            }
        },
        Some(COMPRESSED_DATA) => Ok(with_content(
            COMPRESSED,
            compressed_content(input)
                .map_err(Decline::Fails)?
                .ok_or_else(unread)?,
            configs,
        )?),
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
/// as what it prepares borrows the content, which the document owns: the input's bytes in
/// place, or a spool.
///
/// The content's metadata, which the head shows, starts with its `Content-Length`. Tika's
/// `Pkcs7Parser` parses it from fresh metadata, as a stream opened alone, which never learns
/// its length (#113).
fn with_content<'a>(
    content_type: &'static str,
    content: Input<'a>,
    configs: &'a ParserConfigs,
) -> Result<Prepared<'a>, Deferral> {
    let mut given = TikaMetadata::new();
    given.set("Content-Length", content.len().to_string());
    let (parser, start) = formats::embedded_parser(&content, &given, Opened::Alone)
        .map_err(|unrouted| format!("content Tika's path parses: {unrouted:?}"))?;
    if parser
        .prepare_paged(&content, content.pages(), &start, configs)
        .is_err()
    {
        return Err(format!("content {} declines", parser.name()));
    }
    Ok(Box::new(move |xhtml| {
        xhtml.metadata_mut().set("Content-Type", content_type);
        xhtml.record_parser(parser.name());
        let prepared = parser
            .prepare_paged(&content, content.pages(), &start, configs)
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
pub struct CmsParser;

impl Parser for CmsParser {
    fn name(&self) -> &'static str {
        "extractous::CmsParser"
    }
    fn tika_name(&self) -> &'static str {
        "ai.yobix.CmsParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/pkcs7-mime", SIGNATURE]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    /// Reads the content info in place: signed content of one OCTET STRING stays the input's
    /// bytes, content in several is spooled, and compressed content is inflated to a spool.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        header_content_type(input).map_err(str::to_owned)?;
        if let Some(name) = encryption(input) {
            return Err(Decline::Fails(Error::Encrypted(format!(
                "S/MIME (CMS {name}) data; decrypting it needs the recipient's private key"
            ))));
        }
        prepare_unencrypted(input, pages, metadata, configs)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::Scratch;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{assert_fails_as_tika, conformance_corpus, documents};

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "ai.yobix.CmsParser";

    #[test]
    fn records_encrypted_mail_parts_as_encrypted() {
        let (_, metadata) = crate::Extractor::new()
            .extract_file_to_string("../test_files/documents/encrypted/smime-encrypted.eml")
            .unwrap();
        let failures = &metadata["tk:exception:embedded-exception"];
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            failures[0].contains("org.apache.tika.exception.EncryptedDocumentException: "),
            "{failures:?}"
        );
    }

    /// The `meta` element of the `Content-Length` the head of signed or compressed content
    /// shows, which Tika's path does not give the content (#113).
    const CONTENT_LENGTH: &str = "<meta name=\"Content-Length\" content=\"";

    /// A native text without the `Content-Length` of the content of signed or compressed data
    /// in its head, which Tika's path does not give it (#113): the head of such content shows
    /// the content's metadata, which has no resource name, where a document's own has one.
    fn without_content_length(native: &str) -> String {
        let Some(head_end) = native.find("</head>") else {
            return native.to_owned();
        };
        let head = &native[..head_end];
        if head.contains("name=\"tk:resource-name\"") {
            return native.to_owned();
        }
        let Some(start) = head.find(CONTENT_LENGTH) else {
            return native.to_owned();
        };
        let end = start + native[start..].find("/>\n").expect("the meta element ends") + 3;
        format!("{}{}", &native[..start], &native[end..])
    }

    /// A native result as Tika's path writes it: ZIP content's integrity check out of its head
    /// (#99), and the content's `Content-Length` too (#113).
    #[cfg(feature = "tika")]
    fn as_tika_writes_content(
        text: &mut String,
        metadata: &mut crate::formats::parity::ComparedMetadata,
        tika: &crate::formats::parity::ComparedMetadata,
    ) {
        crate::formats::archive::as_tika_checks_zips(text, metadata, tika);
        *text = without_content_length(text);
    }

    /// [`as_tika_writes_content`] for content that is generic XML (#105).
    #[cfg(feature = "tika")]
    fn as_tika_writes_xml(
        text: &mut String,
        metadata: &mut crate::formats::parity::ComparedMetadata,
        tika: &crate::formats::parity::ComparedMetadata,
    ) {
        crate::formats::dc_xml::without_unread_dublin_core(text, metadata, tika);
        *text = without_content_length(text);
    }

    #[test]
    fn gives_the_content_its_length() {
        let scratch = Scratch::new("cms-length");
        let content = b"Plain signed text.";
        let path = scratch.write("signed-text.p7s", &signed(encapsulated(content)));
        let (xhtml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_file_to_string(path.to_str().unwrap())
            .unwrap();
        let length = format!("{CONTENT_LENGTH}{}\" />", content.len());
        assert!(xhtml.contains(&length), "{xhtml}");
        assert!(!xhtml.contains("tk:resource-name"), "{xhtml}");
        assert_eq!(
            metadata.get("tk:resource-name"),
            Some(&vec!["signed-text.p7s".to_owned()])
        );
        assert_eq!(
            without_content_length(&xhtml)
                .matches(CONTENT_LENGTH)
                .count(),
            0
        );
    }
    /// The detached signatures and certificate lists of Tika's tests, which Tika's path writes
    /// as empty documents.
    #[cfg(feature = "tika")]
    const DETACHED: [&str; 5] = [
        "testDetached.p7s",
        "testPKCS7_certs_only_def.p7c",
        "testPKCS7_certs_only_ind.p7c",
        "testPKCS7_signature_def.p7s",
        "testPKCS7_signature_ind.p7s",
    ];
    /// Enveloped data, which both paths report as encrypted.
    #[cfg(feature = "tika")]
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
            length => {
                let bytes = (length as u64).to_be_bytes();
                let first = bytes.iter().position(|&byte| byte != 0).unwrap_or(7);
                element.push(0x80 | (8 - first) as u8);
                element.extend_from_slice(&bytes[first..]);
            }
        }
        element.extend_from_slice(content);
        element
    }

    /// A BER element of `tag` holding `elements`, of indefinite length.
    #[cfg(feature = "tika")]
    fn ber(tag: u8, elements: &[Vec<u8>]) -> Vec<u8> {
        [vec![tag, 0x80], elements.concat(), vec![0, 0]].concat()
    }

    const DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x01];
    const SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
    #[cfg(feature = "tika")]
    const ZLIB: &[u8] = &[
        0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x10, 0x03, 0x08,
    ];

    /// Signed data of `content`, as `encapsulated` encodes it, without signers.
    fn signed(encapsulated: Vec<u8>) -> Vec<u8> {
        signed_with(encapsulated, None, &[])
    }

    /// Signed data of `content`, as `encapsulated` encodes it, with `certificates`, if any, and
    /// `signers`.
    fn signed_with(
        encapsulated: Vec<u8>,
        certificates: Option<&[Vec<u8>]>,
        signers: &[Vec<u8>],
    ) -> Vec<u8> {
        let algorithm = der(
            SEQUENCE,
            &[der(OBJECT_IDENTIFIER, SHA256), der(0x05, &[])].concat(),
        );
        let certificates = certificates.map_or_else(Vec::new, |certificates| {
            der(CERTIFICATES, &certificates.concat())
        });
        let signed_data = der(
            SEQUENCE,
            &[
                der(INTEGER, &[1]),
                der(SET, &algorithm),
                encapsulated,
                certificates,
                der(SET, &signers.concat()),
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
    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
    const XML: &[u8] = br#"<?xml version="1.0"?><r xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Signed</dc:title>Some signed text.</r>"#;

    #[cfg(feature = "tika")]
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
        assert_same_as_tika_reading(&read.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    /// Asserts parity on `documents`, those whose content is generic XML with that parser's
    /// departures from Tika's (#105) normalised.
    #[cfg(feature = "tika")]
    fn assert_same_as_tika_reading(documents: &[&Path]) {
        let read_as_xml = |path: &&Path| {
            let (_, metadata) = crate::Extractor::new()
                .extract_file_to_string(path.to_str().unwrap())
                .unwrap();
            ["tk:parsed-by", "tk:parsed-by-full-set"]
                .iter()
                .filter_map(|name| metadata.get(*name))
                .flatten()
                .any(|parser| parser.contains("DcXMLParser"))
        };
        let (xml, rest): (Vec<&Path>, Vec<&Path>) = documents
            .iter()
            .copied()
            .partition(|path| read_as_xml(path));
        // Content that is a ZIP archive now has its integrity check in its head (#99).
        crate::formats::parity::assert_same_as_tika_rewriting(
            &rest,
            TIKA_NAME,
            as_tika_writes_content,
        );
        if !xml.is_empty() {
            crate::formats::parity::assert_same_as_tika_normalising(
                &xml,
                TIKA_NAME,
                as_tika_writes_xml,
                crate::formats::dc_xml::without_head,
            );
        }
    }

    #[cfg(feature = "tika")]
    #[test]
    fn reads_the_signed_local_document_as_tikas_path_does() {
        let (parsed, deferred) = documents(&CmsParser, &[Path::new("../test_files/documents")]);
        assert!(deferred.is_empty(), "{deferred:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika_reading(&paths);
    }

    #[test]
    fn types_signed_data_without_content_by_its_certificates_and_signers() {
        let without_content = || der(SEQUENCE, &der(OBJECT_IDENTIFIER, DATA));
        let certificate = der(SEQUENCE, &der(INTEGER, &[7]));
        let signer = der(SEQUENCE, &der(INTEGER, &[1]));
        let typed = |data: Vec<u8>| match signed_content(&data) {
            Some(Signed::Detached(refined)) => refined,
            other => panic!("not detached: {other:?}"),
        };
        assert_eq!(typed(signed(without_content())), Ok(SIGNATURE));
        assert_eq!(
            typed(signed_with(
                without_content(),
                Some(std::slice::from_ref(&certificate)),
                &[]
            )),
            Ok(CERTS_ONLY)
        );
        assert_eq!(
            typed(signed_with(
                without_content(),
                Some(&[certificate]),
                std::slice::from_ref(&signer)
            )),
            Ok(SIGNATURE)
        );
        assert_eq!(
            typed(signed_with(without_content(), Some(&[]), &[signer])),
            Ok(SIGNATURE)
        );
        let certs_only = signed_with(without_content(), Some(&[der(SEQUENCE, &[])]), &[]);
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", "application/pkcs7-mime");
        let configs = ParserConfigs::default();
        assert!(CmsParser.prepare(&certs_only, &metadata, &configs).is_ok());
    }

    /// Signed data of `content`, whose digest algorithm has `parameters`, its encapsulated
    /// OCTET STRING's header `octet_header`.
    fn signed_encoded(parameters: &[u8], octet_header: &[u8], content: &[u8]) -> Vec<u8> {
        let algorithm = der(
            SEQUENCE,
            &[&der(OBJECT_IDENTIFIER, SHA256)[..], parameters].concat(),
        );
        let encapsulated = der(
            SEQUENCE,
            &[
                der(OBJECT_IDENTIFIER, DATA),
                der(EXPLICIT_0, &[octet_header, content].concat()),
            ]
            .concat(),
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

    #[test]
    fn reads_the_ber_encodings_bouncy_castle_reads() {
        let content = b"Indhold";
        let high_tag = [0x9F, 0x81, 0x48, 0x01, 0x00];
        let five_byte_length = [OCTET_STRING, 0x85, 0, 0, 0, 0, content.len() as u8];
        for (parameters, header) in [
            (&high_tag[..], &[OCTET_STRING, content.len() as u8][..]),
            (&[][..], &five_byte_length[..]),
        ] {
            let signed = signed_encoded(parameters, header, content);
            assert_eq!(
                signed_content(&signed),
                Some(Signed::Read(vec![&content[..]])),
                "{signed:02x?}"
            );
        }
    }

    #[test]
    fn fails_on_the_ber_encodings_bouncy_castle_fails_on() {
        let content = b"Indhold";
        let failures = [
            (
                &[OCTET_STRING, 0xFF][..],
                "invalid long form definite-length 0xFF",
            ),
            (
                &[OCTET_STRING, 0x80][..],
                "indefinite-length primitive encoding encountered",
            ),
            (
                &[OCTET_STRING, 0x20][..],
                "DEF length 32 object truncated by",
            ),
            (&[0x1F, 0x1E, 0x01][..], "high tag number < 31 found"),
        ];
        for (header, message) in failures {
            let signed = signed_encoded(&[], header, content);
            let Some(Signed::Fails(exception)) = signed_content(&signed) else {
                panic!("{header:02x?} is read");
            };
            assert!(exception.contains(message), "{exception}");
        }
        assert_eq!(classify(&[SEQUENCE, 0xFF]), None);
        let configs = ParserConfigs::default();
        assert!(CmsParser
            .prepare(&[SEQUENCE, 0x84, 0x7F], &TikaMetadata::new(), &configs)
            .is_ok());
    }

    #[test]
    fn fails_what_bouncy_castle_fails_to_read_as_signed_data() {
        let Some(Signed::Fails(pem)) = signed_content(b"-----BEGIN PKCS7-----\nMIIG") else {
            panic!("PEM is read as signed data");
        };
        assert!(pem.contains("unknown DL object encountered: 0xd"), "{pem}");
        let mut digested = vec![SEQUENCE, 0x1C, OBJECT_IDENTIFIER, 0x09];
        digested.extend_from_slice(DIGESTED_DATA);
        digested.extend_from_slice(&[EXPLICIT_0, 0x0F, SEQUENCE, 0x0D, INTEGER, 0x01, 0x00]);
        digested.extend_from_slice(&[SEQUENCE, 0x08, OBJECT_IDENTIFIER, 0x06]);
        digested.extend_from_slice(&[0x60, 0x86, 0x48, 0x01, 0x65, 0x03]);
        let Some(Signed::Fails(cast)) = signed_content(&digested) else {
            panic!("digested data is read as signed data");
        };
        assert!(cast.contains("ClassCastException"), "{cast}");
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", SIGNATURE);
        let configs = ParserConfigs::default();
        assert!(matches!(
            CmsParser.prepare(&digested, &metadata, &configs),
            Err(crate::formats::Decline::Fails(Error::ParseError(_)))
        ));
    }

    /// Tika's CMS test documents Bouncy Castle fails to read as signed data: digested data
    /// typed as a signature, and signed data in PEM.
    #[cfg(feature = "tika")]
    const UNREADABLE: [&str; 2] = ["testPKCS7_digested.p7", "testPKCS7_signed_data_def.pem"];

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn reads_tikas_test_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&CmsParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        let failing = |names: &[&str], path: &Path| names.iter().any(|name| path.ends_with(name));
        let (enveloped, rest): (Vec<&Path>, Vec<&Path>) = parsed
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| failing(&ENVELOPED, path));
        let (unreadable, rest): (Vec<&Path>, Vec<&Path>) = rest
            .into_iter()
            .partition(|path| failing(&UNREADABLE, path));
        assert_eq!(enveloped.len(), ENVELOPED.len());
        assert_eq!(unreadable.len(), UNREADABLE.len());
        assert!(DETACHED
            .iter()
            .all(|name| rest.iter().any(|path| path.ends_with(name))));
        assert_same_as_tika_reading(&rest);
        assert_fails_as_tika(&enveloped);
        assert_fails_as_tika(&unreadable);
        for path in unreadable {
            let error = crate::Extractor::new()
                .extract_file_to_string(path.to_str().unwrap())
                .unwrap_err();
            assert!(matches!(error, Error::ParseError(_)), "{error:?}");
        }
        let error = crate::Extractor::new()
            .extract_file_to_string(enveloped[0].to_str().unwrap())
            .unwrap_err();
        assert!(matches!(error, Error::Encrypted(_)), "{error:?}");
    }

    #[cfg(not(feature = "tika"))]
    #[test]
    fn reads_signed_content_too_large_to_read_whole_in_place() {
        use std::io::Read;

        use crate::formats::archive::large::Binary;
        use crate::input::MAX_READ_WHOLE;
        use extractous_core::large::{LargeFile, BUDGET};

        let mut content = Vec::new();
        Binary::new(MAX_READ_WHOLE + (16 << 20))
            .read_to_end(&mut content)
            .unwrap();
        let data = signed(encapsulated(&content));
        drop(content);
        let file = LargeFile::written("large.p7m", |out| out.write_all(&data));
        drop(data);
        let (_, metadata) = file.extract_with(
            &extractous_core::Extractor::new(),
            BUDGET,
            "extractous::CmsParser",
        );
        assert_eq!(
            metadata["Content-Type"].first().map(String::as_str),
            Some(SIGNED)
        );
    }
}
