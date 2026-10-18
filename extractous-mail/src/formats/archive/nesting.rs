//! What Tika's top-level `SecureContentHandler` stops in a document a native archive parser
//! reads, which the embedded parses the native parser hands to Tika's path cannot see.
//!
//! The handler stops a parse ten package entries deep, counting the top-level document's
//! entries as the first level. The native parsers write that first level themselves, and the
//! embedded parse that Tika's path runs for an entry counts its package entries from its own
//! start, a level short, so an entry whose archives nest nine deep would reach Tika's limit
//! and not the native path's. The handler also stops a parse whose output passes a million
//! characters and a hundred per byte of the document, as a zip bomb, which the native path
//! does not count. The native parsers leave a document to Tika's path when its entries nest
//! that deep, or decode into enough to come near the output limit. The scan counts the archives
//! and compressed files the port reads; what other formats' parsers write for the documents
//! embedded in them is not counted.
//!
//! Without Tika's path there is nothing to defer to, and the scan finds nothing: the native
//! path's own `SecureContentHandler` stops the document, as a zip bomb, where Tika's top-level
//! handler does.

use super::{rar, DEFERS};
use crate::compress::archiver::Archiver;
use crate::compress::compressor::Compressor;
use crate::compress::zip::file::ZipFile;
use crate::compress::zip::Content;
use crate::compress::MAX_DECODED;
use crate::compress::{ar, cpio, seven_z, tar, ArchiveFile, Limited};
use crate::formats::secure::{MAXIMUM_PACKAGE_ENTRY_DEPTH, MAXIMUM_RATIO, OUTPUT_THRESHOLD};

/// The nesting below an entry at which Tika's count reaches its limit of package entries.
const TOO_DEEP: usize = MAXIMUM_PACKAGE_ENTRY_DEPTH - 1;
/// The most bytes the scan of one document decodes, past which it counts the document as
/// nesting too deep.
const BUDGET: usize = 4 * MAX_DECODED;

/// A scan of the entries of one document.
pub(super) struct Scan {
    budget: usize,
    /// The bytes of the documents the entries hold, the archives and compressed files that
    /// hold them aside.
    documents: usize,
}

impl Scan {
    pub(super) fn new() -> Self {
        Scan {
            budget: BUDGET,
            documents: 0,
        }
    }

    /// Why the native path's output for the document cannot match Tika's because of `content`,
    /// an entry of it, if it cannot: its archives nest too deep, or more than the scan can
    /// decode to tell.
    pub(super) fn entry(&mut self, content: &[u8]) -> Option<String> {
        if !DEFERS {
            return None;
        }
        (self.depth(content, TOO_DEEP) >= TOO_DEEP).then(|| {
            "archives nest nine deep inside an entry, where Tika's top-level \
             SecureContentHandler, which counts the native parser's level too, stops the parse"
                .to_owned()
        })
    }

    /// Why the native path's output for the document of `length` bytes, whose archive holds
    /// `files`, cannot match Tika's, if it cannot.
    pub(super) fn files<'f>(
        mut self,
        length: usize,
        files: impl IntoIterator<Item = &'f ArchiveFile>,
    ) -> Option<String> {
        files
            .into_iter()
            .filter(|file| !file.directory)
            .find_map(|file| self.entry(&file.content))
            .or_else(|| self.document(length))
    }

    /// Why the native path's output for the document of `length` bytes cannot match Tika's
    /// once all its entries are scanned, if it cannot: the documents they hold come to half
    /// the output Tika's top-level `SecureContentHandler` takes for a zip bomb, which their
    /// text may reach.
    pub(super) fn document(&self, length: usize) -> Option<String> {
        if !DEFERS {
            return None;
        }
        let limit = OUTPUT_THRESHOLD.max((length as u64).saturating_mul(MAXIMUM_RATIO));
        ((self.documents as u64).saturating_mul(2) > limit).then(|| {
            format!(
                "the documents its entries hold come to more than half of the {limit} \
                 characters past which Tika's top-level SecureContentHandler stops the parse as \
                 a zip bomb"
            )
        })
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
            return match compressor.decode(data).and_then(Limited::whole) {
                Ok(decoded) if self.spend(decoded.content.len()) => {
                    1 + self.depth(&decoded.content, limit - 1)
                }
                Ok(_) => limit,
                Err(_) => 1,
            };
        }
        let files = match Archiver::detect(data) {
            Some(Archiver::Zip) => return (1 + self.zip_depth(data, limit - 1)).min(limit),
            Some(Archiver::Tar) => tar::files(data).and_then(Limited::whole).ok(),
            Some(Archiver::Ar) => ar::files(data).and_then(Limited::whole).ok(),
            Some(Archiver::Cpio) => cpio::files(data).and_then(Limited::whole).ok(),
            Some(Archiver::SevenZ) => seven_z::files(data, None).and_then(Limited::whole).ok(),
            Some(Archiver::Arj | Archiver::Dump) | None if rar::is_rar(data) => {
                rar::archive_files(data)
            }
            Some(Archiver::Arj | Archiver::Dump) | None => {
                self.documents = self.documents.saturating_add(data.len());
                return 0;
            }
        };
        let Some(files) = files else {
            return 1;
        };
        if !self.spend(files.iter().map(|file| file.content.len()).sum()) {
            return limit;
        }
        let deepest = files
            .iter()
            .filter(|file| !file.directory)
            .map(|file| self.depth(&file.content, limit - 1))
            .max()
            .unwrap_or(0);
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
    use crate::compress::tar_header::tests::tar;
    use crate::compress::zip::file::tests::zip;

    fn nested_eight_deep() -> Vec<u8> {
        let mut nested = zip(&[("level.txt", b"text")]);
        for _ in 1..8 {
            nested = zip(&[("level.txt", b"text"), ("inner.zip", &nested)]);
        }
        nested
    }

    #[test]
    fn counts_the_archives_inside_one_another() {
        let mut scan = Scan::new();
        assert_eq!(scan.depth(b"plain text", TOO_DEEP), 0);
        let tgz = gzip(&tar(&[(b"a.txt", b'0', b"text")]), None);
        assert_eq!(scan.depth(&tgz, TOO_DEEP), 2);
        assert_eq!(scan.depth(&nested_eight_deep(), TOO_DEEP), 8);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn defers_archives_nested_nine_deep() {
        let mut scan = Scan::new();
        let nested = nested_eight_deep();
        assert_eq!(scan.entry(&nested), None);
        assert!(scan.entry(&zip(&[("inner.zip", &nested)])).is_some());
    }

    #[cfg(not(feature = "tika"))]
    #[test]
    fn finds_nothing_to_defer_without_tika() {
        let mut scan = Scan::new();
        assert_eq!(
            scan.entry(&zip(&[("inner.zip", &nested_eight_deep())])),
            None
        );
        assert_eq!(scan.entry(&gzip(&[b'c'; 2_000_000], None)), None);
        assert_eq!(scan.document(1000), None);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn counts_what_the_entries_decode_into() {
        let mut scan = Scan::new();
        assert_eq!(scan.entry(&[b'a'; 400_000]), None);
        assert_eq!(scan.document(1000), None);
        let padded = tar(&[(b"b.txt", b'0', &[b'b'; 99_000])]);
        assert_eq!(scan.entry(&gzip(&padded, None)), None);
        assert_eq!(scan.document(1000), None);
        assert_eq!(scan.entry(&gzip(&[b'c'; 2_000], None)), None);
        assert!(scan.document(1000).is_some());
        assert_eq!(scan.document(20_000), None);
    }
}
