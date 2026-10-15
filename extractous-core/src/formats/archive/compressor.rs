//! Tika's `CompressorParser` over the compressed files whose content type names the compressor
//! it decodes them with: the content, decoded whole, handed to the embedded document extractor
//! as one entry, named by the gzip header or after the compressed file.

use crate::compress::{bzip2, gzip, lzw, zlib, Undecoded};
use crate::config::ParserConfigs;
use crate::formats::embedded::{Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::handlers::is_blank;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};

/// A compressor `CompressorParser` decodes natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compressor {
    Gzip,
    Bzip2,
    Zlib,
    Z,
}

/// `MIMES_TO_NAME`: the content types that name a compressor, each with it.
const NAMED: [(&str, Compressor); 4] = [
    ("application/gzip", Compressor::Gzip),
    ("application/x-bzip2", Compressor::Bzip2),
    ("application/zlib", Compressor::Zlib),
    ("application/x-compress", Compressor::Z),
];

/// What a compressed file decodes into.
struct Decoded {
    /// The original file name a gzip header holds.
    file_name: Option<String>,
    content: Vec<u8>,
}

impl Compressor {
    /// The compressor the exact content type names.
    fn named(content_type: &str) -> Option<Self> {
        NAMED
            .iter()
            .find(|(named, _)| *named == content_type)
            .map(|(_, compressor)| *compressor)
    }

    fn decode(self, input: &[u8]) -> Result<Decoded, Undecoded> {
        let content = |content| Decoded {
            file_name: None,
            content,
        };
        match self {
            Compressor::Gzip => gzip::decode(input).map(|member| Decoded {
                file_name: member.file_name,
                content: member.content,
            }),
            Compressor::Bzip2 => bzip2::decode(input).map(content),
            Compressor::Zlib => zlib::decode(input).map(content),
            Compressor::Z => lzw::decode(input).map(content),
        }
    }
}

/// Tika's `CompressorParser`.
pub(crate) struct CompressorParser;

impl Parser for CompressorParser {
    fn name(&self) -> &'static str {
        "extractous::CompressorParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/gzip",
            "application/x-bzip2",
            "application/zlib",
            "application/x-compress",
        ]
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
        entry_metadata(decoded.file_name, metadata.get(RESOURCE_NAME))
            .err()
            .map(|LowerCaseMismatch| {
                "lower-casing the file name changes its length, which GzipUtils mixes up".to_owned()
            })
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
}
