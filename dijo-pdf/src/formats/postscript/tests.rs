use std::io::Cursor;

use super::*;
use crate::config::TesseractOcrConfig;

/// The XHTML extracted of a document without OCR, with its metadata.
fn extracted(input: &[u8]) -> (String, crate::Metadata) {
    crate::Extractor::new()
        .set_xml_output(true)
        .set_ocr_config(TesseractOcrConfig::new().set_skip_ocr(true))
        .extract_bytes_to_string(input)
        .unwrap()
}

/// An EPS file with a DOS binary header around `program` and a TIFF preview.
fn dos_eps(program: &[u8], tiff: &[u8]) -> Vec<u8> {
    let program_at = DOS_EPS_HEADER;
    let tiff_at = program_at + program.len();
    let mut file = DOS_EPS_MAGIC.to_vec();
    for field in [program_at, program.len(), 0, 0, tiff_at, tiff.len()] {
        file.extend_from_slice(&u32::try_from(field).unwrap().to_le_bytes());
    }
    file.extend_from_slice(&[0xFF, 0xFF]);
    file.extend_from_slice(program);
    file.extend_from_slice(tiff);
    file
}

fn tiff() -> Vec<u8> {
    let image = image::GrayImage::from_pixel(8, 8, image::Luma([255]));
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Tiff)
        .unwrap();
    bytes.into_inner()
}

const PROGRAM: &[u8] = b"%!PS-Adobe-3.0\n%%Title: (Keeper's log)\n%%Creator: hand\n\
    %%Pages: 2\n%%EndComments\n\
    /F {findfont exch scalefont setfont} bind def\n\
    %%Page: 1 1\n12 /Times-Roman F 72 720 moveto (Every evening) show showpage\n\
    %%Page: 2 2\n72 720 moveto (at dusk.) show showpage\n%%EOF\n";

#[test]
fn reads_postscript_page_by_page() {
    let (xml, metadata) = extracted(PROGRAM);
    assert_eq!(metadata["Content-Type"], ["application/postscript"]);
    assert_eq!(metadata["tk:parsed-by"], ["dijo::PostScriptParser"]);
    assert_eq!(metadata["dc:title"], ["Keeper's log"]);
    assert_eq!(metadata["xmpTPg:NPages"], ["2"]);
    assert!(
        xml.contains("<div class=\"page\"><p>Every evening</p>"),
        "{xml}"
    );
    assert!(xml.contains("<div class=\"page\"><p>at dusk.</p>"), "{xml}");
    assert_eq!(metadata.get(UNRESOLVED_TEXT), None);
}

#[test]
fn lists_the_pages_whose_text_is_not_resolved() {
    let program = b"%!PS\n/Courier findfont 10 scalefont setfont 10 10 moveto\n\
        (resolved) show 7 show showpage\n\
        /Garbled 9 dict begin /FontType 3 def /FontMatrix [1 0 0 1 0 0] def\n\
        /Encoding 256 array def 0 1 255 { Encoding exch /.notdef put } for\n\
        Encoding 65 /g1 put currentdict end definefont setfont\n\
        10 10 moveto (AAAAAAAAAAAAAAAAAAAA) show showpage\n";
    let (xml, metadata) = extracted(program);
    assert_eq!(
        metadata[UNRESOLVED_TEXT],
        ["1:computed-string", "2:no-text"]
    );
    assert!(xml.contains("<p>resolved</p>"), "{xml}");
}

#[test]
fn hands_on_the_preview_of_an_eps_without_resolved_text() {
    let preview = tiff();
    let unresolved = dos_eps(
        b"%!PS-Adobe-3.0 EPSF-3.0\n0 0 moveto 1 2 add show\n",
        &preview,
    );
    let (xml, metadata) = extracted(&unresolved);
    assert_eq!(metadata["Content-Type"], ["application/postscript"]);
    assert_eq!(metadata[UNRESOLVED_TEXT], ["1:computed-string"]);
    assert!(
        xml.contains("<div class=\"package-entry\"><h1>preview.tif</h1>"),
        "{xml}"
    );
    assert!(
        metadata["tk:parsed-by-full-set"]
            .iter()
            .any(|parser| parser == "dijo::TiffParser"),
        "{metadata:?}"
    );
    let resolved = dos_eps(
        b"%!PS-Adobe-3.0 EPSF-3.0\n/Helvetica 10 selectfont 0 0 moveto (Lamp) show\n",
        &preview,
    );
    let (xml, _) = extracted(&resolved);
    assert!(xml.contains("<p>Lamp</p>"), "{xml}");
    assert!(!xml.contains("preview.tif"), "{xml}");
}

#[test]
fn reads_the_sections_of_a_dos_binary_header() {
    let file = dos_eps(b"%!PS", b"II*\0");
    let document = Document::read(&file).unwrap();
    assert_eq!(document.program, b"%!PS");
    assert_eq!(
        document.preview,
        Some(Preview {
            format: PreviewFormat::Tiff,
            bytes: b"II*\0"
        })
    );
    let mut truncated = file.clone();
    truncated.truncate(file.len() - 1);
    let error = Document::read(&truncated).unwrap_err();
    assert!(
        error.contains("TIFF preview section at 34 of 4 bytes"),
        "{error}"
    );
    assert!(Document::read(&file[..10])
        .unwrap_err()
        .contains("ends inside its 30-byte DOS binary header"));
    assert_eq!(Document::read(b"%!PS").unwrap().preview, None);
}
