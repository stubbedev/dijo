//! Password protected OOXML packages (`application/x-tika-ooxml-protected`) as Tika's
//! `OfficeParser` reads them: the summary information of the compound document they are
//! encrypted in as metadata, the package decrypted with the given password or Office's default
//! one, and the package then parsed by `OOXMLParser` into the same body and metadata, which this
//! port leaves to the native parser of the package's format. Tika's `OOXMLParser` writes through
//! an XHTML handler of its own into `OfficeParser`'s, so that the line breaks after block
//! elements and the tabs before indented ones come out twice; here they come once (#119).
//!
//! A package whose encryption this port does not open, one whose own parser leaves it to Tika,
//! and macros extracted when asked to, are left to Tika; where no password opens the package,
//! the parse fails as Tika's path fails it.
//!
//! The package is decrypted a chunk at a time into a spool, in memory as far as a file is read
//! whole and to a mapped temporary file beyond, which the parser of its format reads with its
//! pages, so that a protected package of any size is read in bounded memory.

use std::io::Write;

use crate::config::ParserConfigs;
use crate::formats::crypt::{
    self, package_length, PackageEncryption, Unopened, ENCRYPTED_PACKAGE, ENCRYPTION_INFO,
};
use crate::formats::docx::DocxParser;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::office;
use crate::formats::ole2::Summaries;
use crate::formats::pptx::PptxParser;
use crate::formats::xlsx::XlsxParser;
use crate::formats::{
    self, decision, unless_different, Decline, Failure, Footprint, Parser, Prepared, Unported,
    PASSWORD_USED,
};
use crate::input::{Input, Pages, Paging, Spool};
use crate::mime::{self, Origin};
use crate::Error;

const MEDIA_TYPE: &str = "application/x-tika-ooxml-protected";
/// The native parsers of the formats `OOXMLParser` reads.
const PACKAGE_PARSERS: [&dyn Parser; 3] = [&DocxParser, &XlsxParser, &PptxParser];

/// For the parity tests: the line breaks and tabs Tika's path writes twice, where the package's
/// XHTML goes through two handlers (#119), and the head saying whether a workbook's sheets are
/// protected, which Tika's path writes `false` before it reads the sheets (#95).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const DEPARTURES: crate::formats::parity::Departures = &[
    crate::formats::parity::Departure::DoubledBreaks("#119"),
    crate::formats::parity::Departure::FinalInHead(
        crate::formats::xlsx::excel::PROTECTED_WORKSHEET,
        "#95",
    ),
];

pub struct ProtectedParser;

impl Parser for ProtectedParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// The package is decrypted to a spool, and the parsers of the formats it holds are bounded.
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

    /// Fails packages no password opens, and defers those whose encryption this port does not
    /// open, those the parser of their format defers, and macros extracted when asked to.
    ///
    /// The package is decrypted here and prepared by its parser to learn whether that parser
    /// extracts it; the parser prepares it again as the document is written, as what it
    /// prepared borrows the decrypted package, which only the writing outlives.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let Package {
            summaries,
            package,
            media_type,
            parser,
        } = Package::read(input, pages, configs)?;
        let mut prepared_metadata = metadata.clone();
        let asks_password = configs.password.is_some();
        write_metadata(
            &mut prepared_metadata,
            &summaries,
            asks_password,
            &media_type,
        );
        let difference =
            decision(parser.prepare_paged(&package, package.pages(), &prepared_metadata, configs))?;
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            write_metadata(xhtml.metadata_mut(), &summaries, asks_password, &media_type);
            let metadata = xhtml.metadata_mut().clone();
            let prepared = formats::reading(parser.prepare_paged(
                &package,
                package.pages(),
                &metadata,
                configs,
            ))
            .map_err(|_| {
                Failure::Malformed("the decrypted package no longer prepares as it did".to_owned())
            })?;
            prepared(xhtml)
        });
        unless_different(reading, difference)
    }
}

/// What `OfficeParser` writes of the compound document before it hands the package on (the
/// summary information, the type of the document, and, where a password was given, the mark
/// our Java's `PasswordProvider` leaves once `OfficeParser` asks it for the password), and the
/// type of the package `OOXMLExtractorFactory` sets before it extracts it.
fn write_metadata(
    metadata: &mut TikaMetadata,
    summaries: &Summaries,
    asks_password: bool,
    package_type: &str,
) {
    summaries.write(metadata);
    metadata.set("Content-Type", MEDIA_TYPE);
    if asks_password {
        metadata.set(PASSWORD_USED, "true");
    }
    metadata.set("Content-Type", package_type);
}

/// A protected package read: the summary information of its compound document, the package
/// decrypted, and the parser of its format.
struct Package {
    summaries: Summaries,
    package: Input<'static>,
    /// The type of the package, as `OOXMLExtractorFactory` sets it before it extracts it.
    media_type: String,
    parser: &'static dyn Parser,
}

impl Package {
    /// Reads the protected package `input`, whose pages are `pages`, which are released as the
    /// package is decrypted.
    fn read(
        input: &[u8],
        pages: Pages<'_>,
        configs: &ParserConfigs,
    ) -> Result<Package, Decline<'static>> {
        let file = office::open(input, configs, MEDIA_TYPE, "protected package")?;
        let summaries = Summaries::read(&file, file.root())?;
        let info = file.root_document(ENCRYPTION_INFO).map_err(Unported)?;
        let encryption = PackageEncryption::read(&info).map_err(|unopened| match unopened {
            Unopened::Disallowed(_) => Decline::Fails(crypt::encrypted(None)),
            unopened => unopened.unported().into(),
        })?;
        let key = encryption
            .verify(crypt::password(configs))
            .map_err(|unopened| unopened.unported())?
            .ok_or_else(|| Decline::Fails(crypt::encrypted(None)))?;
        let encrypted = file.root_content(ENCRYPTED_PACKAGE).map_err(Unported)?;
        let length = package_length(&encrypted).map_err(|unopened| unopened.unported())?;
        let mut spool = Spool::new(Some(length.min(encrypted.len()) as u64));
        let paging = Paging::new(pages);
        key.decrypt(&encrypted, |chunk| {
            paging.read(chunk.len());
            spool.write_all(chunk).map_err(unspooled)
        })?;
        drop(paging);
        let package = spool.finish().map_err(unspooled)?;
        let media_type = mime::detect(&package, Origin::Bytes)
            .map_err(|_| Unported("the decrypted package is not detected".to_owned()))?
            .media_type
            .to_string();
        let parser = PACKAGE_PARSERS
            .into_iter()
            .find(|parser| parser.media_types().contains(&media_type.as_str()))
            .ok_or_else(|| {
                Unported(format!(
                    "the decrypted package is {media_type}, which no native parser of OOXML \
                     reads"
                ))
            })?;
        Ok(Package {
            summaries,
            package,
            media_type,
            parser,
        })
    }
}

/// Why the decrypted package is not read: spooling it fails.
fn unspooled(error: std::io::Error) -> Decline<'static> {
    Decline::Fails(Error::IoError(format!(
        "Could not spool the decrypted package: {error}"
    )))
}

#[cfg(all(test, feature = "tika"))]
mod tests;
