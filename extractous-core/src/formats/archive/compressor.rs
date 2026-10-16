//! Tika's `CompressorParser` over the compressed files whose content type names the compressor
//! it decodes them with: the content, decoded whole, handed to the embedded document extractor
//! as one entry, named by the gzip header or after the compressed file.

use super::nesting::Scan;
use crate::compress::compressor::Compressor;
use crate::compress::gzip;
use crate::config::ParserConfigs;
use crate::formats::embedded::{Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java::is_blank;

/// `MIMES_TO_NAME`'s compressors that the port decodes.
const NATIVE: [Compressor; 9] = [
    Compressor::Gzip,
    Compressor::Bzip2,
    Compressor::Zlib,
    Compressor::Z,
    Compressor::SnappyFramed,
    Compressor::Lz4Framed,
    Compressor::Xz,
    Compressor::Lzma,
    Compressor::Zstd,
];

const MEDIA_TYPES: [&str; NATIVE.len()] = [
    NATIVE[0].media_type(),
    NATIVE[1].media_type(),
    NATIVE[2].media_type(),
    NATIVE[3].media_type(),
    NATIVE[4].media_type(),
    NATIVE[5].media_type(),
    NATIVE[6].media_type(),
    NATIVE[7].media_type(),
    NATIVE[8].media_type(),
];

/// Tika's `CompressorParser`.
pub(crate) struct CompressorParser;

impl Parser for CompressorParser {
    fn name(&self) -> &'static str {
        "extractous::CompressorParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.pkg.CompressorParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let content_type = metadata.get("Content-Type").unwrap_or_default();
        let Some(compressor) = Compressor::named(content_type) else {
            return Err(Declined::Deferred(format!(
                "the content type {content_type} names no compressor, which commons-compress \
                 then detects"
            )));
        };
        let decoded = compressor
            .decode(input)
            .map_err(|undecoded| format!("the {compressor:?} stream: {undecoded}"))?;
        let mut scan = Scan::new();
        if let Some(reason) = scan.entry(&decoded.content) {
            return Err(Declined::Deferred(reason));
        }
        let entry = entry_metadata(decoded.file_name, metadata.get(RESOURCE_NAME));
        if let Some(reason) = scan.document(input.len()) {
            return Err(Declined::Deferred(reason));
        }
        let content = decoded.content;
        // The document ends whatever the entry's parse does, short of the output stopping:
        // Tika leaves it open when the parse fails (#100).
        Ok(Box::new(move |xhtml| {
            let parsed = xhtml.parse_embedded(&content, &entry, Wrapping::PackageEntry);
            if !matches!(parsed, Err(Failure::Stopped(_))) {
                xhtml.end_document()?;
            }
            parsed
        }))
    }
}

/// The metadata of the entry: the name in the gzip header (`extractGzipMetadata`) unless blank,
/// else the name `setName` makes of the compressed file's `resource_name`.
fn entry_metadata(file_name: Option<String>, resource_name: Option<&str>) -> TikaMetadata {
    let mut entry = TikaMetadata::new();
    if let Some(name) = file_name.filter(|name| !is_blank(name)) {
        entry.set(INTERNAL_PATH, name.clone());
        entry.set(RESOURCE_NAME, name);
    } else if let Some(name) = resource_name.and_then(entry_name) {
        entry.set(RESOURCE_NAME, name);
    }
    entry
}

/// `setName`: the name of the compressed file `resource_name` without the compressor's
/// extension, or none for a blank one. Suffixes match in any case: Tika's own list matches them
/// case-sensitively, so `FILE.GZ` and `file.gz` got different names (#100).
fn entry_name(resource_name: &str) -> Option<String> {
    if is_blank(resource_name) {
        return None;
    }
    let name = file_name(resource_name);
    let stem = |name: &str| name[..name.rfind('.').expect("a suffix")].to_owned();
    let ends_with = |suffixes: &[&str]| {
        suffixes
            .iter()
            .any(|suffix| gzip::ends_with_ignoring_case(name, suffix))
    };
    Some(if ends_with(&[".tgz", ".tbz", ".tbz2"]) {
        format!("{}.tar", stem(name))
    } else if ends_with(&[".bz", ".gz", ".bz2", ".xz", ".zlib", ".pack", ".br"]) {
        stem(name)
    } else {
        gzip::uncompressed_file_name(name)
    })
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
        let named = |name: &str| Some(name.to_owned());
        assert_eq!(entry_name("dir/a.tgz"), named("a.tar"));
        assert_eq!(entry_name("C:x.tar.gz"), named("x.tar"));
        assert_eq!(entry_name("a.TGZ"), named("a.tar"));
        assert_eq!(entry_name("test.svgz"), named("test.svg"));
        assert_eq!(entry_name("x.Z"), named("x"));
        assert_eq!(entry_name("data.br"), named("data"));
        assert_eq!(entry_name("dir/.."), named(""));
        assert_eq!(entry_name(" "), None);
        assert_eq!(entry_name("\u{130}.Z"), named("\u{130}"));
        assert_eq!(entry_name("FILE.GZ"), named("FILE"));
        assert_eq!(entry_name("A.TBZ2"), named("A.tar"));
    }

    #[test]
    fn decodes_what_commons_compress_decodes_whole() {
        assert_eq!(defers(&gzip(b"text", None), "application/gzip", None), None);
        let reason = defers(&gzip(b"text", None), "application/gzip; x=y", None).unwrap();
        assert!(reason.contains("names no compressor"), "{reason}");
        let data = gzip(b"text", None);
        let reason = defers(&data[..data.len() - 3], "application/gzip", None).unwrap();
        assert!(reason.contains("reading it fails"), "{reason}");
        assert_eq!(defers(&data, "application/gzip", Some("\u{130}.Z")), None);
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
