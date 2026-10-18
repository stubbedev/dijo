//! Font files, as Tika's font module reads their metadata: TrueType fonts through FontBox's
//! `TTFParser`, whose port the PDF parser shares, and Adobe font metrics files through FontBox's
//! `AFMParser` ([`afm`]).

mod afm;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::set_nullable;
use crate::formats::metadata::TikaMetadata;
use crate::formats::pdf::truetype::{Read, TrueType};
use crate::formats::{Decline, Parser, Prepared};
use crate::java::jfloat::float_to_string;
use crate::java::time::format_instant;

/// Milliseconds from 1904-01-01, where TrueType counts its dates from, to the Java epoch.
const MILLIS_1904: i64 = -2_082_844_800_000;

/// `NameRecord` name identifiers.
const NAME_COPYRIGHT: u16 = 0;
const NAME_FONT_FAMILY_NAME: u16 = 1;
const NAME_FONT_SUB_FAMILY_NAME: u16 = 2;
const NAME_FULL_FONT_NAME: u16 = 4;
const NAME_POSTSCRIPT_NAME: u16 = 6;
const NAME_TRADEMARK: u16 = 7;

/// The metadata names Tika's font parsers write, `AdobeFontMetricParser.MET_*`.
const DOC_VERSION: &str = "DocVersion";
const FONT_NAME: &str = "FontName";
const FONT_FAMILY_NAME: &str = "FontFamilyName";

/// `readInternationalDate` of `seconds` since 1904, as `DateUtils.formatDate` writes it.
fn international_date(seconds: i64) -> String {
    format_instant(MILLIS_1904.wrapping_add(seconds.wrapping_mul(1000)))
}

pub struct TrueTypeParser;

impl Parser for TrueTypeParser {
    fn name(&self) -> &'static str {
        "extractous::TrueTypeParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.font.TrueTypeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-font-ttf"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let font = match TrueType::read_file(input) {
            Read::Font(font) => font,
            Read::Damaged(_) => {
                return Err(Decline::Fails(Error::ParseError(
                    "FontBox fails to read the TrueType font".to_owned(),
                )))
            }
            Read::Unported(reason) => return Err(Decline::Deferred(reason)),
        };
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set("Content-Type", "application/x-font-ttf");
            metadata.set("dcterms:created", international_date(font.header.created));
            metadata.set("dcterms:modified", international_date(font.header.modified));
            metadata.set(DOC_VERSION, float_to_string(font.header.version));
            for record in &font.names {
                let string = record.string.as_deref();
                match record.name_id {
                    NAME_FONT_FAMILY_NAME => set_nullable(metadata, FONT_FAMILY_NAME, string),
                    NAME_FONT_SUB_FAMILY_NAME => {
                        set_nullable(metadata, "FontSubFamilyName", string);
                    }
                    NAME_FULL_FONT_NAME => {
                        set_nullable(metadata, FONT_NAME, string);
                        set_nullable(metadata, "dc:title", string);
                    }
                    NAME_POSTSCRIPT_NAME => set_nullable(metadata, "PSName", string),
                    NAME_COPYRIGHT => set_nullable(metadata, "dc:rights", string),
                    NAME_TRADEMARK => set_nullable(metadata, "Trademark", string),
                    _ => {}
                }
            }
            Ok(())
        }))
    }
}

pub struct AdobeFontMetricParser;

impl Parser for AdobeFontMetricParser {
    fn name(&self) -> &'static str {
        "extractous::AdobeFontMetricParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.font.AdobeFontMetricParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-font-adobe-metric"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let metrics =
            afm::parse(input).map_err(|thrown| Decline::Fails(Error::ParseError(thrown)))?;
        let (created, comments) = afm::creation_date(&metrics.comments)
            .map_err(|thrown| Decline::Fails(Error::ParseError(thrown)))?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            if let Some(created) = &created {
                metadata.set("dcterms:created", created.clone());
            }
            metadata.set("Content-Type", "application/x-font-adobe-metric");
            set_nullable(metadata, "dc:title", metrics.full_name.as_deref());
            metadata.add(
                "AvgCharacterWidth",
                float_to_string(metrics.average_character_width()),
            );
            metadata.add(DOC_VERSION, float_to_string(metrics.afm_version));
            let texts = [
                (FONT_NAME, &metrics.font_name),
                ("FontFullName", &metrics.full_name),
                (FONT_FAMILY_NAME, &metrics.family_name),
                ("FontVersion", &metrics.font_version),
                ("FontWeight", &metrics.weight),
                ("FontNotice", &metrics.notice),
            ];
            for (name, value) in texts {
                if let Some(value) = value {
                    metadata.add(name, value.clone());
                }
            }
            metadata.add(
                "FontUnderlineThickness",
                float_to_string(metrics.underline_thickness),
            );
            if !comments.is_empty() {
                xhtml.element("h1", "Comments")?;
                xhtml.start_element_with("div", &[("class", "comments")])?;
                for comment in &comments {
                    xhtml.element("p", comment)?;
                }
                xhtml.end_element("div")?;
            }
            Ok(())
        }))
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

    #[test]
    fn extracts_font_metrics_as_tikas_path_does() {
        let scratch = Scratch::new("afm");
        let read = [scratch.write(
            "metrics.afm",
            b"StartFontMetrics 2.0\nComment Creation Date: Mon Jan  1 00:00:00 2024\n\
              Comment Hej\nFontName Test-Bold\nFullName Test Bold\nFamilyName Test\nWeight Bold\n\
              Version 1.0\nNotice Fri font\nUnderlineThickness 33.5\nStartCharMetrics 2\n\
              C 32 ; WX 250 ; N space ; B 0 0 0 0 ;\nC 65 ; WX 667 ; N A ; B 0 0 600 700 ;\n\
              EndCharMetrics\nStartKernData\nStartKernPairs 1\nKPX A A -10\nEndKernPairs\n\
              EndKernData\nEndFontMetrics\n",
        )];
        let failing = [scratch.write(
            "unknown.afm",
            b"StartFontMetrics 2.0\nFontName X\nBogus 1\nEndFontMetrics\n",
        )];
        let (parsed, deferred) = documents(&AdobeFontMetricParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len() + failing.len(), "{parsed:?}");
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            "org.apache.tika.parser.font.AdobeFontMetricParser",
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    /// The fonts PDFBox bundles, which the repository holds for the PDF parser: a TrueType font
    /// and the metrics of the standard 14 fonts, whose kerning pairs `just pdf-resources` cut
    /// while it kept the pairs' count, which FontBox then fails on.
    const BUNDLED_FONTS: &str = "models/pdf";

    #[test]
    fn extracts_bundled_fonts_as_tikas_path_does() {
        let (fonts, deferred) = documents(&TrueTypeParser, &[Path::new(BUNDLED_FONTS)]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(fonts.len(), 1, "{fonts:?}");
        assert_same_as_tika(
            &fonts.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            "org.apache.tika.parser.font.TrueTypeParser",
        );
        let (metrics, deferred) = documents(&AdobeFontMetricParser, &[Path::new(BUNDLED_FONTS)]);
        assert!(deferred.is_empty(), "{deferred:?}");
        let (cut, whole): (Vec<&Path>, Vec<&Path>) =
            metrics.iter().map(PathBuf::as_path).partition(|path| {
                std::fs::read_to_string(path)
                    .unwrap()
                    .contains("StartKernPairs")
            });
        assert_eq!((whole.len(), cut.len()), (6, 8), "{metrics:?}");
        assert_same_as_tika(&whole, "org.apache.tika.parser.font.AdobeFontMetricParser");
        assert_fails_as_tika(&cut);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_fonts_as_tikas_path_does() {
        for (parser, tika_name) in [
            (
                &TrueTypeParser as &dyn crate::formats::Parser,
                "org.apache.tika.parser.font.TrueTypeParser",
            ),
            (
                &AdobeFontMetricParser,
                "org.apache.tika.parser.font.AdobeFontMetricParser",
            ),
        ] {
            let (parsed, deferred) = documents(parser, &[&conformance_corpus()]);
            assert!(deferred.is_empty(), "{deferred:?}");
            assert_eq!(parsed.len(), 1, "{parsed:?}");
            let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
            assert_same_as_tika(&paths, tika_name);
        }
    }
}
