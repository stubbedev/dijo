//! Tika's `CompressorParser` over compressed files: the content, decoded whole with the
//! compressor their content type names or, failing that, the one their first bytes show,
//! handed to the embedded document extractor as one entry, named by the gzip header or after
//! the compressed file. The content is decoded to a spool, which keeps no more of it in memory
//! than a file read whole, so the parser's memory stays bounded whatever the file's size.

use super::{undecoded, within_limit, Cut};
use crate::compress::compressor::Compressor;
use crate::compress::gzip;
use crate::config::ParserConfigs;
use crate::formats::embedded::{Wrapping, INTERNAL_PATH, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
use crate::java::is_blank;
use crate::Error;

/// `MIMES_TO_NAME`'s compressors that the port decodes.
const NATIVE: [Compressor; 10] = [
    Compressor::Gzip,
    Compressor::Bzip2,
    Compressor::Zlib,
    Compressor::Z,
    Compressor::SnappyFramed,
    Compressor::Lz4Framed,
    Compressor::Xz,
    Compressor::Lzma,
    Compressor::Zstd,
    Compressor::Brotli,
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
    NATIVE[9].media_type(),
];

/// The types whose subtypes without parsers of their own Tika reads as the compressed files
/// they are, such as EMZ, a gzip stream holding an EMF picture.
const WITH_SUBTYPES: [&str; 1] = [Compressor::Gzip.media_type()];

/// Tika's `CompressorParser`.
pub struct CompressorParser;

impl Parser for CompressorParser {
    fn name(&self) -> &'static str {
        "extractous::CompressorParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.pkg.CompressorParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &WITH_SUBTYPES
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
        let content_type = metadata.get("Content-Type").unwrap_or_default();
        let (compressor, detected) = match Compressor::named(content_type) {
            Some(compressor) => (compressor, None),
            None => {
                let compressor = Compressor::detect(input).ok_or_else(|| {
                    Decline::Fails(Error::ParseError(format!(
                        "Unable to uncompress document stream: the {content_type} document \
                         starts with no compressor's signature"
                    )))
                })?;
                (compressor, detected_media_type(compressor))
            }
        };
        let what = format!("the {compressor:?} stream");
        let decoded = compressor
            .decode(input)
            .map_err(|reason| undecoded(&what, reason, self.tika_name()))?;
        paging.release();
        let (decoded, cut) = within_limit(what, decoded);
        let entry = entry_metadata(decoded.file_name, metadata.get(RESOURCE_NAME));
        let content = decoded.content;
        // The document ends whatever the entry's parse does, short of the output stopping:
        // Tika leaves it open when the parse fails (#100).
        Ok(Box::new(move |xhtml| {
            if let Some(media_type) = detected {
                xhtml.metadata_mut().set("Content-Type", media_type);
            }
            let parsed = xhtml
                .parse_embedded_input(&content, &entry, Wrapping::PackageEntry)
                .and_then(|()| Cut::end(cut));
            if !matches!(parsed, Err(Failure::Stopped(_))) {
                xhtml.end_document()?;
            }
            parsed
        }))
    }
}

/// The content type `getMediaType` gives the document of a compressor commons-compress detects,
/// none for those it leaves unnamed.
fn detected_media_type(compressor: Compressor) -> Option<&'static str> {
    match compressor {
        Compressor::Lz4Framed | Compressor::Zstd | Compressor::Brotli => None,
        named => Some(named.media_type()),
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

    fn shortfall(input: &[u8], content_type: &str, name: Option<&str>) -> Option<String> {
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", content_type);
        if let Some(name) = name {
            metadata.set(RESOURCE_NAME, name);
        }
        CompressorParser.shortfall(input, &metadata, &ParserConfigs::default())
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
        assert_eq!(
            shortfall(&gzip(b"text", None), "application/gzip", None),
            None
        );
        assert_eq!(
            shortfall(&gzip(b"text", None), "application/gzip; x=y", None),
            None
        );
        assert_eq!(
            shortfall(&gzip(b"text", None), "image/x-emf-compressed", None),
            None
        );
        let data = gzip(b"text", None);
        assert_eq!(
            shortfall(&data, "application/gzip", Some("\u{130}.Z")),
            None
        );
    }

    #[test]
    fn fails_data_that_does_not_decode_as_its_reader_fails_tikas_parser() {
        let data = gzip(b"text", None);
        let cut = &data[..data.len() - 3];
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", "application/gzip");
        let configs = ParserConfigs::default();
        let Err(Decline::Fails(Error::ParseError(reason))) =
            CompressorParser.prepare(cut, &metadata, &configs)
        else {
            panic!("data cut short is not failed");
        };
        assert!(
            reason.starts_with(
                "Parse error occurred : org.apache.tika.exception.TikaException: TIKA-198: \
                 Illegal IOException from org.apache.tika.parser.pkg.CompressorParser <- caused \
                 by java.io.IOException: the Gzip stream: reading it fails"
            ),
            "{reason}"
        );
    }

    #[test]
    fn fails_documents_whose_compressor_it_cannot_detect_as_tika_does() {
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", "image/x-emf-compressed");
        let configs = ParserConfigs::default();
        let declined = CompressorParser.prepare(b"no signature", &metadata, &configs);
        let Err(Decline::Fails(Error::ParseError(reason))) = declined else {
            panic!("a document without a compressor's signature is not failed");
        };
        assert!(reason.contains("Unable to uncompress"), "{reason}");
    }

    #[test]
    fn names_only_the_detected_compressors_get_media_type_names() {
        assert_eq!(
            detected_media_type(Compressor::Gzip),
            Some("application/gzip")
        );
        assert_eq!(detected_media_type(Compressor::Zstd), None);
        assert_eq!(detected_media_type(Compressor::Lz4Framed), None);
    }

    #[test]
    fn fails_streams_needing_more_memory_than_allowed_as_tika_does() {
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", "application/x-compress");
        let configs = ParserConfigs::default();
        let declined = CompressorParser.prepare(&[0x1F, 0x9D, 0x99, 0, 0], &metadata, &configs);
        let Err(crate::formats::Decline::Fails(crate::Error::LimitExceeded(reason))) = declined
        else {
            panic!("a stream beyond the memory limit is not failed as a limit");
        };
        assert!(reason.contains("KiB of memory"), "{reason}");
    }
}
