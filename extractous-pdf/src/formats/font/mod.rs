//! Font files, as Tika's font module reads their metadata: TrueType fonts through FontBox's
//! `TTFParser`, whose port the PDF parser shares, and Adobe font metrics files through FontBox's
//! `AFMParser` ([`afm`]).

mod afm;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::set_nullable;
use crate::formats::metadata::TikaMetadata;
use crate::formats::pdf::truetype::{Failure, TrueType};
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::{Decline, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
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

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let font = TrueType::read_file(input).map_err(|failure| {
            Decline::Fails(match failure {
                Failure::Damaged(_) => {
                    Error::ParseError("FontBox fails to read the TrueType font".to_owned())
                }
                Failure::PostScript => Thrown::Io(Exception::new(
                    "java.io.IOException",
                    "True Type fonts using CFF outlines are not supported",
                ))
                .into_error(self.tika_name()),
                Failure::GlyphOutOfBounds { glyph } => Thrown::Runtime(Exception::new(
                    "java.lang.ArrayIndexOutOfBoundsException",
                    format!("Index {glyph} out of bounds for length {glyph}"),
                ))
                .into_error(self.tika_name()),
            })
        })?;
        let TrueType { header, names, .. } = font;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set("Content-Type", "application/x-font-ttf");
            metadata.set("dcterms:created", international_date(header.created));
            metadata.set("dcterms:modified", international_date(header.modified));
            metadata.set(DOC_VERSION, float_to_string(header.version));
            for record in &names {
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

    /// Reads the file twice, keeping none of its comments: once to read its metrics and find
    /// the comment the creation date is taken from, and again as its comments are written.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let fails = |thrown| Decline::Fails(Error::ParseError(thrown));
        let mut comments = 0usize;
        let mut dated = None;
        let metrics = {
            let paging = Paging::new(pages);
            afm::parse(input, &paging, |comment| {
                if dated.is_none() {
                    dated = afm::creation_date(&comment).map(|date| (comments, date));
                }
                comments += 1;
                Ok::<(), std::convert::Infallible>(())
            })
            .map_err(|interrupted| match interrupted {
                afm::Interrupted::Thrown(thrown) => fails(thrown),
                afm::Interrupted::By(never) => match never {},
            })?
        };
        let (dated, created) = match dated {
            Some((index, date)) => (Some(index), Some(date.map_err(fails)?)),
            None => (None, None),
        };
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            if let Some(created) = created {
                metadata.set("dcterms:created", created);
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
            if comments > usize::from(dated.is_some()) {
                xhtml.element("h1", "Comments")?;
                xhtml.start_element_with("div", &[("class", "comments")])?;
                let paging = Paging::new(pages);
                let mut index = 0;
                afm::parse(input, &paging, |comment| {
                    let written = Some(index) != dated;
                    index += 1;
                    match written {
                        true => xhtml.element("p", &comment),
                        false => Ok(()),
                    }
                })
                .map_err(|interrupted| match interrupted {
                    afm::Interrupted::By(stop) => stop,
                    afm::Interrupted::Thrown(thrown) => {
                        unreachable!("the metrics read once fail when read again: {thrown}")
                    }
                })?;
                xhtml.end_element("div")?;
            }
            Ok(())
        }))
    }
}

#[cfg(test)]
mod large {
    use extractous_core::large::{bodies_past_limit, LargeFile};

    use super::*;
    use crate::formats::pdf::truetype::programs::program;

    /// A font whose last table, `glyf`, makes it too large to read whole is read by its other
    /// tables alone.
    #[test]
    fn reads_fonts_too_large_to_read_whole_in_bounded_memory() {
        let mut font = program(&[(b"glyf", vec![0; 4])]);
        let chunk = vec![0u8; 1 << 20];
        let count = bodies_past_limit(chunk.len());
        let tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
        let length_at = 12 + 16 * (tables - 1) + 12;
        let length = u32::try_from(4 + count * chunk.len() as u64).unwrap();
        font[length_at..length_at + 4].copy_from_slice(&length.to_be_bytes());
        let file = LargeFile::new("large.ttf", &font, count, |_| chunk.clone(), &[]);
        let (_, metadata) = file.extract(TrueTypeParser.name());
        assert_eq!(metadata["Content-Type"], ["application/x-font-ttf"]);
        assert_eq!(metadata["dcterms:created"].len(), 1);
    }

    /// Metrics too large to read whole are read twice, keeping none of their comments. Their
    /// average width is FontBox's, summed in a `float`, which drifts from the width they all
    /// have over so many.
    #[test]
    fn reads_metrics_too_large_to_read_whole_in_bounded_memory() {
        let metric = b"C 65 ; WX 500 ; N A ; B 0 0 600 700 ;\n".to_vec();
        let comment = b"Comment A comment written as it is read again\n".to_vec();
        let count = bodies_past_limit(metric.len() + comment.len());
        let head = format!(
            "StartFontMetrics 2.0\nComment Creation Date: Mon Jan  1 00:00:00 2024\n\
             FontName Large\nStartCharMetrics {count}\n"
        );
        let mut tail = b"EndCharMetrics\n".to_vec();
        for _ in 0..count {
            tail.extend_from_slice(&comment);
        }
        tail.extend_from_slice(b"EndFontMetrics\n");
        let file = LargeFile::new(
            "large.afm",
            head.as_bytes(),
            count,
            |_| metric.clone(),
            &tail,
        );
        let (text, metadata) = file.extract(AdobeFontMetricParser.name());
        assert_eq!(metadata["dcterms:created"], ["Mon Jan  1 00:00:00 2024"]);
        let (total, widths) = (0..count).fold((0.0f32, 0.0f32), |(total, widths), _| {
            (total + 500.0, widths + 1.0)
        });
        assert_eq!(
            metadata["AvgCharacterWidth"],
            [float_to_string(total / widths)]
        );
        assert_ne!(metadata["AvgCharacterWidth"], ["500.0"]);
        assert_eq!(metadata[FONT_NAME], ["Large"]);
        assert!(
            text.contains("A comment written as it is read again"),
            "{text}"
        );
        assert!(!text.contains("Creation Date"), "{text}");
    }
}
