# Extractous

Extractous is a Rust crate that provides a unified approach for detecting and extracting metadata and text content from
various documents
types such as PDF, Word, HTML, and [many other formats](#supported-file-formats).

## Features

* High-level Rust API for extracting text and metadata content for [many file formats](#supported-file-formats).
* Strives to be efficient and fast.
* Internally it calls the [Apache Tika](https://tika.apache.org/) for any file format that is not natively supported in the Rust core.
* Comprehensive documentation and examples to help you get started quickly.

## Usage

* Create and configure an `Extractor` instance
```rust
use extractous::Extractor;
use extractous::PdfParserConfig;

fn main() {
    // Create a new extractor. Note it uses the consuming builder pattern
    let mut extractor = Extractor::new()
        .set_extract_string_max_length(1000);

    // can also perform conditional configuration
    let custom_pdf_config = true;
    if custom_pdf_config {
        extractor = extractor.set_pdf_config(
            PdfParserConfig::new().set_extract_annotation_text(false)
        );
    }
}
```

* Extracting a content of a file to a `String`
```rust
use extractous::Extractor;

fn main() {
  // Get the command-line arguments
  let args: Vec<String> = std::env::args().collect();
  let file_path = &args[1];

  // Extract the provided file content to a string
  let mut extractor = Extractor::new();
  // if you need an xml
  // extractor = extractor.set_xml_output(false);
  // Extract text from a file
  let (content, metadata) = extractor.extract_file_to_string(file_path).unwrap();
  println!("{}", content);
  println!("{:?}", metadata);
}
```

* Extract a content of a file(URL/ bytes) to a `StreamReader` and perform buffered reading
```rust
use std::io::{BufReader, Read};
// use std::fs::File; use for bytes
use extractous::Extractor;

fn main() {
  // Get the command-line arguments
  let args: Vec<String> = std::env::args().collect();
  let file_path = &args[1];

  // Extract the provided file content to a string
  let extractor = Extractor::new();
  let (stream, metadata) = extractor.extract_file(file_path).unwrap();
  // Extract url
  // let (stream, metadata) = extractor.extract_url("https://www.google.com/").unwrap();
  // Extract bytes
  // let mut file = File::open(file_path)?;
  // let mut buffer = Vec::new();
  // file.read_to_end(&mut buffer)?;
  // let (stream, metadata) = extractor.extract_bytes(&file_bytes);

  // Because stream implements std::io::Read trait we can perform buffered reading
  // For example we can use it to create a BufReader
  let mut reader = BufReader::new(stream);
  let mut buffer = Vec::new();
  reader.read_to_end(&mut buffer).unwrap();

  println!("{}", String::from_utf8(buffer).unwrap());
  println!("{:?}", metadata);
}
```

* Extract content of PDF with OCR. You need to have Tesseract installed with the language pack. For example on debian `sudo apt install tesseract-ocr tesseract-ocr-deu`
* If you get `Parse error occurred : Unable to extract PDF content`, it is most likely that OCR language pack is not installed
```rust
use extractous::Extractor;

fn main() {
  let file_path = "../test_files/documents/deu-ocr.pdf";

  let extractor = Extractor::new()
          .set_ocr_config(TesseractOcrConfig::new().set_language("deu"))
          .set_pdf_config(PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::OCR_ONLY));
  // extract file with extractor
  let (content, metadata) = extractor.extract_file_to_string(file_path).unwrap();
  println!("{}", content);
  println!("{:?}", metadata);
}
```


## Building

### Requirements
* Extractous uses [Apache Tika](https://tika.apache.org/) for file formats that are not natively supported in Rust.
  However, to achieve one of Extractous goals, which is speed and efficiency, we do not set up any Tika as a servers or
  run any Java code. We instead, compile [Apache Tika](https://tika.apache.org/) as native shared libraries and use
  them on our Rust core as ffi. [GraalVm](https://www.graalvm.org/) is required to build Tika as native libs.
* Every development dependency (Rust toolchain, cargo-nextest, GraalVM CE, tesseract with the English, German and
  Arabic language data used by the OCR tests, and just) is provided by
  [devenv](https://devenv.sh). Enter it with `devenv shell`, or run `direnv allow` once to load it automatically.

### Building and testing
* All commands are recipes in the repository `justfile`; run `just` to list them.
* `just build` builds Extractous, `just test` runs the tests with cargo-nextest and `just check` runs every gate.
