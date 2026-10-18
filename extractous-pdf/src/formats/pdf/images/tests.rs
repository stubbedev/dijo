use std::io::Write as _;
#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

#[cfg(feature = "tika")]
use super::super::tests::{as_paths, tika_name, CORPORA, XFA_FAILURE};
use super::super::tests::{document, page_objects};
use super::super::PdfParser;
#[cfg(feature = "tika")]
use super::super::DELEGATES;
#[cfg(feature = "tika")]
use crate::config::TesseractOcrConfig;
use crate::config::{ParserConfigs, PdfOcrStrategy, PdfParserConfig};
use crate::formats::metadata::TikaMetadata;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika_with, assert_same_as_tika_delegating_departing_with, conformance_corpus,
    documents_with, spools_as_tika_departing_with, Departure, Departures, Scratch,
};
use crate::formats::Parser;

/// The JDK's own sRGB profile, whose header names no device model.
const JDK_SRGB: &[u8] = include_bytes!("../../../../../extractous-image/models/jdk-srgb.icc");

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
#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
const PNG_WRITER: Departures = &[Departure::PngImages("#131")];

/// Compares the native parser with Tika's path on `paths`, and their spools of what the
/// documents embed, but for [`PNG_WRITER`].
#[cfg(feature = "tika")]
fn assert_same(paths: &[PathBuf], configs: &ParserConfigs) {
    assert_same_departing(paths, configs, &[]);
}

/// [`assert_same`], but for `departures` too.
#[cfg(feature = "tika")]
fn assert_same_departing(paths: &[PathBuf], configs: &ParserConfigs, departures: Departures) {
    let departures: Vec<Departure> = PNG_WRITER.iter().chain(departures).copied().collect();
    assert_same_as_tika_delegating_departing_with(
        &as_paths(paths),
        tika_name(),
        &DELEGATES,
        &departures,
        configs,
    );
    for path in paths {
        let spooled = spools_as_tika_departing_with(path, u64::MAX, &departures, configs);
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
#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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

/// PDFBox's CMYK profile, which an ICC colour space of the fixtures embeds too.
const CMYK_PROFILE: &[u8] = include_bytes!("../../../../models/pdf/CGATS001Compat-v2-micro.icc");

/// The images of [`colour_space_images`] in the CIE-based colour spaces, Lab, CalRGB and
/// CalGray, and a separation over Lab.
const CIE_IMAGES: [usize; 5] = [7, 10, 11, 12, 13];

/// [`colour_space_images`] drawing the images `drawn` of them, each once.
fn colour_space_images_drawing(drawn: impl Iterator<Item = usize>) -> Vec<u8> {
    let content: String = drawn
        .map(|index| format!("q 20 0 0 20 {} 600 cm /Im{index} Do Q ", 22 * index))
        .collect();
    colour_space_images(&content)
}

/// Images in the colour spaces PDFBox converts with profiles, CIE XYZ and tint transforms:
/// DeviceCMYK, ICC CMYK and RGB profiles, palettes over them, separations and DeviceN over
/// each kind of function, Lab, CalRGB and CalGray, each of several rows and columns and of one,
/// which Java2D draws, drawn by `content`.
fn colour_space_images(content: &str) -> Vec<u8> {
    let image8 = |space: &str, width: usize, height: usize, components: usize, seed: u8| {
        image(
            &format!("/Width {width} /Height {height} /ColorSpace {space} /BitsPerComponent 8"),
            "",
            &samples(width * height * components, seed),
        )
    };
    let cmyk_profile = 23;
    let rgb_profile = 24;
    let exponential = 25;
    let calculator = 26;
    let sampled = 27;
    let two_inks = 28;
    let lab = "[/Lab << /WhitePoint [0.9505 1 1.089] /Range [-100 100 -100 100] >>]";
    let images =
        vec![
        image8("/DeviceCMYK", 3, 2, 4, 1),
        image8("/DeviceCMYK", 5, 1, 4, 2),
        image8(&format!("[/ICCBased {cmyk_profile} 0 R]"), 3, 2, 4, 3),
        image(
            &format!(
                "/Width 3 /Height 2 /ColorSpace [/Indexed /DeviceCMYK 2 <{}>] \
                 /BitsPerComponent 8",
                hex(&samples(12, 4))
            ),
            "",
            &[0, 1, 2, 2, 1, 0],
        ),
        image(
            &format!(
                "/Width 3 /Height 2 /ColorSpace [/Indexed [/ICCBased {rgb_profile} 0 R] 2 <{}>] \
                 /BitsPerComponent 8",
                hex(&samples(9, 5))
            ),
            "",
            &[2, 1, 0, 0, 1, 2],
        ),
        image8(&format!("[/Separation /Spot /DeviceRGB {exponential} 0 R]"), 3, 2, 1, 6),
        image(
            &format!(
                "/Width 4 /Height 2 /ColorSpace [/Separation /Spot2 /DeviceCMYK {calculator} 0 R] \
                 /BitsPerComponent 4"
            ),
            "",
            &[0x0f, 0x7a, 0x31, 0xc8],
        ),
        image8(&format!("[/Separation /Spot3 {lab} {sampled} 0 R]"), 3, 2, 1, 7),
        image8(&format!("[/DeviceN [/A /B] /DeviceCMYK {two_inks} 0 R]"), 3, 2, 2, 8),
        image8(
            &format!(
                "[/DeviceN [/Cyan /Spot] /DeviceCMYK {two_inks} 0 R << /Subtype /DeviceN \
                 /Process << /ColorSpace /DeviceCMYK /Components [/Cyan /Magenta /Yellow /Black] \
                 >> /Colorants << /Spot [/Separation /Spot /DeviceRGB {exponential} 0 R] >> >>]"
            ),
            3,
            2,
            2,
            9,
        ),
        image8(lab, 3, 2, 3, 10),
        image8(
            "[/CalRGB << /WhitePoint [1 1 1] /Gamma [2.2 2.2 2.2] /Matrix [0.4124 0.2126 0.0193 \
             0.3576 0.7152 0.1192 0.1805 0.0722 0.9505] >>]",
            3,
            2,
            3,
            11,
        ),
        image8("[/CalRGB << /WhitePoint [0.9505 1 1.089] >>]", 3, 2, 3, 12),
        image8("[/CalGray << /WhitePoint [1 1 1] /Gamma 2.2 >>]", 3, 2, 1, 13),
        image(
            "/Width 2 /Height 2 /ColorSpace /DeviceCMYK /BitsPerComponent 16",
            "/FlateDecode",
            &zlib(&samples(32, 14)),
        ),
        image(
            "/Width 3 /Height 2 /ColorSpace /DeviceCMYK /BitsPerComponent 8 /SMask 29 0 R",
            "",
            &samples(24, 15),
        ),
        image8(&format!("[/ICCBased {cmyk_profile} 0 R]"), 1, 3, 4, 16),
    ];
    let mut objects = images;
    objects.extend([
        stream("/N 4", "", CMYK_PROFILE),
        icc(false),
        "<< /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [0.2 0.4 0.9] /N 1.5 >>".to_owned(),
        stream(
            "/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1 0 1]",
            "",
            b"{ dup 0.5 mul exch dup 0.25 mul exch 0 exch }",
        ),
        stream(
            "/FunctionType 0 /Domain [0 1] /Range [0 100 -128 127 -128 127] /Size [2] \
             /BitsPerSample 8",
            "",
            &[255, 128, 128, 64, 200, 60],
        ),
        stream(
            "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1 0 1]",
            "",
            b"{ 0 0 }",
        ),
        image(
            "/Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 \
             /Matte [0.1 0.2 0.3 0.4]",
            "",
            &samples(6, 17),
        ),
    ]);
    drawing(content, &xobjects(17), &objects)
}

#[cfg(feature = "tika")]
#[test]
fn converts_the_colour_spaces_as_tikas_path_does() {
    let scratch = Scratch::new("pdf-colour-spaces");
    let drawn = (0..17).filter(|index| !CIE_IMAGES.contains(index));
    let path = scratch.write("colours.pdf", &colour_space_images_drawing(drawn));
    let configs = without_page_ocr(false);
    let input = std::fs::read(&path).unwrap();
    assert_eq!(deferral(&input, &configs), None);
    assert_same(std::slice::from_ref(&path), &configs);
}

/// Tika's native image lacks the JDK's CIE XYZ profile, so that PDFBox's conversion of a CIE
/// colour throws a `CMMException` Tika does not catch and the whole document fails; the
/// native parser converts the colours as PDFBox does on the JVM (#125), which the colour
/// space's own test checks.
#[test]
fn converts_cie_based_images_where_tikas_native_image_fails() {
    let input = colour_space_images_drawing(CIE_IMAGES.into_iter());
    let configs = without_page_ocr(false);
    assert_eq!(deferral(&input, &configs), None);
    let (xml, _) = extracting(&input, &configs);
    let names: Vec<String> = CIE_IMAGES
        .iter()
        .enumerate()
        .map(|(number, _)| format!("image-{number}.png"))
        .collect();
    assert_eq!(image_names(&xml), names, "{xml}");
}

/// A JPEG ImageMagick 7 wrote in CMYK, as YCCK under an Adobe marker, and a grey one, each of
/// 23 by 17 pixels.
const CMYK_JPEG: &[u8] = include_bytes!("../../../../tests/fixtures/pdf/cmyk.jpg");
const GRAY_JPEG: &[u8] = include_bytes!("../../../../tests/fixtures/pdf/gray.jpg");

/// JPEG images Java decodes, drawn by `content`: masked ones in RGB, CMYK and grey (`Im0` to
/// `Im2`), which Tika writes as PNG, an unmasked CMYK one and a grey one through a separation
/// (`Im3`, `Im4`), which Java encodes as JPEG anew, and inline ones in grey, copied, and in
/// CMYK, encoded anew.
fn jpeg_images(content: &str) -> Vec<u8> {
    let jpeg_image = |dict: &str, data: &[u8]| image(dict, "/DCTDecode", data);
    let small = "/Width 23 /Height 17 /BitsPerComponent 8";
    let objects = vec![
        jpeg_image(
            "/Width 46 /Height 56 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 11 0 R",
            &jpeg(),
        ),
        jpeg_image(
            &format!("{small} /ColorSpace /DeviceCMYK /SMask 12 0 R"),
            CMYK_JPEG,
        ),
        jpeg_image(
            &format!("{small} /ColorSpace /DeviceGray /Mask 13 0 R"),
            GRAY_JPEG,
        ),
        jpeg_image(&format!("{small} /ColorSpace /DeviceCMYK"), CMYK_JPEG),
        jpeg_image(
            &format!("{small} /ColorSpace [/Separation /Ink /DeviceRGB 14 0 R]"),
            GRAY_JPEG,
        ),
        image(
            "/Width 46 /Height 56 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "",
            &samples(46 * 56, 1),
        ),
        image(
            "/Width 23 /Height 17 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "",
            &samples(23 * 17, 2),
        ),
        image(
            "/Width 23 /Height 17 /ImageMask true",
            "",
            &samples(3 * 17, 3),
        ),
        "<< /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [0.1 0.5 0.2] /N 1 >>".to_owned(),
    ];
    drawing(content, &xobjects(5), &objects)
}

/// An inline JPEG of the colour space `space`, its data in hexadecimal.
fn inline_jpeg(space: &str, data: &[u8]) -> String {
    format!(
        "q 23 0 0 17 0 300 cm BI /W 23 /H 17 /CS /{space} /BPC 8 /F [/AHx /DCT] ID {}> EI Q ",
        hex(data)
    )
}

/// The images of Java's JPEG writer in [`jpeg_images`], unmasked CMYK, grey through a
/// separation and inline CMYK.
fn jpegs_written_anew() -> Vec<u8> {
    jpeg_images(&format!(
        "q 23 0 0 17 0 600 cm /Im3 Do Q q 23 0 0 17 30 600 cm /Im4 Do Q {}",
        inline_jpeg("CMYK", CMYK_JPEG)
    ))
}

#[cfg(feature = "tika")]
#[test]
fn decodes_masked_jpegs_as_tikas_path_does() {
    let scratch = Scratch::new("pdf-masked-jpegs");
    let content = format!(
        "q 46 0 0 56 0 600 cm /Im0 Do Q q 23 0 0 17 50 600 cm /Im1 Do Q \
         q 23 0 0 17 80 600 cm /Im2 Do Q {}",
        inline_jpeg("G", GRAY_JPEG)
    );
    let path = scratch.write("masked.pdf", &jpeg_images(&content));
    let configs = without_page_ocr(false);
    assert_eq!(deferral(&std::fs::read(&path).unwrap(), &configs), None);
    assert_same(&[path], &configs);
}

/// The files the native parser spools of the images of `input`, by name.
fn spooled_images(input: &[u8], configs: &ParserConfigs) -> Vec<(String, Vec<u8>)> {
    let scratch = crate::formats::parity::Scratch::new("pdf-spooled-images");
    let path = scratch.write("input.pdf", input);
    let spool = crate::embedded::EmbeddedSpool {
        dir: scratch.directory().to_path_buf(),
        byte_budget: u64::MAX,
    };
    let (mut stream, _) = crate::Extractor::new()
        .set_pdf_config(configs.pdf.clone())
        .set_ocr_config(configs.ocr.clone())
        .extract_file_spooling_embedded(path.to_str().unwrap(), &spool)
        .unwrap();
    std::io::copy(&mut stream, &mut std::io::sink()).unwrap();
    stream
        .embedded()
        .unwrap()
        .into_iter()
        .map(|file| (file.name, std::fs::read(file.path).unwrap()))
        .collect()
}

/// Java encodes a JPEG of another colour space than grey or RGB anew with its own writer,
/// whose bytes the native parser's encoder does not match (#125): the files are JPEGs of the
/// image's size and colours.
#[test]
fn encodes_jpegs_of_other_colour_spaces_anew() {
    let configs = without_page_ocr(false);
    let input = jpegs_written_anew();
    assert_eq!(deferral(&input, &configs), None);
    let spooled = spooled_images(&input, &configs);
    let names: Vec<&str> = spooled.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["image-0.jpg", "image-1.jpg", "image-2.jpg"]);
    for (name, data) in &spooled {
        let decoded = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .to_rgb8();
        assert_eq!(decoded.dimensions(), (23, 17), "{name}");
    }
    let ink = image::load_from_memory(&spooled[1].1).unwrap().to_rgb8();
    let [red, green, blue] = ink.get_pixel(0, 0).0;
    assert!(
        red < green && blue < green,
        "a greenish separation: {red} {green} {blue}"
    );
}

/// CCITT data ImageMagick 7 wrote of a 40 by 20 drawing of a black rectangle and circle: Group
/// 4, and Group 3 with end-of-line codes ending on byte boundaries.
const GROUP4: &[u8] = include_bytes!("../../../../tests/fixtures/pdf/group4.ccitt");
const GROUP3: &[u8] = include_bytes!("../../../../tests/fixtures/pdf/group3.ccitt");

/// CCITT images drawn by `content`: masked ones of Group 4, of Group 3 sniffed and of Group 3
/// aligned to bytes (`Im0` to `Im2`), which Tika writes as PNG, and an unmasked one and a stencil
/// mask (`Im3`, `Im4`), which Java writes as TIFF.
fn ccitt_images(content: &str) -> Vec<u8> {
    let ccitt = |dict: &str, params: &str, data: &[u8]| {
        image(
            &format!("/Width 40 /Height 20 {dict} /DecodeParms [null << /Columns 40 {params} >>]"),
            "/CCITTFaxDecode",
            data,
        )
    };
    let gray = "/ColorSpace /DeviceGray /BitsPerComponent 1";
    let objects = vec![
        ccitt(&format!("{gray} /SMask 11 0 R"), "/K -1", GROUP4),
        ccitt(&format!("{gray} /SMask 11 0 R"), "/K 0", GROUP3),
        ccitt(
            &format!("{gray} /SMask 11 0 R"),
            "/K 0 /EndOfLine true /EncodedByteAlign true",
            GROUP3,
        ),
        ccitt(gray, "/K -1 /Rows 20", GROUP4),
        ccitt("/ImageMask true", "/K -1 /BlackIs1 true", GROUP4),
        image(
            "/Width 40 /Height 20 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "",
            &samples(800, 4),
        ),
    ];
    drawing(content, &xobjects(5), &objects)
}

#[cfg(feature = "tika")]
#[test]
fn decodes_masked_ccitt_images_as_tikas_path_does() {
    let scratch = Scratch::new("pdf-ccitt");
    let content = "q 40 0 0 20 0 600 cm /Im0 Do Q q 40 0 0 20 50 600 cm /Im1 Do Q";
    let path = scratch.write("masked.pdf", &ccitt_images(content));
    let configs = without_page_ocr(false);
    assert_eq!(deferral(&std::fs::read(&path).unwrap(), &configs), None);
    assert_same(&[path], &configs);
}

/// Group 3 data with end-of-line codes, said to be aligned to bytes, decodes as the same data
/// does without the flag. PDFBox's decoder searches for each end-of-line code from the byte
/// after the row, restarting its search at each mismatch rather than sliding, so that it skips
/// codes behind fill bits and decodes the rows out of place (#125).
#[test]
fn decodes_byte_aligned_group_3_rows_as_unaligned_ones() {
    let configs = without_page_ocr(false);
    let input = ccitt_images("q 40 0 0 20 0 600 cm /Im1 Do Q q 40 0 0 20 50 600 cm /Im2 Do Q");
    assert_eq!(deferral(&input, &configs), None);
    let spooled = spooled_images(&input, &configs);
    let [(_, sniffed), (_, aligned)] = &spooled[..] else {
        panic!(
            "{:?}",
            spooled.iter().map(|(name, _)| name).collect::<Vec<_>>()
        );
    };
    let decode = |data: &[u8]| image::load_from_memory(data).unwrap().to_rgba8();
    assert_eq!(decode(sniffed), decode(aligned));
}

/// The CCITT images Java writes as TIFF with its CCITT T.6 writer: the native parser's TIFF
/// files hold the same black and white image in 8-bit grey (#125), and the rest of the
/// extraction is Tika's.
fn ccitt_written_as_tiff() -> Vec<u8> {
    ccitt_images("q 40 0 0 20 0 600 cm /Im3 Do Q q 40 0 0 20 50 600 cm /Im4 Do Q")
}

#[cfg(feature = "tika")]
#[test]
fn writes_ccitt_images_as_tiff_where_tikas_path_does() {
    let scratch = Scratch::new("pdf-ccitt-tiff");
    let path = scratch.write("tiff.pdf", &ccitt_written_as_tiff());
    assert_same_as_tika_delegating_departing_with(
        &as_paths(&[path]),
        tika_name(),
        &DELEGATES,
        PNG_WRITER,
        &without_page_ocr(false),
    );
}

#[test]
fn writes_ccitt_images_as_black_and_white_tiffs() {
    let configs = without_page_ocr(false);
    let input = ccitt_written_as_tiff();
    assert_eq!(deferral(&input, &configs), None);
    let spooled = spooled_images(&input, &configs);
    let names: Vec<&str> = spooled.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["image-0.tif", "image-1.tif"]);
    for (name, data) in &spooled {
        let decoded = image::load_from_memory_with_format(data, image::ImageFormat::Tiff)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .to_luma8();
        assert_eq!(decoded.dimensions(), (40, 20), "{name}");
        assert!(
            decoded.pixels().all(|pixel| matches!(pixel[0], 0 | 255)),
            "{name}"
        );
        if name == "image-0.tif" {
            let level = |x, y| decoded.get_pixel(x, y)[0];
            let (rectangle, circle) = (level(10, 8), level(30, 10));
            assert!(rectangle == circle && rectangle != level(1, 1), "{name}");
        }
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
#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
#[test]
fn copies_jpeg_jpeg_2000_and_jbig2_data_as_tikas_path_does() {
    let scratch = Scratch::new("pdf-image-copies");
    let path = scratch.write("copies.pdf", &copied_images());
    let configs = without_page_ocr(false);
    assert_eq!(deferral(&copied_images(), &configs), None);
    assert_same(&[path], &configs);
}

/// Images in forms, transparency groups, patterns and soft masks.
#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
#[test]
fn finds_images_in_forms_patterns_and_soft_masks_as_tikas_path_does() {
    let scratch = Scratch::new("pdf-image-painted");
    let path = scratch.write("painted.pdf", &painted_images());
    for configs in [without_page_ocr(false), without_page_ocr(true)] {
        assert_eq!(deferral(&painted_images(), &configs), None);
        assert_same(std::slice::from_ref(&path), &configs);
    }
}

#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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

/// A document of Helvetica text painted with a tiling pattern that draws an image.
fn text_painted_with_images() -> Vec<u8> {
    let mask = image(
        "/Width 3 /Height 3 /ColorSpace /DeviceGray /BitsPerComponent 8",
        "",
        &samples(9, 1),
    );
    drawing(
        "/Pattern cs /P0 scn BT /F1 12 Tf (Tekst) Tj [(T) 120 (o)] TJ ET",
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
    )
}

#[cfg(feature = "tika")]
#[test]
fn draws_the_images_of_a_pattern_once_a_glyph_as_tikas_path_does() {
    let scratch = Scratch::new("pdf-pattern-text");
    let path = scratch.write("text.pdf", &text_painted_with_images());
    for unique_only in [false, true] {
        let configs = without_page_ocr(unique_only);
        assert_eq!(deferral(&std::fs::read(&path).unwrap(), &configs), None);
        assert_same(std::slice::from_ref(&path), &configs);
    }
}

#[test]
fn draws_the_images_of_a_pattern_once_a_glyph() {
    let (xml, _) = extracting(&text_painted_with_images(), &without_page_ocr(false));
    assert_eq!(image_names(&xml), ["image-0.png"; 7], "{xml}");
}

/// Java2D scales the smaller of an image and its mask to the larger, which the native parser
/// does with the `image` crate (#125): the image takes the size of the larger, and its alpha
/// the mask's.
#[test]
fn scales_the_smaller_of_an_image_and_its_mask() {
    let images = [
        image(
            "/Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 8 0 R",
            "",
            &[0, 80, 160, 240],
        ),
        image(
            "/Width 4 /Height 4 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Mask 9 0 R",
            "",
            &samples(48, 2),
        ),
        image(
            "/Width 3 /Height 3 /ColorSpace /DeviceGray /BitsPerComponent 8",
            "",
            &[255; 9],
        ),
        image(
            "/Width 2 /Height 2 /ImageMask true",
            "",
            &[0b0100_0000, 0b1000_0000],
        ),
    ];
    let input = drawing(&draw_each(2), &xobjects(2), &images);
    let configs = without_page_ocr(false);
    assert_eq!(deferral(&input, &configs), None);
    let spooled = spooled_images(&input, &configs);
    let decoded: Vec<image::RgbaImage> = spooled
        .iter()
        .map(|(_, data)| image::load_from_memory(data).unwrap().to_rgba8())
        .collect();
    assert_eq!(decoded[0].dimensions(), (3, 3));
    assert!(decoded[0].pixels().all(|pixel| pixel[3] == 255));
    assert_eq!(decoded[1].dimensions(), (4, 4));
    let alpha = |x, y| decoded[1].get_pixel(x, y)[3];
    assert_eq!(
        [alpha(0, 0), alpha(3, 0), alpha(0, 3), alpha(3, 3)],
        [255, 0, 0, 255]
    );
}

/// Images extracted while our `TextLayerCheckingPdfParser` weighs each page's text, the text OCR
/// reads in its images with it.
#[cfg(feature = "tika")]
fn weighing_pages() -> ParserConfigs {
    ParserConfigs {
        pdf: PdfParserConfig::new().set_extract_inline_images(true),
        ..ParserConfigs::default()
    }
}

#[cfg(feature = "tika")]
#[test]
fn weighs_the_ocr_text_of_images_with_their_page_as_tikas_path_does() {
    assert!(crate::ocr::tesseract_runs(), "tesseract is not installed");
    let configs = weighing_pages();
    let scratch = Scratch::new("pdf-images-weighed");
    let sampled = scratch.write("sampled.pdf", &sampled_images());
    let scanned = PathBuf::from("../test_files/documents/deu-ocr.pdf");
    for path in [&sampled, &scanned] {
        let input = std::fs::read(path).unwrap();
        assert_eq!(deferral(&input, &configs), None, "{}", path.display());
    }
    assert_same(&[sampled, scanned], &configs);
}

#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
const TIKA_PATH_FAILS: [(&str, &str); 1] = [(
    "testPDF_acroform3.pdf",
    "the JDK rejects an ICC profile of a content stream's colour space, and the class \
     java.awt.color.CMMException that PDFBox catches for it is missing from the native image",
)];

/// The documents of Tika's tests whose image files the native parser writes otherwise than
/// Java (#125), and why: their extraction is compared, their files are not.
#[cfg(feature = "tika")]
const WRITTEN_OTHERWISE: [(&str, &str); 3] = [
    ("testPDFVarious.pdf", "a mask Java2D scales to its image"),
    (
        "testPDF_childAttachments.pdf",
        "a CCITT image Java writes as a TIFF with its CCITT T.6 writer",
    ),
    (
        "testPDF_incrementalUpdates.pdf",
        "a JPEG in an ICC colour space, which Java's JPEG writer encodes anew",
    ),
];

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_the_images_of_the_conformance_documents_as_tikas_path_does() {
    for configs in [without_page_ocr(false), without_page_ocr(true)] {
        let (parsed, deferred) = documents_with(&PdfParser, &[&conformance_corpus()], &configs);
        assert_eq!(deferred, Vec::<PathBuf>::new());
        let (written_otherwise, parsed): (Vec<PathBuf>, Vec<PathBuf>) =
            parsed.into_iter().partition(|path| {
                let name = path.file_name().unwrap().to_string_lossy();
                WRITTEN_OTHERWISE
                    .iter()
                    .any(|(written, _)| *written == name)
            });
        assert_eq!(written_otherwise.len(), WRITTEN_OTHERWISE.len());
        assert_same_as_tika_delegating_departing_with(
            &as_paths(&written_otherwise),
            tika_name(),
            &DELEGATES,
            PNG_WRITER,
            &configs,
        );
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
        let (malformed, parsed): (Vec<PathBuf>, Vec<PathBuf>) = parsed
            .into_iter()
            .partition(|path| path.ends_with("testPDF_malformedXFA.pdf"));
        assert_eq!(malformed.len(), 1, "{malformed:?}");
        assert_same_departing(&malformed, &configs, XFA_FAILURE);
        // Tika extracts the Mac, DOS and Unix copies of one file as files of their own (#109).
        let compared: Vec<PathBuf> = parsed
            .into_iter()
            .filter(|path| !path.ends_with("testPDF_multiFormatEmbFiles.pdf"))
            .collect();
        assert_same(&compared, &configs);
    }
}
