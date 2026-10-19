//! Tika's `SecureContentHandler`, which `AutoDetectParser` puts between a parser's
//! `XHTMLContentHandler` and the handler the output goes to: it stops a parse whose elements
//! nest a hundred deep, whose `package-entry` divs nest ten deep, or whose output passes a
//! million characters and a hundred per byte of the document, as a suspected zip bomb.
//!
//! `AutoDetectParser` puts one in front of every document it parses, embedded ones included, so
//! a guard sees the events of the documents embedded in its own, and passes on the stop of a
//! guard further out. A guard tells its own stop from those by [`Secure::tripped`], as
//! `throwIfCauseOf` does.

use std::fmt;

use crate::formats::embedded::PACKAGE_ENTRY;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{ContentHandler, Stop};
use crate::java::utf16_length;

/// The message of the `TikaException` `throwIfCauseOf` throws for a zip bomb its guard found.
pub const ZIP_BOMB_DETECTED: &str = "Zip bomb detected!";
/// `OutputLimits`' default zip bomb threshold: the characters a document may write whatever its
/// size.
pub const OUTPUT_THRESHOLD: u64 = 1_000_000;
/// `OutputLimits`' default zip bomb ratio: the characters a document may write per byte of it.
pub const MAXIMUM_RATIO: u64 = 100;
/// `OutputLimits`' default maximum XML depth.
const MAXIMUM_DEPTH: usize = 100;
/// `OutputLimits`' default maximum package entry depth.
pub const MAXIMUM_PACKAGE_ENTRY_DEPTH: usize = 10;

/// What `SecureContentHandler` takes a document for a zip bomb by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZipBomb {
    /// Its elements nest this deep.
    ElementNesting(usize),
    /// Its `package-entry` divs nest this deep.
    PackageEntryNesting(usize),
    /// Its `bytes` produced `characters` of output.
    Ratio { bytes: u64, characters: u64 },
}

impl fmt::Display for ZipBomb {
    /// The message of Tika's `SecureSAXException`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZipBomb::ElementNesting(depth) => write!(
                f,
                "Suspected zip bomb: {depth} levels of XML element nesting"
            ),
            ZipBomb::PackageEntryNesting(depth) => write!(
                f,
                "Suspected zip bomb: {depth} levels of package entry nesting"
            ),
            ZipBomb::Ratio { bytes, characters } => write!(
                f,
                "Suspected zip bomb: {bytes} input bytes produced {characters} output characters"
            ),
        }
    }
}

/// `SecureContentHandler` over `next`, for a document of `bytes` bytes.
pub struct Secure<H> {
    next: H,
    bytes: u64,
    characters: u64,
    depth: usize,
    /// The element depth of each `package-entry` div open, the innermost last.
    package_entries: Vec<usize>,
    tripped: Option<ZipBomb>,
}

impl<H: ContentHandler> Secure<H> {
    pub fn new(next: H, bytes: usize) -> Self {
        Self {
            next,
            bytes: bytes as u64,
            characters: 0,
            depth: 0,
            package_entries: Vec::new(),
            tripped: None,
        }
    }

    /// The zip bomb this guard stopped the parse for, if it did; not one a guard further out
    /// stopped it for.
    pub fn tripped(&self) -> Option<ZipBomb> {
        self.tripped
    }

    /// Stops the parse as the zip bomb `bomb`.
    fn trip(&mut self, bomb: ZipBomb) -> Stop {
        self.tripped = Some(bomb);
        Stop::ZipBomb(bomb)
    }

    /// `advance`: counts `text`, stopping the parse once the output passes the threshold and
    /// the ratio.
    fn advance(&mut self, text: &str) -> Result<(), Stop> {
        self.characters += utf16_length(text) as u64;
        if self.characters > OUTPUT_THRESHOLD
            && self.characters > self.bytes.saturating_mul(MAXIMUM_RATIO)
        {
            return Err(self.trip(ZipBomb::Ratio {
                bytes: self.bytes,
                characters: self.characters,
            }));
        }
        Ok(())
    }
}

impl<H: ContentHandler> ContentHandler for Secure<H> {
    fn start_document(&mut self) -> Result<(), Stop> {
        self.next.start_document()
    }

    fn start_prefix_mapping(&mut self, prefix: &str, uri: &str) -> Result<(), Stop> {
        self.next.start_prefix_mapping(prefix, uri)
    }

    fn start_element_in(
        &mut self,
        namespace: &str,
        name: &str,
        attributes: &[(&str, &str)],
    ) -> Result<(), Stop> {
        self.depth += 1;
        if self.depth >= MAXIMUM_DEPTH {
            return Err(self.trip(ZipBomb::ElementNesting(self.depth)));
        }
        let package_entry = name == "div"
            && attributes
                .iter()
                .any(|&(attribute, value)| attribute == "class" && value == PACKAGE_ENTRY);
        if package_entry {
            self.package_entries.push(self.depth);
            if self.package_entries.len() >= MAXIMUM_PACKAGE_ENTRY_DEPTH {
                return Err(self.trip(ZipBomb::PackageEntryNesting(self.package_entries.len())));
            }
        }
        self.next.start_element_in(namespace, name, attributes)
    }

    fn end_element_in(&mut self, namespace: &str, name: &str) -> Result<(), Stop> {
        self.next.end_element_in(namespace, name)?;
        if self.package_entries.last() == Some(&self.depth) {
            self.package_entries.pop();
        }
        self.depth -= 1;
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        self.advance(text)?;
        self.next.characters(text)
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        self.advance(text)?;
        self.next.ignorable_whitespace(text)
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        self.next.end_document()
    }

    fn flush(&mut self) -> Result<(), Stop> {
        self.next.flush()
    }

    fn head_metadata(&mut self, metadata: &TikaMetadata) {
        self.next.head_metadata(metadata);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::xhtml::Held;

    fn open_entries(secure: &mut impl ContentHandler, count: usize) -> Result<(), Stop> {
        for _ in 0..count {
            secure.start_element("div", &[("class", PACKAGE_ENTRY)])?;
        }
        Ok(())
    }

    #[test]
    fn tells_its_own_stop_from_that_of_a_guard_further_out() {
        let mut outer = Secure::new(Held::default(), 100);
        open_entries(&mut outer, 8).unwrap();
        let mut inner = Secure::new(&mut outer, 100);
        assert_eq!(
            open_entries(&mut inner, 2),
            Err(Stop::ZipBomb(ZipBomb::PackageEntryNesting(10)))
        );
        assert_eq!(inner.tripped(), None);
        assert_eq!(outer.tripped(), Some(ZipBomb::PackageEntryNesting(10)));
        let mut outer = Secure::new(Held::default(), 1_000_000);
        let mut inner = Secure::new(&mut outer, 100);
        let bomb = Err(Stop::ZipBomb(ZipBomb::Ratio {
            bytes: 100,
            characters: 1_000_001,
        }));
        assert_eq!(inner.characters(&"a".repeat(1_000_001)), bomb);
        assert_eq!(
            inner.tripped(),
            Some(ZipBomb::Ratio {
                bytes: 100,
                characters: 1_000_001,
            })
        );
        assert_eq!(outer.tripped(), None);
    }

    #[test]
    fn stops_at_ten_package_entries_open_at_once() {
        let mut secure = Secure::new(Held::default(), 100);
        assert_eq!(open_entries(&mut secure, 9), Ok(()));
        secure.end_element("div").unwrap();
        secure
            .start_element("div", &[("class", "embedded")])
            .unwrap();
        assert_eq!(open_entries(&mut secure, 1), Ok(()));
        assert_eq!(
            open_entries(&mut secure, 1),
            Err(Stop::ZipBomb(ZipBomb::PackageEntryNesting(10)))
        );
    }

    #[test]
    fn stops_at_a_hundred_elements_deep() {
        let mut secure = Secure::new(Held::default(), 100);
        for _ in 0..99 {
            secure.start_element("p", &[]).unwrap();
        }
        assert_eq!(
            secure.start_element("p", &[]),
            Err(Stop::ZipBomb(ZipBomb::ElementNesting(100)))
        );
    }

    #[test]
    fn stops_output_past_the_threshold_and_the_ratio() {
        let mut secure = Secure::new(Held::default(), 20_000);
        let text = "a".repeat(1_000_000);
        secure.characters(&text).unwrap();
        secure.ignorable_whitespace(&text).unwrap();
        assert_eq!(
            secure.characters("é"),
            Err(Stop::ZipBomb(ZipBomb::Ratio {
                bytes: 20_000,
                characters: 2_000_001,
            }))
        );
        let mut small = Secure::new(Held::default(), 1);
        assert_eq!(small.characters(&text), Ok(()));
        assert!(small.characters("a").is_err());
    }
}
