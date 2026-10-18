//! Cuts an extraction's output short after a number of characters of text.

use std::num::NonZeroU64;

use crate::schema::{ErrorCategory, ExtractionError};

/// Counts the characters of UTF-8 text that may arrive cut anywhere, a multi-byte character
/// included, until a number of them is reached.
#[derive(Debug)]
struct Characters {
    max: NonZeroU64,
    counted: u64,
    reached: bool,
}

impl Characters {
    fn new(max: NonZeroU64) -> Self {
        Self {
            max,
            counted: 0,
            reached: false,
        }
    }

    /// Whether `byte` may pass: a byte that starts a character is counted, and once the limit is
    /// reached it and everything after it is refused.
    fn admit(&mut self, byte: u8) -> bool {
        if self.reached {
            return false;
        }
        if byte & 0xC0 == 0x80 {
            return true;
        }
        if self.counted == self.max.get() {
            self.reached = true;
            return false;
        }
        self.counted += 1;
        true
    }
}

/// Where in XHTML the scan of its markup stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Markup {
    Text,
    /// An entity or character reference, which counts as the one character it stands for.
    Reference,
    /// A tag, a comment or a processing instruction, and the quote an attribute value is in.
    Tag {
        quote: Option<u8>,
    },
}

/// Passes on the characters of XHTML's text up to a limit, with the markup between them, and
/// then closes the elements left open, so that what is cut short stays well-formed.
#[derive(Debug)]
struct Xhtml {
    characters: Characters,
    markup: Markup,
    tag: Vec<u8>,
    open: Vec<Vec<u8>>,
}

impl Xhtml {
    fn admit<'b>(&mut self, bytes: &'b [u8]) -> &'b [u8] {
        if self.characters.reached {
            return &[];
        }
        for (index, &byte) in bytes.iter().enumerate() {
            match self.markup {
                Markup::Text if byte == b'<' => {
                    self.tag.clear();
                    self.tag.push(byte);
                    self.markup = Markup::Tag { quote: None };
                }
                Markup::Text => {
                    if !self.characters.admit(byte) {
                        return &bytes[..index];
                    }
                    if byte == b'&' {
                        self.markup = Markup::Reference;
                    }
                }
                Markup::Reference => {
                    if byte == b';' {
                        self.markup = Markup::Text;
                    }
                }
                Markup::Tag { quote } => {
                    self.tag.push(byte);
                    self.markup = match (quote, byte) {
                        (Some(open), _) if byte == open => Markup::Tag { quote: None },
                        (Some(_), _) => self.markup,
                        (None, b'"' | b'\'') if !self.in_comment() => {
                            Markup::Tag { quote: Some(byte) }
                        }
                        (None, b'>') if !self.in_comment() || self.tag.ends_with(b"-->") => {
                            self.end_tag();
                            Markup::Text
                        }
                        (None, _) => self.markup,
                    };
                }
            }
        }
        bytes
    }

    fn in_comment(&self) -> bool {
        self.tag.starts_with(b"<!--")
    }

    fn end_tag(&mut self) {
        match self.tag.get(1) {
            Some(b'/') => {
                self.open.pop();
            }
            Some(b'?' | b'!') => {}
            _ if self.tag.ends_with(b"/>") => {}
            _ => {
                let name = self.tag[1..]
                    .iter()
                    .position(|&byte| byte.is_ascii_whitespace() || byte == b'/' || byte == b'>')
                    .map_or(&self.tag[1..], |end| &self.tag[1..=end]);
                self.open.push(name.to_vec());
            }
        }
    }

    fn closing_tags(&self) -> Vec<u8> {
        let mut closing = Vec::new();
        for name in self.open.iter().rev() {
            closing.extend_from_slice(b"</");
            closing.extend_from_slice(name);
            closing.push(b'>');
        }
        closing
    }
}

/// The limit on the characters of text an extraction outputs, if it has one.
#[derive(Debug)]
pub struct OutputLimit(Kind);

#[derive(Debug)]
enum Kind {
    Unlimited,
    /// Plain text, cut after its last admitted character.
    Text(Characters),
    /// XHTML, whose markup does not count and whose open elements are closed when it is cut.
    Xhtml(Xhtml),
}

impl OutputLimit {
    pub fn text(max: Option<NonZeroU64>) -> Self {
        OutputLimit(max.map_or(Kind::Unlimited, |max| Kind::Text(Characters::new(max))))
    }

    pub fn xhtml(max: Option<NonZeroU64>) -> Self {
        OutputLimit(max.map_or(Kind::Unlimited, |max| {
            Kind::Xhtml(Xhtml {
                characters: Characters::new(max),
                markup: Markup::Text,
                tag: Vec::new(),
                open: Vec::new(),
            })
        }))
    }

    /// The part of `bytes`, the next of the output, that is within the limit. UTF-8 is never cut
    /// inside a character, nor XHTML inside its markup.
    pub fn admit<'b>(&mut self, bytes: &'b [u8]) -> &'b [u8] {
        match &mut self.0 {
            Kind::Unlimited => bytes,
            Kind::Text(characters) => bytes
                .iter()
                .position(|&byte| !characters.admit(byte))
                .map_or(bytes, |end| &bytes[..end]),
            Kind::Xhtml(xhtml) => xhtml.admit(bytes),
        }
    }

    /// Why the output was cut short, once it was.
    pub fn reached(&self) -> Option<ExtractionError> {
        let characters = match &self.0 {
            Kind::Unlimited => return None,
            Kind::Text(characters) => characters,
            Kind::Xhtml(xhtml) => &xhtml.characters,
        };
        characters.reached.then(|| ExtractionError {
            category: ErrorCategory::LimitExceeded,
            message: format!(
                "The text was cut short after the output limit of {} characters",
                characters.max
            ),
            location: None,
        })
    }

    /// What ends output cut short: the end tags of the XHTML elements it left open.
    pub fn closing(&self) -> Vec<u8> {
        match &self.0 {
            Kind::Xhtml(xhtml) if xhtml.characters.reached => xhtml.closing_tags(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limited(mut limit: OutputLimit, text: &str, chunk: usize) -> (String, bool) {
        let mut out = Vec::new();
        for piece in text.as_bytes().chunks(chunk) {
            out.extend_from_slice(limit.admit(piece));
        }
        out.extend(limit.closing());
        (String::from_utf8(out).unwrap(), limit.reached().is_some())
    }

    fn max(value: u64) -> Option<NonZeroU64> {
        NonZeroU64::new(value)
    }

    #[test]
    fn text_is_cut_after_whole_characters_in_any_chunks() {
        for chunk in 1..8 {
            assert_eq!(
                limited(OutputLimit::text(max(4)), "æø漢🎉 and more", chunk),
                ("æø漢🎉".to_string(), true)
            );
            assert_eq!(
                limited(OutputLimit::text(max(4)), "æø漢🎉", chunk),
                ("æø漢🎉".to_string(), false)
            );
        }
        assert_eq!(
            limited(OutputLimit::text(None), "anything", 3),
            ("anything".to_string(), false)
        );
    }

    #[test]
    fn xhtml_counts_its_text_and_stays_well_formed() {
        let xhtml =
            "<?xml version=\"1.0\"?><html xmlns=\"x\"><head><meta name=\"a&gt;b\" content='>'/>\
                     <title>Tït</title></head><body><!-- <p> --><p class=\"a\">One &amp; two</p>\
                     <p>three</p></body></html>";
        for chunk in 1..9 {
            assert_eq!(
                limited(OutputLimit::xhtml(max(9)), xhtml, chunk),
                (
                    "<?xml version=\"1.0\"?><html xmlns=\"x\"><head><meta name=\"a&gt;b\" content='>'/>\
                     <title>Tït</title></head><body><!-- <p> --><p class=\"a\">One &amp; </p></body></html>"
                        .to_string(),
                    true
                )
            );
        }
        assert_eq!(
            limited(OutputLimit::xhtml(max(1000)), xhtml, 5),
            (xhtml.to_string(), false)
        );
    }
}
