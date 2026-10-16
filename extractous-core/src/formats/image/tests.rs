use std::path::{Path, PathBuf};

use super::*;
use crate::config::TesseractOcrConfig;
use crate::formats::parity::{
    assert_fails_as_tika_with, assert_same_as_tika_delegating_with, conformance_corpus,
    documents_with,
};

/// The OCR enricher's names, the port's paired with Tika's.
const DELEGATES: [(&str, &str); 1] = [(PREPARING_OCR_PARSER, "ai.yobix.PreparingOcrParser")];

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
    assert_same_as_tika_delegating_with(&paths, tika_name, &DELEGATES, &configs);
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
fn defers_images_tika_reads_with_tesseract() {
    let jpeg = std::fs::read("../quality/corpus/prose-photo-perspective.jpg").unwrap();
    let mut metadata = TikaMetadata::new();
    metadata.set("Content-Type", "image/jpeg");
    let deferred = JpegParser.defers(&jpeg, &metadata, &ParserConfigs::default());
    assert_eq!(
        deferred.is_some(),
        crate::ocr::tesseract_runs(),
        "{deferred:?}"
    );
    assert!(JpegParser
        .defers(&jpeg, &metadata, &skipping_ocr())
        .is_none());
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

#[test]
fn reads_local_pngs_and_gifs_as_tika_does() {
    let deferred = same_as_tika(
        &ImageParser,
        "org.apache.tika.parser.image.ImageParser",
        &local_roots(),
    );
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
    assert_same_as_tika_delegating_with(
        &paths,
        "org.apache.tika.parser.image.ImageParser",
        &DELEGATES,
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
    let deferred = same_as_tika(
        &ImageParser,
        "org.apache.tika.parser.image.ImageParser",
        &roots,
    );
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
    assert_same_as_tika_delegating_with(
        &[&caption],
        "ai.yobix.RecognizedImageParser",
        &DELEGATES,
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
    assert_same_as_tika_delegating_with(
        &paths,
        "ai.yobix.RecognizedImageParser",
        &DELEGATES,
        &skipping_ocr(),
    );
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
