//! The document's catalog and pages as PDFBox's `PDDocumentCatalog`, `PDPageTree` and `PDPage`
//! read them: pages in tree order with their inherited attributes, and their content streams
//! joined as PDFBox joins them.

use std::collections::HashSet;

use flate2::{Decompress, FlushDecompress, Status};
use lopdf::{Dictionary, Object};

use super::cos::{as_dict, Identity, Pdf};
use super::font::rectangle;
use super::Unported;

/// `PDRectangle.LETTER`, the media box PDFBox gives a page without one.
const LETTER: [f32; 4] = [0.0, 0.0, 612.0, 792.0];

/// A page of the document.
#[derive(Debug, Clone, Copy)]
pub(super) struct Page<'a> {
    pub(super) identity: Identity,
    pub(super) dict: &'a Dictionary,
}

impl Pdf {
    /// The document catalog, `/Root` of the trailer.
    pub(super) fn catalog(&self) -> Option<&Dictionary> {
        self.get(&self.doc.trailer, b"Root").and_then(as_dict)
    }

    /// The root of the page tree.
    fn page_tree_root(&self) -> Option<(Identity, &Dictionary)> {
        let catalog = self.catalog()?;
        let (identity, object) = self.get_identified(catalog, b"Pages")?;
        as_dict(object).map(|dict| (identity, dict))
    }

    /// `PDDocument.getNumberOfPages`: the `/Count` of the page tree root, as PDFBox reads it,
    /// which need not be the number of pages the tree holds.
    pub(super) fn number_of_pages(&self) -> Result<i32, Unported> {
        let (_, root) = self.page_tree_root().ok_or_else(|| {
            Unported("a document without a page tree, which PDFBox fails on".to_owned())
        })?;
        if self.name(root, b"Type") == Some(b"Page") {
            return Ok(1);
        }
        Ok(self.int(root, b"Count", 0))
    }

    /// `PDPageTree.iterator`: the pages, leaves of the tree in order, each visited once.
    pub(super) fn pages(&self) -> Result<Vec<Page<'_>>, Unported> {
        let (identity, root) = self.page_tree_root().ok_or_else(|| {
            Unported("a document without a page tree, which PDFBox fails on".to_owned())
        })?;
        let mut pages = Vec::new();
        if self.name(root, b"Type") == Some(b"Page") {
            pages.push(Page {
                identity,
                dict: root,
            });
            return Ok(pages);
        }
        let mut visited: HashSet<Identity> = HashSet::new();
        self.enqueue_kids(root, &mut visited, &mut pages)?;
        Ok(pages)
    }

    fn is_page_tree_node(&self, node: &Dictionary) -> bool {
        self.name(node, b"Type") == Some(b"Pages") || node.has(b"Kids")
    }

    fn enqueue_kids<'a>(
        &'a self,
        node: &'a Dictionary,
        visited: &mut HashSet<Identity>,
        pages: &mut Vec<Page<'a>>,
    ) -> Result<(), Unported> {
        if !self.is_page_tree_node(node) {
            if self.name(node, b"Type") == Some(b"Page") {
                return Err(Unported(
                    "a page tree node without kids that is a page".to_owned(),
                ));
            }
            return Ok(());
        }
        let Some(kids) = self.array(node, b"Kids") else {
            return Ok(());
        };
        for kid in kids {
            let Some((identity, object)) = self.resolve_identified(kid) else {
                return Err(Unported(
                    "a page tree with a missing kid, which PDFBox replaces by an empty page"
                        .to_owned(),
                ));
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
                self.enqueue_kids(dict, visited, pages)?;
            } else if self.name(dict, b"Type") == Some(b"Page") {
                pages.push(Page { identity, dict });
            }
        }
        Ok(())
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
                    return Ok(inflate_leniently(&stream.content));
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
/// damaged. Of the chunk the damage cuts short, PDFBox keeps what the inflater wrote up to its
/// last byte that is not zero.
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
        decoded.fill(0);
        let (read, written) = (inflater.total_in(), inflater.total_out());
        let inflated = inflater.decompress(input, &mut decoded, FlushDecompress::None);
        input = &input[(inflater.total_in() - read) as usize..];
        let written = (inflater.total_out() - written) as usize;
        match inflated {
            Ok(Status::StreamEnd) => {
                output.extend_from_slice(&decoded[..written]);
                return output;
            }
            Ok(Status::Ok | Status::BufError) => output.extend_from_slice(&decoded[..written]),
            Err(_) => {
                let kept = decoded
                    .iter()
                    .rposition(|&byte| byte != 0)
                    .map_or(0, |at| at + 1);
                output.extend_from_slice(&decoded[..kept]);
                return output;
            }
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
    fn inflates_as_pdfbox_does_unchecked_truncated_and_damaged_streams() {
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
        assert_eq!(inflate_leniently(&damaged), b"ab");
    }
}
