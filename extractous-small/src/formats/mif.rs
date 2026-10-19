//! FrameMaker Interchange Format files, as Tika's `MIFParser` reads them: the lines read as
//! `MIFExtractor` matches them into elements, the text of each `String` statement in a `Para`
//! written as a paragraph, and the pages counted by type.
//!
//! The text is written as it is. Tika escapes it with `StringEscapeUtils.escapeXml` before the
//! XHTML serializer escapes it again, so `&` reads `&amp;` in plain text; and it types every
//! file by the first of its types in `HashSet` order, where the native parser keeps the type
//! detected (#118).

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{unless_different, Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::java;

/// `MIFParser.SUPPORTED_TYPES`.
const MEDIA_TYPES: [&str; 3] = [
    "application/vnd.mif",
    "application/x-maker",
    "application/x-mif",
];

/// Tika's `MIFParser`.
pub struct MifParser;

impl Parser for MifParser {
    fn name(&self) -> &'static str {
        "extractous::MIFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.mif.MIFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
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
        unless_different(
            Box::new(move |xhtml| write(input, pages, xhtml)),
            AutoDetectReader::difference(input, metadata.get("Content-Type")),
        )
    }
}

/// Writes the elements of `input`, its lines read as they are decoded, its `pages` released as
/// they are read.
fn write<'a>(input: &'a [u8], pages: Pages<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let content_type = xhtml.metadata_mut().get("Content-Type").map(str::to_owned);
    let (detection, text) = AutoDetectReader::stream(input, content_type.as_deref(), pages);
    let metadata = xhtml.metadata_mut();
    auto_detect_reader::set_metadata(&detection, metadata);
    metadata.set("Content-Encoding", detection.decode_as.name());
    let detected = content_type.filter(|detected| MEDIA_TYPES.contains(&detected.as_str()));
    metadata.set(
        "Content-Type",
        detected.unwrap_or_else(|| MEDIA_TYPES[0].to_owned()),
    );
    let mut handler = Mif {
        xhtml: &mut *xhtml,
        in_content: false,
        in_page: false,
        body_pages: 0,
        master_pages: 0,
        reference_pages: 0,
    };
    extract(text.lines(), &mut handler)?;
    Ok(())
}

/// Whether Java's regex `\s` matches `c`.
fn is_space(c: char) -> bool {
    java::is_regex_space(c)
}

/// Whether Java's regex `.` matches `c`: anything but a line terminator.
fn is_any(c: char) -> bool {
    !matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `openTagPattern`, `<(\S*).*`, matched against all of `line`: the name after the `<`.
fn open_tag(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('<')?;
    let name_end = rest.find(is_space).unwrap_or(rest.len());
    rest[name_end..]
        .chars()
        .all(is_any)
        .then(|| &rest[..name_end])
}

/// `selfTagPattern`, `(<(\S*))(\s)(.*)(\>).*`, matched against all of `line`: the name after
/// the `<`, and what lies between the space after it and the last `>` the rest reaches.
fn self_tag(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('<')?;
    let name_end = rest.find(is_space)?;
    let name = &rest[..name_end];
    let space = rest[name_end..].chars().next()?;
    let after = &rest[name_end + space.len_utf8()..];
    let reach = after.find(|c: char| !is_any(c)).unwrap_or(after.len());
    if reach != after.len() {
        return None;
    }
    let close = after.rfind('>')?;
    Some((name, &after[..close]))
}

/// `MIFExtractor.parse`: each line an open tag, a close tag or a statement on one line.
fn extract(lines: impl Iterator<Item = String>, handler: &mut Mif<'_, '_>) -> Result<(), Failure> {
    let mut open: Vec<String> = Vec::new();
    for line in lines {
        let line = line.as_str();
        let trimmed = java::trim(line);
        if line.contains('<') && !line.contains('>') {
            if let Some(name) = open_tag(trimmed) {
                open.push(name.to_owned());
                handler.start_element(name)?;
            }
        } else if trimmed.starts_with('>') {
            let closing = &trimmed[trimmed.rfind(' ').map_or(0, |space| space + 1)..];
            let Some(parent) = open.last().cloned() else {
                continue;
            };
            if *closing == parent {
                open.pop();
            }
            handler.end_element(&parent)?;
        } else if let Some((name, value)) = self_tag(trimmed) {
            handler.start_element(name)?;
            let value = value.strip_prefix('`').unwrap_or(value);
            let value = value.strip_suffix('\'').unwrap_or(value);
            handler.characters(value)?;
            handler.end_element(name)?;
        }
    }
    handler.end_document();
    Ok(())
}

/// `MIFContentHandler`.
struct Mif<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    in_content: bool,
    in_page: bool,
    body_pages: u32,
    master_pages: u32,
    reference_pages: u32,
}

impl Mif<'_, '_> {
    fn start_element(&mut self, name: &str) -> Result<(), Failure> {
        match name {
            "PageType" => self.in_page = true,
            "Para" => self.xhtml.start_element("p")?,
            "String" => self.in_content = true,
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        if self.in_content {
            self.xhtml.characters_chunk(text)?;
        }
        if self.in_page {
            match text {
                "BodyPage" => self.body_pages += 1,
                "LeftMasterPage" | "RightMasterPage" | "OtherMasterPage" => {
                    self.master_pages += 1;
                }
                "ReferencePage" => self.reference_pages += 1,
                _ => {}
            }
        }
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Failure> {
        match name {
            "String" => self.in_content = false,
            "Para" => self.xhtml.end_element("p")?,
            "PageType" => self.in_page = false,
            _ => {}
        }
        Ok(())
    }

    fn end_document(&mut self) {
        let metadata = self.xhtml.metadata_mut();
        metadata.set("meta:page-count", self.body_pages.to_string());
        metadata.set("mif:master-page-count", self.master_pages.to_string());
        metadata.set("mif:reference-page-count", self.reference_pages.to_string());
        metadata.set(
            "mif:total-page-count",
            (self.body_pages + self.reference_pages + self.master_pages).to_string(),
        );
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn matches_lines_as_mif_extractor_does() {
        assert_eq!(open_tag("<Para "), Some("Para"));
        assert_eq!(open_tag("<Para"), Some("Para"));
        assert_eq!(open_tag("x<Para"), None);
        assert_eq!(
            self_tag("<String `Ost & sk\u{f8}l'> # note"),
            Some(("String", "`Ost & sk\u{f8}l'"))
        );
        assert_eq!(self_tag("<String `a>b'>"), Some(("String", "`a>b'")));
        assert_eq!(self_tag("<String>"), None);
        assert_eq!(self_tag("<A \u{2028}>"), None);
    }

    const MIF: &str = "<MIFFile 2015> # Generated by FrameMaker\n\
<Page \n <PageType BodyPage>\n> # end of Page\n\
<Page \n <PageType LeftMasterPage>\n> # end of Page\n\
<Page \n <PageType ReferencePage>\n> # end of Page\n\
<TextFlow \n <Para \n  <PgfTag `Body'>\n  <ParaLine \n   <String `Fisk & \u{f8}l <i dag>'>\n\
   <Char Tab>\n   <String `Sm\u{f8}rrebr\u{f8}d'>\n  > # end of ParaLine\n > # end of Para\n\
 <Para \n  <ParaLine \n   <String `'>\n  > # end of ParaLine\n > # end of Para\n\
> # end of TextFlow\n> # end of nothing\n# End of MIFFile\n";

    /// A MIF file too large to read whole is read a line at a time, each paragraph written as
    /// it ends, which `MIFExtractor` finds by the comment FrameMaker closes it with.
    #[test]
    fn reads_mif_files_too_large_to_read_whole() {
        let para = |index: u64| {
            format!(
                " <Para \n  <ParaLine \n   <String `Linje {index}'>\n  > # end of ParaLine\n \
                 > # end of Para\n"
            )
            .into_bytes()
        };
        let count = extractous_core::large::bodies_past_limit(para(0).len());
        let file = extractous_core::large::LargeFile::new(
            "long.mif",
            b"<MIFFile 2015>\n<TextFlow \n",
            count,
            para,
            b"> # end of TextFlow\n",
        );
        let mut paragraphs = 0;
        file.extract_lines(MifParser.name(), |line| {
            if !line.is_empty() {
                assert_eq!(line, format!("Linje {paragraphs}"));
                paragraphs += 1;
            }
        });
        assert_eq!(paragraphs, count);
    }

    #[test]
    fn writes_text_as_it_is() {
        // Tika escapes it once before the serializer escapes it again (#118).
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(MIF.as_bytes())
            .unwrap();
        assert!(text.contains("Fisk & \u{f8}l <i dag>"), "{text}");
        assert!(MEDIA_TYPES.contains(&metadata["Content-Type"][0].as_str()));
    }
}
