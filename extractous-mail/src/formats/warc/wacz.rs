//! Web archive collections, WACZ, as Tika's `WACZParser` reads them: each file under
//! `archive/`, in the order of the central directory, handed to the embedded document extractor,
//! gunzipped where it starts as gzip does; the data package and everything else left unread.
//! Tika's also hands on each folder under `archive/` as an empty document (#118).

use crate::compress::zip::file::ZipFile;
use crate::compress::zip::Content;
use crate::compress::{gzip, Limited};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Difference, Failure, Opened, Parser, Prepared};
use crate::input::Input;

/// The folder of a collection's archives.
const ARCHIVE: &str = "archive/";
/// `GZIPInputStream.GZIP_MAGIC`, as the first two bytes of a stream.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// An archive of the collection, as the embedded document extractor is handed it: as much of
/// it as is read, and, where it is not read whole, the failure of the collection after it.
#[derive(Debug)]
struct Archive<'a> {
    path: String,
    name: String,
    size: i64,
    data: Input<'a>,
    unread: Option<Unread>,
}

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
                crate::compress::Undecoded::TooLarge
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

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let zip = ZipFile::open(input).map_err(|_| {
            Decline::Fails(Error::ParseError(
                "the WACZ collection is not a ZIP archive".to_owned(),
            ))
        })?;
        let archives = archives(&zip);
        let unread = archives
            .last()
            .and_then(|archive| archive.unread.map(|unread| (unread, archive.path.clone())));
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            for archive in &archives {
                let mut metadata = TikaMetadata::new();
                metadata.set(INTERNAL_PATH, archive.path.as_str());
                metadata.set(RESOURCE_NAME, archive.name.as_str());
                metadata.set("Content-Length", archive.size.to_string());
                xhtml.parse_embedded_opened(
                    &archive.data,
                    &metadata,
                    Wrapping::PackageEntry,
                    Opened::Alone,
                )?;
                if let Some(unread) = archive.unread {
                    return Err(unread.failure(&archive.path));
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

/// The archives of the collection, an archive gunzipped where its first two bytes are gzip's,
/// as commons-compress's `IOUtils.readFully` reads them, zero past a shorter entry's end, up to
/// the first that is not read whole, as much of it as is read, which fails the collection while
/// the archive's parser reads.
fn archives<'a>(zip: &ZipFile<'a>) -> Vec<Archive<'a>> {
    let mut archives = Vec::new();
    for entry in zip.entries().iter().filter(|entry| !entry.is_directory()) {
        let Some(name) = entry.name.strip_prefix(ARCHIVE) else {
            continue;
        };
        let (data, unread) = match zip.read(entry) {
            Some(Content::Complete(data)) => (Input::from(data), None),
            Some(Content::Truncated(data)) => (data.into(), Some(Unread::Cut)),
            Some(Content::Oversized(data)) => (data.into(), Some(Unread::TooLarge)),
            None => (Input::Borrowed(&[]), Some(Unread::Cut)),
        };
        let (data, unread) = match unread {
            None if data.starts_with(&GZIP_MAGIC) => match gzip::decode(&data) {
                Ok(Limited::Whole(member)) => (member.content, None),
                Ok(Limited::Cut(member)) => (member.content, Some(Unread::TooLarge)),
                Err(_) => (Input::Borrowed(&[]), Some(Unread::Gzip)),
            },
            unread => (data, unread),
        };
        archives.push(Archive {
            path: entry.name.clone(),
            name: name.to_owned(),
            size: entry.size,
            data,
            unread,
        });
        if unread.is_some() {
            break;
        }
    }
    archives
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::compress::gzip::tests::gzip;
    use crate::compress::zip::file::tests::zip;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

    const WARC: &[u8] = b"WARC/1.0\r\nWARC-Type: response\r\nWARC-Record-ID: <urn:uuid:1>\r\n\
WARC-Target-URI: http://example.dk/\r\nContent-Type: application/http; msgtype=response\r\n\
Content-Length: 64\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nGod morgen, Danmark\r\n\r\n\r\n";

    const PACKAGE: &[u8] = br#"{"profile": "data-package", "resources": []}"#;

    #[test]
    fn extracts_web_archive_collections_as_tikas_path_does() {
        let scratch = Scratch::new("wacz");
        let gzipped = gzip(WARC, Some("data.warc"));
        let read = scratch.write(
            "samling.wacz",
            &zip(&[
                ("datapackage.json", PACKAGE),
                ("archive/data.warc", WARC),
                ("archive/data.warc.gz", &gzipped),
                ("pages/pages.jsonl", b"{}"),
            ]),
        );
        let short = scratch.write(
            "kort.wacz",
            &zip(&[
                ("datapackage.json", PACKAGE),
                ("archive/data.warc", WARC),
                ("archive/kort.warc", b"\x1f"),
                ("archive/tom.warc", b""),
            ]),
        );
        let (parsed, deferred) = documents(&WaczParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        assert_same_as_tika(&[read.as_path(), short.as_path()], WaczParser.tika_name());
    }

    #[test]
    fn reads_up_to_an_archive_that_does_not_gunzip_as_differing() {
        let collection = zip(&[
            ("archive/data.warc", WARC),
            ("archive/broken.warc.gz", b"\x1f\x8b not gzip"),
            ("archive/after.warc", WARC),
        ]);
        let archives = archives(&ZipFile::open(&collection).unwrap());
        let read: Vec<(&str, Option<Unread>)> = archives
            .iter()
            .map(|archive| (archive.path.as_str(), archive.unread))
            .collect();
        assert_eq!(
            read,
            [
                ("archive/data.warc", None),
                ("archive/broken.warc.gz", Some(Unread::Gzip))
            ]
        );
        let configs = ParserConfigs::default();
        assert!(matches!(
            WaczParser.prepare(&collection, &TikaMetadata::new(), &configs),
            Err(Decline::Differs { .. })
        ));
    }

    #[test]
    fn hands_on_no_folders() {
        let collection = zip(&[
            ("archive/", b""),
            ("archive/data.warc", WARC),
            ("archive/2024/", b""),
        ]);
        let archives = archives(&ZipFile::open(&collection).unwrap());
        let paths: Vec<&str> = archives
            .iter()
            .map(|archive| archive.path.as_str())
            .collect();
        assert_eq!(paths, ["archive/data.warc"]);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_web_archive_collections_as_tikas_path_does() {
        let (parsed, deferred) = documents(&WaczParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, WaczParser.tika_name());
    }
}
