//! FictionBook (FB2) books, as our Java `FictionBookParser` reads them: the book's title, authors
//! and language as metadata, its sections as headings and paragraphs, verse and quotations as
//! paragraphs, and no embedded binaries. Tika reads FB2 as generic XML, running paragraphs
//! together and keeping the names of embedded images and the language code in the text.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::unicode::is_whitespace;

const MAX_HEADING_LEVEL: usize = 6;
const HEADINGS: [&str; MAX_HEADING_LEVEL] = ["h1", "h2", "h3", "h4", "h5", "h6"];
const PARAGRAPHS: [&str; 4] = ["p", "v", "text-author", "subtitle"];
const NAME_PARTS: [&str; 4] = ["first-name", "middle-name", "last-name", "nickname"];
/// The elements of the title info collected besides the parts of an author's name.
const DESCRIBED: [&str; 2] = ["book-title", "lang"];

pub struct FictionBookParser;

impl Parser for FictionBookParser {
    fn name(&self) -> &'static str {
        "dijo::FictionBookParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::FictionBookParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-fictionbook+xml"]
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
        Ok(Box::new(move |xhtml| read_book(input, pages, xhtml)))
    }
}

/// A heading or a paragraph of the book.
#[derive(Debug, PartialEq, Eq)]
struct Block {
    element: &'static str,
    text: String,
}

/// Collects the book's metadata, and each block as it ends, while its XML is read: how deep it
/// is in the elements it tells apart, and the element it collects the text of, if any.
#[derive(Default)]
struct Book {
    binary: usize,
    title_info: usize,
    body: usize,
    text: String,
    name: Vec<String>,
    sections: usize,
    collecting: Option<&'static str>,
}

impl Book {
    /// The depth `element` counts in, if it is one the book tells apart.
    fn depth(&mut self, element: &str) -> Option<&mut usize> {
        match element {
            "binary" => Some(&mut self.binary),
            "title-info" => Some(&mut self.title_info),
            "body" => Some(&mut self.body),
            "section" => Some(&mut self.sections),
            _ => None,
        }
    }

    fn start(&mut self, element: &str) {
        if let Some(depth) = self.depth(element) {
            *depth += 1;
        }
        if self.binary > 0 || self.collecting.is_some() {
            return;
        }
        if self.title_info > 0 {
            self.collecting = named(DESCRIBED.into_iter().chain(NAME_PARTS), element);
        }
        if self.collecting.is_none() && self.body > 0 {
            self.collecting = named(["title"].into_iter().chain(PARAGRAPHS), element);
        }
        self.text.clear();
    }

    fn text(&mut self, text: &str) {
        if self.collecting.is_some() && self.binary == 0 {
            self.text.push_str(text);
        }
    }

    /// Ends `element`, adding what it names to `metadata`: the block it ends, if any.
    fn end(&mut self, element: &str, metadata: &mut TikaMetadata) -> Option<Block> {
        if let Some(depth) = self.depth(element) {
            *depth = depth.saturating_sub(1);
        }
        if element == "p" && self.collecting == Some("title") {
            self.text.push(' ');
            return None;
        }
        if element == "author" && self.title_info > 0 && !self.name.is_empty() {
            metadata.add("dc:creator", self.name.join(" "));
            self.name.clear();
        }
        if self.collecting != Some(element) {
            return None;
        }
        self.collecting = None;
        let text = collapse_whitespace(&self.text);
        if text.is_empty() {
            return None;
        }
        let element = match element {
            "book-title" => {
                metadata.set("dc:title", text);
                return None;
            }
            "lang" => {
                metadata.set("dc:language", text);
                return None;
            }
            _ if NAME_PARTS.contains(&element) => {
                self.name.push(text);
                return None;
            }
            "title" => heading(self.sections + 1),
            "subtitle" => heading(self.sections + 2),
            _ => "p",
        };
        Some(Block { element, text })
    }
}

/// The one of `names` that `element` is, if any.
fn named(names: impl IntoIterator<Item = &'static str>, element: &str) -> Option<&'static str> {
    names.into_iter().find(|&name| name == element)
}

fn heading(level: usize) -> &'static str {
    HEADINGS[level.min(MAX_HEADING_LEVEL) - 1]
}

/// Java's `replaceAll("\\s+", " ").strip()`: runs of ASCII whitespace become one space, then
/// leading and trailing whitespace as `Character.isWhitespace` defines it goes.
fn collapse_whitespace(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut in_run = false;
    for character in text.chars() {
        if matches!(character, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r') {
            if !in_run {
                collapsed.push(' ');
            }
            in_run = true;
        } else {
            collapsed.push(character);
            in_run = false;
        }
    }
    collapsed.truncate(collapsed.trim_end_matches(is_whitespace).len());
    collapsed.replace_range(
        ..collapsed.len() - collapsed.trim_start_matches(is_whitespace).len(),
        "",
    );
    collapsed
}

const WHAT: &str = "The FictionBook XML";

/// The book being read and the document it is written to, as the SAX handler of its XML.
struct Reading<'x, 'w> {
    book: Book,
    xhtml: &'x mut Xhtml<'w>,
}

impl SaxHandler for Reading<'_, '_> {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.book.start(local_name);
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, local_name: &str) -> Result<(), Failure> {
        if let Some(block) = self.book.end(local_name, self.xhtml.metadata_mut()) {
            self.xhtml.element(block.element, &block.text)?;
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.book.text(text);
        Ok(())
    }
}

/// Writes the book's blocks as its XML is read, its `pages` released as they are, and its
/// metadata.
fn read_book(input: &[u8], pages: Pages<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut reading = Reading {
        book: Book::default(),
        xhtml,
    };
    xml::parse_paged(input, pages, WHAT, &mut reading)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A book too large to read whole, in windows-1251 with CRLF line ends as many are, is
    /// decoded a window at a time and written a block at a time, in either build, under a
    /// memory budget far below its size.
    #[test]
    fn writes_books_too_large_to_read_whole_a_block_at_a_time() {
        let chapter = "\u{413}\u{43b}\u{430}\u{432}\u{430}";
        let paragraph = |index: u64| {
            [
                &b"<p>\xc3\xeb\xe0\xe2\xe0 "[..],
                index.to_string().as_bytes(),
                b"</p>\r\n",
            ]
            .concat()
        };
        let count = dijo_core::large::bodies_past_limit(paragraph(0).len());
        let file = dijo_core::large::LargeFile::new(
            "long.fb2",
            b"<?xml version=\"1.0\" encoding=\"windows-1251\"?>\r\n\
<FictionBook xmlns=\"http://www.gribuser.ru/xml/fictionbook/2.0\"><body><section>\r\n",
            count,
            paragraph,
            b"</section></body></FictionBook>\r\n",
        );
        let mut paragraphs = 0;
        file.extract_lines(FictionBookParser.name(), |line| {
            if !line.is_empty() {
                assert_eq!(line, format!("{chapter} {paragraphs}"));
                paragraphs += 1;
            }
        });
        assert_eq!(paragraphs, count);
    }

    #[test]
    fn collapses_whitespace_as_java_does() {
        assert_eq!(collapse_whitespace("  a \t\n b\u{a0}\u{2003}"), "a b\u{a0}");
        assert_eq!(collapse_whitespace("\u{2003}a\u{85}"), "a\u{85}");
    }

    /// Java's `replaceAll("\\s+", " ").strip()`, stripped into a second string.
    fn collapse_whitespace_twice(text: &str) -> String {
        let mut collapsed = String::with_capacity(text.len());
        let mut in_run = false;
        for character in text.chars() {
            if matches!(character, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r') {
                if !in_run {
                    collapsed.push(' ');
                }
                in_run = true;
            } else {
                collapsed.push(character);
                in_run = false;
            }
        }
        crate::java::strip(&collapsed).to_owned()
    }

    const TEXTS: [&str; 10] = [
        "",
        " ",
        "  Anna \t",
        "\u{2003}x\u{85}",
        "Del",
        "\n linje \r\n",
        "\u{a0}",
        "\u{1c}\u{2028} \u{3000}",
        "a\u{2003}\u{2003}b \u{a0} ",
        "æ \u{b}\u{c} ø\u{205f}",
    ];

    #[test]
    fn strips_whitespace_in_place_as_into_a_second_string() {
        for first in TEXTS {
            for second in TEXTS {
                let text = format!("{first}{second}");
                assert_eq!(
                    collapse_whitespace(&text),
                    collapse_whitespace_twice(&text),
                    "{text:?}"
                );
            }
        }
    }

    /// The book as it was collected with a stack of the open elements' names.
    #[derive(Default)]
    struct StackBook {
        open: Vec<String>,
        text: String,
        name: Vec<String>,
        sections: usize,
        collecting: Option<String>,
    }

    impl StackBook {
        fn inside(&self, element: &str) -> bool {
            self.open.iter().any(|open| open == element)
        }

        fn start(&mut self, element: &str) {
            if element == "section" {
                self.sections += 1;
            }
            self.open.push(element.to_owned());
            if self.inside("binary") || self.collecting.is_some() {
                return;
            }
            let described = self.inside("title-info");
            if (described
                && (element == "book-title" || element == "lang" || NAME_PARTS.contains(&element)))
                || (self.inside("body") && (element == "title" || PARAGRAPHS.contains(&element)))
            {
                self.collecting = Some(element.to_owned());
            }
            self.text.clear();
        }

        fn text(&mut self, text: &str) {
            if self.collecting.is_some() && !self.inside("binary") {
                self.text.push_str(text);
            }
        }

        fn end(&mut self, element: &str, metadata: &mut TikaMetadata) -> Option<(String, String)> {
            self.open.pop();
            if element == "section" {
                self.sections = self.sections.saturating_sub(1);
            }
            if element == "p" && self.collecting.as_deref() == Some("title") {
                self.text.push(' ');
                return None;
            }
            if element == "author" && self.inside("title-info") && !self.name.is_empty() {
                metadata.add("dc:creator", self.name.join(" "));
                self.name.clear();
            }
            if self.collecting.as_deref() != Some(element) {
                return None;
            }
            self.collecting = None;
            let text = collapse_whitespace_twice(&self.text);
            if text.is_empty() {
                return None;
            }
            let element = match element {
                "book-title" => {
                    metadata.set("dc:title", text);
                    return None;
                }
                "lang" => {
                    metadata.set("dc:language", text);
                    return None;
                }
                _ if NAME_PARTS.contains(&element) => {
                    self.name.push(text);
                    return None;
                }
                "title" => format!("h{}", (self.sections + 1).min(MAX_HEADING_LEVEL)),
                "subtitle" => format!("h{}", (self.sections + 2).min(MAX_HEADING_LEVEL)),
                _ => "p".to_owned(),
            };
            Some((element, text))
        }
    }

    /// What a SAX parse of a well-formed document hands over.
    enum Event {
        Start(&'static str),
        Text(&'static str),
        End(&'static str),
    }

    /// The next number below `bound` drawn from `seed`.
    fn next(seed: &mut u64, bound: usize) -> usize {
        *seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(*seed >> 33).unwrap() % bound
    }

    /// A well-formed tree of elements below `depth` levels, its shape drawn from `seed`.
    fn tree(seed: &mut u64, depth: usize, events: &mut Vec<Event>) {
        const ELEMENTS: [&str; 22] = [
            "description",
            "title-info",
            "document-info",
            "author",
            "first-name",
            "middle-name",
            "last-name",
            "nickname",
            "book-title",
            "lang",
            "body",
            "section",
            "title",
            "p",
            "subtitle",
            "v",
            "text-author",
            "poem",
            "stanza",
            "binary",
            "image",
            "emphasis",
        ];
        let children = if depth == 0 { 0 } else { next(seed, 5) };
        for _ in 0..children {
            if next(seed, 3) == 0 {
                events.push(Event::Text(TEXTS[next(seed, TEXTS.len())]));
                continue;
            }
            let element = ELEMENTS[next(seed, ELEMENTS.len())];
            events.push(Event::Start(element));
            tree(seed, depth - 1, events);
            events.push(Event::End(element));
        }
    }

    /// A book counting how deep it is in the elements it tells apart collects what one keeping
    /// a stack of the open elements does, block by block and in its metadata.
    #[test]
    fn collects_with_depths_as_with_a_stack() {
        for mut seed in 0..300u64 {
            let mut events = vec![Event::Start("FictionBook")];
            tree(&mut seed, 7, &mut events);
            events.push(Event::End("FictionBook"));
            let (mut book, mut stack) = (Book::default(), StackBook::default());
            let (mut metadata, mut stack_metadata) = (TikaMetadata::new(), TikaMetadata::new());
            for event in &events {
                match *event {
                    Event::Start(element) => {
                        book.start(element);
                        stack.start(element);
                    }
                    Event::Text(text) => {
                        book.text(text);
                        stack.text(text);
                    }
                    Event::End(element) => {
                        let block = book
                            .end(element, &mut metadata)
                            .map(|block| (block.element.to_owned(), block.text));
                        assert_eq!(block, stack.end(element, &mut stack_metadata));
                    }
                }
                assert_eq!(metadata, stack_metadata);
            }
        }
    }

    #[test]
    fn reads_titles_authors_and_nested_sections() {
        let book = br##"<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description><title-info><author><first-name>Anna</first-name><last-name>Berg</last-name></author>
<book-title>Fyrets bog</book-title><lang>da</lang></title-info>
<document-info><author><nickname>ignored</nickname></author></document-info></description>
<body><title><p>Del</p><p>en</p></title>
<section><title><p>Kapitel 1</p></title><p>Hver aften &amp; nat&#x21;</p>
<image l:href="#cover"/><section><subtitle>Vers</subtitle><poem><stanza><v>linje</v></stanza></poem></section></section></body>
<binary id="cover">AAAA</binary></FictionBook>"##;
        let (xml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(book)
            .unwrap();
        let body = &xml[xml.find("<body>").unwrap()..];
        assert_eq!(
            body,
            "<body><h1>Del en</h1>\n<h2>Kapitel 1</h2>\n<p>Hver aften &amp; nat!</p>\n\
             <h4>Vers</h4>\n<p>linje</p>\n</body></html>"
        );
        assert_eq!(metadata["dc:creator"], ["Anna Berg"]);
        assert_eq!(metadata["dc:title"], ["Fyrets bog"]);
        assert_eq!(metadata["dc:language"], ["da"]);
    }

    #[test]
    fn fails_on_malformed_xml_and_undeclared_entities() {
        for body in [
            "<body><p>x</body></FictionBook>",
            "<body><p>&nbsp;</p></body></FictionBook>",
            "<body>",
        ] {
            let book = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">{body}"#
            );
            let extracted = crate::Extractor::new().extract_bytes_to_string(book.as_bytes());
            assert!(extracted.is_err(), "{body}: {extracted:?}");
        }
    }
}
