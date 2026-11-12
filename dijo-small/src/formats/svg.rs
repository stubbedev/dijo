//! SVG images, read for the text they show rather than for all the character data of their XML,
//! as Tika reads them through `DcXMLParser`.
//!
//! Each `<text>` (and Inkscape's flowed `<flowRoot>`) is a paragraph, its `<tspan>`s,
//! `<textPath>`s and `<a>`s read in document order, its whitespace collapsed as SVG renders it
//! unless `xml:space` preserves it. A tspan that starts a line (Inkscape's
//! `sodipodi:role="line"`, or a new `x` together with a new `y` or a nonzero `dy`) breaks the
//! line, a tspan that only moves to a new `x` separates words, and one that only shifts the
//! baseline, as a superscript does, runs on. The XHTML of a `<foreignObject>` is text, a
//! paragraph to each of its blocks. Text inside `<defs>` and `<symbol>` is read once where it is
//! written, whatever `<use>` shows it; every child of a `<switch>` is read, whatever its
//! `systemLanguage`. The `<title>` and `<desc>` of the root element are its `dc:title` and
//! `dc:description`, and a nested one, outside a text element, a paragraph of its own; inside a
//! text element, they are tooltips and left out. The Dublin Core of the RDF in `<metadata>`, as
//! Inkscape writes it, is metadata, each property read whole, so that the `dc:title` of the
//! agent of `dc:creator` is the creator's name. Style sheets, scripts, path data and the other
//! attributes of graphics are not read. An image a `data:` URI holds, in an `<image>` or in an
//! XHTML `<img>`, is handed to the embedded document extractor, where it is read and recognised
//! as any image is. Nothing outside the document is ever fetched: the XML is read without its
//! external DTD, and other `href`s are left alone.
//!
//! Elements in no namespace are read as SVG elements, as the elements of a file that forgets its
//! namespace declaration are. A gzip-compressed SVGZ file is detected as gzip, whose parser hands
//! the image it holds to this one.

use crate::config::ParserConfigs;
use crate::formats::data_uri;
use crate::formats::element_metadata::{ElementHandlers, DUBLIN_CORE_NS};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml, XHTML_NAMESPACE};
use crate::formats::xml::{self, attribute, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;

const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const SODIPODI_NAMESPACE: &str = "http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd";

const NAME: &str = "dijo::SvgParser";
const WHAT: &str = "The SVG image";

/// The text of SVG images.
pub struct SvgParser;

impl Parser for SvgParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/svg+xml"]
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
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        Ok(Box::new(move |xhtml| read_image(input, pages, xhtml)))
    }
}

/// Writes the text of the image `input` as its XML is read, its `pages` released as they are,
/// and adds its metadata.
fn read_image(input: &[u8], pages: Pages<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut reading = Reading {
        xhtml,
        frames: Vec::new(),
        paragraph: Paragraph::default(),
        label: String::new(),
        dublin_core: DublinCore::default(),
    };
    let parsed = xml::parse_paged(input, pages, WHAT, &mut reading);
    let ended = reading.paragraph.end(reading.xhtml);
    parsed?;
    ended.map_err(Failure::from)
}

/// What the character data inside an element is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    /// Graphics, whose character data is not shown.
    Graphics,
    /// Text content: the text of a text element, or a nested title or description.
    Text,
    /// The XHTML of a foreign object.
    Html,
    /// Neither it nor anything inside it is read: a style sheet, a script, a tooltip.
    Skipped,
    /// The title or description of the image.
    Label(Label),
    /// The RDF of the image's metadata.
    Metadata,
}

/// A title or description of the root element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Label {
    Title,
    Description,
}

impl Label {
    fn key(self) -> &'static str {
        match self {
            Label::Title => "dc:title",
            Label::Description => "dc:description",
        }
    }
}

/// How whitespace in text is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spacing {
    /// Each run of whitespace one space, none at the ends of a paragraph: SVG's
    /// `xml:space="default"` as CSS renders it, and HTML outside `<pre>`.
    Collapse,
    /// Each whitespace character a space: SVG's `xml:space="preserve"`.
    Preserve,
    /// Each line end a line break: HTML's `<pre>`.
    Lines,
}

/// What ending an element does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Close {
    Nothing,
    /// Ends the paragraph.
    Paragraph,
    /// Breaks the line.
    Line,
    /// Sets the label from the text collected.
    Label(Label),
}

/// An open element: what is inside it, and what ending it does.
#[derive(Debug, Clone, Copy)]
struct Frame {
    context: Context,
    spacing: Spacing,
    close: Close,
}

impl Frame {
    /// The frame around the root element.
    const DOCUMENT: Frame = Frame {
        context: Context::Graphics,
        spacing: Spacing::Collapse,
        close: Close::Nothing,
    };

    /// The frame of an element whose end does nothing.
    fn inline(context: Context, spacing: Spacing) -> Frame {
        Frame {
            context,
            spacing,
            close: Close::Nothing,
        }
    }
}

/// What separates the text written next from the text written before it in a paragraph.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Pending {
    #[default]
    Nothing,
    Space,
    Break,
}

/// The paragraph being written: started by its first text, so that an element without text
/// writes none, with the separator its next text needs.
#[derive(Debug, Default)]
struct Paragraph {
    open: bool,
    pending: Pending,
}

impl Paragraph {
    fn separate(&mut self) {
        if self.open {
            self.pending = self.pending.max(Pending::Space);
        }
    }

    fn break_line(&mut self) {
        if self.open {
            self.pending = Pending::Break;
        }
    }

    fn end(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        if self.open {
            xhtml.end_element("p")?;
        }
        *self = Paragraph::default();
        Ok(())
    }

    /// Writes `content` after what separates it from the text before it.
    fn write(&mut self, xhtml: &mut Xhtml, content: &str) -> Result<(), Stop> {
        if !self.open {
            xhtml.start_element("p")?;
            self.open = true;
        }
        match std::mem::take(&mut self.pending) {
            Pending::Nothing => {}
            Pending::Space => xhtml.characters(" ")?,
            Pending::Break => {
                xhtml.start_element("br")?;
                xhtml.end_element("br")?;
            }
        }
        xhtml.characters(content)
    }

    fn text(&mut self, xhtml: &mut Xhtml, text: &str, spacing: Spacing) -> Result<(), Stop> {
        match spacing {
            Spacing::Collapse => self.collapsed(xhtml, text),
            Spacing::Preserve if text.contains(['\t', '\n', '\r']) => {
                self.write(xhtml, &text.replace(['\t', '\n', '\r'], " "))
            }
            Spacing::Preserve => self.write(xhtml, text),
            Spacing::Lines => {
                let mut lines = text.split('\n');
                if let Some(first) = lines.next().filter(|first| !first.is_empty()) {
                    self.write(xhtml, first)?;
                }
                for line in lines {
                    self.break_line();
                    if !line.is_empty() {
                        self.write(xhtml, line)?;
                    }
                }
                Ok(())
            }
        }
    }

    /// Writes `text` with each run of whitespace in it one space, each run of words joined by
    /// single spaces written at once.
    fn collapsed(&mut self, xhtml: &mut Xhtml, text: &str) -> Result<(), Stop> {
        let bytes = text.as_bytes();
        let space = |at: usize| matches!(bytes.get(at), Some(b' ' | b'\t' | b'\n' | b'\r'));
        let mut at = 0;
        while at < bytes.len() {
            let start = at;
            while space(at) {
                at += 1;
            }
            if at > start {
                self.separate();
            }
            if at == bytes.len() {
                break;
            }
            let words = at;
            loop {
                while at < bytes.len() && !space(at) {
                    at += 1;
                }
                if bytes.get(at) == Some(&b' ') && at + 1 < bytes.len() && !space(at + 1) {
                    at += 1;
                    continue;
                }
                break;
            }
            self.write(xhtml, &text[words..at])?;
        }
        Ok(())
    }
}

/// The Dublin Core handlers of `DcXMLParser`, handed each property of the RDF whole: the Dublin
/// Core elements nested in a property, as the `dc:title` of the `cc:Agent` of a `dc:creator`,
/// are its text, not properties of their own.
#[derive(Debug)]
struct DublinCore {
    handlers: ElementHandlers,
    open: usize,
}

impl Default for DublinCore {
    fn default() -> Self {
        Self {
            handlers: ElementHandlers::dublin_core(),
            open: 0,
        }
    }
}

impl DublinCore {
    fn start(&mut self, namespace: &str, local_name: &str) {
        if namespace == DUBLIN_CORE_NS {
            self.open += 1;
            if self.open > 1 {
                return;
            }
        }
        self.handlers.start(namespace, local_name);
    }

    fn end(&mut self, namespace: &str, local_name: &str, metadata: &mut TikaMetadata) {
        if namespace == DUBLIN_CORE_NS {
            self.open = self.open.saturating_sub(1);
            if self.open > 0 {
                return;
            }
        }
        self.handlers.end(namespace, local_name, metadata);
    }
}

/// Whether `namespace` is that of SVG elements.
fn is_svg(namespace: &str) -> bool {
    namespace == SVG_NAMESPACE || namespace.is_empty()
}

/// Whether the attribute `local_name`, in no namespace, is given a value.
fn is_given(attributes: &[Attribute], local_name: &str) -> bool {
    attribute(attributes, "", local_name).is_some_and(|value| !value.trim().is_empty())
}

/// Whether the first length of the list `lengths` is other than zero.
fn first_is_nonzero(lengths: &str) -> bool {
    let first = lengths
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .find(|length| !length.is_empty())
        .unwrap_or_default();
    let number = first
        .find(|character: char| !matches!(character, '0'..='9' | '.' | '+' | '-'))
        .map_or(first, |unit| &first[..unit]);
    number.parse::<f64>().is_ok_and(|number| number != 0.0)
}

/// How a `<tspan>` with `attributes` moves from the text before it.
fn tspan_separator(attributes: &[Attribute]) -> Pending {
    let line = attribute(attributes, SODIPODI_NAMESPACE, "role") == Some("line")
        || (is_given(attributes, "x")
            && (is_given(attributes, "y")
                || attribute(attributes, "", "dy").is_some_and(first_is_nonzero)));
    if line {
        Pending::Break
    } else if is_given(attributes, "x") {
        Pending::Space
    } else {
        Pending::Nothing
    }
}

/// `text` with each run of whitespace one space, and none at its ends.
fn collapse_whitespace(text: &str) -> String {
    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// The image being read and the document it is written to, as the SAX handler of its XML.
struct Reading<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    frames: Vec<Frame>,
    paragraph: Paragraph,
    /// The text of the title or description of the image being read.
    label: String,
    dublin_core: DublinCore,
}

impl Reading<'_, '_> {
    /// Hands the image `href` names to the embedded document extractor, if a `data:` URI holds
    /// it; a link to anything else is not followed.
    fn embed(&mut self, href: Option<&str>) -> Result<(), Failure> {
        let Some(uri) = href
            .map(str::trim_start)
            .filter(|href| href.starts_with("data:"))
            .and_then(data_uri::parse)
        else {
            return Ok(());
        };
        self.paragraph.end(self.xhtml)?;
        uri.embed(self.xhtml, NAME)
    }

    fn block(&mut self, context: Context, spacing: Spacing) -> Result<Frame, Failure> {
        self.paragraph.end(self.xhtml)?;
        Ok(Frame {
            context,
            spacing,
            close: Close::Paragraph,
        })
    }

    /// The frame of an element inside graphics, or of the root element.
    fn graphics_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
        spacing: Spacing,
    ) -> Result<Frame, Failure> {
        let inline = |context| Frame::inline(context, spacing);
        if !is_svg(namespace) {
            return Ok(inline(Context::Graphics));
        }
        let label = match local_name {
            "title" => Label::Title,
            "desc" => Label::Description,
            "text" | "flowRoot" => return self.block(Context::Text, spacing),
            "foreignObject" => return self.block(Context::Html, Spacing::Collapse),
            "style" | "script" => return Ok(inline(Context::Skipped)),
            "metadata" => return Ok(inline(Context::Metadata)),
            "image" => {
                self.embed(
                    attribute(attributes, "", "href")
                        .or_else(|| attribute(attributes, XLINK_NAMESPACE, "href")),
                )?;
                return Ok(inline(Context::Graphics));
            }
            _ => return Ok(inline(Context::Graphics)),
        };
        if self.frames.len() != 1 {
            return self.block(Context::Text, spacing);
        }
        self.label.clear();
        Ok(Frame {
            context: Context::Label(label),
            spacing,
            close: Close::Label(label),
        })
    }

    /// The frame of an element inside a text element.
    fn text_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Context {
        if !is_svg(namespace) {
            return Context::Text;
        }
        match local_name {
            "title" | "desc" | "flowRegion" => Context::Skipped,
            "tspan" => {
                match tspan_separator(attributes) {
                    Pending::Break => self.paragraph.break_line(),
                    Pending::Space => self.paragraph.separate(),
                    Pending::Nothing => {}
                }
                Context::Text
            }
            _ => Context::Text,
        }
    }

    /// The frame of an element inside a foreign object.
    fn html_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
        spacing: Spacing,
    ) -> Result<Frame, Failure> {
        if namespace == SVG_NAMESPACE {
            return self.graphics_element(namespace, local_name, attributes, spacing);
        }
        let inline = |context| Frame::inline(context, spacing);
        if namespace != XHTML_NAMESPACE {
            return Ok(inline(Context::Html));
        }
        match local_name {
            "script" | "style" | "head" | "template" => Ok(inline(Context::Skipped)),
            "br" => {
                self.paragraph.break_line();
                Ok(inline(Context::Html))
            }
            "td" | "th" => {
                self.paragraph.separate();
                Ok(inline(Context::Html))
            }
            "img" => {
                self.embed(attribute(attributes, "", "src"))?;
                Ok(inline(Context::Html))
            }
            "pre" => self.block(Context::Html, Spacing::Lines),
            _ if Xhtml::is_endline(local_name) => self.block(Context::Html, spacing),
            _ => Ok(inline(Context::Html)),
        }
    }
}

impl SaxHandler for Reading<'_, '_> {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        let parent = self.frames.last().copied().unwrap_or(Frame::DOCUMENT);
        let spacing = match attribute(attributes, XML_NAMESPACE, "space") {
            Some("preserve") => Spacing::Preserve,
            Some("default") => Spacing::Collapse,
            _ => parent.spacing,
        };
        let inline = |context| Frame::inline(context, spacing);
        let frame = match parent.context {
            Context::Skipped | Context::Label(_) => inline(parent.context),
            Context::Metadata => {
                self.dublin_core.start(namespace, local_name);
                inline(Context::Metadata)
            }
            Context::Graphics => {
                self.graphics_element(namespace, local_name, attributes, spacing)?
            }
            Context::Text if is_svg(namespace) && matches!(local_name, "flowPara" | "flowDiv") => {
                self.paragraph.break_line();
                Frame {
                    context: Context::Text,
                    spacing,
                    close: Close::Line,
                }
            }
            Context::Text => inline(self.text_element(namespace, local_name, attributes)),
            Context::Html => self.html_element(namespace, local_name, attributes, spacing)?,
        };
        self.frames.push(frame);
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        let Some(frame) = self.frames.pop() else {
            return Ok(());
        };
        if frame.context == Context::Metadata
            && self.frames.last().map(|parent| parent.context) == Some(Context::Metadata)
        {
            self.dublin_core
                .end(namespace, local_name, self.xhtml.metadata_mut());
        }
        match frame.close {
            Close::Nothing => {}
            Close::Paragraph => self.paragraph.end(self.xhtml)?,
            Close::Line => self.paragraph.break_line(),
            Close::Label(label) => {
                let value = collapse_whitespace(&self.label);
                self.label.clear();
                let metadata = self.xhtml.metadata_mut();
                if !value.is_empty() && !metadata.values(label.key()).contains(&value) {
                    metadata.add(label.key(), value);
                }
            }
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        let Some(frame) = self.frames.last() else {
            return Ok(());
        };
        match frame.context {
            Context::Text | Context::Html => {
                self.paragraph.text(self.xhtml, text, frame.spacing)?;
            }
            Context::Label(_) => self.label.push_str(text),
            Context::Metadata => self.dublin_core.handlers.characters(text),
            Context::Graphics | Context::Skipped => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd">"#;

    type Metadata = std::collections::HashMap<String, Vec<String>>;

    /// The body of the XHTML of `image`, and its metadata.
    fn read(image: &[u8]) -> (String, Metadata) {
        body(
            crate::Extractor::new()
                .set_xml_output(true)
                .extract_bytes_to_string(image)
                .unwrap(),
        )
    }

    /// The body of the XHTML of the image at `path`, and its metadata.
    fn read_file(path: &str) -> (String, Metadata) {
        body(
            crate::Extractor::new()
                .set_xml_output(true)
                .extract_file_to_string(path)
                .unwrap(),
        )
    }

    fn body((xml, metadata): (String, Metadata)) -> (String, Metadata) {
        let body = xml[xml.find("<body>").unwrap() + "<body>".len()..]
            .strip_suffix("</body></html>")
            .unwrap()
            .to_owned();
        (body, metadata)
    }

    fn body_of(content: &str) -> String {
        read(format!("{HEAD}{content}</svg>").as_bytes()).0
    }

    #[test]
    fn collapses_whitespace_unless_it_is_preserved() {
        assert_eq!(
            body_of("<text>\n  Two\t words <tspan> and </tspan>  more  </text>"),
            "<p>Two words and more</p>\n"
        );
        assert_eq!(
            body_of(
                "<text xml:space=\"preserve\"> a\tb\n <tspan xml:space=\"default\"> c \n d </tspan></text>"
            ),
            "<p> a b   c d</p>\n"
        );
    }

    #[test]
    fn breaks_lines_where_tspans_start_them() {
        assert_eq!(
            body_of(
                r#"<text x="0" y="10"><tspan sodipodi:role="line">First</tspan><tspan sodipodi:role="line">second</tspan><tspan x="0" dy="1.2em">third</tspan><tspan x="0" y="40">fourth</tspan><tspan x="0" dy="0">and</tspan>, <tspan x="9">fifth</tspan></text>"#
            ),
            "<p>First<br />\nsecond<br />\nthird<br />\nfourth and, fifth</p>\n"
        );
        assert_eq!(
            body_of(r#"<text>x<tspan dy="-4">2</tspan><tspan y="12" dy="4">+1</tspan></text>"#),
            "<p>x2+1</p>\n"
        );
    }

    #[test]
    fn reads_flowed_text_a_line_to_each_paragraph() {
        assert_eq!(
            body_of(
                r#"<flowRoot><flowRegion><rect width="9" height="9"/><desc>region</desc></flowRegion><flowPara>One</flowPara><flowPara>two <flowSpan>words</flowSpan></flowPara></flowRoot>"#
            ),
            "<p>One<br />\ntwo words</p>\n"
        );
    }

    #[test]
    fn reads_titles_as_metadata_or_text_where_they_stand() {
        let (body, metadata) = read(
            format!(
                "{HEAD}<title> The\n chart </title><desc>Of the coast</desc>\
                 <g><title>Wall</title><desc>Stone</desc></g>\
                 <text>Reef<title>tooltip</title></text>\
                 <metadata><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" \
                 xmlns:cc=\"http://creativecommons.org/ns#\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
                 <cc:Work><dc:title>The chart</dc:title><dc:creator><cc:Agent><dc:title> Anna\n Berg </dc:title>\
                 </cc:Agent></dc:creator><dc:rights><cc:Agent><dc:title>Lighthouse Press</dc:title></cc:Agent></dc:rights>\
                 </cc:Work></rdf:RDF></metadata></svg>"
            )
            .as_bytes(),
        );
        assert_eq!(body, "<p>Wall</p>\n<p>Stone</p>\n<p>Reef</p>\n");
        assert_eq!(metadata["dc:title"], ["The chart"]);
        assert_eq!(metadata["dc:description"], ["Of the coast"]);
        assert_eq!(metadata["dc:creator"], ["Anna\n Berg"]);
        assert_eq!(metadata["dc:rights"], ["Lighthouse Press"]);
    }

    #[test]
    fn leaves_out_style_sheets_scripts_and_graphics() {
        assert_eq!(
            body_of(
                r##"<style>text { fill: red }</style><script>alert("x")</script><path d="M 0,0 L 9,9"/>loose<g>text</g><defs><text id="t">Shown</text></defs><use xlink:href="#t"/><switch><text systemLanguage="da">Hej</text><text>Hello</text></switch>"##
            ),
            "<p>Shown</p>\n<p>Hej</p>\n<p>Hello</p>\n"
        );
    }

    #[test]
    fn reads_the_xhtml_of_foreign_objects_a_paragraph_to_each_block() {
        assert_eq!(
            body_of(
                r#"<foreignObject><div xmlns="http://www.w3.org/1999/xhtml">Lead <b>in</b><p>One
                two</p><script>x()</script><table><tr><td>a</td><td>b</td></tr></table>line<br/>break<pre>x  y
z</pre><svg xmlns="http://www.w3.org/2000/svg"><text>Inner</text></svg></div></foreignObject>"#
            ),
            "<p>Lead in</p>\n<p>One two</p>\n<p>a b</p>\n<p>line<br />\nbreak</p>\n\
             <p>x  y<br />\nz</p>\n<p>Inner</p>\n"
        );
    }

    #[test]
    fn hands_images_in_data_uris_to_the_embedded_document_extractor() {
        let body = body_of(
            r#"<text>Before</text><image xlink:href="data:text/plain;base64,TGFudGVybg=="/><image href="https://example.invalid/x.png"/><foreignObject><p xmlns="http://www.w3.org/1999/xhtml">Text <img src="data:text/plain,Lens"/>after</p></foreignObject>"#,
        );
        assert_eq!(
            body,
            "<p>Before</p>\n<div class=\"package-entry\"><p>Lantern</p>\n\n</div>\n\
             <p>Text</p>\n<div class=\"package-entry\"><p>Lens</p>\n\n</div>\n<p>after</p>\n"
        );
    }

    #[test]
    fn expands_internal_entities_and_never_fetches_the_dtd() {
        let image = format!(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \
             \"http://example.invalid/svg11.dtd\" [<!ENTITY who \"keeper\">]>\n\
             {HEAD}<text>The &who;&#x2019;s &amp; lamp</text></svg>"
        );
        assert_eq!(
            read(image.as_bytes()).0,
            "<p>The keeper\u{2019}s &amp; lamp</p>\n"
        );
    }

    #[test]
    fn reads_elements_in_no_namespace_as_svg() {
        let scratch = crate::formats::testing::Scratch::new("svg-bare");
        let path = scratch.write(
            "bare.svg",
            b"<?xml version=\"1.0\"?>\n<svg><title>Bare</title><style>x</style><text>Plain</text></svg>",
        );
        let (body, metadata) = read_file(path.to_str().unwrap());
        assert_eq!(body, "<p>Plain</p>\n");
        assert_eq!(metadata["dc:title"], ["Bare"]);
    }

    #[test]
    fn reads_svgz_through_the_gzip_parser() {
        let image = format!("{HEAD}<text>Compressed</text></svg>");
        let gzipped = crate::compress::gzip::tests::gzip(image.as_bytes(), None);
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&gzipped)
            .unwrap();
        assert_eq!(metadata["Content-Type"], ["application/gzip"]);
        assert!(text.contains("Compressed"), "{text}");
    }

    #[test]
    fn fails_on_malformed_xml() {
        for image in [
            format!("{HEAD}<text>open</svg>"),
            format!("{HEAD}<text>&undeclared;</text></svg>"),
        ] {
            let extracted = crate::Extractor::new().extract_bytes_to_string(image.as_bytes());
            assert!(extracted.is_err(), "{image}: {extracted:?}");
        }
    }

    #[test]
    fn tells_a_nonzero_first_length() {
        assert!(first_is_nonzero("1.2em"));
        assert!(first_is_nonzero(" -3, 0"));
        assert!(!first_is_nonzero("0 5"));
        assert!(!first_is_nonzero("0.0px"));
        assert!(!first_is_nonzero(""));
    }

    /// An image too large to read whole, of many text elements and long path data, is written
    /// a paragraph at a time, under a memory budget far below its size.
    #[test]
    fn writes_images_too_large_to_read_whole_as_they_are_read() {
        let element = |index: u64| {
            format!(
                "<path d=\"M 0,0 L {index},{index} L 1,{index} Z\"/><text x=\"0\" y=\"{index}\">\
                 Row <tspan>{index}</tspan></text>\n"
            )
            .into_bytes()
        };
        let count = dijo_core::large::bodies_past_limit(element(0).len());
        let file = dijo_core::large::LargeFile::new(
            "large.svg",
            HEAD.as_bytes(),
            count,
            element,
            b"</svg>\n",
        );
        let mut rows = 0;
        file.extract_lines(SvgParser.name(), |line| {
            if !line.is_empty() {
                assert_eq!(line, format!("Row {rows}"));
                rows += 1;
            }
        });
        assert_eq!(rows, count);
    }
}
