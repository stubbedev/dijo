use std::path::{Path, PathBuf};

use super::*;
use crate::config::TesseractOcrConfig;
use crate::formats::parity::{
    assert_fails_as_tika_with, assert_same_as_tika_delegating_departing_with, conformance_corpus,
    deferral, documents_with, Departure, Departures,
};

/// What the image parsers give otherwise than Tika's path on purpose: the modified date of the
/// file directory in UTC, where Tika writes it in the machine's zone (#114).
pub(super) const IMAGE_DEPARTURES: Departures =
    &[Departure::Values("img:File Modified Date", "#114")];

/// Asserts that the image parser of `paths`, which Tika's path parses with `tika_name`,
/// extracts what Tika's path does under `configs`, but for [`IMAGE_DEPARTURES`].
fn assert_same_as_tika_reading_images(paths: &[&Path], tika_name: &str, configs: &ParserConfigs) {
    assert_same_as_tika_delegating_departing_with(
        paths,
        tika_name,
        &TIKA_DELEGATES,
        IMAGE_DEPARTURES,
        configs,
    );
}

fn skipping_ocr() -> ParserConfigs {
    ParserConfigs {
        ocr: TesseractOcrConfig::new().set_skip_ocr(true),
        ..ParserConfigs::default()
    }
}

fn local_roots() -> [&'static Path; 2] {
    [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ]
}

/// The conformance corpus's image documents.
fn conformance_roots() -> Vec<PathBuf> {
    let modules = conformance_corpus();
    [
        "tika-parsers-standard/tika-parsers-standard-modules/tika-parser-image-module",
        "tika-parsers-standard/tika-parsers-standard-modules/tika-parser-ocr-module",
        "tika-parsers-standard/tika-parsers-standard-modules/tika-parser-xmp-commons",
        "tika-parsers-extended",
        "tika-parsers-standard/tika-parsers-standard-integration-tests",
    ]
    .iter()
    .map(|module| modules.join(module))
    .collect()
}

fn same_as_tika(parser: &dyn Parser, tika_name: &str, roots: &[&Path]) -> Vec<PathBuf> {
    let configs = skipping_ocr();
    let (parsed, deferred) = documents_with(parser, roots, &configs);
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_reading_images(&paths, tika_name, &configs);
    deferred
}

#[test]
fn reads_local_jpegs_as_tika_does() {
    let deferred = same_as_tika(
        &JpegParser,
        "org.apache.tika.parser.image.JpegParser",
        &local_roots(),
    );
    assert!(deferred.is_empty(), "{deferred:?}");
}

#[test]
fn reads_local_tiffs_as_tika_does() {
    let deferred = same_as_tika(
        &TiffParser,
        "org.apache.tika.parser.image.TiffParser",
        &local_roots(),
    );
    assert!(deferred.is_empty(), "{deferred:?}");
}

#[test]
#[ignore = "needs the conformance corpus: eval \"$(conformance/prepare.sh target/conformance)\""]
fn reads_tikas_test_jpegs_as_tika_does() {
    let roots = conformance_roots();
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let deferred = same_as_tika(
        &JpegParser,
        "org.apache.tika.parser.image.JpegParser",
        &roots,
    );
    println!("deferred: {deferred:?}");
}

#[test]
#[ignore = "needs the conformance corpus: eval \"$(conformance/prepare.sh target/conformance)\""]
fn reads_tikas_test_tiffs_as_tika_does() {
    let roots = conformance_roots();
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let deferred = same_as_tika(
        &TiffParser,
        "org.apache.tika.parser.image.TiffParser",
        &roots,
    );
    println!("deferred: {deferred:?}");
}

#[test]
fn reads_local_heif_images_as_tika_does() {
    let deferred = same_as_tika(
        &HeifParser,
        "org.apache.tika.parser.image.HeifParser",
        &local_roots(),
    );
    assert!(deferred.is_empty(), "{deferred:?}");
}

#[test]
#[ignore = "needs the conformance corpus: eval \"$(conformance/prepare.sh target/conformance)\""]
fn reads_tikas_test_heif_images_as_tika_does() {
    let roots = conformance_roots();
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let deferred = same_as_tika(
        &HeifParser,
        "org.apache.tika.parser.image.HeifParser",
        &roots,
    );
    println!("deferred: {deferred:?}");
}

#[test]
#[ignore = "needs the conformance corpus: eval \"$(conformance/prepare.sh target/conformance)\""]
fn reads_tikas_test_heif_images_with_tesseract_as_tikas_path_does() {
    let roots = conformance_roots();
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let configs = automatic_language();
    let (parsed, deferred) = documents_with(&HeifParser, &roots, &configs);
    let reasons: Vec<_> = deferred
        .iter()
        .map(|path| (path, deferral(&HeifParser, path)))
        .collect();
    println!("read: {parsed:?}\ndeferred: {reasons:?}");
    reads_with_tesseract_as_tikas_path_does(
        &HeifParser,
        "org.apache.tika.parser.image.HeifParser",
        &parsed,
        &configs,
    );
}

/// The images of the quality corpus with the given names.
fn corpus(files: &[&str]) -> Vec<PathBuf> {
    files
        .iter()
        .map(|file| Path::new("../quality/corpus").join(file))
        .collect()
}

/// Reads images with tesseract as Tika's path writes them, the OCR text aside, and reads text
/// in each that Tika's path reads text in.
fn reads_with_tesseract_as_tikas_path_does(
    parser: &dyn Parser,
    tika_name: &str,
    paths: &[PathBuf],
    configs: &ParserConfigs,
) {
    assert!(crate::ocr::tesseract_runs(), "tesseract is not installed");
    for path in paths {
        assert!(
            deferral(parser, path).is_none(),
            "{} is deferred: {:?}",
            path.display(),
            deferral(parser, path)
        );
    }
    let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_reading_images(&paths, tika_name, configs);
}

fn automatic_language() -> ParserConfigs {
    ParserConfigs {
        ocr: TesseractOcrConfig::new().set_language(TesseractOcrConfig::AUTO_LANGUAGE),
        ..ParserConfigs::default()
    }
}

#[test]
fn reads_scans_with_tesseract_as_tikas_path_does() {
    reads_with_tesseract_as_tikas_path_does(
        &ImageParser,
        "org.apache.tika.parser.image.ImageParser",
        &corpus(&[
            "danish-scan.png",
            "prose-scan-rotated.png",
            "greek-scan.png",
        ]),
        &automatic_language(),
    );
}

#[test]
fn reads_right_to_left_scans_with_tesseract_as_tikas_path_does() {
    let scan = [PathBuf::from("../test_files/documents/ara-ocr.png")];
    let arabic = ParserConfigs {
        ocr: TesseractOcrConfig::new().set_language("ara"),
        ..ParserConfigs::default()
    };
    for configs in [automatic_language(), arabic] {
        reads_with_tesseract_as_tikas_path_does(
            &ImageParser,
            "org.apache.tika.parser.image.ImageParser",
            &scan,
            &configs,
        );
    }
}

#[test]
fn reads_jbig2_and_jpeg_2000_scans_with_tesseract_as_tikas_path_does() {
    reads_with_tesseract_as_tikas_path_does(
        &ImageParser,
        "org.apache.tika.parser.image.ImageParser",
        &corpus(&["prose-scan.jp2"]),
        &automatic_language(),
    );
    let text = assert_jbig2_dimension_read_once(&corpus(&["prose-scan.jb2"])[0]);
    assert!(text.contains("keeper"), "{text}");
}

#[test]
fn reads_heic_and_avif_scans_with_tesseract_as_tikas_path_does() {
    reads_with_tesseract_as_tikas_path_does(
        &HeifParser,
        "org.apache.tika.parser.image.HeifParser",
        &corpus(&["prose-scan.heic", "prose-scan.avif"]),
        &automatic_language(),
    );
}

#[test]
fn reads_scans_in_the_configured_language_as_tikas_path_does() {
    reads_with_tesseract_as_tikas_path_does(
        &ImageParser,
        "org.apache.tika.parser.image.ImageParser",
        &corpus(&["prose-scan-150dpi.png"]),
        &ParserConfigs::default(),
    );
}

#[test]
fn reads_photos_and_tiff_pages_with_tesseract_as_tikas_path_does() {
    reads_with_tesseract_as_tikas_path_does(
        &JpegParser,
        "org.apache.tika.parser.image.JpegParser",
        &corpus(&["prose-photo-perspective.jpg"]),
        &automatic_language(),
    );
    reads_with_tesseract_as_tikas_path_does(
        &TiffParser,
        "org.apache.tika.parser.image.TiffParser",
        &corpus(&["report-scan.tif"]),
        &automatic_language(),
    );
}

#[test]
fn reads_images_tesseract_reads_as_they_are_as_tikas_path_does() {
    reads_with_tesseract_as_tikas_path_does(
        &RecognizedImageParser,
        "ai.yobix.RecognizedImageParser",
        &corpus(&[
            "prose-scan.webp",
            "prose-scan.pbm",
            "prose-scan.psd",
            "prose-scan.pcx",
            "prose-scan.tga",
        ]),
        &automatic_language(),
    );
}

#[test]
fn fails_on_jpegs_tika_fails_on() {
    let scratch = crate::formats::parity::Scratch::new("image-fails");
    let negative = scratch.write(
        "negative-segment.jpg",
        &[0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x01, 0xFF, 0xD9],
    );
    assert_fails_as_tika_with(&[&negative], &skipping_ocr());
}

#[test]
fn reads_local_webps_and_photoshop_images_as_tika_does() {
    let deferred = same_as_tika(
        &RecognizedImageParser,
        "ai.yobix.RecognizedImageParser",
        &local_roots(),
    );
    assert!(deferred.is_empty(), "{deferred:?}");
}

/// Asserts that `ImageParser` reads the images under `roots` as Tika does, but for JBIG2 files,
/// whose dimension node jbig2-imageio gives Tika twice, its pixel size off (#120): each of their
/// dimension values is read once. The images left to Tika are returned.
fn reads_apart_from_jbig2_as_tika_does(roots: &[&Path]) -> Vec<PathBuf> {
    let configs = skipping_ocr();
    let (parsed, deferred) = documents_with(&ImageParser, roots, &configs);
    let (jbig2, paths): (Vec<&Path>, Vec<&Path>) = parsed
        .iter()
        .map(PathBuf::as_path)
        .partition(|path| path.extension().is_some_and(|extension| extension == "jb2"));
    assert_same_as_tika_reading_images(
        &paths,
        "org.apache.tika.parser.image.ImageParser",
        &configs,
    );
    assert!(!jbig2.is_empty());
    for jbig2 in jbig2 {
        assert_jbig2_dimension_read_once(jbig2);
    }
    deferred
}

/// Asserts that the JBIG2 file at `jbig2` has each value of its dimension node once, where
/// jbig2-imageio gives Tika the node twice, its pixel size off (#120); returns the text read.
fn assert_jbig2_dimension_read_once(jbig2: &Path) -> String {
    let (text, metadata) = crate::Extractor::new()
        .extract_file_to_string(jbig2.to_str().unwrap())
        .unwrap();
    assert_eq!(
        metadata
            .get("img:Dimension PixelAspectRatio")
            .map(Vec::as_slice),
        Some(&["1.0".to_owned()][..]),
        "{jbig2:?}"
    );
    for (key, values) in metadata.iter() {
        assert!(
            !key.starts_with("img:Dimension") || values.len() == 1,
            "{jbig2:?}: {key} {values:?}"
        );
    }
    text
}

#[test]
fn reads_local_pngs_and_gifs_as_tika_does() {
    let deferred = reads_apart_from_jbig2_as_tika_does(&local_roots());
    assert!(deferred.is_empty(), "{deferred:?}");
}

#[test]
fn reads_images_imageio_has_no_reader_for_as_tika_does() {
    let scratch = crate::formats::parity::Scratch::new("image-io");
    let icon_bitmap = [
        &[40, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 1, 0, 32, 0][..],
        &[0; 32],
    ]
    .concat();
    let icon = [
        &[
            0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 32, 0, 48, 0, 0, 0, 22, 0, 0, 0,
        ][..],
        &icon_bitmap,
    ]
    .concat();
    scratch.write("tiny.ico", &icon);
    scratch.write(
        "tiny.xcf",
        &[&b"gimp xcf file\0"[..], &[0, 0, 0, 1, 0, 0, 0, 1], &[0; 20]].concat(),
    );
    scratch.write(
        "tiny.jpx",
        &[
            0, 0, 0, 12, b'j', b'P', b' ', b' ', 13, 10, 0x87, 10, 0, 0, 0, 20, b'f', b't', b'y',
            b'p', b'j', b'p', b'x', b' ', 0, 0, 0, 0, b'j', b'p', b'x', b' ',
        ],
    );
    let (parsed, deferred) = documents_with(&ImageParser, &[scratch.directory()], &skipping_ocr());
    assert_eq!(
        (parsed.len(), deferred.len()),
        (3, 0),
        "{parsed:?} {deferred:?}"
    );
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_reading_images(
        &paths,
        "org.apache.tika.parser.image.ImageParser",
        &skipping_ocr(),
    );
}

#[test]
fn defers_bmps_and_wbmps_tika_reads_with_a_reader_picked_at_random() {
    let bmp = [
        &b"BM"[..],
        &[
            58, 0, 0, 0, 0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 24, 0,
        ],
        &[
            0, 0, 0, 0, 4, 0, 0, 0, 0x13, 0x0B, 0, 0, 0x13, 0x0B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ],
        &[0xFF, 0xFF, 0xFF, 0],
    ]
    .concat();
    for (media_type, image) in [
        ("image/bmp", &bmp[..]),
        ("image/vnd.wap.wbmp", &[0, 0, 0x88, 0x01, 2, 0xAA, 0x55][..]),
    ] {
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", media_type);
        let deferred = ImageParser.defers(image, &metadata, &skipping_ocr());
        assert!(
            deferred
                .as_deref()
                .is_some_and(|reason| reason.contains("at random")),
            "{media_type}: {deferred:?}"
        );
    }
}

#[test]
#[ignore = "needs the conformance corpus: eval \"$(conformance/prepare.sh target/conformance)\""]
fn reads_tikas_test_pngs_and_gifs_as_tika_does() {
    let roots = conformance_roots();
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let deferred = reads_apart_from_jbig2_as_tika_does(&roots);
    println!("deferred: {deferred:?}");
}

/// A Photoshop file of `version` with these image resources and no image data.
fn photoshop(version: u8, resources: &[u8]) -> Vec<u8> {
    [
        &b"8BPS"[..],
        &[
            0, version, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0, 2, 0, 8, 0, 3,
        ],
        &[0, 0, 0, 0],
        &(resources.len() as u32).to_be_bytes(),
        resources,
    ]
    .concat()
}

#[test]
fn reads_photoshop_captions_as_tika_does() {
    let scratch = crate::formats::parity::Scratch::new("image-psd");
    let caption = scratch.write(
        "caption.psd",
        &photoshop(1, b"8BIM\x03\xF0A\x00\x00\x00\x00\x05Hi\xE9!\x00\x008BIM\x04\x0B\x00\x00\x00\x00\x00\x01X\x00"),
    );
    assert_same_as_tika_reading_images(
        &[&caption],
        "ai.yobix.RecognizedImageParser",
        &skipping_ocr(),
    );
}

#[test]
fn fails_on_photoshop_images_tika_fails_on() {
    let scratch = crate::formats::parity::Scratch::new("image-psd-fails");
    let header = photoshop;
    let documents = [
        scratch.write("version.psd", &header(3, &[])),
        scratch.write(
            "signature.psd",
            &header(1, b"8BIX\x04\x24\x00\x00\x00\x00\x00\x00"),
        ),
        scratch.write(
            "caption.psd",
            &header(1, b"8BIM\x03\xF0\x00\x00\x00\x00\x00\x00"),
        ),
    ];
    let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
    assert_fails_as_tika_with(&paths, &skipping_ocr());
}

#[test]
fn reads_images_tika_has_no_parser_for_as_tika_does() {
    let scratch = crate::formats::parity::Scratch::new("image-recognized");
    scratch.write("tiny.pbm", b"P4\n8 2\n\xaa\x55");
    scratch.write("tiny.pgm", b"P5\n2 2\n255\n\x00\xff\xff\x00");
    scratch.write(
        "tiny.pam",
        b"P7\nWIDTH 2\nHEIGHT 2\nDEPTH 1\nMAXVAL 255\nTUPLTYPE GRAYSCALE\nENDHDR\n\x00\xff\xff\x00",
    );
    let pcx_header = [
        &[10, 5, 1, 8, 0, 0, 0, 0, 1, 0, 1, 0, 72, 0, 72, 0][..],
        &[0; 48],
        &[0, 1, 2, 0, 1, 0],
        &[0; 58],
    ]
    .concat();
    scratch.write(
        "tiny.pcx",
        &[&pcx_header[..], &[0xC2, 0, 0xC2, 0xFF]].concat(),
    );
    scratch.write(
        "tiny.tga",
        &[
            0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 2, 0, 8, 0, 0, 255, 255, 0,
        ],
    );
    let (parsed, deferred) = documents_with(
        &RecognizedImageParser,
        &[scratch.directory()],
        &skipping_ocr(),
    );
    assert_eq!(
        (parsed.len(), deferred.len()),
        (5, 0),
        "{parsed:?} {deferred:?}"
    );
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_reading_images(&paths, "ai.yobix.RecognizedImageParser", &skipping_ocr());
}

#[test]
#[ignore = "needs the conformance corpus: eval \"$(conformance/prepare.sh target/conformance)\""]
fn reads_tikas_test_webps_and_photoshop_images_as_tika_does() {
    let roots = conformance_roots();
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let deferred = same_as_tika(
        &RecognizedImageParser,
        "ai.yobix.RecognizedImageParser",
        &roots,
    );
    println!("deferred: {deferred:?}");
}

/// An ISO base media file box of `kind` around `payload`.
fn bmff(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    [
        &((payload.len() + 8) as u32).to_be_bytes()[..],
        kind,
        payload,
    ]
    .concat()
}

/// A full box: a box whose payload starts with a version and three bytes of flags.
fn full_bmff(kind: &[u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
    bmff(kind, &[&[version, 0, 0, 0][..], payload].concat())
}

/// A big-endian IFD entry of `tag`, `format`, `count` and a value or offset of four bytes.
fn ifd_entry(tag: u16, format: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
    [
        &tag.to_be_bytes()[..],
        &format.to_be_bytes(),
        &count.to_be_bytes(),
        &value,
    ]
    .concat()
}

/// A big-endian IFD of these entries, with no IFD after it.
fn ifd(entries: &[Vec<u8>]) -> Vec<u8> {
    [
        &(entries.len() as u16).to_be_bytes()[..],
        &entries.concat(),
        &[0; 4],
    ]
    .concat()
}

/// The binary property list of an Apple run time: valid, with these seconds at a nanosecond
/// scale.
fn apple_run_time(seconds: i64) -> Vec<u8> {
    let objects: Vec<Vec<u8>> = vec![
        vec![0xD4, 1, 2, 3, 4, 5, 6, 7, 8],
        [&[0x55][..], b"flags"].concat(),
        [&[0x55][..], b"value"].concat(),
        [&[0x59][..], b"timescale"].concat(),
        [&[0x55][..], b"epoch"].concat(),
        vec![0x10, 1],
        [&[0x13][..], &(seconds * 1_000_000_000).to_be_bytes()].concat(),
        [&[0x12][..], &1_000_000_000u32.to_be_bytes()].concat(),
        vec![0x10, 0],
    ];
    let mut plist = b"bplist00".to_vec();
    let mut offsets = Vec::new();
    for object in &objects {
        offsets.push(plist.len() as u8);
        plist.extend_from_slice(object);
    }
    let table = plist.len() as i64;
    plist.extend_from_slice(&offsets);
    plist.extend_from_slice(&[0; 6]);
    plist.extend_from_slice(&[1, 1]);
    plist.extend_from_slice(&(objects.len() as i64).to_be_bytes());
    plist.extend_from_slice(&0i64.to_be_bytes());
    plist.extend_from_slice(&table.to_be_bytes());
    plist
}

/// An Apple maker note: its header, then an IFD of its run time, acceleration vector, HDR image
/// type and live photo id, whose offsets count from the note's start.
fn apple_maker_note() -> Vec<u8> {
    const HEADER: usize = 14;
    const ENTRIES: u16 = 4;
    let data_start = HEADER + 2 + 12 * usize::from(ENTRIES) + 4;
    let run_time = apple_run_time(595_778);
    let acceleration: Vec<u8> = [(-93i32, 100i32), (1, 100), (35, -100)]
        .iter()
        .flat_map(|(numerator, denominator)| {
            [numerator.to_be_bytes(), denominator.to_be_bytes()].concat()
        })
        .collect();
    let acceleration_at = data_start + run_time.len();
    let note = ifd(&[
        ifd_entry(
            0x0003,
            7,
            run_time.len() as u32,
            (data_start as u32).to_be_bytes(),
        ),
        ifd_entry(0x0008, 10, 3, (acceleration_at as u32).to_be_bytes()),
        ifd_entry(0x000a, 9, 1, 3i32.to_be_bytes()),
        ifd_entry(0x0017, 4, 1, 8192u32.to_be_bytes()),
    ]);
    [&b"Apple iOS\0\0\x01MM"[..], &note, &run_time, &acceleration].concat()
}

/// Big-endian TIFF data of an iPhone's Exif: IFD0's make and model, and the Exif IFD with its
/// maker note.
fn iphone_exif() -> Vec<u8> {
    let maker_note = apple_maker_note();
    let ifd0_entries = 4u32;
    let ifd0_length = 2 + 12 * ifd0_entries + 4;
    let strings_at = 8 + ifd0_length;
    let exif_ifd_at = strings_at + 16;
    let exif_ifd_length = 2 + 12 + 4;
    let note_at = exif_ifd_at + exif_ifd_length;
    let ifd0 = ifd(&[
        ifd_entry(0x010f, 2, 6, strings_at.to_be_bytes()),
        ifd_entry(0x0110, 2, 9, (strings_at + 6).to_be_bytes()),
        ifd_entry(0x0112, 3, 1, [0, 1, 0, 0]),
        ifd_entry(0x8769, 4, 1, exif_ifd_at.to_be_bytes()),
    ]);
    let exif_ifd = ifd(&[ifd_entry(
        0x927c,
        7,
        maker_note.len() as u32,
        note_at.to_be_bytes(),
    )]);
    [
        &b"MM\0\x2a\0\0\0\x08"[..],
        &ifd0,
        b"Apple\0iPhone 8\0\0",
        &exif_ifd,
        &maker_note,
    ]
    .concat()
}

#[test]
fn reads_apple_maker_notes_as_tika_does() {
    let scratch = crate::formats::parity::Scratch::new("image-apple");
    let jpeg = scratch.write(
        "iphone.jpg",
        &[
            &[0xFF, 0xD8, 0xFF, 0xE1][..],
            &((iphone_exif().len() + 8) as u16).to_be_bytes(),
            b"Exif\0\0",
            &iphone_exif(),
            &[0xFF, 0xD9],
        ]
        .concat(),
    );
    let (parsed, deferred) = documents_with(&JpegParser, &[&jpeg], &skipping_ocr());
    assert_eq!((parsed.len(), deferred.len()), (1, 0), "{deferred:?}");
    assert_same_as_tika_reading_images(
        &[&jpeg],
        "org.apache.tika.parser.image.JpegParser",
        &skipping_ocr(),
    );
}

/// A HEIF image as an iPhone writes it, without the image: its file type, its meta box of a
/// picture handler, an image item and an Exif item, the image's properties, and the media data
/// of both items.
fn iphone_heic() -> Vec<u8> {
    heic(false)
}

/// A HEIF image as [`iphone_heic`], or with `exif_first` the Exif item in a free box before
/// the meta box.
fn heic(exif_first: bool) -> Vec<u8> {
    let file_type = bmff(b"ftyp", &[&b"heic"[..], &[0; 4], b"mif1heic"].concat());
    let exif = [&6u32.to_be_bytes()[..], b"Exif\0\0", &iphone_exif()].concat();
    let image = b"not an image".to_vec();
    let meta = |image_at: u32, exif_at: u32| {
        let info = full_bmff(
            b"iinf",
            0,
            &[
                &2u16.to_be_bytes()[..],
                &full_bmff(b"infe", 2, &[&[0, 1, 0, 0][..], b"hvc1\0"].concat()),
                &full_bmff(b"infe", 2, &[&[0, 2, 0, 0][..], b"Exif\0"].concat()),
            ]
            .concat(),
        );
        let location = |item: u16, at: u32, length: usize| {
            [
                &item.to_be_bytes()[..],
                &[0, 0, 0, 1],
                &at.to_be_bytes(),
                &(length as u32).to_be_bytes(),
            ]
            .concat()
        };
        let locations = full_bmff(
            b"iloc",
            0,
            &[
                &[0x44, 0x00, 0, 2][..],
                &location(1, image_at, image.len()),
                &location(2, exif_at, exif.len()),
            ]
            .concat(),
        );
        let properties = bmff(
            b"iprp",
            &bmff(
                b"ipco",
                &[
                    full_bmff(
                        b"ispe",
                        0,
                        &[&512u32.to_be_bytes()[..], &384u32.to_be_bytes()].concat(),
                    ),
                    full_bmff(b"pixi", 0, &[3, 8, 8, 8]),
                    bmff(b"irot", &[1]),
                    bmff(
                        b"colr",
                        &[&b"nclx"[..], &[0, 1, 0, 13, 0, 6, 0x80]].concat(),
                    ),
                ]
                .concat(),
            ),
        );
        full_bmff(
            b"meta",
            0,
            &[
                full_bmff(
                    b"hdlr",
                    0,
                    &[&[0; 4][..], b"pict", &[0; 12], b"\0"].concat(),
                ),
                full_bmff(b"pitm", 0, &1u16.to_be_bytes()),
                info,
                locations,
                properties,
            ]
            .concat(),
        )
    };
    if exif_first {
        let free = bmff(b"free", &exif);
        let exif_at = (file_type.len() + 8) as u32;
        let media_at = (file_type.len() + free.len() + meta(0, 0).len() + 8) as u32;
        return [
            file_type,
            free,
            meta(media_at, exif_at),
            bmff(b"mdat", &image),
        ]
        .concat();
    }
    let media_at = (file_type.len() + meta(0, 0).len() + 8) as u32;
    let exif_at = media_at + image.len() as u32;
    [
        file_type,
        meta(media_at, exif_at),
        bmff(b"mdat", &[&image[..], &exif].concat()),
    ]
    .concat()
}

#[test]
fn reads_heif_images_as_tika_does() {
    let scratch = crate::formats::parity::Scratch::new("image-heif");
    let heic = scratch.write("iphone.heic", &iphone_heic());
    let (parsed, deferred) = documents_with(&HeifParser, &[&heic], &skipping_ocr());
    assert_eq!((parsed.len(), deferred.len()), (1, 0), "{deferred:?}");
    assert_same_as_tika_reading_images(
        &[&heic],
        "org.apache.tika.parser.image.HeifParser",
        &skipping_ocr(),
    );
}

#[test]
fn reads_the_exif_item_of_a_heif_image_wherever_it_stands() {
    // The Exif item before the media data, where metadata-extractor reads the image as the
    // Exif (#114).
    let swapped = heic(true);
    let (_, metadata) = crate::Extractor::new()
        .set_ocr_config(TesseractOcrConfig::new().set_skip_ocr(true))
        .extract_bytes_to_string(&swapped)
        .unwrap();
    assert_eq!(
        metadata.get("tiff:Model").map(Vec::as_slice),
        Some(&["iPhone 8".to_owned()][..]),
        "{metadata:?}"
    );
}

#[test]
fn describes_an_image_read_from_memory_by_its_size_alone() {
    let input = std::fs::read("../quality/corpus/prose-scan.webp").unwrap();
    let (_, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&input)
        .unwrap();
    // Tika describes the temporary file it spools the image to (#114).
    assert_eq!(
        metadata.get("img:File Size").map(Vec::as_slice),
        Some(&[format!("{} bytes", input.len())][..]),
        "{metadata:?}"
    );
    assert!(!metadata.contains_key("img:File Name"), "{metadata:?}");
    assert!(
        !metadata.contains_key("img:File Modified Date"),
        "{metadata:?}"
    );
}

/// Tika's `JXLParser` writes no document of a JPEG XL image, only the XMP it finds.
#[test]
#[ignore = "needs the conformance corpus: eval \"$(conformance/prepare.sh target/conformance)\""]
fn reads_tikas_test_jpeg_xl_images_as_tika_does() {
    let roots = conformance_roots();
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let (parsed, deferred) = documents_with(&JxlParser, &roots, &skipping_ocr());
    assert_eq!(parsed.len(), 3, "{parsed:?}");
    assert!(deferred.is_empty(), "{deferred:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    crate::formats::parity::assert_same_as_tika(&paths, JxlParser.tika_name());
}
