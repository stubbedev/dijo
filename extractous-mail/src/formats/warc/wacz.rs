//! Web archive collections, WACZ, as Tika's `WACZParser` reads them: each file under
//! `archive/`, in the order of the central directory, handed to the embedded document extractor,
//! gunzipped where it starts as gzip does; the data package and everything else left unread.
//! Tika's also hands on each folder under `archive/` as an empty document (#118).
//!
//! The archives are read twice, so that a collection of any size is read in bounded memory:
//! once to find the first that is not read whole, keeping nothing, and again as each is handed
//! on, a stored archive in place and a compressed or gzipped one decoded to a spool.

use crate::compress::zip::file::{ZipEntry, ZipFile};
use crate::compress::zip::Decoded;
use crate::compress::{gzip, Limited, Undecoded};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Difference, Failure, Footprint, Opened, Parser, Prepared};
use crate::input::{Input, Pages, Paging};

/// The folder of a collection's archives.
const ARCHIVE: &str = "archive/";
/// `GZIPInputStream.GZIP_MAGIC`, as the first two bytes of a stream.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Why an archive of the collection is not read whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unread {
    /// Its data is cut short or corrupt.
    Cut,
    /// It starts as gzip does, and does not gunzip.
    Gzip,
    /// It decodes into more than the native path decodes.
    TooLarge,
}

impl Unread {
    /// The failure of the collection once what there is of the archive of `path` is written.
    fn failure(self, path: &str) -> Failure {
        match self {
            Unread::Cut => Failure::Malformed(format!(
                "EOFException: the data of the entry {path} is cut short or corrupt"
            )),
            Unread::Gzip => Failure::Malformed(format!(
                "IOException: the entry {path} starts as gzip does, and does not gunzip"
            )),
            Unread::TooLarge => Failure::Embedded(Error::LimitExceeded(format!(
                "the entry {path}: {}, the most the native parser decodes",
                Undecoded::TooLarge
            ))),
        }
    }
}

/// Tika's `WACZParser`.
pub struct WaczParser;

impl Parser for WaczParser {
    fn name(&self) -> &'static str {
        "extractous::WACZParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.wacz.WACZParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-wacz"]
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
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let zip = ZipFile::open(input).map_err(|_| {
            Decline::Fails(Error::ParseError(
                "the WACZ collection is not a ZIP archive".to_owned(),
            ))
        })?;
        let paging = Paging::new(pages);
        let unread = first_unread(&zip, pages, &paging).map_err(Decline::Fails)?;
        paging.release();
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            for (entry, name) in archives(&zip) {
                let (data, unread) = read(&zip, entry, pages).map_err(Failure::Extraction)?;
                let mut metadata = TikaMetadata::new();
                metadata.set(INTERNAL_PATH, entry.name.as_str());
                metadata.set(RESOURCE_NAME, name);
                metadata.set("Content-Length", entry.size.to_string());
                xhtml.parse_embedded_opened(
                    &data,
                    &metadata,
                    Wrapping::PackageEntry,
                    Opened::Alone,
                )?;
                paging.read(compressed_size(entry));
                if let Some(unread) = unread {
                    return Err(unread.failure(&entry.name));
                }
            }
            Ok(())
        });
        let Some((unread, path)) = unread else {
            return Ok(reading);
        };
        Err(Decline::Differs {
            reading,
            difference: Difference::new(format!(
                "the archive {path} is not read whole ({unread:?}): the native parser parses \
                 what it reads of it, then fails the collection, where the stream Tika's \
                 embedded parser reads throws, which fails it unless that parser catches it"
            )),
        })
    }
}

/// The archives of the collection, the files under [`ARCHIVE`], each with its name there.
fn archives<'z>(zip: &'z ZipFile) -> impl Iterator<Item = (&'z ZipEntry, &'z str)> {
    zip.entries()
        .iter()
        .filter(|entry| !entry.is_directory())
        .filter_map(|entry| Some((entry, entry.name.strip_prefix(ARCHIVE)?)))
}

/// The bytes of the collection an archive takes.
fn compressed_size(entry: &ZipEntry) -> usize {
    usize::try_from(entry.compressed_size).unwrap_or(usize::MAX)
}

/// The first archive of the collection that is not read whole, with why and its path, read
/// keeping nothing but what a compressed archive spools to be gunzipped, the pages of the
/// collection that `paging` holds released as they are read; or why an archive cannot be
/// decoded.
fn first_unread(
    zip: &ZipFile,
    pages: Pages,
    paging: &Paging,
) -> Result<Option<(Unread, String)>, Error> {
    for (entry, _) in archives(zip) {
        let (data, cut) = unzipped(zip, entry, pages)?;
        let unread = match cut {
            Some(cut) => Some(cut),
            None => match gunzipped(&data, gzip::verify).map_err(unspooled(entry))? {
                Gunzipped::Plain | Gunzipped::Whole(_) => None,
                Gunzipped::Cut(_) => Some(Unread::TooLarge),
                Gunzipped::Fails => Some(Unread::Gzip),
            },
        };
        paging.read(compressed_size(entry));
        if let Some(unread) = unread {
            return Ok(Some((unread, entry.name.clone())));
        }
    }
    Ok(None)
}

/// An archive of the collection, as the embedded document extractor is handed it: as much of it
/// as is read, gunzipped where its first two bytes are gzip's, as commons-compress's
/// `IOUtils.readFully` reads it, zero past a shorter entry's end, and, where it is not read
/// whole, why, which fails the collection once the archive's parser has read it; or why it
/// cannot be decoded.
fn read<'a>(
    zip: &ZipFile<'a>,
    entry: &ZipEntry,
    pages: Pages<'a>,
) -> Result<(Input<'a>, Option<Unread>), Error> {
    let (data, cut) = unzipped(zip, entry, pages)?;
    if cut.is_some() {
        return Ok((data, cut));
    }
    let gunzip = |data: &[u8]| gzip::decode(data).map(|member| member.map(|member| member.content));
    Ok(match gunzipped(&data, gunzip).map_err(unspooled(entry))? {
        Gunzipped::Plain => (data, None),
        Gunzipped::Whole(content) => (content, None),
        Gunzipped::Cut(content) => (content, Some(Unread::TooLarge)),
        Gunzipped::Fails => (Input::Borrowed(&[]), Some(Unread::Gzip)),
    })
}

/// An archive's data as the collection holds it, stored in place or decoded to a spool, and why
/// it is not read whole, if it is not; or why it cannot be spooled.
fn unzipped<'a>(
    zip: &ZipFile<'a>,
    entry: &ZipEntry,
    pages: Pages<'a>,
) -> Result<(Input<'a>, Option<Unread>), Error> {
    Ok(match zip.spool(entry, pages) {
        Some(Ok(Decoded::Complete(data))) => (data, None),
        Some(Ok(Decoded::Truncated(data))) => (data, Some(Unread::Cut)),
        Some(Ok(Decoded::Oversized(data))) => (data, Some(Unread::TooLarge)),
        Some(Err(undecoded)) => return Err(unspooled(entry)(undecoded)),
        None => (Input::Borrowed(&[]), Some(Unread::Cut)),
    })
}

/// The failure of a collection whose archive `entry` cannot be decoded to a spool, as the
/// [`Undecoded`] it is handed says.
fn unspooled(entry: &ZipEntry) -> impl Fn(Undecoded) -> Error + '_ {
    move |undecoded| Error::IoError(format!("the entry {}: {undecoded}", entry.name))
}

/// What an archive's data, read whole, makes when it is gunzipped.
enum Gunzipped<T> {
    /// It does not start as gzip does, and is handed on as it is.
    Plain,
    /// It gunzips whole into this.
    Whole(T),
    /// It gunzips into more than the native path decodes, cut there.
    Cut(T),
    /// It starts as gzip does, and does not gunzip.
    Fails,
}

/// What `gunzip` makes of `data` where it starts as gzip does; or why what it decodes cannot be
/// spooled.
fn gunzipped<T>(
    data: &[u8],
    gunzip: impl FnOnce(&[u8]) -> Result<Limited<T>, Undecoded>,
) -> Result<Gunzipped<T>, Undecoded> {
    if !data.starts_with(&GZIP_MAGIC) {
        return Ok(Gunzipped::Plain);
    }
    match gunzip(data) {
        Ok(Limited::Whole(content)) => Ok(Gunzipped::Whole(content)),
        Ok(Limited::Cut(content)) => Ok(Gunzipped::Cut(content)),
        Err(undecoded @ Undecoded::Unspooled(_)) => Err(undecoded),
        Err(_) => Ok(Gunzipped::Fails),
    }
}
