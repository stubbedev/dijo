//! Tika's `CompressorParser` over the compressed files whose content type names the compressor
//! it decodes them with: the content, decoded whole, handed to the embedded document extractor
//! as one entry, named by the gzip header or after the compressed file.

use super::nesting::Scan;
use crate::compress::compressor::Compressor;
use crate::compress::gzip;
use crate::config::ParserConfigs;
use crate::formats::embedded::{Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::handlers::is_blank;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};

/// `MIMES_TO_NAME`'s compressors that the port decodes.
const NATIVE: [Compressor; 6] = [
    Compressor::Gzip,
    Compressor::Bzip2,
    Compressor::Zlib,
    Compressor::Z,
    Compressor::SnappyFramed,
    Compressor::Lz4Framed,
];

const MEDIA_TYPES: [&str; NATIVE.len()] = [
    NATIVE[0].media_type(),
    NATIVE[1].media_type(),
    NATIVE[2].media_type(),
    NATIVE[3].media_type(),
    NATIVE[4].media_type(),
    NATIVE[5].media_type(),
];

/// Tika's `CompressorParser`.
pub(crate) struct CompressorParser;

impl Parser for CompressorParser {
    fn name(&self) -> &'static str {
        "extractous::CompressorParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    fn defers(
        &self,
        input: &[u8],
        metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<String> {
        let content_type = metadata.get("Content-Type").unwrap_or_default();
        let Some(compressor) = Compressor::named(content_type) else {
            return Some(format!(
                "the content type {content_type} names no compressor, which commons-compress \
                 then detects"
            ));
        };
        let decoded = match compressor.decode(input) {
            Ok(decoded) => decoded,
            Err(undecoded) => return Some(format!("the {compressor:?} stream: {undecoded}")),
        };
        let mut scan = Scan::new();
        if let Some(reason) = scan.entry(&decoded.content) {
            return Some(reason);
        }
        if entry_metadata(decoded.file_name, metadata.get(RESOURCE_NAME)).is_err() {
            return Some(
                "lower-casing the file name changes its length, which GzipUtils mixes up"
                    .to_owned(),
            );
        }
        scan.document(input.len())
    }

    fn parse(
        &self,
        input: &[u8],
        _configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let metadata = xhtml.metadata_mut();
        let content_type = metadata.get("Content-Type").unwrap_or_default();
        let compressor = Compressor::named(content_type)
            .ok_or_else(|| Failure::Malformed(format!("{content_type} names no compressor")))?;
        let decoded = compressor
            .decode(input)
            .map_err(|undecoded| Failure::Malformed(undecoded.to_string()))?;
        let entry = entry_metadata(decoded.file_name, metadata.get(RESOURCE_NAME)).map_err(
            |LowerCaseMismatch| {
                Failure::Malformed("the file name's lower case differs in length".to_owned())
            },
        )?;
        xhtml.parse_embedded(&decoded.content, &entry, Wrapping::PackageEntry)
    }
}

/// Lower-casing a name changed its length, which `GzipUtils` mixes up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LowerCaseMismatch;

/// The metadata of the entry: the name in the gzip header (`extractGzipMetadata`) unless blank,
/// else the name `setName` makes of the compressed file's `resource_name`.
fn entry_metadata(
    file_name: Option<String>,
    resource_name: Option<&str>,
) -> Result<TikaMetadata, LowerCaseMismatch> {
    let mut entry = TikaMetadata::new();
    if let Some(name) = file_name.filter(|name| !is_blank(name)) {
        entry.set(INTERNAL_PATH, name.clone());
        entry.set(RESOURCE_NAME, name);
    } else if let Some(name) = resource_name.map(entry_name).transpose()?.flatten() {
        entry.set(RESOURCE_NAME, name);
    }
    Ok(entry)
}

/// `setName`: the name of the compressed file `resource_name` without the compressor's
/// extension, or none for a blank one.
fn entry_name(resource_name: &str) -> Result<Option<String>, LowerCaseMismatch> {
    if is_blank(resource_name) {
        return Ok(None);
    }
    let name = file_name(resource_name);
    let stem = |name: &str| name[..name.rfind('.').expect("a suffix")].to_owned();
    let name = if [".tgz", ".tbz", ".tbz2"].iter().any(|s| name.ends_with(s)) {
        format!("{}.tar", stem(name))
    } else if [".bz", ".gz", ".bz2", ".xz", ".zlib", ".pack", ".br"]
        .iter()
        .any(|s| name.ends_with(s))
    {
        stem(name)
    } else if name.is_empty() {
        String::new()
    } else {
        gzip::uncompressed_file_name(name).ok_or(LowerCaseMismatch)?
    };
    Ok(Some(name))
}

/// Tika's `FilenameUtils.getName`: what follows the last slash, backslash or colon, nothing for
/// `.` and `..`.
fn file_name(path: &str) -> &str {
    let name = path
        .rfind(['/', '\\', ':'])
        .map_or(path, |separator| &path[separator + 1..]);
    if name == "." || name == ".." {
        ""
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::gzip::tests::gzip;

    fn defers(input: &[u8], content_type: &str, name: Option<&str>) -> Option<String> {
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", content_type);
        if let Some(name) = name {
            metadata.set(RESOURCE_NAME, name);
        }
        CompressorParser.defers(input, &metadata, &ParserConfigs::default())
    }

    #[test]
    fn names_the_entry_as_set_name_does() {
        let named = |name: &str| Ok(Some(name.to_owned()));
        assert_eq!(entry_name("dir/a.tgz"), named("a.tar"));
        assert_eq!(entry_name("C:x.tar.gz"), named("x.tar"));
        assert_eq!(entry_name("a.TGZ"), named("a.tar"));
        assert_eq!(entry_name("test.svgz"), named("test.svg"));
        assert_eq!(entry_name("x.Z"), named("x"));
        assert_eq!(entry_name("data.br"), named("data"));
        assert_eq!(entry_name("dir/.."), named(""));
        assert_eq!(entry_name(" "), Ok(None));
        assert_eq!(entry_name("\u{130}.Z"), Err(LowerCaseMismatch));
    }

    #[test]
    fn decodes_what_commons_compress_decodes_whole() {
        assert_eq!(defers(&gzip(b"text", None), "application/gzip", None), None);
        let reason = defers(&gzip(b"text", None), "application/gzip; x=y", None).unwrap();
        assert!(reason.contains("names no compressor"), "{reason}");
        let data = gzip(b"text", None);
        let reason = defers(&data[..data.len() - 3], "application/gzip", None).unwrap();
        assert!(reason.contains("commons-compress throws"), "{reason}");
        let reason = defers(&data, "application/gzip", Some("\u{130}.Z")).unwrap();
        assert!(reason.contains("lower-casing"), "{reason}");
        assert_eq!(
            defers(
                &gzip(b"text", Some("named")),
                "application/gzip",
                Some("\u{130}.Z")
            ),
            None
        );
    }

    #[test]
    fn defers_what_tika_may_take_for_a_zip_bomb() {
        let bomb = gzip(&vec![b'a'; 600_000], None);
        let reason = defers(&bomb, "application/gzip", None).unwrap();
        assert!(reason.contains("zip bomb"), "{reason}");
        assert_eq!(
            defers(&gzip(&vec![b'a'; 400_000], None), "application/gzip", None),
            None
        );
    }
}
