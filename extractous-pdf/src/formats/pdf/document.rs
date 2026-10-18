//! The document's catalog and pages as PDFBox's `PDDocumentCatalog`, `PDPageTree` and `PDPage`
//! read them: pages in tree order with their inherited attributes, and their content streams
//! joined as PDFBox joins them.

use std::collections::HashSet;
use std::sync::LazyLock;

use flate2::{Decompress, FlushDecompress, Status};
use lopdf::{Dictionary, Object};

use super::cos::{as_dict, Identity, Pdf};
use super::font::rectangle;
use super::Unported;

/// The empty page `PDPageTree.getKids` puts in place of a kid that reads as `null`.
static EMPTY_PAGE: LazyLock<Dictionary> = LazyLock::new(|| {
    let mut page = Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page
});

/// `PDRectangle.LETTER`, the media box PDFBox gives a page without one.
const LETTER: [f32; 4] = [0.0, 0.0, 612.0, 792.0];

/// A page of the document.
#[derive(Debug, Clone, Copy)]
pub(super) struct Page<'a> {
    pub(super) identity: Identity,
    pub(super) dict: &'a Dictionary,
}

impl Pdf<'_> {
    /// The document catalog, `/Root` of the trailer.
    pub(super) fn catalog(&self) -> Option<&Dictionary> {
        self.get(self.trailer(), b"Root").and_then(as_dict)
    }

    /// The document's catalog and page tree root, or the `IOException` `PDFParser.initialParse`
    /// throws for a catalog the trailer does not name, or a page tree root that is no
    /// dictionary. A document that loads has both.
    pub(super) fn check_catalog(&self) -> Result<(), &'static str> {
        let catalog = self
            .catalog()
            .ok_or("Missing root object specification in trailer.")?;
        self.dict(catalog, b"Pages")
            .ok_or("Page tree root must be a dictionary")?;
        Ok(())
    }

    /// The root of the page tree, which a document that loads has ([`Pdf::check_catalog`]).
    fn page_tree_root(&self) -> (Identity, &Dictionary) {
        self.catalog()
            .and_then(|catalog| self.get_identified(catalog, b"Pages"))
            .and_then(|(identity, object)| as_dict(object).map(|dict| (identity, dict)))
            .expect("a loaded document has a page tree root")
    }

    /// `PDDocument.getNumberOfPages`: the `/Count` of the page tree root, as PDFBox reads it,
    /// which need not be the number of pages the tree holds.
    pub(super) fn number_of_pages(&self) -> i32 {
        let (_, root) = self.page_tree_root();
        if self.name(root, b"Type") == Some(b"Page") {
            return 1;
        }
        self.int(root, b"Count", 0)
    }

    /// `PDPageTree.iterator`: the pages, leaves of the tree in order, each visited once.
    pub(super) fn pages(&self) -> Vec<Page<'_>> {
        let (identity, root) = self.page_tree_root();
        let mut pages = Vec::new();
        if self.name(root, b"Type") == Some(b"Page") {
            pages.push(Page {
                identity,
                dict: root,
            });
            return pages;
        }
        let mut visited: HashSet<Identity> = HashSet::new();
        self.enqueue_kids(root, self.trailer_rebuilt, &mut visited, &mut pages);
        pages
    }

    fn is_page_tree_node(&self, node: &Dictionary) -> bool {
        self.name(node, b"Type") == Some(b"Pages") || node.has(b"Kids")
    }

    /// `enqueueKids`, over `getKids`, which puts an empty page in place of a kid that reads as
    /// `null`, unless `checked`, as the kids of the page tree nodes `COSParser.checkPages`
    /// walks are where the trailer was rebuilt: it drops such a kid, and one held directly.
    fn enqueue_kids<'a>(
        &'a self,
        node: &'a Dictionary,
        checked: bool,
        visited: &mut HashSet<Identity>,
        pages: &mut Vec<Page<'a>>,
    ) {
        if !self.is_page_tree_node(node) {
            return;
        }
        let Some(kids) = self.array(node, b"Kids") else {
            return;
        };
        for kid in kids {
            if checked && !matches!(kid, Object::Reference(_)) {
                continue;
            }
            let Some((identity, object)) = self.resolve_identified(kid) else {
                if !checked {
                    pages.push(Page {
                        identity: Identity::of_direct(kid),
                        dict: &EMPTY_PAGE,
                    });
                }
                continue;
            };
            let Some(dict) = as_dict(object) else {
                continue;
            };
            if visited.contains(&identity) {
                continue;
            }
            if dict.has(b"Kids") {
                visited.insert(identity);
            }
            if self.is_page_tree_node(dict) {
                let checked = checked && self.name(dict, b"Type") == Some(b"Pages");
                self.enqueue_kids(dict, checked, visited, pages);
            } else if self.name(dict, b"Type") == Some(b"Page") {
                pages.push(Page { identity, dict });
            }
        }
    }

    /// `PDPageTree.getInheritableAttribute`.
    pub(super) fn inherited<'a>(&'a self, page: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
        let mut node = page;
        let mut seen: Vec<*const Dictionary> = Vec::new();
        loop {
            if seen.contains(&std::ptr::from_ref(node)) {
                return None;
            }
            seen.push(std::ptr::from_ref(node));
            if let Some(value) = self.get(node, key) {
                return Some(value);
            }
            let parent = self
                .dict(node, b"Parent")
                .or_else(|| self.dict(node, b"P"))?;
            if self.name(parent, b"Type") != Some(b"Pages") {
                return None;
            }
            node = parent;
        }
    }
}

impl<'a> Page<'a> {
    /// `PDPage.getMediaBox`.
    pub(super) fn media_box(&self, pdf: &Pdf) -> [f32; 4] {
        match pdf.inherited(self.dict, b"MediaBox") {
            Some(Object::Array(array)) => rectangle(pdf, array),
            _ => LETTER,
        }
    }

    /// `PDPage.getCropBox`: the crop box clipped to the media box, or the media box.
    pub(super) fn crop_box(&self, pdf: &Pdf) -> [f32; 4] {
        let media = self.media_box(pdf);
        match pdf.inherited(self.dict, b"CropBox") {
            Some(Object::Array(array)) => {
                let crop = rectangle(pdf, array);
                [
                    super::jfloat::max(media[0], crop[0]),
                    super::jfloat::max(media[1], crop[1]),
                    super::jfloat::min(media[2], crop[2]),
                    super::jfloat::min(media[3], crop[3]),
                ]
            }
            _ => media,
        }
    }

    /// `PDPage.getRotation`: a multiple of 90 in 0 to 270, else 0.
    pub(super) fn rotation(&self, pdf: &Pdf) -> i32 {
        match pdf
            .inherited(self.dict, b"Rotate")
            .and_then(super::cos::int_value)
        {
            Some(angle) if angle % 90 == 0 => (angle % 360 + 360) % 360,
            _ => 0,
        }
    }

    /// `PDPage.getResources`: the inherited resources, if they are a dictionary.
    pub(super) fn resources(&self, pdf: &'a Pdf) -> Option<&'a Dictionary> {
        pdf.inherited(self.dict, b"Resources").and_then(as_dict)
    }

    /// `PDPage.hasContents`.
    pub(super) fn has_contents(&self, pdf: &Pdf) -> bool {
        match pdf.get(self.dict, b"Contents") {
            Some(Object::Stream(stream)) => !stream.dict.is_empty(),
            Some(Object::Array(array)) => !array.is_empty(),
            _ => false,
        }
    }

    /// `PDPage.getContentsForStreamParsing`: the page's content, a stream compressed with
    /// Flate alone inflated as far as it inflates, an array of streams joined with line feeds.
    pub(super) fn contents(&self, pdf: &Pdf) -> Result<Vec<u8>, Unported> {
        match pdf.get(self.dict, b"Contents") {
            Some(Object::Stream(stream)) => {
                let flate_only = matches!(
                    pdf.get(&stream.dict, b"Filter"),
                    Some(Object::Name(name)) if name == b"FlateDecode"
                );
                if flate_only {
                    return Ok(inflate_leniently(&pdf.raw(stream)?));
                }
                pdf.decode(stream).map_err(|unported| {
                    Unported(format!("a page's content stream: {}", unported.0))
                })
            }
            Some(Object::Array(array)) => {
                let mut contents = Vec::new();
                for item in array {
                    if let Some(Object::Stream(stream)) = pdf.resolve(item) {
                        let data = pdf.decode(stream).map_err(|unported| {
                            Unported(format!("a page's content stream: {}", unported.0))
                        })?;
                        contents.extend_from_slice(&data);
                        contents.push(b'\n');
                    }
                }
                Ok(contents)
            }
            _ => Ok(Vec::new()),
        }
    }
}

/// How much of the raw stream `FlateFilterDecoderStream` hands its inflater at a time.
const RAW_CHUNK: usize = 2048;
/// How much `FlateFilterDecoderStream` inflates at a time.
const DECODED_CHUNK: usize = 4096;

/// `FlateFilterDecoderStream`, read to its end: the two bytes of the zlib header skipped and the
/// raw deflate data after them inflated, its checksum never read, until it ends, runs out or is
/// damaged, everything inflated before the damage kept. PDFBox drops the zero bytes the chunk
/// the damage cuts short ends in (#129).
fn inflate_leniently(data: &[u8]) -> Vec<u8> {
    let mut chunks = data.get(2..).unwrap_or_default().chunks(RAW_CHUNK);
    let mut inflater = Decompress::new(false);
    let mut input: &[u8] = &[];
    let mut decoded = [0u8; DECODED_CHUNK];
    let mut output = Vec::with_capacity(data.len().saturating_mul(4));
    loop {
        if input.is_empty() {
            match chunks.next() {
                Some(chunk) => input = chunk,
                None => return output,
            }
        }
        let (read, written) = (inflater.total_in(), inflater.total_out());
        let inflated = inflater.decompress(input, &mut decoded, FlushDecompress::None);
        input = &input[(inflater.total_in() - read) as usize..];
        let written = (inflater.total_out() - written) as usize;
        output.extend_from_slice(&decoded[..written]);
        if !matches!(inflated, Ok(Status::Ok | Status::BufError)) {
            return output;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    #[test]
    fn inflates_unchecked_truncated_and_damaged_streams_keeping_what_precedes_the_damage() {
        let text = b"BT /F1 12 Tf (Hello) Tj ET";
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(text).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(inflate_leniently(&compressed), text);
        let mut wrong_checksum = compressed.clone();
        let last = wrong_checksum.len() - 1;
        wrong_checksum[last] ^= 0xFF;
        assert_eq!(inflate_leniently(&wrong_checksum), text);
        let truncated = &compressed[..compressed.len() - 6];
        assert!(text.starts_with(&inflate_leniently(truncated)));
        let mut stored = vec![0x78, 0x9C, 0x01, 5, 0, !5, 0xFF];
        stored.extend(b"ab\0\0c");
        stored.push(0x07);
        assert_eq!(inflate_leniently(&stored), b"ab\0\0c");
        let damaged = [
            0x78, 0x9C, 0x00, 3, 0, 0xFC, 0xFF, b'a', b'b', 0, 0x00, 3, 0, 0, 0,
        ];
        assert_eq!(inflate_leniently(&damaged), b"ab\0");
    }
}
