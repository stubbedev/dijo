use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::super::tests::{as_paths, document, page_objects, tika_name, CORPORA};
use super::super::{PdfParser, DELEGATES};
use crate::config::{ParserConfigs, PdfOcrStrategy, PdfParserConfig, TesseractOcrConfig};
use crate::formats::metadata::TikaMetadata;
use crate::formats::parity::{
    assert_fails_as_tika_with, assert_same_as_tika_delegating_with, conformance_corpus,
    documents_with, spools_as_tika_departing_with, Departure, Departures, Scratch,
};
use crate::formats::Parser;

/// The JDK's own sRGB profile, whose header names no device model.
const JDK_SRGB: &[u8] = include_bytes!("../../../../models/jdk-srgb.icc");

/// Images extracted, pages never read by OCR, and the images read by OCR as Tika reads them.
fn without_page_ocr(unique_only: bool) -> ParserConfigs {
    ParserConfigs {
        pdf: PdfParserConfig::new()
            .set_extract_inline_images(true)
            .set_extract_unique_inline_images_only(unique_only)
            .set_ocr_strategy(PdfOcrStrategy::NO_OCR),
        ..ParserConfigs::default()
    }
}

/// Images extracted, the pages weighed by our `TextLayerCheckingPdfParser` and nothing read by
/// OCR.
fn skipping_ocr() -> ParserConfigs {
    ParserConfigs {
        pdf: PdfParserConfig::new().set_extract_inline_images(true),
        ocr: TesseractOcrConfig::new().set_skip_ocr(true),
        ..ParserConfigs::default()
    }
}

fn deferral(input: &[u8], configs: &ParserConfigs) -> Option<String> {
    PdfParser.defers(input, &TikaMetadata::new(), configs)
}

/// The images the native parser writes as PNG otherwise than the JDK's writer, which leaves
/// every row unfiltered (#131): their spooled files are compared by the images they hold.
const PNG_WRITER: Departures = &[Departure::PngImages("#131")];

/// Compares the native parser with Tika's path on `paths`, and their spools of what the
/// documents embed, but for [`PNG_WRITER`].
fn assert_same(paths: &[PathBuf], configs: &ParserConfigs) {
    assert_same_as_tika_delegating_with(&as_paths(paths), tika_name(), &DELEGATES, configs);
    for path in paths {
        let spooled = spools_as_tika_departing_with(path, u64::MAX, PNG_WRITER, configs);
        assert!(spooled.is_ok(), "{}", spooled.unwrap_err());
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// A stream of `data`, hex-encoded ahead of `filters`.
fn stream(dict: &str, filters: &str, data: &[u8]) -> String {
    let encoded = format!("{}>", hex(data));
    format!(
        "<< {dict} /Filter [/ASCIIHexDecode {filters}] /Length {} >>\nstream\n{encoded}\nendstream",
        encoded.len()
    )
}

/// An image XObject.
fn image(dict: &str, filters: &str, data: &[u8]) -> String {
    stream(
        &format!("/Type /XObject /Subtype /Image {dict}"),
        filters,
        data,
    )
}

/// A one-page document of Helvetica text and the objects `extra`, numbered from 6 on, which
/// the page's resources `resources` name, drawn by `content`.
fn drawing(content: &str, resources: &str, extra: &[String]) -> Vec<u8> {
    let content = format!("BT /F1 12 Tf 72 720 Td (Billeder p\\345 siden.) Tj ET {content}");
    let mut objects = page_objects(&content);
    objects[2] = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> {resources} >> >>"
    );
    objects.extend(extra.iter().cloned());
    let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
    document(&objects, b"", &[])
}

/// The XObject resources naming objects 6 on `/Im0`, `/Im1` and so on.
fn xobjects(count: usize) -> String {
    let names: String = (0..count)
        .map(|index| format!("/Im{index} {} 0 R ", index + 6))
        .collect();
    format!("/XObject << {names}>>")
}

/// Draws each of `count` images, and the first again.
fn draw_each(count: usize) -> String {
    let mut content: String = (0..count)
        .map(|index| format!("q 20 0 0 20 {} 600 cm /Im{index} Do Q ", 30 * index))
        .collect();
    content.push_str("q 20 0 0 20 0 500 cm /Im0 Do Q");
    content
}

/// Samples of `count` bytes that vary.
fn samples(count: usize, seed: u8) -> Vec<u8> {
    (0..count)
        .map(|index| (index as u8).wrapping_mul(37).wrapping_add(seed))
        .collect()
}

/// The JPEG data of an image of the 10-Q in the documents.
fn jpeg() -> Vec<u8> {
    let input = std::fs::read("../test_files/documents/2022_Q3_AAPL.pdf").unwrap();
    let doc = lopdf::Document::load_mem(&input).unwrap();
    doc.objects
        .values()
        .find_map(|object| match object {
            lopdf::Object::Stream(stream)
                if stream.dict.get(b"Filter").ok()
                    == Some(&lopdf::Object::Name(b"DCTDecode".to_vec())) =>
            {
                Some(stream.content.clone())
            }
            _ => None,
        })
        .unwrap()
}

/// An ICC colour space of the JDK's sRGB profile, named an sRGB device's when `srgb`.
fn icc(srgb: bool) -> String {
    let mut profile = JDK_SRGB.to_vec();
    if srgb {
        profile[52..56].copy_from_slice(b"sRGB");
    }
    stream("/N 3", "", &profile)
}

/// Images in each colour space and sample depth the port reads, and with each kind of mask.
fn sampled_images() -> Vec<u8> {
    let lookup = hex(&[255, 0, 0, 0, 128, 255, 20, 200, 30, 250, 250, 250]);
    let rgb = |seed, extra: &str| {
        image(
            &format!("/Width 3 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 {extra}"),
            "",
            &samples(18, seed),
        )
    };
    let gray = |seed, extra: &str| {
        image(
            &format!("/Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 {extra}"),
            "",
            &samples(6, seed),
        )
    };
    let images = vec![
        image(
            "/Width 4 /Height 3 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "/FlateDecode",
            &zlib(&samples(12, 1)),
        ),
        rgb(2, ""),
        image(
            "/Width 9 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0]",
            "",
            &[0b1010_0110, 0b1000_0000, 0b0110_1111, 0b0000_0000],
        ),
        image(
            &format!(
                "/Width 5 /Height 2 /ColorSpace [/Indexed /DeviceRGB 3 <{lookup}>] \
                 /BitsPerComponent 2"
            ),
            "",
            &[0b0001_1011, 0b1100_0000, 0b1110_0100, 0b0100_0000],
        ),
        image(
            "/Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 16",
            "/FlateDecode",
            &zlib(&samples(24, 3)),
        ),
        rgb(4, "/SMask 22 0 R"),
        rgb(5, "/Mask 23 0 R"),
        rgb(6, "/Mask [0 120 0 255 0 255]"),
        image(
            "/Width 8 /Height 2 /ImageMask true",
            "",
            &[0b1100_1010, 0b0101_0011],
        ),
        gray(7, "/Decode [1 0]"),
        image(
            &format!(
                "/Width 3 /Height 2 /ColorSpace [/Indexed /DeviceGray 1 <{}>] \
                 /BitsPerComponent 8",
                hex(&[30, 220])
            ),
            "",
            &[0, 1, 1, 0, 5, 1],
        ),
        image(
            "/Width 3 /Height 2 /ColorSpace [/ICCBased 24 0 R] /BitsPerComponent 8",
            "",
            &samples(18, 8),
        ),
        image(
            "/Width 3 /Height 2 /ColorSpace 25 0 R /BitsPerComponent 8",
            "",
            &samples(18, 9),
        ),
        image(
            "/Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 4",
            "/FlateDecode",
            &zlib(&[0x12, 0x30, 0x45, 0x60]),
        ),
        image(
            "/Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "",
            &samples(3, 10),
        ),
        rgb(11, "/SMask 27 0 R"),
        gray(12, ""),
        image(
            "/Width 3 /Height 2 /ImageMask true /Decode [1 0]",
            "",
            &[0b1010_0000, 0b0100_0000],
        ),
        icc(false),
        "[/ICCBased 26 0 R]".to_owned(),
        icc(true),
        gray(13, "/Matte [0.25 0.5 0.75]"),
    ];
    let content = format!(
        "{} q BI /W 2 /H 2 /CS /G /BPC 8 /F /AHx ID 00ff7f80> EI Q \
         q BI /W 3 /H 1 /CS /RGB /BPC 8 ID abcdefabcdefabc\nEI Q \
         q BI /W 2 /H 1 /CS [/I /RGB 1 <ff000000ff00>] /BPC 1 ID \u{80}\nEI Q \
         q BI /W 8 /H 1 /IM true ID \u{55}\nEI Q",
        draw_each(16)
    );
    drawing(&content, &xobjects(16), &images)
}

#[test]
fn extracts_images_of_every_colour_space_and_mask_as_tikas_path_does() {
    let scratch = Scratch::new("pdf-images");
    let path = scratch.write("sampled.pdf", &sampled_images());
    for configs in [
        without_page_ocr(false),
        without_page_ocr(true),
        skipping_ocr(),
    ] {
        let input = std::fs::read(&path).unwrap();
        assert_eq!(deferral(&input, &configs), None, "{configs:?}");
        assert_same(std::slice::from_ref(&path), &configs);
    }
}

/// The names of the images the XHTML `xml` shows, in order.
fn image_names(xml: &str) -> Vec<&str> {
    xml.split("<img src=\"embedded:")
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect()
}

/// An image XObject drawn again keeps its own number, where Tika numbers it 0 (#131), unless
/// only unique images are extracted.
#[test]
fn numbers_an_image_drawn_again_by_its_own_number_unless_only_unique_ones_are_extracted() {
    let images: Vec<String> = (0..3)
        .map(|seed| {
            image(
                "/Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
                "",
                &samples(4, seed),
            )
        })
        .collect();
    let content = "q 20 0 0 20 0 600 cm /Im0 Do Q q 20 0 0 20 30 600 cm /Im1 Do Q \
                   q 20 0 0 20 60 600 cm /Im2 Do Q q 20 0 0 20 0 500 cm /Im1 Do Q";
    let input = drawing(content, &xobjects(3), &images);
    let (xml, _) = extracting(&input, &without_page_ocr(false));
    assert_eq!(
        image_names(&xml),
        ["image-0.png", "image-1.png", "image-2.png", "image-1.png"]
    );
    let (xml, _) = extracting(&input, &without_page_ocr(true));
    assert_eq!(
        image_names(&xml),
        ["image-0.png", "image-1.png", "image-2.png"]
    );
}

fn extracting(input: &[u8], configs: &ParserConfigs) -> (String, crate::Metadata) {
    let (pdf, ocr) = (configs.pdf.clone(), configs.ocr.clone());
    crate::Extractor::new()
        .set_xml_output(true)
        .set_pdf_config(pdf)
        .set_ocr_config(ocr)
        .extract_bytes_to_string(input)
        .unwrap()
}

/// Images whose data is copied as it is, and those Java cannot read.
fn copied_images() -> Vec<u8> {
    let jpeg = jpeg();
    let images = vec![
        image(
            "/Width 46 /Height 56 /ColorSpace /DeviceRGB /BitsPerComponent 8",
            "/DCTDecode",
            &jpeg,
        ),
        image(
            "/Width 46 /Height 56 /ColorSpace /DeviceRGB /BitsPerComponent 8",
            "/JPXDecode",
            &jpeg,
        ),
        image(
            "/Width 46 /Height 56 /ColorSpace [/ICCBased 11 0 R] /BitsPerComponent 8",
            "/JPXDecode",
            &jpeg,
        ),
        image(
            "/Width 46 /Height 56 /ColorSpace /DeviceGray /BitsPerComponent 1",
            "/JBIG2Decode",
            &jpeg[..64],
        ),
        image(
            "/Width 46 /Height 56 /BitsPerComponent 8",
            "/DCTDecode",
            &jpeg,
        ),
        icc(false),
    ];
    drawing(&draw_each(5), &xobjects(5), &images)
}

#[test]
fn copies_jpeg_jpeg_2000_and_jbig2_data_as_tikas_path_does() {
    let scratch = Scratch::new("pdf-image-copies");
    let path = scratch.write("copies.pdf", &copied_images());
    let configs = without_page_ocr(false);
    assert_eq!(deferral(&copied_images(), &configs), None);
    assert_same(&[path], &configs);
}

/// Images in forms, transparency groups, patterns and soft masks.
fn painted_images() -> Vec<u8> {
    let gray = |seed| {
        image(
            "/Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "",
            &samples(4, seed),
        )
    };
    let form = |dict: &str, content: &str| {
        stream(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 10 10] {dict}"),
            "",
            content.as_bytes(),
        )
    };
    let pattern = |paint: u8, content: &str| {
        stream(
            &format!(
                "/PatternType 1 /PaintType {paint} /TilingType 1 /BBox [0 0 10 10] /XStep 10 \
                 /YStep 10 /Resources << /XObject << /A 12 0 R /S 13 0 R >> >>"
            ),
            "",
            content.as_bytes(),
        )
    };
    let objects = vec![
        form("/Resources << /XObject << /A 12 0 R >> >>", "/A Do"),
        form(
            "/Group << /S /Transparency >> /Resources << /XObject << /A 14 0 R /F 6 0 R >> >>",
            "/A Do /F Do",
        ),
        pattern(1, "/A Do"),
        pattern(2, "/A Do /S Do"),
        form(
            "/Group << /S /Transparency >> /Resources << /XObject << /A 15 0 R >> >>",
            "/A Do",
        ),
        "<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 10 0 R >> >>".to_owned(),
        gray(1),
        image("/Width 8 /Height 1 /ImageMask true", "", &[0b1011_0001]),
        gray(2),
        gray(3),
    ];
    let resources = "/XObject << /Fm0 6 0 R /Fm1 7 0 R >> \
                     /Pattern << /P0 8 0 R /P1 9 0 R >> /ExtGState << /G0 11 0 R >> \
                     /ColorSpace << /CS0 [/Pattern /DeviceGray] >>";
    let content = "/Fm0 Do /Fm1 Do /Pattern cs /P0 scn 0 0 10 10 re f \
                   /CS0 cs 0.5 /P1 scn 0 0 10 10 re f /P0 scn 0 0 5 5 re f* \
                   /Pattern CS /P0 SCN 0 0 10 10 re S /Fm0 Do";
    drawing(content, resources, &objects)
}

#[test]
fn finds_images_in_forms_patterns_and_soft_masks_as_tikas_path_does() {
    let scratch = Scratch::new("pdf-image-painted");
    let path = scratch.write("painted.pdf", &painted_images());
    for configs in [without_page_ocr(false), without_page_ocr(true)] {
        assert_eq!(deferral(&painted_images(), &configs), None);
        assert_same(std::slice::from_ref(&path), &configs);
    }
}

#[test]
fn extracts_images_under_the_ocr_strategies_as_tikas_path_does() {
    if !crate::ocr::tesseract_runs() {
        return;
    }
    let scratch = Scratch::new("pdf-image-strategies");
    let path = scratch.write("painted.pdf", &painted_images());
    for strategy in [
        PdfOcrStrategy::OCR_ONLY,
        PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION,
    ] {
        let configs = ParserConfigs {
            pdf: PdfParserConfig::new()
                .set_extract_inline_images(true)
                .set_ocr_strategy(strategy),
            ..ParserConfigs::default()
        };
        assert_eq!(deferral(&painted_images(), &configs), None, "{strategy}");
        assert_same(std::slice::from_ref(&path), &configs);
    }
}

#[test]
fn records_failures_to_read_images_as_tikas_path_does() {
    let images = vec![
        image("/Width 2 /Height 2 /BitsPerComponent 8", "", &[1, 2, 3, 4]),
        image(
            "/Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 4",
            "",
            &[1],
        ),
        image(
            "/Width 0 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "",
            &[1, 2],
        ),
        image(
            "/Width 2 /Height 2 /ColorSpace /Missing /BitsPerComponent 8",
            "",
            &[1, 2, 3, 4],
        ),
        stream(
            "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray \
             /BitsPerComponent 8",
            "",
            &[],
        ),
    ];
    let scratch = Scratch::new("pdf-image-failures");
    let path = scratch.write(
        "failures.pdf",
        &drawing(&draw_each(5), &xobjects(5), &images),
    );
    let configs = without_page_ocr(false);
    assert_eq!(deferral(&std::fs::read(&path).unwrap(), &configs), None);
    assert_same(&[path], &configs);
}

#[test]
fn defers_images_the_port_does_not_write() {
    let jpeg = jpeg();
    let cases = [
        (
            image(
                "/Width 2 /Height 2 /ColorSpace /DeviceCMYK /BitsPerComponent 8",
                "",
                &samples(16, 1),
            ),
            "an image in the colour space DeviceCMYK",
        ),
        (
            image(
                "/Width 46 /Height 56 /ColorSpace /DeviceCMYK /BitsPerComponent 8",
                "/DCTDecode",
                &jpeg,
            ),
            "an image in the colour space DeviceCMYK",
        ),
        (
            image(
                "/Width 8 /Height 1 /ImageMask true",
                "/CCITTFaxDecode",
                &[0],
            ),
            "a CCITT image, which Java writes",
        ),
        (
            image(
                "/Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 7 0 R",
                "",
                &[1, 2, 3, 4],
            ),
            "a mask of other dimensions",
        ),
        (
            image(
                "/Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 7 0 R",
                "/DCTDecode",
                &jpeg,
            ),
            "an image of the filter DCTDecode",
        ),
    ];
    let mask = image(
        "/Width 3 /Height 3 /ColorSpace /DeviceGray /BitsPerComponent 8",
        "",
        &samples(9, 1),
    );
    for (image, reason) in cases {
        let input = drawing(&draw_each(1), &xobjects(1), &[image, mask.clone()]);
        let deferred = deferral(&input, &without_page_ocr(false)).unwrap_or_default();
        assert!(deferred.starts_with(reason), "{deferred}");
    }
    let inline = drawing("BI /W 1 /H 1 /CS /G /BPC 8 /F /DCT ID \u{ff}\nEI", "", &[]);
    let deferred = deferral(&inline, &without_page_ocr(false)).unwrap_or_default();
    assert!(
        deferred.starts_with("an inline image of the filter DCTDecode"),
        "{deferred}"
    );
    let text = drawing(
        "/Pattern cs /P0 scn BT /F1 12 Tf (Tekst) Tj ET",
        "/Pattern << /P0 6 0 R >> /XObject << /Im0 7 0 R >>",
        &[
            stream(
                "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 \
                 /YStep 10 /Resources << /XObject << /Im0 7 0 R >> >>",
                "",
                b"/Im0 Do",
            ),
            mask,
        ],
    );
    let deferred = deferral(&text, &without_page_ocr(false)).unwrap_or_default();
    assert!(
        deferred.starts_with("text painted with a tiling pattern"),
        "{deferred}"
    );
}

#[test]
fn defers_images_whose_ocr_text_our_page_check_weighs() {
    if !crate::ocr::tesseract_runs() {
        return;
    }
    let configs = ParserConfigs {
        pdf: PdfParserConfig::new().set_extract_inline_images(true),
        ..ParserConfigs::default()
    };
    let deferred = deferral(&sampled_images(), &configs).unwrap_or_default();
    assert!(
        deferred.starts_with("an image our TextLayerCheckingPdfParser"),
        "{deferred}"
    );
}

#[test]
fn extracts_the_images_of_the_corpora_as_tikas_path_does() {
    let configs = without_page_ocr(false);
    let roots = CORPORA.map(Path::new);
    let (parsed, deferred) = documents_with(&PdfParser, &roots, &configs);
    assert_eq!(deferred, Vec::<PathBuf>::new());
    // A document only its password opens fails on both paths, as the PDF tests compare.
    let parsed: Vec<PathBuf> = parsed
        .into_iter()
        .filter(|path| !path.ends_with("user-password.pdf"))
        .collect();
    assert_same(&parsed, &configs);
    let weighed = PathBuf::from("../test_files/documents/2022_Q3_AAPL.pdf");
    assert_same(&[weighed], &skipping_ocr());
}

/// The documents of Tika's tests Tika's path does not extract as Java would with images
/// extracted, and why.
const TIKA_PATH_FAILS: [(&str, &str); 1] = [(
    "testPDF_acroform3.pdf",
    "the JDK rejects an ICC profile of a content stream's colour space, and the class \
     java.awt.color.CMMException that PDFBox catches for it is missing from the native image",
)];

/// The documents of Tika's tests that the port leaves to Tika with images extracted, by the
/// start of the reason, the OCR of pages skipped.
const DEFERRED: [(&str, &str); 4] = [
    ("testPDFVarious.pdf", "a mask of other dimensions"),
    ("testPDF_childAttachments.pdf", "a CCITT image"),
    (
        "testPDF_incrementalUpdates.pdf",
        "a JPEG image in the colour space ICCBased",
    ),
    (
        "testPDF_malformedXFA.pdf",
        "an XFA form whose XML does not parse",
    ),
];

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_the_images_of_the_conformance_documents_as_tikas_path_does() {
    for configs in [without_page_ocr(false), without_page_ocr(true)] {
        let (parsed, deferred) = documents_with(&PdfParser, &[&conformance_corpus()], &configs);
        let mut found: Vec<(String, String)> = deferred
            .iter()
            .map(|path| {
                let input = std::fs::read(path).unwrap();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, deferral(&input, &configs).unwrap())
            })
            .collect();
        found.sort();
        assert_eq!(found.len(), DEFERRED.len(), "{found:?}");
        for ((name, reason), (expected, start)) in found.iter().zip(DEFERRED) {
            assert!(
                name == expected && reason.starts_with(start),
                "{name}: {reason}"
            );
        }
        let (failing, parsed): (Vec<PathBuf>, Vec<PathBuf>) =
            parsed.into_iter().partition(|path| {
                let name = path.file_name().unwrap().to_string_lossy();
                TIKA_PATH_FAILS.iter().any(|(failing, _)| *failing == name)
            });
        assert_eq!(failing.len(), TIKA_PATH_FAILS.len());
        for path in &failing {
            let (xml, _) = extracting(&std::fs::read(path).unwrap(), &configs);
            assert!(xml.contains("<div class=\"page\">"), "{xml}");
        }
        let (locked, parsed): (Vec<PathBuf>, Vec<PathBuf>) = parsed
            .into_iter()
            .partition(|path| path.to_string_lossy().ends_with("_owner_user.pdf"));
        assert_eq!(locked.len(), 2, "{locked:?}");
        assert_fails_as_tika_with(&as_paths(&locked), &configs);
        // Tika extracts the Mac, DOS and Unix copies of one file as files of their own (#109).
        let compared: Vec<PathBuf> = parsed
            .into_iter()
            .filter(|path| !path.ends_with("testPDF_multiFormatEmbFiles.pdf"))
            .collect();
        assert_same(&compared, &configs);
    }
}
