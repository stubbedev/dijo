//! FictionBook (FB2) books, as our Java `FictionBookParser` reads them: the book's title, authors
//! and language as metadata, its sections as headings and paragraphs, verse and quotations as
//! paragraphs, and no embedded binaries. Tika reads FB2 as generic XML, running paragraphs
//! together and keeping the names of embedded images and the language code in the text.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java::strip;

const MAX_HEADING_LEVEL: usize = 6;
const PARAGRAPHS: [&str; 4] = ["p", "v", "text-author", "subtitle"];
const NAME_PARTS: [&str; 4] = ["first-name", "middle-name", "last-name", "nickname"];

pub(crate) struct FictionBookParser;

impl Parser for FictionBookParser {
    fn name(&self) -> &'static str {
        "extractous::FictionBookParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "ai.yobix.FictionBookParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-fictionbook+xml"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        Ok(Box::new(move |xhtml| {
            let blocks = read_book(input, xhtml.metadata_mut())?;
            for block in blocks {
                xhtml.element(&block.element, &block.text)?;
            }
            Ok(())
        }))
    }
}

/// A heading or a paragraph of the book.
#[derive(Debug, PartialEq, Eq)]
struct Block {
    element: String,
    text: String,
}

/// Collects the book's metadata and blocks while its XML is read.
#[derive(Default)]
struct Book {
    blocks: Vec<Block>,
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

    fn end(&mut self, element: &str, metadata: &mut TikaMetadata) {
        self.open.pop();
        if element == "section" {
            self.sections = self.sections.saturating_sub(1);
        }
        if element == "p" && self.collecting.as_deref() == Some("title") {
            self.text.push(' ');
            return;
        }
        if element == "author" && self.inside("title-info") && !self.name.is_empty() {
            metadata.add("dc:creator", self.name.join(" "));
            self.name.clear();
        }
        if self.collecting.as_deref() != Some(element) {
            return;
        }
        self.collecting = None;
        let value = collapse_whitespace(&self.text);
        if value.is_empty() {
            return;
        }
        match element {
            "book-title" => metadata.set("dc:title", value),
            "lang" => metadata.set("dc:language", value),
            "title" => self.block(heading(self.sections + 1), value),
            "subtitle" => self.block(heading(self.sections + 2), value),
            _ if NAME_PARTS.contains(&element) => self.name.push(value),
            _ => self.block("p".to_owned(), value),
        }
    }

    fn block(&mut self, element: String, text: String) {
        self.blocks.push(Block { element, text });
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

/// The book being read and the metadata it fills, as the SAX handler of its XML.
struct Reading<'m> {
    book: Book,
    metadata: &'m mut TikaMetadata,
}

impl SaxHandler for Reading<'_> {
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
        self.book.end(local_name, self.metadata);
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Failure> {
        self.book.text(text);
        Ok(())
    }
}

fn read_book(input: &[u8], metadata: &mut TikaMetadata) -> Result<Vec<Block>, Failure> {
    let mut reading = Reading {
        book: Book::default(),
        metadata,
    };
    xml::parse_sax(input, WHAT, &mut reading)?;
    Ok(reading.book.blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::assert_same_as_tika;
    #[cfg(feature = "tika")]
    use std::path::Path;

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts() {
        let documents = [
            "../test_files/documents/book.fb2",
            "../test_files/documents/book-cp1251.fb2",
        ];
        let paths: Vec<&Path> = documents.iter().map(Path::new).collect();
        assert_same_as_tika(&paths, "ai.yobix.FictionBookParser");
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
        let mut metadata = TikaMetadata::new();
        let blocks = read_book(book, &mut metadata).unwrap();
        let blocks: Vec<(&str, &str)> = blocks
            .iter()
            .map(|block| (block.element.as_str(), block.text.as_str()))
            .collect();
        assert_eq!(
            blocks,
            [
                ("h1", "Del en"),
                ("h2", "Kapitel 1"),
                ("p", "Hver aften & nat!"),
                ("h4", "Vers"),
                ("p", "linje"),
            ]
        );
        assert_eq!(metadata.values("dc:creator"), ["Anna Berg"]);
        assert_eq!(metadata.get("dc:title"), Some("Fyrets bog"));
        assert_eq!(metadata.get("dc:language"), Some("da"));
    }

    #[test]
    fn fails_on_malformed_xml_and_undeclared_entities() {
        let mut metadata = TikaMetadata::new();
        assert!(read_book(
            b"<FictionBook><body><p>x</body></FictionBook>",
            &mut metadata
        )
        .is_err());
        assert!(read_book(
            b"<FictionBook><body><p>&nbsp;</p></body></FictionBook>",
            &mut metadata
        )
        .is_err());
        assert!(read_book(b"<FictionBook><body>", &mut metadata).is_err());
    }
}
