//! Java, Groovy and C++ source code, as Tika's `SourceCodeParser` reads it: the text read
//! through an `AutoDetectReader` line by line, each `@author` a creator and the lines counted;
//! then highlighted as XHTML by jhighlight's renderer for the language, whose JFlex scanner
//! [`lexer`] runs on the scanner's own tables ([`tables`]); and that XHTML parsed by jsoup and
//! walked by Tika's `TikaNodeFilter` into the document.
//!
//! The text is read twice as it is decoded, a line at a time: once for the authors and the
//! lines counted, which the head shows, and again as it is highlighted. Each line's markup is
//! balanced and ends where no text runs on, so the code is parsed and walked a chunk of lines at
//! a time inside the `code` element of the rest of the renderer's XHTML, which is parsed
//! without them.

mod lexer;
mod tables;

use std::sync::OnceLock;

use lexer::{Scanner, Tables};

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::embedded::RESOURCE_NAME;
use crate::formats::html::{walk_parsed, ParsedNode, Visitor};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{unless_different, Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::java;
use crate::mime::MediaType;

/// The width `StringUtils.convertTabsToSpaces` expands a tab to the next multiple of.
const TAB_WIDTH: usize = 4;
/// `XhtmlRenderer.getXhtmlHeader`, the CSS of its `style` element left out: the head, which
/// the walk skips, and the body up to the code.
const HEADER: &str = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\"\n                      \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"en\" lang=\"en\">\n<head>\n    <meta http-equiv=\"content-type\" content=\"text/html; charset=ISO-8859-1\" />\n    <title></title>\n    <link rel=\"Help\" href=\"http://jhighlight.dev.java.net\" />\n    <style type=\"text/css\">\n    </style>\n</head>\n<body>\n<h1>";
/// `XhtmlRenderer.getXhtmlFooter`.
const FOOTER: &str = "</code>\n</body>\n</html>\n";
/// The UTF-16 units of rendered code parsed at a time.
const CHUNK: usize = 1 << 16;

/// The characters jhighlight's `StringUtils.encodeHtml` writes as entities, by range, which the
/// renderer's writer therefore never has to encode in the document's charset.
const ENTITY_CHARACTERS: [(char, char); 4] = [
    ('\u{A0}', '\u{FF}'),
    ('\u{152}', '\u{3D6}'),
    ('\u{2002}', '\u{2666}'),
    ('"', '"'),
];

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
        let difference = AutoDetectReader::difference(input, metadata.get("Content-Type"));
        let given = metadata.get("Content-Type").map(str::to_owned);
        let (detection, _) = AutoDetectReader::stream(input, given.as_deref(), pages);
        let name = metadata.get(RESOURCE_NAME).unwrap_or_default().to_owned();
        let unicode = detection.decode_as.name().starts_with("UTF-");
        if !unicode && name.chars().any(|c| !c.is_ascii() && !is_entity(c)) {
            return Err(Decline::Unread(
                "a resource name the renderer writes in the document's charset, which may not \
                 hold it"
                    .to_owned(),
            ));
        }
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            let content_type = xhtml
                .metadata_mut()
                .get("Content-Type")
                .map(str::to_owned)
                .ok_or_else(|| {
                    Failure::Malformed(
                        "TikaException: media type must be set in metadata before parse".to_owned(),
                    )
                })?;
            let (detection, text) = AutoDetectReader::stream(input, given.as_deref(), pages);
            let metadata = xhtml.metadata_mut();
            auto_detect_reader::set_metadata(&detection, metadata);
            let media_type = MediaType::parse(&content_type).ok_or_else(|| {
                Failure::Malformed(format!("the content type {content_type:?} does not parse"))
            })?;
            metadata.set("Content-Type", media_type.to_string());
            metadata.set("Content-Encoding", detection.decode_as.name());
            let mut lines = 0u64;
            for line in text.lines() {
                if let Some(author) = author(&line) {
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
            let (_, text) = AutoDetectReader::stream(input, given.as_deref(), pages);
            let mut code = text.lines().map(|line| {
                if unicode {
                    line
                } else {
                    line.replace('\u{FFFD}', "?")
                }
            });
            let mut walk = Walk {
                xhtml,
                ignore: true,
                left_out: 0,
                written: Vec::new(),
            };
            let walked = walk_parsed(
                &frame(&name),
                &mut Framed {
                    walk: &mut walk,
                    code: Some(Code {
                        renderer: Renderer::new(language),
                        lines: &mut code,
                    }),
                },
            );
            xhtml.end_document()?;
            walked
        });
        unless_different(reading, difference)
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

/// `XhtmlRenderer.highlight`'s XHTML around the code, which is parsed without it: the head, the
/// resource name as the heading, and an empty `code` element. The entities jhighlight writes are
/// written as the characters jsoup reads them as.
fn frame(name: &str) -> String {
    let mut html: Vec<u16> = HEADER.encode_utf16().collect();
    escape_into(name.encode_utf16(), &mut html);
    html.extend("</h1><code>".encode_utf16());
    html.extend(FOOTER.encode_utf16());
    text_of(html)
}

/// Markup as UTF-16 units, as a string, an unpaired surrogate as `?`.
fn text_of(html: Vec<u16>) -> String {
    char::decode_utf16(html)
        .map(|unit| unit.unwrap_or('?'))
        .collect()
}

/// `XhtmlRenderer.highlight` of the code, a line at a time: each line, its tabs expanded,
/// scanned into tokens written in spans of their style's class, a new span where the style
/// changes and at each line, spaces as no-break spaces, and each line ended with a break, the
/// scanner's state carried on from line to line.
struct Renderer {
    language: Language,
    state: usize,
    previous_style: u8,
    newline: bool,
}

impl Renderer {
    fn new(language: Language) -> Self {
        Self {
            language,
            state: 0,
            previous_style: 0,
            newline: false,
        }
    }

    /// Adds the markup of `line` to `html`, its spans closed by its end.
    fn line(&mut self, line: &str, html: &mut Vec<u16>) {
        let scanner = self.language.scanner();
        let line = tabs_to_spaces(line);
        let units: Vec<u16> = line.encode_utf16().chain([u16::from(b'\n')]).collect();
        let mut index = 0;
        while index < units.len() {
            let (action, length) = scanner
                .next(&units, index, self.state)
                .expect("the scanners' fallback rule matches any character");
            let (style, begin) = self.language.action(action);
            if let Some(begin) = begin {
                self.state = begin;
            }
            if style != self.previous_style || self.newline {
                if self.previous_style != 0 && !self.newline {
                    html.extend("</span>".encode_utf16());
                }
                html.extend("<span class=\"".encode_utf16());
                html.extend(self.language.css_class(style).encode_utf16());
                html.extend("\">".encode_utf16());
                self.previous_style = style;
            }
            self.newline = false;
            let token = units[index..index + length]
                .iter()
                .copied()
                .filter(|&unit| unit != u16::from(b'\n'))
                .map(|unit| if unit == u16::from(b' ') { 0xA0 } else { unit });
            escape_into(token, html);
            index += length;
        }
        html.extend("</span><br />\n".encode_utf16());
        self.newline = true;
    }
}

/// The code to render into the `code` element of the frame: its renderer, and its lines, read
/// as they are decoded.
struct Code<'l> {
    renderer: Renderer,
    lines: &'l mut dyn Iterator<Item = String>,
}

/// The walk of the frame by `walk`, which walks the code inside its `code` element as that
/// starts, a chunk of lines at a time.
struct Framed<'w, 'l, V> {
    walk: &'w mut V,
    code: Option<Code<'l>>,
}

impl<V: Visitor<Stop = Failure>> Visitor for Framed<'_, '_, V> {
    type Stop = Failure;

    fn start(&mut self, node: &ParsedNode<'_>) -> Result<(), Failure> {
        self.walk.start(node)?;
        if !matches!(node, ParsedNode::Element { name: "code", .. }) {
            return Ok(());
        }
        let Some(Code {
            mut renderer,
            lines,
        }) = self.code.take()
        else {
            return Ok(());
        };
        let mut html: Vec<u16> = Vec::with_capacity(CHUNK + CHUNK / 4);
        let mut lines = lines.peekable();
        while lines.peek().is_some() {
            html.clear();
            html.extend("<code>".encode_utf16());
            while html.len() < CHUNK {
                let Some(line) = lines.next() else {
                    break;
                };
                renderer.line(&line, &mut html);
            }
            html.extend("</code>".encode_utf16());
            walk_parsed(
                &text_of(std::mem::take(&mut html)),
                &mut Inside {
                    walk: &mut *self.walk,
                    depth: None,
                },
            )?;
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), Failure> {
        self.walk.text(text)
    }

    fn end(&mut self, node: &ParsedNode<'_>) -> Result<(), Failure> {
        self.walk.end(node)
    }
}

/// The walk of a chunk of code inside a `code` element, which hands `walk` only what the
/// element holds.
struct Inside<'w, V> {
    walk: &'w mut V,
    /// How deep the walk is inside the `code` element, once it is inside it.
    depth: Option<usize>,
}

impl<V: Visitor<Stop = Failure>> Visitor for Inside<'_, V> {
    type Stop = Failure;

    fn start(&mut self, node: &ParsedNode<'_>) -> Result<(), Failure> {
        match &mut self.depth {
            Some(depth) => {
                *depth += 1;
                self.walk.start(node)
            }
            None => {
                if matches!(node, ParsedNode::Element { name: "code", .. }) {
                    self.depth = Some(0);
                }
                Ok(())
            }
        }
    }

    fn text(&mut self, text: &str) -> Result<(), Failure> {
        match self.depth {
            Some(_) => self.walk.text(text),
            None => Ok(()),
        }
    }

    fn end(&mut self, node: &ParsedNode<'_>) -> Result<(), Failure> {
        match &mut self.depth {
            Some(0) => {
                self.depth = None;
                Ok(())
            }
            Some(depth) => {
                *depth -= 1;
                self.walk.end(node)
            }
            None => Ok(()),
        }
    }
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

/// `TikaNodeFilter` over jsoup's tree: from the `html` element on, the `head`, `meta`, `title`,
/// `link` and `style` elements left out with what they hold, the `html` and `body` elements but
/// not what they hold, text and data written as characters, and every other node as an element
/// in no namespace with its attributes.
struct Walk<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    /// Whether the walk is outside the `html` element.
    ignore: bool,
    /// How deep the walk is in an element it leaves out, if it is.
    left_out: usize,
    /// Whether each node started and not yet ended was written as an element.
    written: Vec<bool>,
}

fn node_name<'n>(node: &ParsedNode<'n>) -> &'n str {
    match node {
        ParsedNode::Document => "#document",
        ParsedNode::Element { name, .. } => name,
        ParsedNode::Comment => "#comment",
        ParsedNode::Doctype => "#doctype",
    }
}

impl Visitor for Walk<'_, '_> {
    type Stop = Failure;

    fn start(&mut self, node: &ParsedNode<'_>) -> Result<(), Failure> {
        if self.left_out > 0 {
            self.left_out += 1;
            return Ok(());
        }
        let name = node_name(node);
        if name == "html" {
            self.ignore = false;
        }
        if !self.ignore && matches!(name, "head" | "meta" | "title" | "link" | "style") {
            self.left_out = 1;
            return Ok(());
        }
        let written = !self.ignore && !matches!(name, "html" | "body");
        if written {
            match node {
                ParsedNode::Element { attributes, .. } => {
                    let attributes: Vec<(&str, &str)> = attributes
                        .iter()
                        .map(|attribute| (attribute.key.as_str(), attribute.value.as_str()))
                        .collect();
                    self.xhtml.start_element_with(name, &attributes)?;
                }
                ParsedNode::Document | ParsedNode::Comment | ParsedNode::Doctype => {
                    self.xhtml.start_element(name)?;
                }
            }
        }
        self.written.push(written);
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), Failure> {
        if self.left_out == 0 && !self.ignore {
            self.xhtml.characters(text)?;
        }
        Ok(())
    }

    fn end(&mut self, node: &ParsedNode<'_>) -> Result<(), Failure> {
        if self.left_out > 0 {
            self.left_out -= 1;
            return Ok(());
        }
        let name = node_name(node);
        if name == "html" {
            self.ignore = true;
        }
        if self
            .written
            .pop()
            .expect("the walk ends the nodes it started")
        {
            self.xhtml.end_element_in("", name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
