//! RTF documents, as Tika's `RTFParser` reads them: the text of the body in paragraphs, list
//! items, bold and italic runs and hyperlinks, decoded with the code pages the document names,
//! the title, authors, dates and counts of its `\info` group as metadata, and its pictures and
//! embedded objects parsed as documents of their own.

mod charsets;
mod compressed;
mod embedded;
mod extractor;
mod html;
mod input;

pub(crate) use compressed::decompress;
pub(crate) use html::decapsulate;

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use extractor::Interrupt;

const MEDIA_TYPE: &str = "application/rtf";

pub struct RtfParser;

impl Parser for RtfParser {
    fn name(&self) -> &'static str {
        "dijo::RTFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.rtf.RTFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// It reads the document once, writing its text as it goes and releasing what it has read.
    /// It holds the state of the groups it is in, the text Tika's `TextExtractor` holds until a
    /// control word or group writes it, and the picture or object being read, spooled to a file
    /// past what is read whole.
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
        Ok(Box::new(move |xhtml| write(input, pages, xhtml)))
    }
}

/// `RTFParser.parseInline` of `input` into another document's `xhtml`, its header's metadata
/// into fresh metadata.
pub(crate) fn write_inline(input: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
    extractor::extract_inline(input, xhtml, TikaMetadata::new()).map_err(Interrupt::into_failure)
}

/// Parses as `RTFParser.parse`, which ends the document even when the parse fails.
fn write<'a>(input: &'a [u8], pages: Pages<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
    let Err(interrupt) = extractor::extract(input, pages, xhtml) else {
        return Ok(());
    };
    xhtml.end_document()?;
    Err(interrupt.into_failure())
}

#[cfg(test)]
mod tests {

    /// The XHTML the native path extracts from `rtf`.
    fn extract(rtf: &[u8]) -> String {
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(rtf)
            .unwrap()
            .0
    }

    /// A document too large to read whole is read once, in place, its text written as it goes
    /// and what it has read released, in memory that does not grow with it.
    #[test]
    fn extracts_documents_too_large_to_read_whole_in_bounded_memory() {
        use dijo_core::large::{bodies_past_limit, LargeFile};
        let filler = "0123456789abcdef".repeat(4096);
        let body = |index: u64| {
            format!("{{\\*\\datastore {filler}}}\\pard Linje {index} i logbogen\\par\n")
                .into_bytes()
        };
        let count = bodies_past_limit(body(0).len());
        let file = LargeFile::new("large.rtf", br"{\rtf1\ansi ", count, body, b"}");
        let (text, _) = file.extract("dijo::RTFParser");
        assert!(text.contains("Linje 0 i logbogen\n"), "{text}");
        let last = format!("Linje {} i logbogen", count - 1);
        assert!(
            text.trim_end().ends_with(&last),
            "{}",
            &text[text.len() - 200..]
        );
    }

    #[test]
    fn parses_pictures_as_embedded_documents() {
        let xhtml = extract(br"{\rtf1\ansi\pard x{\pict\pngblip 89504e470d0a1a0a}y\par}");
        assert!(
            xhtml.contains(r#"<div class="package-entry"><h1>embedded-0.png</h1>"#),
            "{xhtml}"
        );
        assert!(xhtml.contains("xy"), "{xhtml}");
    }

    #[test]
    fn reads_creation_dates_as_utc_wall_clock_times() {
        // Tika reads them in the JVM's default zone, and makes something of any fields (#65).
        let created = |info: &str| {
            let xhtml = extract(format!(r"{{\rtf1\ansi{{\info{info}}}\pard a\par}}").as_bytes());
            let start = xhtml.find(r#"<meta name="dcterms:created" content=""#)?;
            let value = &xhtml[start + 38..];
            Some(value[..value.find('"').unwrap()].to_owned())
        };
        let date = |text: &str| Some(text.to_owned());
        assert_eq!(
            created(r"{\creatim\yr2006\mo3\dy1\hr10\min15}"),
            date("2006-03-01T10:15:00Z")
        );
        assert_eq!(
            created(r"{\creatim\yr2024\mo3\dy31\hr2\min30}"),
            date("2024-03-31T02:30:00Z")
        );
        assert_eq!(
            created(r"{\creatim\yr2024\mo2\dy29}"),
            date("2024-02-29T00:00:00Z")
        );
        assert_eq!(created(r"{\creatim\yr2023\mo2\dy29}"), None);
        assert_eq!(created(r"{\creatim\yr1500\mo14\dy-3\hr30\min-5}"), None);
        assert_eq!(created(r"{\creatim\yr2024\mo1\dy1\hr24}"), None);
        assert_eq!(created(r"{\creatim}"), None);
        assert_eq!(created(r"{\creatim\yr0\mo0\dy0\hr0\min0}"), None);
        // Each date group names its own fields: the revision date's do not fill in creation's.
        assert_eq!(created(r"{\revtim\yr2007\mo4\dy2}{\creatim\hr1}"), None);
        assert_eq!(
            created(r"{\creatim\yr2007\mo4\dy2}{\revtim\yr2008\mo5\dy3}"),
            date("2007-04-02T00:00:00Z")
        );
    }
}
