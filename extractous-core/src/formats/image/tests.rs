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
fn reads_local_webps_as_tika_does() {
    let deferred = same_as_tika(
        &RecognizedImageParser,
        "ai.yobix.RecognizedImageParser",
        &local_roots(),
    );
    assert!(deferred.is_empty(), "{deferred:?}");
}

#[test]
#[ignore = "needs the conformance corpus: eval \"$(conformance/prepare.sh target/conformance)\""]
fn reads_tikas_test_webps_as_tika_does() {
    let roots = conformance_roots();
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let deferred = same_as_tika(
        &RecognizedImageParser,
        "ai.yobix.RecognizedImageParser",
        &roots,
    );
    println!("deferred: {deferred:?}");
}
