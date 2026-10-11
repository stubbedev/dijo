# Extractous

Text and metadata extraction for documents, from Rust, without a JVM.

This is a fork of [yobix-ai/extractous](https://github.com/yobix-ai/extractous). Every format is parsed by
[Apache Tika](https://tika.apache.org/), compiled ahead of time with [GraalVM](https://www.graalvm.org/) native-image
into a shared library (`libtika_native`) that the Rust crate calls through JNI. No Java runtime is installed or
started; Tika runs as native code inside your process.

## Install

The fork is not published to crates.io. Depend on it from git:

```toml
[dependencies]
extractous = { git = "https://github.com/stubbedev/extractous" }
```

Building the crate compiles Tika, so the build machine needs a GraalVM JDK with `native-image`, pointed to by
`GRAALVM_HOME`, matching the version `extractous-core/tika-native/build.gradle` requires. The first build takes a few
minutes; later builds reuse the compiled library until the Tika sources or the GraalVM release change.

At runtime `libtika_native` and the JDK libraries GraalVM emits beside it (`libawt.so`, `libfontmanager.so`, ...)
must be loadable. `cargo run` and `cargo test` arrange that; standalone binaries do not find them yet.

OCR runs the `tesseract` executable, which must be on `PATH` together with the language data you request.

## Usage

The entry point is `Extractor`, configured with a consuming builder.

Extract a file to a string:

```rust,no_run
use extractous::Extractor;

let extractor = Extractor::new().set_extract_string_max_length(1000);
let (text, metadata) = extractor.extract_file_to_string("document.pdf").unwrap();
println!("{text}");
println!("{metadata:?}");
```

Stream the content of a file, URL or byte slice through `std::io::Read`:

```rust,no_run
use std::io::Read;
use extractous::Extractor;

let extractor = Extractor::new();
let (mut stream, metadata) = extractor.extract_file("document.docx").unwrap();
// or: extractor.extract_url("https://example.com/")
// or: extractor.extract_bytes(&bytes)
let mut text = String::new();
stream.read_to_string(&mut text).unwrap();
```

Set `set_xml_output(true)` to receive Tika's XHTML instead of plain text.

OCR a scanned PDF in German:

```rust,no_run
use extractous::{Extractor, PdfOcrStrategy, PdfParserConfig, TesseractOcrConfig};

let extractor = Extractor::new()
    .set_ocr_config(TesseractOcrConfig::new().set_language("deu"))
    .set_pdf_config(PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::OCR_ONLY));
let (text, _) = extractor.extract_file_to_string("scan.pdf").unwrap();
```

`Unable to extract PDF content` from an OCR run usually means the requested tesseract language data is missing.

Unpack embedded documents (images, attachments) from a container:

```rust,no_run
use extractous::Extractor;

let result = Extractor::new().extract_embedded_from_file("workbook.xlsx").unwrap();
for document in &result.documents {
    println!("{} {} {} bytes", document.resource_name, document.content_type, document.size());
}
```

## Supported formats

The native library contains Tika's standard parser package and these parser modules: Microsoft Office (DOC, DOCX, XLS,
XLSX, PPT, PPTX, MSG), PDF, HTML, Apple iWork, audio/video metadata, CAD, encrypted documents, fonts, mail (EML, MBOX,
PST), legacy office formats (WordPerfect, Lotus, dBase), RSS/Atom, OCR via tesseract, archives (ZIP, TAR, 7Z, RAR),
text, XML, web archives (WARC), source code and advanced media.

Compiled in is not the same as working: native-image needs reflection metadata for each parser it reaches. The test
suite covers PDF, DOC, DOCX, PPTX, XLSX, ODT, CSV, EPUB, and PNG and PDF through OCR.

## Development

Every development dependency (Rust toolchain, cargo-nextest, GraalVM CE, tesseract with the English, German and Arabic
language data the OCR tests use, and just) comes from [devenv](https://devenv.sh). Enter it with `devenv shell`, or run
`direnv allow` once to load it on entry.

All commands are recipes in the `justfile`; run `just` to list them.

| Recipe | What it does |
| --- | --- |
| `just check` | Every gate in order: format check, lint, tests |
| `just build` | Build the crate, compiling Tika on first run |
| `just test` | Run the tests with cargo-nextest, then the doc tests |
| `just lint` | Clippy over every target, warnings fail |
| `just fmt` | Format every Rust source |
| `just bench` | Run the criterion benchmarks |
| `just tika-agent` | Run Tika under the GraalVM tracing agent to regenerate native-image metadata |

## License

Apache License 2.0, see [LICENSE](LICENSE). Originally developed by [Yobix AI](https://yobix.ai).
