//! FrameMaker Interchange Format files, as Tika's `MIFParser` reads them: the lines read as
//! `MIFExtractor` matches them into elements, the text of each `String` statement in a `Para`
//! written as a paragraph, and the pages counted by type.
//!
//! The text is written as it is. Tika escapes it with `StringEscapeUtils.escapeXml` before the
//! XHTML serializer escapes it again, so `&` reads `&amp;` in plain text; and it types every
//! file by the first of its types in `HashSet` order, where the native parser keeps the type
//! detected (#118).

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, lines, AutoDetectReader};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java;

/// `MIFParser.SUPPORTED_TYPES`.
const MEDIA_TYPES: [&str; 3] = [
    "application/vnd.mif",
    "application/x-maker",
    "application/x-mif",
];

/// Tika's `MIFParser`.
pub(crate) struct MifParser;

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

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        if let Some(reason) = AutoDetectReader::defers(input) {
            return Err(Declined::Deferred(reason.to_owned()));
        }
        Ok(Box::new(move |xhtml| write(input, xhtml)))
    }
}

fn write(input: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
    let content_type = xhtml.metadata_mut().get("Content-Type").map(str::to_owned);
    let reader = AutoDetectReader::read(input, content_type.as_deref())?;
    let metadata = xhtml.metadata_mut();
    auto_detect_reader::set_metadata(&reader.detection, metadata);
    metadata.set("Content-Encoding", reader.detection.decode_as.name());
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
    extract(&reader.text, &mut handler)?;
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
fn extract(text: &str, handler: &mut Mif<'_, '_>) -> Result<(), Failure> {
    let mut open: Vec<String> = Vec::new();
    for line in lines(text) {
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

/// A native result as Tika's path writes it (#118): typed `application/x-mif`, the first of
/// the parser's types in `HashSet` order, and the text of each paragraph escaped once more.
#[cfg(all(test, feature = "tika"))]
pub(crate) fn as_tika_writes_it(
    text: &mut String,
    metadata: &mut crate::formats::parity::ComparedMetadata,
    _: &crate::formats::parity::ComparedMetadata,
) {
    for (name, values) in metadata.iter_mut() {
        if name == "Content-Type" {
            *values = vec!["application/x-mif".to_owned()];
        }
    }
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('"', "&amp;quot;")
            .replace('\'', "&amp;apos;")
    };
    let xml = text.starts_with("<?xml") || text.starts_with("<html");
    let Some(body) = text.find("<body").filter(|_| xml) else {
        // Plain text: escaped once.
        *text = text
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;");
        return;
    };
    let content_type = r#"<meta name="Content-Type" content=""#;
    if let Some(start) = text.find(content_type) {
        let start = start + content_type.len();
        let end = start + text[start..].find('"').expect("the attribute's end");
        text.replace_range(start..end, "application/x-mif");
    }
    let mut rewritten = text[..body].to_owned();
    let mut rest = &text[body..];
    while let Some(start) = rest.find('<') {
        rewritten.push_str(&escape(&rest[..start]));
        let end = start
            + rest[start..]
                .find('>')
                .map_or(rest.len() - start, |end| end + 1);
        rewritten.push_str(&rest[start..end]);
        rest = &rest[end..];
    }
    rewritten.push_str(&escape(rest));
    *text = rewritten;
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_same_as_tika_rewriting, conformance_corpus, documents, Scratch,
    };

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

    #[test]
    fn writes_text_as_it_is() {
        // Tika escapes it once before the serializer escapes it again (#118).
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(MIF.as_bytes())
            .unwrap();
        assert!(text.contains("Fisk & \u{f8}l <i dag>"), "{text}");
        assert!(MEDIA_TYPES.contains(&metadata["Content-Type"][0].as_str()));
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_mif_files_as_tikas_path_does() {
        let scratch = Scratch::new("mif");
        let utf8 = scratch.write("frokost.mif", MIF.as_bytes());
        let latin1: Vec<u8> = MIF.chars().map(|c| u8::try_from(c).unwrap()).collect();
        let latin1 = scratch.write("frokost-latin1.mif", &latin1);
        let (parsed, deferred) = documents(&MifParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        assert_same_as_tika_rewriting(
            &[utf8.as_path(), latin1.as_path()],
            "org.apache.tika.parser.mif.MIFParser",
            as_tika_writes_it,
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_mif_files_as_tikas_path_does() {
        let (parsed, deferred) = documents(&MifParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika_rewriting(&paths, MifParser.tika_name(), as_tika_writes_it);
    }
}
