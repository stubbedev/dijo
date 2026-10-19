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
use crate::java::strip;

const MAX_HEADING_LEVEL: usize = 6;
const PARAGRAPHS: [&str; 4] = ["p", "v", "text-author", "subtitle"];
const NAME_PARTS: [&str; 4] = ["first-name", "middle-name", "last-name", "nickname"];

pub struct FictionBookParser;

impl Parser for FictionBookParser {
    fn name(&self) -> &'static str {
        "extractous::FictionBookParser"
    }
    fn tika_name(&self) -> &'static str {
        "ai.yobix.FictionBookParser"
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
    element: String,
    text: String,
}

/// Collects the book's metadata, and each block as it ends, while its XML is read.
#[derive(Default)]
struct Book {
    open: Vec<String>,
    text: String,
    name: Vec<String>,
    sections: usize,
    collecting: Option<String>,
}

impl Book {
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

    /// Ends `element`, adding what it names to `metadata`: the block it ends, if any.
    fn end(&mut self, element: &str, metadata: &mut TikaMetadata) -> Option<Block> {
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
            _ => "p".to_owned(),
        };
        Some(Block { element, text })
    }
}

fn heading(level: usize) -> String {
    format!("h{}", level.min(MAX_HEADING_LEVEL))
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
    strip(&collapsed).to_owned()
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
            self.xhtml.element(&block.element, &block.text)?;
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
        let count = extractous_core::large::bodies_past_limit(paragraph(0).len());
        let file = extractous_core::large::LargeFile::new(
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
