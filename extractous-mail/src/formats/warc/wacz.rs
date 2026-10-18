//! Web archive collections, WACZ, as Tika's `WACZParser` reads them: each file under
//! `archive/`, in the order of the central directory, handed to the embedded document extractor,
//! gunzipped where it starts as gzip does; the data package and everything else left unread.
//! Tika's also hands on each folder under `archive/` as an empty document (#118).

use crate::compress::gzip;
use crate::compress::zip::file::ZipFile;
use crate::compress::zip::Content;
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Deferral, Opened, Parser, Prepared};

/// The folder of a collection's archives.
const ARCHIVE: &str = "archive/";
/// `GZIPInputStream.GZIP_MAGIC`, as the first two bytes of a stream.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// An archive of the collection, as the embedded document extractor is handed it.
#[derive(Debug)]
struct Archive {
    path: String,
    name: String,
    size: i64,
    data: Vec<u8>,
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
    ) -> Result<Prepared<'a>, Decline<'static>> {
        let zip = ZipFile::open(input).map_err(|_| {
            Decline::Fails(Error::ParseError(
                "the WACZ collection is not a ZIP archive".to_owned(),
            ))
        })?;
        let archives = archives(&zip).map_err(Decline::Deferred)?;
        Ok(Box::new(move |xhtml| {
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
            }
            Ok(())
        }))
    }
}

/// The archives of the collection, an archive gunzipped where its first two bytes are gzip's,
/// as commons-compress's `IOUtils.readFully` reads them, zero past a shorter entry's end; the
/// port leaves to Tika an entry it does not read whole and gzip that does not decode, which fail
/// while the archive's parser reads.
fn archives(zip: &ZipFile<'_>) -> Result<Vec<Archive>, Deferral> {
    let mut archives = Vec::new();
    for entry in zip.entries().iter().filter(|entry| !entry.is_directory()) {
        let Some(name) = entry.name.strip_prefix(ARCHIVE) else {
            continue;
        };
        let data = match zip.read(entry) {
            Some(Content::Complete(data)) => data.into_owned(),
            _ => return Err("an archive entry the port does not read whole".to_owned()),
        };
        let data = if data.starts_with(&GZIP_MAGIC) {
            gzip::decode(&data)
                .map_err(|_| "a gzip archive the port does not decode".to_owned())?
                .content
        } else {
            data
        };
        archives.push(Archive {
            path: entry.name.clone(),
            name: name.to_owned(),
            size: entry.size,
            data,
        });
    }
    Ok(archives)
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
    fn hands_on_no_folders() {
        let collection = zip(&[
            ("archive/", b""),
            ("archive/data.warc", WARC),
            ("archive/2024/", b""),
        ]);
        let archives = archives(&ZipFile::open(&collection).unwrap()).unwrap();
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
