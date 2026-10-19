//! Tika's `SevenZParser`: the files of a 7z archive but its directories, in the order the
//! archive lists them, handed to the embedded document extractor with the metadata
//! `handleEntryMetadata` gives them, their creation and modification times among it. The
//! archive is decoded twice, first to rule out what the native path does not read as Tika's
//! does, keeping nothing, then as it is written, each file spooled and parsed before the next is
//! decoded, so the parser's memory stays bounded whatever the archive's size.

use std::convert::Infallible;

use super::{undecoded, within_limit, write_file, Cut};
use crate::compress::{seven_z, Decoding, Limited, Undecoded};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{crypt, Decline, Failure, Footprint, Parser, Prepared, PASSWORD_USED};
use crate::input::{Pages, Paging};

/// The archive, as a failure names it.
const WHAT: &str = "the 7z archive";

const SEVEN_Z: &str = "application/x-7z-compressed";
/// What commons-compress's `SevenZFile` throws on encrypted data it was given no password for,
/// reading the archive from a channel, which it knows no name of.
const PASSWORD_REQUIRED: &str = "org.apache.commons.compress.PasswordRequiredException: Cannot \
                                 read encrypted content from unknown archive without a password.";

/// Tika's `SevenZParser`.
pub struct SevenZParser;

impl Parser for SevenZParser {
    fn name(&self) -> &'static str {
        "extractous::SevenZParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.pkg.SevenZParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[SEVEN_Z]
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

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let password = configs.password.as_ref().map(|password| password.expose());
        let declined = |failure| match failure {
            Undecoded::Encrypted => Decline::Fails(crypt::encrypted(Some(PASSWORD_REQUIRED))),
            reason => undecoded(WHAT, reason, self.tika_name()),
        };
        let paging = Paging::new(pages);
        let whole = seven_z::visit(
            input,
            password,
            |compressed, _| Decoding::discarded(compressed),
            |_, _| Ok::<(), Infallible>(()),
        )
        .map_err(declined)?
        .unwrap_or_else(|never| match never {});
        let read = if whole {
            Limited::Whole(())
        } else {
            Limited::Cut(())
        };
        let ((), cut) = within_limit(WHAT.to_owned(), read);
        paging.release();
        Ok(Box::new(move |xhtml| {
            if password.is_some() {
                xhtml.metadata_mut().set(PASSWORD_USED, "true");
            }
            xhtml.metadata_mut().set("Content-Type", SEVEN_Z);
            let written = write(input, password, xhtml, &paging).and_then(|()| Cut::end(cut));
            xhtml.end_document()?;
            written
        }))
    }
}

/// The loop of `SevenZParser` over the files of the archive `input` but the directories, each
/// decoded to a spool as it is reached and handed to the embedded document extractor, the pages
/// of the archive released by `paging` after each.
fn write(
    input: &[u8],
    password: Option<&str>,
    xhtml: &mut Xhtml,
    paging: &Paging,
) -> Result<(), Failure> {
    let visited = seven_z::visit(input, password, Decoding::spooled, |listed, content| {
        let (Limited::Whole(content) | Limited::Cut(content)) = content;
        let file = listed.file(content);
        if !file.directory {
            write_file(&file, xhtml)?;
        }
        paging.release();
        Ok(())
    });
    match visited {
        Ok(written) => written.map(|_| ()),
        Err(undecoded) => Err(Failure::Extraction(Error::IoError(format!(
            "{WHAT} was read whole when it was prepared, but not when it is written: {undecoded}"
        )))),
    }
}
