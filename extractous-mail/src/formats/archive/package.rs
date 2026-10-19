//! Tika's `PackageParser` over TAR, ar, cpio, ARJ and Unix dump archives: each file but a
//! directory handed to the embedded document extractor with the metadata `handleEntryMetadata`
//! gives it, in archive order, and a file whose data the archive's stream cannot read recorded
//! as such, its name written in a paragraph. The archive is read as the format its detected
//! type names, and keeps that type; Tika reads it as the format `ArchiveStreamFactory`
//! recognizes in its bytes and sets that format's type, but for GNU TAR (#101). Without a type
//! the parser names, it is read as the format of its bytes: ZIP data by its local headers, as
//! `ZipArchiveInputStream` reads it; 7z data refused, as commons-compress refuses to stream it.
//! The files' data is read in place, so the parser's memory stays bounded whatever the
//! archive's size, but for a sparse TAR file, compressed ZIP data and a dump's data, which are
//! decoded to a spool.

use super::{undecoded, within_limit, write_file, Cut};
use crate::compress::archiver::Archiver;
use crate::compress::arj::{self, ArjData};
use crate::compress::dump;
use crate::compress::zip::stream::{Failure as ZipFailure, ZipStream};
use crate::compress::zip::Decoded;
use crate::compress::{ar, cpio, tar, ArchiveFile, Limited, Undecoded};
use crate::config::ParserConfigs;
use crate::formats::embedded::record_stream_exception;
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Difference, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
use crate::mime::MediaType;

const GTAR: &str = "application/x-gtar";
const ARJ: &str = "application/x-arj";
const DUMP: &str = "application/x-tika-unix-dump";

/// The format `content_type` names.
fn named(content_type: &str) -> Option<Archiver> {
    match MediaType::parse(content_type)?.base_type().as_str() {
        "application/x-tar" | GTAR => Some(Archiver::Tar),
        "application/x-archive" => Some(Archiver::Ar),
        "application/x-cpio" => Some(Archiver::Cpio),
        ARJ => Some(Archiver::Arj),
        DUMP => Some(Archiver::Dump),
        _ => None,
    }
}

/// An entry of the archive as the parser handles it.
enum Packaged<'a> {
    /// A file whose data the archive's stream reads, or a directory.
    Read(ArchiveFile<'a>),
    /// A file whose data it does not read, `canReadEntryData` false, by its name.
    Unread(String),
}

/// What the parser reads of an archive before it writes it: its entries, where the native path
/// stops decoding, if it does, and the first detail in which the native reading differs from
/// Tika's.
struct Package<'a> {
    entries: Vec<Packaged<'a>>,
    cut: Option<Cut>,
    difference: Option<Difference>,
}

impl<'a> Package<'a> {
    /// An archive of `files`, all of them read.
    fn of(files: Vec<ArchiveFile<'a>>, cut: Option<Cut>) -> Self {
        Package {
            entries: files.into_iter().map(Packaged::Read).collect(),
            cut,
            difference: None,
        }
    }
}

/// The error of an archive `ArchiveStreamFactory` opens no stream of, for the reason `cause`:
/// Tika's parser is unable to unpack it.
fn unable_to_unpack(cause: Exception) -> Decline<'static> {
    Decline::Fails(
        Thrown::Tika(
            Exception::new(
                "org.apache.tika.exception.TikaException",
                "Unable to unpack document stream",
            )
            .caused_by(cause),
        )
        .into_error(PackageParser.tika_name()),
    )
}

/// The format `content_type` names, else the one `ArchiveStreamFactory` recognizes in `input`,
/// whose pages are `pages`, and its entries as its stream gives them, or why the parser fails
/// on it.
fn read<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    content_type: Option<&str>,
) -> Result<Package<'a>, Decline<'static>> {
    let archiver = content_type
        .and_then(named)
        .or_else(|| Archiver::detect(input))
        .ok_or_else(|| {
            unable_to_unpack(Exception::new(
                "org.apache.commons.compress.archivers.ArchiveException",
                "No Archiver found for the stream signature",
            ))
        })?;
    let what = format!("the {archiver:?} archive");
    let failed = |reason| undecoded(&what, reason, PackageParser.tika_name());
    match archiver {
        Archiver::Tar => {
            let (files, cut) =
                within_limit(what.clone(), tar::files(input, pages).map_err(failed)?);
            Ok(Package::of(files, cut))
        }
        Archiver::Ar => Ok(Package::of(ar::files(input, pages).map_err(failed)?, None)),
        Archiver::Cpio => Ok(Package::of(
            cpio::files(input, pages).map_err(failed)?,
            None,
        )),
        Archiver::Arj => read_arj(input, pages).map_err(|unread| match unread {
            arj::Unread::Unopened(reason) => unable_to_unpack(Exception::new(
                "org.apache.commons.compress.archivers.ArchiveException",
                reason,
            )),
            arj::Unread::Fails(reason) => failed(reason),
        }),
        Archiver::Zip => read_zip(input, pages, &what),
        Archiver::SevenZ => Err(unable_to_unpack(Exception::new(
            "org.apache.commons.compress.archivers.StreamingNotSupportedException",
            "The 7z doesn't support streaming.",
        ))),
        Archiver::Dump => {
            let limited = dump::entries(input).map_err(|unread| match unread {
                dump::Unread::Unopened(reason) => unable_to_unpack(Exception::new(
                    "org.apache.commons.compress.archivers.ArchiveException",
                    reason,
                )),
                dump::Unread::Fails(reason) => failed(reason),
            })?;
            let files = limited.map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| ArchiveFile {
                        name: entry.name,
                        size: entry.size,
                        modified_millis: Some(entry.modified_millis),
                        created_millis: None,
                        directory: entry.directory,
                        content: entry.content,
                    })
                    .collect()
            });
            let (files, cut) = within_limit(what.clone(), files);
            Ok(Package::of(files, cut))
        }
    }
}

/// The entries of the ARJ archive `input`, whose pages are `pages`: stored data in place,
/// compressed data unread.
fn read_arj<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<Package<'a>, arj::Unread> {
    let entries = arj::entries(input)?
        .into_iter()
        .map(|entry| match entry.data {
            ArjData::Compressed if !entry.directory => Packaged::Unread(entry.name),
            data => Packaged::Read(ArchiveFile {
                name: entry.name,
                size: entry.size,
                modified_millis: entry.modified_millis,
                created_millis: None,
                directory: entry.directory,
                content: match data {
                    ArjData::Stored(bytes) => crate::input::Input::Within(bytes, pages),
                    ArjData::Compressed => crate::input::Input::Borrowed(&[]),
                },
            }),
        })
        .collect();
    Ok(Package {
        entries,
        cut: None,
        difference: None,
    })
}

/// The entries of the ZIP data `input`, whose pages are `pages`, `what` it is, read by their
/// local headers as `ZipArchiveInputStream` reads them for the parser: stored data in place,
/// compressed data decoded to a spool, an entry the stream does not read unread.
fn read_zip<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    what: &str,
) -> Result<Package<'a>, Decline<'static>> {
    let failed =
        |reason: String| undecoded(what, Undecoded::Fails(reason), PackageParser.tika_name());
    let mut stream = ZipStream::new(input, false);
    let mut entries = Vec::new();
    let mut difference = None;
    let mut cut = None;
    while let Some(entry) = stream
        .next_entry()
        .map_err(|failure| failed(format!("the stream fails: {failure:?}")))?
    {
        if entry.is_directory() {
            continue;
        }
        if entry.streams_without_decompressor() {
            difference = difference.or_else(|| {
                Some(Difference::new(format!(
                    "the entry {} has a data descriptor after data compressed with method {}, \
                     for which commons-compress's stream opens no decompressor, so that Tika's \
                     path fails the archive with a NullPointerException, where the native \
                     parser reads the data to its end",
                    entry.name, entry.method
                )))
            });
        }
        let file = |content| ArchiveFile {
            name: entry.name.clone(),
            size: entry.size,
            modified_millis: entry
                .modified()
                .map(|modified| modified.seconds * 1000 + i64::from(modified.nanos / 1_000_000)),
            created_millis: None,
            directory: false,
            content,
        };
        match stream.spool(pages) {
            Err(ZipFailure::Unsupported | ZipFailure::DataDescriptor) => {
                entries.push(Packaged::Unread(entry.name.clone()));
            }
            Err(failure) => return Err(failed(format!("the stream fails: {failure:?}"))),
            Ok(Err(reason)) => return Err(undecoded(what, reason, PackageParser.tika_name())),
            Ok(Ok(Decoded::Complete(content))) => entries.push(Packaged::Read(file(content))),
            Ok(Ok(Decoded::Truncated(_))) => {
                return Err(failed(format!(
                    "the data of the entry {} is cut short or corrupt",
                    entry.name
                )))
            }
            Ok(Ok(Decoded::Oversized(content))) => {
                let (content, cutting) = within_limit(what.to_owned(), Limited::Cut(content));
                entries.push(Packaged::Read(file(content)));
                cut = cutting;
                break;
            }
        }
    }
    Ok(Package {
        entries,
        cut,
        difference,
    })
}

/// Tika's `PackageParser`.
pub struct PackageParser;

impl Parser for PackageParser {
    fn name(&self) -> &'static str {
        "extractous::PackageParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.pkg.PackageParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/x-tar",
            GTAR,
            "application/x-archive",
            "application/x-cpio",
            ARJ,
            DUMP,
        ]
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
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let paging = Paging::new(pages);
        let package = read(input, pages, metadata.get("Content-Type"))?;
        paging.release();
        let Package {
            entries,
            cut,
            difference,
        } = package;
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            let written = write_entries(&entries, xhtml, &paging).and_then(|()| Cut::end(cut));
            xhtml.end_document()?;
            written
        });
        match difference {
            None => Ok(reading),
            Some(difference) => Err(Decline::Differs {
                reading,
                difference,
            }),
        }
    }
}

/// `PackageParser`'s loop over the entries but the directories: a file whose data the stream
/// reads handed to the embedded document extractor, the pages of the archive released by
/// `paging` as its files are read; one it does not read recorded, its name in a paragraph.
fn write_entries(entries: &[Packaged], xhtml: &mut Xhtml, paging: &Paging) -> Result<(), Failure> {
    for entry in entries {
        match entry {
            Packaged::Read(file) if file.directory => {}
            Packaged::Read(file) => {
                write_file(file, xhtml)?;
                paging.read(file.content.len());
            }
            Packaged::Unread(name) => {
                record_stream_exception(
                    xhtml.metadata_mut(),
                    &format!(
                        "org.apache.tika.exception.TikaException: Can't read archive stream \
                         ({name})"
                    ),
                );
                xhtml.element("p", name)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::arj::tests::arj;
    use crate::compress::cpio::tests::cpio;
    use crate::compress::tar_header::tests::tar;
    use crate::compress::zip::file::tests::zip;

    fn defers(input: &[u8]) -> Option<String> {
        PackageParser.defers(input, &TikaMetadata::new(), &ParserConfigs::default())
    }

    /// The error preparing `input` fails with.
    fn failure(input: &[u8]) -> String {
        let configs = ParserConfigs::default();
        let failure = match PackageParser.prepare(input, &TikaMetadata::new(), &configs) {
            Err(Decline::Fails(error)) => error.to_string(),
            Err(declined) => panic!("not failed: {declined:?}"),
            Ok(_) => panic!("not failed"),
        };
        failure
    }

    /// Each entry of `input`, typed `content_type`, as the parser reads it: its name, and its
    /// data where the stream reads it.
    fn entries(input: &[u8], content_type: Option<&str>) -> Vec<(String, Option<Vec<u8>>)> {
        let Ok(package) = read(input, Pages::IN_MEMORY, content_type) else {
            panic!("the archive is not read");
        };
        package
            .entries
            .into_iter()
            .map(|entry| match entry {
                Packaged::Read(file) => (file.name, Some(file.content.to_vec())),
                Packaged::Unread(name) => (name, None),
            })
            .collect()
    }

    #[test]
    fn reads_what_commons_compress_streams_without_an_archive_type() {
        assert_eq!(defers(&tar(&[(b"a.txt", b'0', b"text")])), None);
        assert_eq!(defers(&cpio(&[("a.txt", b"text")])), None);
        let zipped = zip(&[("dir/", b""), ("a.txt", b"text")]);
        assert_eq!(
            entries(&zipped, None),
            [("a.txt".to_owned(), Some(b"text".to_vec()))]
        );
        let mut seven_z = b"7z\xBC\xAF\x27\x1C".to_vec();
        seven_z.resize(64, 0);
        let refused = failure(&seven_z);
        assert!(
            refused.contains("The 7z doesn't support streaming."),
            "{refused}"
        );
    }

    #[test]
    fn reads_stored_arj_entries_and_records_compressed_ones_unread() {
        let archive = arj(&[
            (b"DIR", 0, 3, b""),
            (b"A.TXT", 0, 0, b"stored"),
            (b"B.BIN", 1, 0, b"\x01\x02"),
        ]);
        assert_eq!(
            entries(&archive, Some(ARJ)),
            [
                ("DIR".to_owned(), Some(Vec::new())),
                ("A.TXT".to_owned(), Some(b"stored".to_vec())),
                ("B.BIN".to_owned(), None),
            ]
        );
        let (text, metadata) = extractous_core::Extractor::new()
            .extract_bytes_to_string(&archive)
            .unwrap();
        assert!(text.contains("stored"), "{text}");
        let exceptions = &metadata[crate::formats::embedded::EMBEDDED_STREAM_EXCEPTION];
        assert!(
            exceptions
                .iter()
                .any(|exception| exception.contains("Can't read archive stream (B.BIN)")),
            "{exceptions:?}"
        );
    }

    #[test]
    fn reads_unix_dumps() {
        use crate::compress::dump::tests::{data, directory, dump, header, DIRECTORY, FILE};
        let tape = dump(&[
            header(2, 2, DIRECTORY, 1024, 1),
            directory(&[(2, "."), (2, ".."), (10, "notes.txt")]),
            header(2, 10, FILE, 13, 1),
            data(b"dumped notes\n"),
        ]);
        assert_eq!(
            entries(&tape, Some(DUMP)),
            [
                (String::new(), Some(Vec::new())),
                ("notes.txt".to_owned(), Some(b"dumped notes\n".to_vec())),
            ]
        );
        assert_eq!(entries(&tape, None), entries(&tape, Some(DUMP)));
    }

    #[test]
    fn fails_what_commons_compress_fails_on_as_tika_does() {
        assert_eq!(
            failure(b"plain text"),
            "Parse error occurred : org.apache.tika.exception.TikaException: Unable to unpack \
             document stream <- caused by org.apache.commons.compress.archivers.\
             ArchiveException: No Archiver found for the stream signature"
        );
        let data = tar(&[(b"a.txt", b'0', b"text")]);
        let old_cpio = b"070707000000000000000000000000000000000000000000";
        let io = "TIKA-198: Illegal IOException from org.apache.tika.parser.pkg.PackageParser <- \
                  caused by java.io.IOException: the";
        let cut = failure(&data[..515]);
        assert!(
            cut.contains(&format!("{io} Tar archive: reading it fails")),
            "{cut}"
        );
        let old_cpio = failure(old_cpio);
        assert!(
            old_cpio.contains(&format!("{io} Cpio archive: reading it fails")),
            "{old_cpio}"
        );
    }
}
