//! PDFs too large to read whole, extracted under a memory budget far below their size.

use std::fmt::Write as _;

use extractous_core::large::LargeFile;

use super::truetype::programs::of_glyphs;
use super::PdfParser;
use crate::formats::Parser;
use crate::input::MAX_READ_WHOLE;

/// A document of `count` pages, each of the same number of objects, its page dictionary first,
/// after the catalog, object 1, and the page tree, object 2.
struct Paged<'a> {
    count: u64,
    /// The bodies of the objects of the page `index`, numbered from `first`.
    page: &'a dyn Fn(u64, u64) -> Vec<Vec<u8>>,
    objects_per_page: u64,
}

impl Paged<'_> {
    fn first_object(&self, index: u64) -> u64 {
        3 + self.objects_per_page * index
    }

    /// The objects of the page `index`, and where each starts in them.
    fn page(&self, index: u64) -> (Vec<u8>, Vec<u64>) {
        let first = self.first_object(index);
        let mut out = Vec::new();
        let mut starts = Vec::new();
        for (number, body) in (first..).zip((self.page)(index, first)) {
            starts.push(out.len() as u64);
            out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            out.extend_from_slice(&body);
            out.extend_from_slice(b"\nendobj\n");
        }
        assert_eq!(starts.len() as u64, self.objects_per_page);
        (out, starts)
    }

    /// Writes the document, which must be too large to read whole.
    fn write(&self, name: &str) -> LargeFile {
        let mut kids = String::new();
        for index in 0..self.count {
            write!(kids, "{} 0 R ", self.first_object(index)).unwrap();
        }
        let mut head = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (number, body) in [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", self.count),
        ]
        .iter()
        .enumerate()
        {
            offsets.push(head.len() as u64);
            head.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", number + 1).as_bytes());
        }
        let mut at = head.len() as u64;
        for index in 0..self.count {
            let (page, starts) = self.page(index);
            offsets.extend(starts.iter().map(|start| at + start));
            at += page.len() as u64;
        }
        let mut tail = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1);
        for offset in &offsets {
            writeln!(tail, "{offset:010} 00000 n ").unwrap();
        }
        write!(
            tail,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n",
            offsets.len() + 1
        )
        .unwrap();
        LargeFile::new(
            name,
            &head,
            self.count,
            |index| self.page(index).0,
            tail.as_bytes(),
        )
    }
}

/// A stream object's body.
fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut body = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\nendstream");
    body
}

/// Pages each of a font of their own, embedding a program of thousands of glyphs: the fonts of
/// the pages long past are let go, so that the fonts held stay bounded however many pages
/// there are.
#[test]
fn extracts_documents_of_a_font_per_page_in_bounded_memory() {
    let program = of_glyphs(4000, 200_000);
    let count = MAX_READ_WHOLE / program.len() as u64 + 1;
    let file = Paged {
        count,
        objects_per_page: 5,
        page: &|index, first| {
            let content = format!("BT /F 12 Tf 72 720 Td (Page {index}) Tj ET");
            vec![
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] \
                     /Resources << /Font << /F {} 0 R >> >> /Contents {} 0 R >>",
                    first + 2,
                    first + 1
                )
                .into_bytes(),
                stream("", content.as_bytes()),
                format!(
                    "<< /Type /Font /Subtype /TrueType /BaseFont /Font{index} \
                     /Encoding /WinAnsiEncoding /FontDescriptor {} 0 R >>",
                    first + 3
                )
                .into_bytes(),
                format!(
                    "<< /Type /FontDescriptor /FontName /Font{index} /Flags 32 \
                     /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 800 /Descent -200 \
                     /CapHeight 700 /StemV 80 /FontFile2 {} 0 R >>",
                    first + 4
                )
                .into_bytes(),
                stream(&format!("/Length1 {}", program.len()), &program),
            ]
        },
    }
    .write("fonts.pdf");
    let (text, metadata) = file.extract(PdfParser.name());
    assert_eq!(metadata["xmpTPg:NPages"], [count.to_string()]);
    let pages: Vec<String> = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|words| words.join(" "))
        .collect();
    let expected: Vec<String> = (0..count).map(|index| format!("Page {index}")).collect();
    assert_eq!(pages, expected);
}
