//! Java, Groovy and C++ source code, as Tika's `SourceCodeParser` reads it: the text read
//! through an `AutoDetectReader` line by line, each `@author` a creator and the lines counted;
//! then highlighted as XHTML by jhighlight's renderer for the language, whose JFlex scanner
//! [`lexer`] runs on the scanner's own tables ([`tables`]); and that XHTML parsed by jsoup and
//! walked by Tika's `TikaNodeFilter` into the document.

mod lexer;
mod tables;

use std::sync::OnceLock;

use lexer::{Scanner, Tables};

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::embedded::RESOURCE_NAME;
use crate::formats::html::{ParsedDocument, ParsedNode};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java;
use crate::mime::MediaType;

/// `System.lineSeparator()`, which ends each line of the code the renderer reads.
const LINE_SEPARATOR: &str = if cfg!(windows) { "\r\n" } else { "\n" };
/// The width `StringUtils.convertTabsToSpaces` expands a tab to the next multiple of.
const TAB_WIDTH: usize = 4;
/// `XhtmlRenderer.getXhtmlHeader`, the CSS of its `style` element left out: the head, which
/// the walk skips, and the body up to the code.
const HEADER: &str = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\"\n                      \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"en\" lang=\"en\">\n<head>\n    <meta http-equiv=\"content-type\" content=\"text/html; charset=ISO-8859-1\" />\n    <title></title>\n    <link rel=\"Help\" href=\"http://jhighlight.dev.java.net\" />\n    <style type=\"text/css\">\n    </style>\n</head>\n<body>\n<h1>";
/// `XhtmlRenderer.getXhtmlFooter`.
const FOOTER: &str = "</code>\n</body>\n</html>\n";

/// The characters jhighlight's `StringUtils.encodeHtml` writes as entities, by range, which the
/// renderer's writer therefore never has to encode in the document's charset.
const ENTITY_CHARACTERS: [(char, char); 4] = [
    ('\u{A0}', '\u{FF}'),
    ('\u{152}', '\u{3D6}'),
    ('\u{2002}', '\u{2666}'),
    ('"', '"'),
];

/// Where the parser's output departs from Tika's, for the parity tests: the HTML document it
/// writes, which the HTML parser reads by the WHATWG algorithm (#84).
#[cfg(all(feature = "tika", any(test, feature = "testing")))]
pub const DEPARTURES: crate::formats::parity::Departures = crate::formats::html::DEPARTURES;

/// Tika's `SourceCodeParser`.
pub struct SourceCodeParser;

impl Parser for SourceCodeParser {
    fn name(&self) -> &'static str {
        "extractous::SourceCodeParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.code.SourceCodeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["text/x-c++src", "text/x-java-source", "text/x-groovy"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        if let Some(reason) = AutoDetectReader::defers(input) {
            return Err(Declined::Deferred(reason.to_owned()));
        }
        let reader = AutoDetectReader::read(input, metadata.get("Content-Type"))
            .map_err(|_| Declined::Deferred("the text cannot be decoded natively".to_owned()))?;
        let name = metadata.get(RESOURCE_NAME).unwrap_or_default().to_owned();
        let unicode = reader.detection.decode_as.name().starts_with("UTF-");
        if !unicode && name.chars().any(|c| !c.is_ascii() && !is_entity(c)) {
            return Err(Declined::Deferred(
                "a resource name the renderer writes in the document's charset, which may not \
                 hold it"
                    .to_owned(),
            ));
        }
        Ok(Box::new(move |xhtml| {
            let content_type = xhtml
                .metadata_mut()
                .get("Content-Type")
                .map(str::to_owned)
                .ok_or_else(|| {
                    Failure::Malformed(
                        "TikaException: media type must be set in metadata before parse".to_owned(),
                    )
                })?;
            let metadata = xhtml.metadata_mut();
            auto_detect_reader::set_metadata(&reader.detection, metadata);
            let media_type = MediaType::parse(&content_type).ok_or_else(|| {
                Failure::Malformed(format!("the content type {content_type:?} does not parse"))
            })?;
            metadata.set("Content-Type", media_type.to_string());
            metadata.set("Content-Encoding", reader.detection.decode_as.name());
            let mut code = String::with_capacity(reader.text.len());
            let mut lines = 0;
            for line in auto_detect_reader::lines(&reader.text) {
                code.push_str(line);
                code.push_str(LINE_SEPARATOR);
                if let Some(author) = author(line) {
                    metadata.add("dc:creator", author);
                }
                lines += 1;
            }
            metadata.set("LoC", lines.to_string());
            let language = Language::of(&media_type.to_string()).ok_or_else(|| {
                Failure::Malformed(format!(
                    "TikaException: unparseable content type {media_type}"
                ))
            })?;
            if !unicode {
                code = code.replace('\u{FFFD}', "?");
            }
            let html = render(language, &name, &code);
            let document = ParsedDocument::parse(&html);
            let walked = walk(&document, document.root(), xhtml, &mut true);
            xhtml.end_document()?;
            walked
        }))
    }
}

/// Whether `StringUtils.encodeHtml` writes `c` as an entity.
fn is_entity(c: char) -> bool {
    ENTITY_CHARACTERS
        .iter()
        .any(|&(first, last)| (first..=last).contains(&c))
}

/// `parserAuthor`: what follows `@author ` on the line, matched ignoring ASCII case, up to a
/// line terminator, trimmed.
fn author(line: &str) -> Option<&str> {
    const TAG: &str = "@author ";
    let at = line.char_indices().map(|(index, _)| index).find(|&index| {
        line.get(index..index + TAG.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(TAG))
    })?;
    let rest = &line[at + TAG.len()..];
    let end = rest.find(java::is_line_terminator).unwrap_or(rest.len());
    Some(java::trim(&rest[..end]))
}

/// The languages Tika renders, with their renderer's scanner, CSS classes and actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    Java,
    Groovy,
    Cpp,
}

/// What a scanner's action does: the style of the token, and the lexical state it enters.
type Action = (u8, Option<usize>);

impl Language {
    /// `TYPES_TO_RENDERER`, keyed by the content type with its parameters.
    fn of(content_type: &str) -> Option<Self> {
        match content_type {
            "text/x-java-source" => Some(Self::Java),
            "text/x-groovy" => Some(Self::Groovy),
            "text/x-c++src" => Some(Self::Cpp),
            _ => None,
        }
    }

    fn scanner(self) -> &'static Scanner {
        static JAVA: OnceLock<Scanner> = OnceLock::new();
        static GROOVY: OnceLock<Scanner> = OnceLock::new();
        static CPP: OnceLock<Scanner> = OnceLock::new();
        let (cell, tables): (&'static OnceLock<Scanner>, &Tables) = match self {
            Self::Java => (&JAVA, &tables::JAVA),
            Self::Groovy => (&GROOVY, &tables::GROOVY),
            Self::Cpp => (&CPP, &tables::CPP),
        };
        Scanner::of(cell, tables)
    }

    /// The scanner's `yylex` actions: the style each returns, and the state it begins.
    fn action(self, action: i32) -> Action {
        match (self, action) {
            (_, 1) => (1, None),
            (_, 2) => (6, None),
            (_, 3) => (5, None),
            (_, 4) => (4, None),
            (_, 5) => (7, None),
            (_, 6) => (8, None),
            (_, 7) => (3, None),
            (_, 8) => (2, None),
            (Self::Java | Self::Cpp, 9) => (7, Some(1)),
            (Self::Groovy, 9) => (7, Some(2)),
            (_, 10) => (7, Some(0)),
            (_, 11) => (8, Some(0)),
            (_, 12) => (9, None),
            (Self::Java, 13) => (8, Some(2)),
            (Self::Groovy, 13) => (6, Some(1)),
            (Self::Cpp, 13) => (10, None),
            (Self::Java, 14) => (2, None),
            (Self::Groovy, 14) => (8, Some(3)),
            (Self::Cpp, 14) => (8, Some(2)),
            (Self::Groovy, 15) => (6, Some(0)),
            (language, action) => {
                unreachable!("{language:?}'s scanner has no action {action}")
            }
        }
    }

    /// `getCssClass`: the class of a style's spans.
    fn css_class(self, style: u8) -> &'static str {
        const JAVA: [&str; 9] = [
            "java_plain",
            "java_keyword",
            "java_type",
            "java_operator",
            "java_separator",
            "java_literal",
            "java_comment",
            "java_javadoc_comment",
            "java_javadoc_tag",
        ];
        const CPP: [&str; 10] = [
            "cpp_plain",
            "cpp_keyword",
            "cpp_type",
            "cpp_operator",
            "cpp_separator",
            "cpp_literal",
            "cpp_comment",
            "cpp_doxygen_comment",
            "cpp_doxygen_tag",
            "cpp_preproc",
        ];
        let classes: &[&str] = match self {
            Self::Java | Self::Groovy => &JAVA,
            Self::Cpp => &CPP,
        };
        classes[usize::from(style) - 1]
    }
}

/// `XhtmlRenderer.highlight` of `code`, as the string jsoup then parses: each line, its tabs
/// expanded, scanned into tokens written in spans of their style's class, a new span where the
/// style changes and at each line, spaces as no-break spaces, and each line ended with a
/// break. The entities jhighlight writes are written as the characters jsoup reads them as.
fn render(language: Language, name: &str, code: &str) -> String {
    let scanner = language.scanner();
    let mut html: Vec<u16> = Vec::with_capacity(code.len() * 2);
    html.extend(HEADER.encode_utf16());
    escape_into(name.encode_utf16(), &mut html);
    html.extend("</h1><code>".encode_utf16());
    let mut state = 0usize;
    let mut previous_style = 0u8;
    let mut newline = false;
    for line in auto_detect_reader::lines(code) {
        let line = tabs_to_spaces(line);
        let units: Vec<u16> = line.encode_utf16().chain([u16::from(b'\n')]).collect();
        let mut index = 0;
        while index < units.len() {
            let (action, length) = scanner
                .next(&units, index, state)
                .expect("the scanners' fallback rule matches any character");
            let (style, begin) = language.action(action);
            if let Some(begin) = begin {
                state = begin;
            }
            if style != previous_style || newline {
                if previous_style != 0 && !newline {
                    html.extend("</span>".encode_utf16());
                }
                html.extend("<span class=\"".encode_utf16());
                html.extend(language.css_class(style).encode_utf16());
                html.extend("\">".encode_utf16());
                previous_style = style;
            }
            newline = false;
            let token = units[index..index + length]
                .iter()
                .copied()
                .filter(|&unit| unit != u16::from(b'\n'))
                .map(|unit| if unit == u16::from(b' ') { 0xA0 } else { unit });
            escape_into(token, &mut html);
            index += length;
        }
        html.extend("</span><br />\n".encode_utf16());
        newline = true;
    }
    html.extend(FOOTER.encode_utf16());
    char::decode_utf16(html)
        .map(|unit| unit.unwrap_or('?'))
        .collect()
}

/// Writes `units` as markup text, `&`, `<` and `>` as their entities.
fn escape_into(units: impl IntoIterator<Item = u16>, html: &mut Vec<u16>) {
    for unit in units {
        match unit {
            0x26 => html.extend("&amp;".encode_utf16()),
            0x3C => html.extend("&lt;".encode_utf16()),
            0x3E => html.extend("&gt;".encode_utf16()),
            _ => html.push(unit),
        }
    }
}

/// `StringUtils.convertTabsToSpaces`: each tab as the spaces to the next multiple of the tab
/// width, counted in UTF-16 units.
fn tabs_to_spaces(line: &str) -> std::borrow::Cow<'_, str> {
    if !line.contains('\t') {
        return std::borrow::Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len() + TAB_WIDTH);
    let mut column = 0;
    for c in line.chars() {
        if c == '\t' {
            let size = TAB_WIDTH - column % TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', size));
            column += size;
        } else {
            out.push(c);
            column += c.len_utf16();
        }
    }
    std::borrow::Cow::Owned(out)
}

/// `TikaNodeFilter` over jsoup's tree from `node`: from the `html` element on, the `head`,
/// `meta`, `title`, `link` and `style` elements left out with what they hold, the `html` and
/// `body` elements but not what they hold, text and data written as characters, and every
/// other node as an element in no namespace with its attributes. `ignore` holds whether the
/// walk is outside the `html` element.
fn walk(
    document: &ParsedDocument,
    node: usize,
    xhtml: &mut Xhtml,
    ignore: &mut bool,
) -> Result<(), Failure> {
    let parsed = document.node(node);
    let name = match &parsed {
        ParsedNode::Document => "#document",
        ParsedNode::Element { name, .. } => name,
        ParsedNode::Text(_) => "#text",
        ParsedNode::Other(name) => name,
    };
    if name == "html" {
        *ignore = false;
    }
    let mut emitted = false;
    if !*ignore {
        if matches!(name, "head" | "meta" | "title" | "link" | "style") {
            return Ok(());
        }
        match &parsed {
            _ if matches!(name, "html" | "body") => {}
            ParsedNode::Text(text) => xhtml.characters(text)?,
            ParsedNode::Element { attributes, .. } => {
                xhtml.start_element_with(name, attributes)?;
                emitted = true;
            }
            ParsedNode::Document | ParsedNode::Other(_) => {
                xhtml.start_element(name)?;
                emitted = true;
            }
        }
    }
    for &child in document.children(node) {
        walk(document, child, xhtml, ignore)?;
    }
    if name == "html" {
        *ignore = true;
    }
    if emitted {
        xhtml.end_element_in("", name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
