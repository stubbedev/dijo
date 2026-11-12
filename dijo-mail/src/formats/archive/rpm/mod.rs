//! RPM packages, which Tika reads with no parser, read as the archive parsers read theirs: the
//! main header's name, version, release, summary, license, URL, packager, vendor and build time
//! as the package's metadata; its summary, description and changelog as its text; and each file
//! of its payload but a directory handed to the embedded document extractor with the metadata
//! `handleEntryMetadata` gives an archive's entry, as `PackageParser` hands on a cpio archive's.
//!
//! The lead and headers are read as [`header`] reads them. The payload is the cpio archive
//! after the main header, compressed as its `PAYLOADCOMPRESSOR` says: with gzip, bzip2, xz, lzma
//! or zstd, decoded to a spool within [`decode_limit`](crate::compress::decode_limit) bytes;
//! without the tag, with gzip where it starts as gzip does and else not at all, as rpm's own
//! reader, which passes data that is not gzip through, takes it. Its entries are in the new
//! ASCII format, or stripped, as rpm writes a package of files of 4 GiB or more and every
//! format 6 package: each such entry is the file of the header's list at its index, named as
//! rpm names the files of an archive in the new format, its size that of the list, but none for
//! a hard link's data that a later link of the same file carries.
//!
//! Where the headers cannot be read, the package fails; where the payload cannot be, the
//! package fails after its metadata and text are written.

mod header;

use std::cell::OnceCell;
use std::collections::HashMap;

use header::Header;

use super::{past_the_limit, within_limit, write_file, Cut};
use crate::compress::compressor::Compressor;
use crate::compress::cpio::{self, Listed};
use crate::compress::Undecoded;
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Paging};
use crate::java::time::format_instant;

const NAME: u32 = 1000;
const VERSION: u32 = 1001;
const RELEASE: u32 = 1002;
const EPOCH: u32 = 1003;
const SUMMARY: u32 = 1004;
const DESCRIPTION: u32 = 1005;
const BUILD_TIME: u32 = 1006;
const VENDOR: u32 = 1011;
const LICENSE: u32 = 1014;
const PACKAGER: u32 = 1015;
const URL: u32 = 1020;
const ARCH: u32 = 1022;
const FILE_SIZES: u32 = 1028;
const FILE_MODES: u32 = 1030;
const FILE_MTIMES: u32 = 1034;
const FILE_FLAGS: u32 = 1037;
const CHANGELOG_NAME: u32 = 1081;
const CHANGELOG_TEXT: u32 = 1082;
const FILE_DEVICES: u32 = 1095;
const FILE_INODES: u32 = 1096;
const DIR_INDEXES: u32 = 1116;
const BASENAMES: u32 = 1117;
const DIRNAMES: u32 = 1118;
const PAYLOAD_FORMAT: u32 = 1124;
const PAYLOAD_COMPRESSOR: u32 = 1125;
const LONG_FILE_SIZES: u32 = 5008;

/// `RPMFILE_GHOST`: a file the package owns but does not hold.
const GHOST: u64 = 1 << 6;
const FILE_TYPE: u64 = 0o170000;
const REGULAR: u64 = 0o100000;
const SYMBOLIC_LINK: u64 = 0o120000;

/// The payload, as a failure names it.
const WHAT: &str = "the payload of the RPM package";

/// The metadata the main header's string entries give, by their tags.
const STRING_METADATA: [(u32, &str); 9] = [
    (NAME, "dc:title"),
    (VERSION, "rpm:version"),
    (RELEASE, "rpm:release"),
    (ARCH, "rpm:arch"),
    (SUMMARY, "dc:description"),
    (LICENSE, "dc:rights"),
    (URL, "rpm:url"),
    (PACKAGER, "dc:creator"),
    (VENDOR, "dc:publisher"),
];

/// The parser of RPM packages.
pub struct RpmParser;

impl Parser for RpmParser {
    fn name(&self) -> &'static str {
        "dijo::RpmParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::RpmParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-rpm"]
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
        let unread = |reason: String| {
            Decline::Fails(Error::ParseError(format!("the RPM package: {reason}")))
        };
        let header::Package { header, payload } = header::read(input).map_err(unread)?;
        let described = Described::of(&header).map_err(unread)?;
        let compression = Compression::of(&header, payload);
        Ok(Box::new(move |xhtml| {
            described.write(xhtml)?;
            let compression =
                compression.map_err(|reason| Failure::Malformed(format!("{WHAT}: {reason}")))?;
            write_payload(&header, compression, payload, pages, xhtml)
        }))
    }
}

/// What the main header says of the package: its metadata, summary, description and
/// changelog.
struct Described {
    metadata: Vec<(&'static str, String)>,
    summary: Option<String>,
    description: Option<String>,
    /// Each entry's author and version line, and its text.
    changelog: Vec<(String, String)>,
}

impl Described {
    fn of(header: &Header) -> Result<Self, String> {
        let mut metadata = Vec::with_capacity(STRING_METADATA.len() + 2);
        for (tag, key) in STRING_METADATA {
            if let Some(value) = header.string(tag)? {
                metadata.push((key, value));
            }
        }
        if let Some(epoch) = header.number(EPOCH)? {
            metadata.push(("rpm:epoch", epoch.to_string()));
        }
        if let Some(seconds) = header.number(BUILD_TIME)? {
            let millis = i64::try_from(seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(1000))
                .ok_or_else(|| format!("its build time {seconds} is out of range"))?;
            metadata.push(("dcterms:created", format_instant(millis)));
        }
        let names = header.strings(CHANGELOG_NAME)?.unwrap_or_default();
        let texts = header.strings(CHANGELOG_TEXT)?.unwrap_or_default();
        if names.len() != texts.len() {
            return Err(format!(
                "its changelog has {} names for {} texts",
                names.len(),
                texts.len()
            ));
        }
        Ok(Described {
            metadata,
            summary: header.string(SUMMARY)?,
            description: header.string(DESCRIPTION)?,
            changelog: names.into_iter().zip(texts).collect(),
        })
    }

    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        for (key, value) in &self.metadata {
            xhtml.metadata_mut().set(key, value.clone());
        }
        for text in [&self.summary, &self.description].into_iter().flatten() {
            xhtml.element("p", text)?;
        }
        for (name, text) in &self.changelog {
            xhtml.element("h2", name)?;
            xhtml.element("p", text)?;
        }
        Ok(())
    }
}

/// How the payload is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    Compressed(Compressor),
    Uncompressed,
}

impl Compression {
    /// How the payload `payload` of the package of `header` is compressed, or why it cannot be
    /// read.
    fn of(header: &Header, payload: &[u8]) -> Result<Self, String> {
        match header.string(PAYLOAD_FORMAT)?.as_deref() {
            None | Some("cpio") => {}
            Some(other) => return Err(format!("it is a {other} archive, not cpio")),
        }
        let compressor = match header.string(PAYLOAD_COMPRESSOR)?.as_deref() {
            None if Compressor::detect(payload) == Some(Compressor::Gzip) => Compressor::Gzip,
            None => return Ok(Compression::Uncompressed),
            Some("gzip") => Compressor::Gzip,
            Some("bzip2") => Compressor::Bzip2,
            Some("xz") => Compressor::Xz,
            Some("lzma") => Compressor::Lzma,
            Some("zstd") => Compressor::Zstd,
            Some(other) => {
                return Err(format!(
                    "it is compressed with {other}, which is none of rpm's compressors"
                ))
            }
        };
        Ok(Compression::Compressed(compressor))
    }
}

/// Hands each file of the payload `payload`, whose pages are `pages`, compressed so, but the
/// directories to the embedded document extractor.
fn write_payload(
    header: &Header,
    compression: Compression,
    payload: &[u8],
    pages: Pages,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let content = match compression {
        Compression::Uncompressed => Input::Within(payload, pages),
        Compression::Compressed(compressor) => {
            let decoded = compressor
                .decode(payload)
                .map_err(|reason| unread(&format!("{WHAT}, {compressor:?}"), reason))?;
            pages.release();
            let (decoded, cut) = within_limit(WHAT.to_owned(), decoded);
            if cut.is_some() {
                return Cut::end(cut);
            }
            decoded.content
        }
    };
    let listing = OnceCell::new();
    let listed = |index: u32| {
        let files = listing
            .get_or_init(|| listing_of(header))
            .as_ref()
            .map_err(|reason: &String| Undecoded::Fails(reason.clone()))?;
        files.get(index as usize).cloned().ok_or_else(|| {
            Undecoded::Fails(format!(
                "a stripped entry is of file {index}, but the header lists {}",
                files.len()
            ))
        })
    };
    let files = cpio::package_files(&content, content.pages(), &listed)
        .map_err(|reason| unread(WHAT, reason))?;
    let paging = Paging::new(content.pages());
    for file in files.iter().filter(|file| !file.directory) {
        write_file(file, xhtml)?;
        paging.read(file.content.len());
    }
    Ok(())
}

/// The failure of a package whose payload, `what` it is, is not read, for the reason
/// `undecoded`.
fn unread(what: &str, undecoded: Undecoded) -> Failure {
    match undecoded {
        Undecoded::TooLarge => Failure::Embedded(past_the_limit(what)),
        Undecoded::MemoryLimit(_) => {
            Failure::Embedded(Error::LimitExceeded(format!("{what}: {undecoded}")))
        }
        Undecoded::Unspooled(_) => {
            Failure::Extraction(Error::IoError(format!("{what}: {undecoded}")))
        }
        Undecoded::Fails(_)
        | Undecoded::Unported(_)
        | Undecoded::Encrypted
        | Undecoded::BadPassword => Failure::Malformed(format!("{what}: {undecoded}")),
    }
}

/// The files the main header `header` lists, by their index, as a stripped payload holds them,
/// or why they cannot be listed.
fn listing_of(header: &Header) -> Result<Vec<Listed>, String> {
    let required = |tag: u32, what: &str| {
        header
            .numbers(tag)?
            .ok_or_else(|| format!("the header lists no file {what}"))
    };
    let basenames = header
        .strings(BASENAMES)?
        .ok_or("the header lists no file names")?;
    let dirnames = header
        .strings(DIRNAMES)?
        .ok_or("the header lists no directory names")?;
    let sizes = match header.numbers(LONG_FILE_SIZES)? {
        Some(sizes) => sizes,
        None => required(FILE_SIZES, "sizes")?,
    };
    let columns = [
        (
            "directory indexes",
            required(DIR_INDEXES, "directory indexes")?,
        ),
        ("sizes", sizes),
        ("modes", required(FILE_MODES, "modes")?),
        (
            "modification times",
            required(FILE_MTIMES, "modification times")?,
        ),
        ("flags", required(FILE_FLAGS, "flags")?),
        ("devices", required(FILE_DEVICES, "devices")?),
        ("inodes", required(FILE_INODES, "inodes")?),
    ];
    for (what, column) in &columns {
        if column.len() != basenames.len() {
            return Err(format!(
                "the header lists {} file {what} for {} files",
                column.len(),
                basenames.len()
            ));
        }
    }
    let [(_, directories), (_, sizes), (_, modes), (_, mtimes), (_, flags), (_, devices), (_, inodes)] =
        columns;
    let file_type = |index: usize| modes[index] & FILE_TYPE;
    let mut last_link = HashMap::new();
    for index in 0..basenames.len() {
        if file_type(index) == REGULAR && flags[index] & GHOST == 0 {
            last_link.insert((devices[index], inodes[index]), index);
        }
    }
    basenames
        .into_iter()
        .enumerate()
        .map(|(index, basename)| {
            let directory = usize::try_from(directories[index])
                .ok()
                .and_then(|directory| dirnames.get(directory))
                .ok_or_else(|| {
                    format!(
                        "the file {basename} is in directory {}, of {} the header lists",
                        directories[index],
                        dirnames.len()
                    )
                })?;
            let prefix = if directory.starts_with('/') { "." } else { "" };
            let carries_data = match file_type(index) {
                REGULAR => last_link.get(&(devices[index], inodes[index])) == Some(&index),
                SYMBOLIC_LINK => true,
                _ => false,
            };
            let mtime = i64::try_from(mtimes[index]).map_err(|_| {
                format!(
                    "the file {basename} has the modification time {}, out of range",
                    mtimes[index]
                )
            })?;
            Ok(Listed {
                name: format!("{prefix}{directory}{basename}"),
                mode: modes[index],
                mtime,
                size: if carries_data { sizes[index] } else { 0 },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
