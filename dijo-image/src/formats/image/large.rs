//! Images too large to read whole, extracted under a memory budget far below their size: their
//! metadata read in place, and with OCR a page at a time.

use dijo_core::large::{bodies_past_limit, LargeFile, BUDGET};

use crate::config::TesseractOcrConfig;
use crate::formats::Parser;
use crate::Extractor;

/// The scan every OCR test reads, as the page with text of a large file.
const SCAN: &str = "../test_files/documents/table-multi-row-column-cells.png";
/// A word of [`SCAN`] tesseract reads.
const SCANNED_WORD: &str = "Participants";

/// The pixels of [`SCAN`] in grey, and its width and height.
fn scan() -> (Vec<u8>, u32, u32) {
    let gray = image::open(SCAN).unwrap().to_luma8();
    let (width, height) = gray.dimensions();
    (gray.into_raw(), width, height)
}

/// An XMP packet naming the document's title, which Tika finds anywhere in a TIFF.
const XMP: &[u8] = b"<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Large pages</rdf:li></rdf:Alt></dc:title>\
</rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>";

/// An IFD entry of a little-endian TIFF.
fn entry(tag: u16, kind: u16, count: u32, value: u32) -> Vec<u8> {
    let mut entry = tag.to_le_bytes().to_vec();
    entry.extend(kind.to_le_bytes());
    entry.extend(count.to_le_bytes());
    entry.extend(value.to_le_bytes());
    entry
}

const SHORT: u16 = 3;
const LONG: u16 = 4;
/// The bytes of a page's IFD, padded to a word.
const IFD: u32 = 2 + 9 * 12 + 4 + 2;
/// The size of the header of a TIFF.
const HEADER: u32 = 8;

/// A page of a TIFF of `pages` grey pages of `width` by `height` pixels, each its IFD then its
/// pixels, the page `index` at its place in the file.
fn tiff_page(index: u32, pages: u32, width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let size = IFD + width * height;
    let at = HEADER + index * size;
    let next = if index + 1 < pages { at + size } else { 0 };
    let mut page = 9u16.to_le_bytes().to_vec();
    for entry in [
        entry(256, LONG, 1, width),
        entry(257, LONG, 1, height),
        entry(258, SHORT, 1, 8),
        entry(259, SHORT, 1, 1),
        entry(262, SHORT, 1, 1),
        entry(273, LONG, 1, at + IFD),
        entry(278, LONG, 1, height),
        entry(279, LONG, 1, width * height),
        entry(297, SHORT, 2, index | (pages << 16)),
    ] {
        page.extend(entry);
    }
    page.extend(next.to_le_bytes());
    page.extend([0, 0]);
    page.extend(pixels);
    page
}

/// A TIFF too large to read whole, of pages as large as [`SCAN`]: the scan first and last, and
/// white between, with an XMP packet after its last page.
fn large_tiff() -> (LargeFile, u32) {
    let (scan, width, height) = scan();
    let white = vec![u8::MAX; scan.len()];
    let pages = u32::try_from(bodies_past_limit((IFD + width * height) as usize)).unwrap();
    let mut head = b"II*\0".to_vec();
    head.extend(HEADER.to_le_bytes());
    let file = LargeFile::new(
        "pages.tiff",
        &head,
        u64::from(pages),
        |index| {
            let index = u32::try_from(index).unwrap();
            let pixels = if index == 0 || index + 1 == pages {
                &scan
            } else {
                &white
            };
            tiff_page(index, pages, width, height, pixels)
        },
        XMP,
    );
    (file, pages)
}

/// A TIFF too large to read whole has its XMP found after its last page and its directories
/// read in place, and with OCR each page decoded, read and freed in turn, all under a budget
/// far below its size.
#[test]
fn extracts_tiffs_too_large_to_read_whole_in_bounded_memory() {
    let (file, pages) = large_tiff();
    let skipping = Extractor::new().set_ocr_config(TesseractOcrConfig::new().set_skip_ocr(true));
    let (text, metadata) = file.extract_with(&skipping, BUDGET, "dijo::TiffParser");
    assert_eq!(text.trim(), "");
    assert_eq!(metadata["dc:title"], ["Large pages"]);
    assert_eq!(metadata["exif:PageCount"], [pages.to_string()]);
    let (text, metadata) =
        file.extract_with(&Extractor::new(), BUDGET, super::PREPARING_OCR_PARSER);
    assert_eq!(text.matches(SCANNED_WORD).count(), 2, "{text}");
    assert_eq!(metadata[crate::ocr::REPORT].len(), pages as usize);
    assert!(!metadata.contains_key("tk:exception:warn"), "{metadata:?}");
}

/// The bytes of zeros padding a file of each format past the limit, a body at a time.
const PADDING: usize = 1 << 20;

/// The bytes of padding that bodies of `length` bytes, each holding [`PADDING`] zeros, make.
fn padding_of(length: usize) -> u32 {
    u32::try_from(bodies_past_limit(length) * PADDING as u64).unwrap()
}

/// `head`, then `body` as many times as makes a file too large to read whole, then `tail`.
fn padded(name: &str, head: &[u8], body: &[u8], tail: &[u8]) -> LargeFile {
    LargeFile::new(
        name,
        head,
        bodies_past_limit(body.len()),
        |_| body.to_vec(),
        tail,
    )
}

/// `head`, then zeros past the limit, then `tail`.
fn zero_padded(name: &str, head: &[u8], tail: &[u8]) -> LargeFile {
    padded(name, head, &[0; PADDING], tail)
}

/// The metadata of `file`, extracted with OCR skipped under [`BUDGET`] by `parser`.
fn metadata_of(file: &LargeFile, parser: &str) -> crate::Metadata {
    let skipping = Extractor::new().set_ocr_config(TesseractOcrConfig::new().set_skip_ocr(true));
    file.extract_with(&skipping, BUDGET, parser).1
}

/// A JPEG of 64 by 48 grey pixels whose scan is too large to read whole has its segments read
/// up to the scan, in place.
#[test]
fn extracts_jpegs_too_large_to_read_whole_in_bounded_memory() {
    let mut head = vec![
        0xFF, 0xD8, 0xFF, 0xC0, 0, 11, 8, 0, 48, 0, 64, 1, 1, 0x11, 0,
    ];
    head.extend([0xFF, 0xDA, 0, 8, 1, 1, 0, 0, 0x3F, 0]);
    let file = zero_padded("scan.jpg", &head, &[0xFF, 0xD9]);
    let metadata = metadata_of(&file, "dijo::JpegParser");
    assert_eq!(metadata["tiff:ImageWidth"], ["64"]);
    assert_eq!(metadata["tiff:ImageLength"], ["48"]);
}

/// A PNG chunk of `kind` around `data`, its CRC left zero, which the metadata reader does not
/// check.
fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    chunk.extend(kind);
    chunk.extend(data);
    chunk.extend([0; 4]);
    chunk
}

/// A PNG whose image data is too large to read whole has its chunks walked in place, its data
/// skipped.
#[test]
fn extracts_pngs_too_large_to_read_whole_in_bounded_memory() {
    let mut header = 64u32.to_be_bytes().to_vec();
    header.extend(48u32.to_be_bytes());
    header.extend([8, 0, 0, 0, 0]);
    let head = [&b"\x89PNG\r\n\x1a\n"[..], &png_chunk(b"IHDR", &header)].concat();
    let file = padded(
        "image.png",
        &head,
        &png_chunk(b"IDAT", &[0; PADDING]),
        &png_chunk(b"IEND", &[]),
    );
    let metadata = metadata_of(&file, "dijo::ImageParser");
    assert_eq!(metadata["tiff:ImageWidth"], ["64"]);
    assert_eq!(metadata["tiff:ImageLength"], ["48"]);
}

/// A Photoshop image whose layers are too large to read whole has its header and resources
/// read, its layers skipped.
#[test]
fn extracts_photoshop_images_too_large_to_read_whole_in_bounded_memory() {
    let mut head = b"8BPS".to_vec();
    head.extend(1u16.to_be_bytes());
    head.extend([0; 6]);
    head.extend(1u16.to_be_bytes());
    head.extend(48u32.to_be_bytes());
    head.extend(64u32.to_be_bytes());
    head.extend(8u16.to_be_bytes());
    head.extend(1u16.to_be_bytes());
    head.extend(0u32.to_be_bytes());
    head.extend(0u32.to_be_bytes());
    head.extend(padding_of(PADDING).to_be_bytes());
    let image = [&0u16.to_be_bytes()[..], &[0; 64 * 48]].concat();
    let file = zero_padded("layers.psd", &head, &image);
    let metadata = metadata_of(&file, "dijo::RecognizedImageParser");
    assert_eq!(metadata["tiff:ImageWidth"], ["64"]);
    assert_eq!(metadata["tiff:ImageLength"], ["48"]);
}

/// A BPG image whose picture is too large to read whole has its header read alone.
#[test]
fn extracts_bpg_images_too_large_to_read_whole_in_bounded_memory() {
    let head = [&b"BPG\xfb"[..], &[0x22, 0x10, 0x81, 0x00, 0x40, 0x05]].concat();
    let file = zero_padded("picture.bpg", &head, &[]);
    let metadata = metadata_of(&file, "dijo::BPGParser");
    assert_eq!(metadata["tiff:ImageWidth"], ["128"]);
    assert_eq!(metadata["tiff:ImageLength"], ["64"]);
}

/// An Apple icon file too large to read whole, its icons followed by padding, has the length
/// its header declares read alone: Tika refuses one that declares more than 10 MiB.
#[test]
fn extracts_apple_icons_too_large_to_read_whole_in_bounded_memory() {
    let icon = [&b"ic07"[..], &16u32.to_be_bytes(), &[0; 8]].concat();
    let head = [&b"icns"[..], &(8 + icon.len() as u32).to_be_bytes(), &icon].concat();
    let file = zero_padded("icon.icns", &head, &[]);
    let metadata = metadata_of(&file, super::IcnsParser.name());
    assert_eq!(metadata["Icon count"], ["1"]);
}

/// A HEIF image followed by a box too large to read whole has its boxes walked in place.
#[test]
fn extracts_heif_images_too_large_to_read_whole_in_bounded_memory() {
    let mut head = super::tests::heic(true);
    head.extend((padding_of(PADDING) + 8).to_be_bytes());
    head.extend(b"free");
    let file = zero_padded("photo.heic", &head, &[]);
    let metadata = metadata_of(&file, "dijo::HeifParser");
    assert_eq!(metadata["img:Width"], ["512 pixels"]);
}

/// The JPEG XL test card, its codestream split over `jxlp` boxes, with a box of a type no reader
/// knows, too large to read whole, after its first `jxlp` box: before the rest of its
/// codestream and its Brotli-compressed Exif and XMP boxes.
fn large_jpeg_xl() -> LargeFile {
    let card = std::fs::read(super::tests::jxl_fixture("card.jxl")).unwrap();
    let first_jxlp = 32;
    assert_eq!(&card[first_jxlp + 4..first_jxlp + 8], b"jxlp");
    let size = u32::from_be_bytes(card[first_jxlp..first_jxlp + 4].try_into().unwrap());
    let rest = first_jxlp + size as usize;
    let mut head = card[..rest].to_vec();
    head.extend((padding_of(PADDING) + 8).to_be_bytes());
    head.extend(b"zzzz");
    zero_padded("card.jxl", &head, &card[rest..])
}

/// A JPEG XL image with a box too large to read whole has its boxes walked in place and its
/// codestream's header parsed from its first bytes, and with OCR its codestream alone decoded.
#[test]
fn extracts_jpeg_xl_images_too_large_to_read_whole_in_bounded_memory() {
    let file = large_jpeg_xl();
    let metadata = metadata_of(&file, "dijo::JXLParser");
    for (name, expected) in [
        ("tiff:ImageWidth", "96"),
        ("tiff:ImageLength", "80"),
        ("tiff:BitsPerSample", "8"),
        ("tiff:Orientation", "6"),
        ("tiff:Make", "Extractous"),
        ("dc:title", "Fyret"),
    ] {
        assert_eq!(metadata[name], [expected], "{name}: {metadata:?}");
    }
    assert!(!metadata.contains_key("tk:exception:warn"), "{metadata:?}");
    let (_, metadata) = file.extract_with(&Extractor::new(), BUDGET, super::PREPARING_OCR_PARSER);
    let [report] = metadata[crate::ocr::REPORT].as_slice() else {
        panic!("one reading: {metadata:?}");
    };
    assert!(report.contains("\"prepared\":true"), "{report}");
    assert!(!metadata.contains_key("tk:exception:warn"), "{metadata:?}");
}

/// A raw camera image too large to read whole has its IFDs walked for previews in place.
#[test]
fn extracts_raw_images_too_large_to_read_whole_in_bounded_memory() {
    let mut head = b"II*\0".to_vec();
    head.extend(8u32.to_le_bytes());
    head.extend(1u16.to_le_bytes());
    head.extend(entry(256, LONG, 1, 64));
    head.extend(0u32.to_le_bytes());
    let file = zero_padded("photo.nef", &head, &[]);
    let metadata = metadata_of(&file, super::RawTiffParser.name());
    assert_eq!(metadata["Content-Type"], ["image/x-raw-nikon"]);
}
