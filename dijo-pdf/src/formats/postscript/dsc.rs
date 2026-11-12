//! The document's own description in its document structuring comments: the header's `%%Title`,
//! `%%Creator`, `%%CreationDate`, `%%For` and `%%Pages`, each deferred to the trailer by
//! `(atend)` read there, and none of the documents the program includes
//! (`%%BeginDocument` to `%%EndDocument`).

use super::scanner::{Object, Scanner, Token};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xmp::date;
use crate::java::time::format_instant;

/// The comment value that defers a header comment to the trailer.
const AT_END: &[u8] = b"(atend)";
/// A creation date that does not read as an instant, as the comment gives it.
pub(super) const CREATION_DATE: &str = "ps:creation-date";
const PAGES: &str = "xmpTPg:NPages";

/// The header comments read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Title,
    Creator,
    CreationDate,
    For,
    Pages,
}

impl Key {
    const ALL: [Key; 5] = [
        Key::Title,
        Key::Creator,
        Key::CreationDate,
        Key::For,
        Key::Pages,
    ];

    fn comment(self) -> &'static [u8] {
        match self {
            Key::Title => b"Title:",
            Key::Creator => b"Creator:",
            Key::CreationDate => b"CreationDate:",
            Key::For => b"For:",
            Key::Pages => b"Pages:",
        }
    }
}

/// Where in the document's outermost structure a comment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Header,
    Body,
    Trailer,
}

/// The values of the header comments, by [`Key::ALL`]'s order, as the comments give them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Comments {
    values: [Option<Vec<u8>>; 5],
}

impl Comments {
    /// The structuring comments of `program`.
    pub(super) fn read(program: &[u8]) -> Self {
        let mut comments = Comments::default();
        let mut deferred = [false; 5];
        let mut section = Section::Header;
        let mut included = 0usize;
        let mut last: Option<usize> = None;
        for line in program.split(|&byte| matches!(byte, b'\n' | b'\r')) {
            let Some(comment) = line.strip_prefix(b"%%") else {
                if section == Section::Header && !line.starts_with(b"%") && !line.is_empty() {
                    section = Section::Body;
                }
                continue;
            };
            if comment.starts_with(b"BeginDocument") {
                included += 1;
                continue;
            }
            if comment.starts_with(b"EndDocument") {
                included = included.saturating_sub(1);
                continue;
            }
            if included > 0 {
                continue;
            }
            if comment.starts_with(b"EndComments") && section == Section::Header {
                section = Section::Body;
                continue;
            }
            if comment.starts_with(b"Trailer") {
                section = Section::Trailer;
                continue;
            }
            if let Some(continued) = comment.strip_prefix(b"+") {
                if let Some(value) = last.and_then(|index| comments.values[index].as_mut()) {
                    value.push(b' ');
                    value.extend_from_slice(trim(continued));
                }
                continue;
            }
            last = None;
            let Some((index, value)) = Key::ALL.iter().enumerate().find_map(|(index, key)| {
                comment
                    .strip_prefix(key.comment())
                    .map(|value| (index, trim(value)))
            }) else {
                continue;
            };
            let take = match section {
                Section::Header => comments.values[index].is_none(),
                Section::Body => false,
                Section::Trailer => deferred[index],
            };
            if !take {
                continue;
            }
            if section == Section::Header && value == AT_END {
                deferred[index] = true;
                continue;
            }
            deferred[index] = false;
            comments.values[index] = Some(value.to_vec());
            last = Some(index);
        }
        comments
    }

    fn value(&self, key: Key) -> Option<String> {
        let index = Key::ALL.iter().position(|&listed| listed == key)?;
        let value = self.values[index].as_deref()?;
        let text = text(value);
        (!text.is_empty()).then_some(text)
    }

    /// Records the comments in `metadata`: the title as `dc:title`, the creator as
    /// `xmp:CreatorTool`, whom the document is for as `dc:creator`, the page count as
    /// `xmpTPg:NPages`, and the creation date as `dcterms:created` where it reads as an instant,
    /// else as it is given in [`CREATION_DATE`].
    pub(super) fn record(&self, metadata: &mut TikaMetadata) {
        if let Some(title) = self.value(Key::Title) {
            metadata.set("dc:title", title);
        }
        if let Some(creator) = self.value(Key::Creator) {
            metadata.set("xmp:CreatorTool", creator);
        }
        if let Some(person) = self.value(Key::For) {
            metadata.set("dc:creator", person);
        }
        if let Some(pages) = self.value(Key::Pages).as_deref().and_then(page_count) {
            metadata.set(PAGES, pages.to_string());
        }
        if let Some(created) = self.value(Key::CreationDate) {
            match date::to_calendar(&created) {
                Ok(Some(millis)) => metadata.set("dcterms:created", format_instant(millis)),
                Ok(None) | Err(_) => metadata.set(CREATION_DATE, created),
            }
        }
    }
}

/// The first number of a `%%Pages` comment, which may go on with the page order.
fn page_count(value: &str) -> Option<u32> {
    value.split_whitespace().next()?.parse().ok()
}

fn trim(bytes: &[u8]) -> &[u8] {
    bytes.trim_ascii()
}

/// The text of a comment value: a PostScript string's content where it is one, as UTF-8 where
/// it is valid, else as ISO 8859-1.
fn text(value: &[u8]) -> String {
    let content = match Scanner::new(value).next_token() {
        Some(Token::Object(Object::String(content)))
            if value.starts_with(b"(") && value.ends_with(b")") =>
        {
            content
        }
        _ => value.to_vec(),
    };
    let decoded = match String::from_utf8(content) {
        Ok(text) => text,
        Err(error) => crate::java::latin1(error.into_bytes()),
    };
    decoded.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded(program: &[u8]) -> TikaMetadata {
        let mut metadata = TikaMetadata::new();
        Comments::read(program).record(&mut metadata);
        metadata
    }

    #[test]
    fn reads_the_header_comments() {
        let metadata = recorded(
            b"%!PS-Adobe-3.0\n%%Title: (Lighthouse \\(log\\))\n%%Creator: hand\n\
              %%+ written\n%%For: The Keeper\r\n%%CreationDate: D:20240115103000Z\n\
              %%Pages: 2 1\n%%EndComments\n%%Title: Later\n",
        );
        assert_eq!(metadata.get("dc:title"), Some("Lighthouse (log)"));
        assert_eq!(metadata.get("xmp:CreatorTool"), Some("hand written"));
        assert_eq!(metadata.get("dc:creator"), Some("The Keeper"));
        assert_eq!(
            metadata.get("dcterms:created"),
            Some("2024-01-15T10:30:00Z")
        );
        assert_eq!(metadata.get(PAGES), Some("2"));
    }

    #[test]
    fn reads_deferred_comments_in_the_trailer() {
        let metadata = recorded(
            b"%!PS-Adobe-3.0\n%%Pages: (atend)\n%%Title: Outer\n%%EndComments\n\
              %%BeginDocument: inner.eps\n%%Title: Inner\n%%Trailer\n%%Pages: 9\n%%EndDocument\n\
              %%Trailer\n%%Pages: 3\n%%Title: Ignored\n%%EOF\n",
        );
        assert_eq!(metadata.get(PAGES), Some("3"));
        assert_eq!(metadata.get("dc:title"), Some("Outer"));
    }

    #[test]
    fn keeps_a_creation_date_that_is_no_instant_as_given() {
        let metadata = recorded(b"%!PS\n%%CreationDate: Mon Jan 15 10:30:00 2024\n");
        assert_eq!(metadata.get("dcterms:created"), None);
        assert_eq!(
            metadata.get(CREATION_DATE),
            Some("Mon Jan 15 10:30:00 2024")
        );
    }

    #[test]
    fn ends_the_header_at_the_first_line_of_program() {
        let metadata = recorded(b"%!PS\n/x 1 def\n%%Title: Too late\n");
        assert_eq!(metadata.get("dc:title"), None);
    }

    #[test]
    fn reads_iso_8859_1_titles() {
        let metadata = recorded(b"%!PS\n%%Title: Fyrt\xe5rn\n");
        assert_eq!(metadata.get("dc:title"), Some("Fyrtårn"));
    }
}
