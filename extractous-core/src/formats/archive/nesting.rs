//! How deep archives nest inside a document a native parser hands to Tika's path.
//!
//! Tika's top-level `SecureContentHandler` stops a parse ten package entries deep, counting the
//! top-level document's entries as the first level. The native parsers write that first level
//! themselves, and the embedded parse that Tika's path runs for an entry counts its package
//! entries from its own start, a level short. An entry whose archives nest nine deep would
//! reach Tika's limit, so the native parsers leave such a document to Tika's path. The depth
//! counts the archives and compressed files the port reads; package entries that other
//! formats' parsers write, for the documents embedded in them, are not counted.

use crate::compress::ar::ArStream;
use crate::compress::archiver::Archiver;
use crate::compress::compressor::Compressor;
use crate::compress::cpio::CpioStream;
use crate::compress::tar::TarStream;
use crate::compress::zip::file::ZipFile;
use crate::compress::zip::Content;
use crate::compress::MAX_DECODED;

/// Why a native parser leaves a document whose archives nest too deep to Tika's path.
pub(super) const TOO_DEEP_REASON: &str = "archives nest nine deep inside an entry, where Tika's \
     top-level SecureContentHandler, which counts the native parser's level too, stops the parse";

/// The nesting below an entry at which Tika's count reaches its limit of ten package entries.
const TOO_DEEP: usize = 9;
/// The most bytes the scan of one document decodes, past which it counts the document as
/// nesting too deep.
const BUDGET: usize = 4 * MAX_DECODED;

/// A scan of the entries of one document.
pub(super) struct Nesting {
    budget: usize,
}

impl Nesting {
    pub(super) fn new() -> Self {
        Nesting { budget: BUDGET }
    }

    /// Whether the archives in `content`, an entry of the document, nest too deep for the
    /// native path to match Tika's, or more than the scan can decode to tell.
    pub(super) fn too_deep(&mut self, content: &[u8]) -> bool {
        self.depth(content, TOO_DEEP) >= TOO_DEEP
    }

    /// Takes `length` decoded bytes from the budget, `false` once it runs out.
    fn spend(&mut self, length: usize) -> bool {
        match self.budget.checked_sub(length) {
            Some(rest) => {
                self.budget = rest;
                true
            }
            None => false,
        }
    }

    /// How many archives nest in `data`, `data` itself the first, up to `limit`.
    fn depth(&mut self, data: &[u8], limit: usize) -> usize {
        if limit == 0 {
            return 0;
        }
        if let Some(compressor) = Compressor::detect(data) {
            return match compressor.decode(data) {
                Ok(decoded) if self.spend(decoded.content.len()) => {
                    1 + self.depth(&decoded.content, limit - 1)
                }
                Ok(_) => limit,
                Err(_) => 1,
            };
        }
        let deepest = match Archiver::detect(data) {
            Some(Archiver::Zip) => self.zip_depth(data, limit - 1),
            Some(Archiver::Tar) => {
                let mut stream = TarStream::new(data);
                let mut deepest = 0;
                while let Ok(Some(entry)) = stream.next_entry() {
                    if let Ok(content) = stream.content(&entry) {
                        deepest = deepest.max(self.depth(content, limit - 1));
                    }
                }
                deepest
            }
            Some(Archiver::Ar) => {
                let mut stream = ArStream::new(data);
                let mut deepest = 0;
                while let Ok(Some(entry)) = stream.next_entry() {
                    deepest = deepest.max(self.depth(stream.content(&entry), limit - 1));
                }
                deepest
            }
            Some(Archiver::Cpio) => {
                let mut stream = CpioStream::new(data);
                let mut deepest = 0;
                while let Ok(Some(entry)) = stream.next_entry() {
                    if let Ok(content) = stream.content(&entry) {
                        deepest = deepest.max(self.depth(content, limit - 1));
                    }
                }
                deepest
            }
            Some(Archiver::Arj | Archiver::SevenZ | Archiver::Dump) => 0,
            None => return 0,
        };
        (1 + deepest).min(limit)
    }

    fn zip_depth(&mut self, data: &[u8], limit: usize) -> usize {
        let Ok(zip) = ZipFile::open(data) else {
            return 0;
        };
        let mut deepest = 0;
        for entry in zip.entries() {
            if entry.is_directory() || deepest >= limit {
                continue;
            }
            match zip.read(entry) {
                Some(Content::Complete(content) | Content::Truncated(content))
                    if self.spend(content.len()) =>
                {
                    deepest = deepest.max(self.depth(&content, limit));
                }
                Some(_) => return limit,
                None => {}
            }
        }
        deepest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::gzip::tests::gzip;
    use crate::compress::tar::tests::tar;
    use crate::compress::zip::file::tests::zip;

    #[test]
    fn counts_the_archives_inside_one_another() {
        let mut nesting = Nesting::new();
        assert_eq!(nesting.depth(b"plain text", TOO_DEEP), 0);
        let tgz = gzip(&tar(&[(b"a.txt", b'0', b"text")]), None);
        assert_eq!(nesting.depth(&tgz, TOO_DEEP), 2);
        let mut nested = zip(&[("level.txt", b"text")]);
        for _ in 1..8 {
            nested = zip(&[("level.txt", b"text"), ("inner.zip", &nested)]);
        }
        assert_eq!(nesting.depth(&nested, TOO_DEEP), 8);
        assert!(!nesting.too_deep(&nested));
        assert!(nesting.too_deep(&zip(&[("inner.zip", &nested)])));
    }
}
