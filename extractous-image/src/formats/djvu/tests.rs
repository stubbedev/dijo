use std::path::Path;
#[cfg(feature = "tika")]
use std::path::PathBuf;

use super::*;
#[cfg(feature = "tika")]
use crate::config::TesseractOcrConfig;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika_with, assert_same_as_tika_delegating_with, conformance_corpus,
    documents_with, Scratch,
};

#[cfg(feature = "tika")]
const TIKA_NAME: &str = "ai.yobix.DjvuParser";
#[cfg(feature = "tika")]
const DELEGATES: [(&str, &str); 1] = [(PREPARING_OCR_PARSER, "ai.yobix.PreparingOcrParser")];

#[cfg(feature = "tika")]
fn local_roots() -> [&'static Path; 2] {
    [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ]
}

#[cfg(feature = "tika")]
fn skipping_ocr() -> ParserConfigs {
    ParserConfigs {
        ocr: TesseractOcrConfig::new().set_skip_ocr(true),
        ..ParserConfigs::default()
    }
}

#[cfg(feature = "tika")]
fn automatic_language() -> ParserConfigs {
    ParserConfigs {
        ocr: TesseractOcrConfig::new().set_language(TesseractOcrConfig::AUTO_LANGUAGE),
        ..ParserConfigs::default()
    }
}

/// The DjVu documents under `roots`, none of which the parser defers.
#[cfg(feature = "tika")]
fn djvu_documents(roots: &[&Path], configs: &ParserConfigs) -> Vec<PathBuf> {
    let (parsed, deferred) = documents_with(&DjvuParser, roots, configs);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert!(!parsed.is_empty(), "no DjVu documents under {roots:?}");
    parsed
}

#[cfg(feature = "tika")]
#[test]
fn reads_text_layers_as_tika_does() {
    let configs = skipping_ocr();
    let paths = djvu_documents(&local_roots(), &configs);
    let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_delegating_with(&paths, TIKA_NAME, &DELEGATES, &configs);
}

/// The pages without a text layer are rendered and read as Tika's path reads them, and every
/// page Tika's path reads text on is read to text natively too.
#[cfg(feature = "tika")]
#[test]
fn reads_pages_without_text_by_ocr_as_tikas_path_does() {
    assert!(crate::ocr::tesseract_runs(), "tesseract is not installed");
    let configs = automatic_language();
    let paths = djvu_documents(&local_roots(), &configs);
    let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_delegating_with(&paths, TIKA_NAME, &DELEGATES, &configs);
}

/// Tika's one DjVu test document, `testDJVU.djvu`, is an indirect document, whose pages are
/// files of their own, which both paths fail on.
#[cfg(feature = "tika")]
#[test]
#[ignore = "needs the conformance corpus: eval \"$(conformance/prepare.sh target/conformance)\""]
fn fails_on_tikas_test_djvu_as_tika_does() {
    let root =
        conformance_corpus().join("tika-parsers-standard/tika-parsers-standard-integration-tests");
    let paths = djvu_documents(&[&root], &skipping_ocr());
    let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    assert_fails_as_tika_with(&paths, &skipping_ocr());
}

/// Documents cut short inside their chunks, and a bundled document whose directory says its
/// pages are files of their own, fail on both paths: before any output where the document's
/// structure is read, or at the page that cannot be read.
#[cfg(feature = "tika")]
#[test]
fn fails_on_documents_tika_fails_on() {
    let scratch = Scratch::new("djvu-fails");
    let read = |file: &str| std::fs::read(Path::new("../test_files/documents").join(file)).unwrap();
    let shared = read("shared-shapes.djvu");
    let single = std::fs::read("../quality/corpus/prose-scan.djvu").unwrap();
    let mut indirect = shared.clone();
    let dirm = shared
        .windows(4)
        .position(|window| window == b"DIRM")
        .expect("a bundled document has a directory");
    indirect[dirm + 8] &= !0x80;
    let paths = [
        scratch.write("cut-bundle.djvu", &shared[..shared.len() / 2]),
        scratch.write("cut-page.djvu", &single[..single.len() - 100]),
        scratch.write("indirect.djvu", &indirect),
    ];
    let paths: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    assert_fails_as_tika_with(&paths, &skipping_ocr());
}

/// The largest difference between grey levels of a JPEG layer decoded by the image crate and by
/// ImageIO, which round the same image differently.
const JPEG_ROUNDING: u8 = 2;

/// `extractous-image/tests/fixtures/djvu/renders.tsv`: each page of the DjVu documents, rendered
/// by our Java `DjvuRenderer` (`just djvu-reference`), as its size and the CRC-32 of its pixels,
/// or the failure it renders with. A page with a JPEG layer, which ImageIO decodes for Java and
/// the image crate natively, matches the PGM of the Java rendering the reference names
/// instead, within [`JPEG_ROUNDING`] levels.
#[test]
fn renders_pages_as_the_java_renderer_does() {
    let fixtures = Path::new("tests/fixtures/djvu");
    let reference =
        std::fs::read_to_string(fixtures.join("renders.tsv")).expect("the render reference");
    let mut checked = 0;
    for line in reference.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        let [file, page, rendered, pixels] = fields[..] else {
            panic!("renders.tsv line {line:?}");
        };
        let input = std::fs::read(Path::new("..").join(file)).unwrap();
        let document = DjvuDocument::read(&input).unwrap();
        let page: usize = page.parse().unwrap();
        let ours = render::page(&document, &document.pages()[page - 1]);
        if pixels.is_empty() {
            let ours = match ours {
                Ok(image) => format!(
                    "{}x{} {:08x}",
                    image.width(),
                    image.height(),
                    crate::compress::crc32(image.as_raw())
                ),
                Err(error) => format!("failed: {}", error.message()),
            };
            assert_eq!(ours, rendered, "{file} page {page}");
        } else {
            let ours = ours.unwrap_or_else(|e| panic!("{file} page {page}: {e:?}"));
            let java = image::open(fixtures.join(pixels)).unwrap().to_luma8();
            assert_eq!(ours.dimensions(), java.dimensions(), "{file} page {page}");
            let largest = ours
                .pixels()
                .zip(java.pixels())
                .map(|(a, b)| a.0[0].abs_diff(b.0[0]))
                .max()
                .unwrap_or_default();
            assert!(
                largest <= JPEG_ROUNDING,
                "{file} page {page}: {largest} levels off"
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "renders.tsv lists no page");
}

/// The quality corpus's scan, coded with cjb2 at 300 dpi from its bitmap, renders as that
/// bitmap.
#[test]
fn renders_the_corpus_scan_as_its_bitmap() {
    let input = std::fs::read("../quality/corpus/prose-scan.djvu").unwrap();
    let document = DjvuDocument::read(&input).unwrap();
    let rendered = render::page(&document, &document.pages()[0]).unwrap();
    let bitmap = image::open("../quality/corpus/prose-scan.pbm")
        .unwrap()
        .to_luma8();
    assert_eq!(rendered, bitmap);
}

#[test]
fn splits_text_layers_into_paragraphs_at_block_ends() {
    let mut layer = vec![0, 0, 0];
    let text = "One column\u{b}\u{1f}Another region \u{1d} \u{1d}Last";
    layer.extend_from_slice(text.as_bytes());
    layer[2] = text.len() as u8;
    assert_eq!(layer_text(&layer).unwrap(), text);
    assert!(layer_text(&[0, 0, 9, b'x']).is_err());
    assert!(layer_text(&[0, 0]).is_err());
    assert_eq!(layer_text(&[]).unwrap(), "");
}

/// A page too large to read whole has its text layer read in place, the chunks after it skipped.
#[test]
fn extracts_djvu_documents_too_large_to_read_whole_in_bounded_memory() {
    use extractous_core::large::{bodies_past_limit, LargeFile};
    const PADDING: usize = 1 << 20;
    let text = b"A large page";
    let mut layer = vec![0, 0, text.len() as u8];
    layer.extend(text);
    layer.push(0);
    let padding = u32::try_from(bodies_past_limit(PADDING) * PADDING as u64).unwrap();
    let chunks = 4 + 8 + layer.len() as u32 + 8 + padding;
    let mut head = b"AT&TFORM".to_vec();
    head.extend(chunks.to_be_bytes());
    head.extend(b"DJVU");
    head.extend(b"TXTa");
    head.extend((layer.len() as u32).to_be_bytes());
    head.extend(&layer);
    head.extend(b"Xpad");
    head.extend(padding.to_be_bytes());
    let file = LargeFile::new(
        "page.djvu",
        &head,
        bodies_past_limit(PADDING),
        |_| vec![0; PADDING],
        &[],
    );
    let (extracted, metadata) = file.extract(DjvuParser.name());
    assert_eq!(extracted.trim(), "A large page");
    assert_eq!(metadata[PAGES], ["1"]);
}
