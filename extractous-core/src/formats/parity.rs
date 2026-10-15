//! Compares a native parser with Tika's path, which it replaces, on the same documents: the
//! streamed text and XHTML, their final metadata, and the text and XHTML of a string extraction
//! cut by a small length limit. The only difference allowed is the parser's name in
//! `tk:parsed-by`, which the native output is compared under Tika's.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::config::ParserConfigs;
use crate::formats::{self, Output, Parser, Routed, StreamOptions};
use crate::isolate::isolate;
use crate::tika::{self, Source};
use crate::{CharSet, Metadata};

const SMALL_LIMIT: usize = 100;

/// One extraction's result, as the two paths are compared on it.
#[derive(Debug, PartialEq, Eq)]
struct Extracted {
    text: Vec<u8>,
    metadata: Vec<(String, Vec<String>)>,
}

fn sorted(metadata: Metadata) -> Vec<(String, Vec<String>)> {
    let mut sorted: Vec<_> = metadata.into_iter().collect();
    sorted.sort();
    sorted
}

fn tika_stream(path: &str, output: Output, char_set: CharSet) -> Extracted {
    let options = tika::StreamOptions {
        char_set,
        as_xml: output == Output::Xml,
        timeout: None,
        embedded: None,
    };
    let (mut reader, _) = tika::parse_to_stream(
        isolate().unwrap(),
        Source::File(path),
        &ParserConfigs::default(),
        None,
        &options,
    )
    .unwrap_or_else(|e| panic!("Tika could not extract {path}: {e}"));
    let mut text = Vec::new();
    reader.read_to_end(&mut text).unwrap();
    Extracted {
        text,
        metadata: sorted(reader.metadata().unwrap()),
    }
}

fn tika_string(path: &str, output: Output) -> Extracted {
    let (text, metadata) = tika::parse_to_string(
        isolate().unwrap(),
        Source::File(path),
        i32::try_from(SMALL_LIMIT).unwrap(),
        &ParserConfigs::default(),
        None,
        output == Output::Xml,
    )
    .unwrap_or_else(|e| panic!("Tika could not extract {path}: {e}"));
    Extracted {
        text: text.into_bytes(),
        metadata: sorted(metadata),
    }
}

fn routed(path: &str) -> Routed<'_> {
    formats::route(Source::File(path), false)
        .unwrap()
        .unwrap_or_else(|| panic!("{path} has no native parser, or its parser defers it to Tika"))
}

fn native_stream(path: &str, output: Output, char_set: CharSet) -> Extracted {
    let routed = routed(path);
    let options = StreamOptions {
        char_set,
        output,
        timeout: None,
    };
    let (mut stream, _) = formats::parse_to_stream(
        routed.parser,
        routed.input.into_owned(),
        routed.detected,
        options,
    )
    .unwrap();
    let mut text = Vec::new();
    stream.read_to_end(&mut text).unwrap();
    Extracted {
        text,
        metadata: sorted(stream.metadata().unwrap()),
    }
}

fn native_string(path: &str, output: Output) -> Extracted {
    let routed = routed(path);
    let (text, metadata) = formats::parse_to_string(
        routed.parser,
        &routed.input,
        &routed.detected,
        SMALL_LIMIT,
        output,
    )
    .unwrap();
    Extracted {
        text: text.into_bytes(),
        metadata: sorted(metadata),
    }
}

/// The native result as Tika's path would name its parser.
fn as_tika(native: Extracted, native_name: &str, tika_name: &str, char_set: CharSet) -> Extracted {
    let mut native_bytes = Vec::new();
    let mut tika_bytes = Vec::new();
    formats::encode(native_name, char_set, &mut native_bytes);
    formats::encode(tika_name, char_set, &mut tika_bytes);
    Extracted {
        text: replace_bytes(&native.text, &native_bytes, &tika_bytes),
        metadata: native
            .metadata
            .into_iter()
            .map(|(name, values)| {
                let values = values
                    .into_iter()
                    .map(|value| {
                        if value == native_name {
                            tika_name.to_owned()
                        } else {
                            value
                        }
                    })
                    .collect();
                (name, values)
            })
            .collect(),
    }
}

fn replace_bytes(haystack: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut replaced = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(index) = rest.windows(from.len()).position(|window| window == from) {
        replaced.extend_from_slice(&rest[..index]);
        replaced.extend_from_slice(to);
        rest = &rest[index + from.len()..];
    }
    replaced.extend_from_slice(rest);
    replaced
}

/// The non-empty files under `roots` that Tika's detection gives a type `parser` reads, split into
/// those it parses and those it defers to Tika.
pub(crate) fn documents(parser: &dyn Parser, roots: &[&str]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut pending: Vec<PathBuf> = roots.iter().map(PathBuf::from).collect();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            let entries = std::fs::read_dir(&path)
                .unwrap_or_else(|e| panic!("cannot list {}: {e}", path.display()));
            pending.extend(entries.map(|entry| entry.unwrap().path()));
        } else {
            files.push(path);
        }
    }
    files.sort();
    let (mut parsed, mut deferred) = (Vec::new(), Vec::new());
    for file in files {
        let path = file.to_str().unwrap();
        let (media_type, _) = tika::detect_with_metadata(isolate().unwrap(), Source::File(path))
            .unwrap_or_else(|e| panic!("Tika could not detect {path}: {e}"));
        if !parser.media_types().contains(&media_type.as_str()) {
            continue;
        }
        let input = std::fs::read(&file).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
        if input.is_empty() {
            continue;
        }
        if parser.defers(&input).is_some() {
            deferred.push(file);
        } else {
            parsed.push(file);
        }
    }
    (parsed, deferred)
}

/// Asserts that the native parser of each document, which Tika's path parses with
/// `tika_name`, extracts exactly what Tika's path does.
pub(crate) fn assert_same_as_tika(documents: &[&Path], tika_name: &str) {
    let mut differences = Vec::new();
    for document in documents {
        let path = document.to_str().unwrap();
        let native_name = routed(path).parser.name();
        for output in [Output::Text, Output::Xml] {
            let mut comparisons = vec![(
                "string",
                CharSet::UTF_8,
                tika_string(path, output),
                native_string(path, output),
            )];
            for char_set in [CharSet::UTF_8, CharSet::US_ASCII, CharSet::UTF_16BE] {
                comparisons.push((
                    "stream",
                    char_set,
                    tika_stream(path, output, char_set),
                    native_stream(path, output, char_set),
                ));
            }
            for (entry, char_set, tika, native) in comparisons {
                let native = as_tika(native, native_name, tika_name, char_set);
                if native != tika {
                    differences.push(format!(
                        "{path} ({entry}, {output:?}, {char_set}):\n  tika   {}\n         {:?}\n  native {}\n         {:?}",
                        String::from_utf8_lossy(&tika.text),
                        tika.metadata,
                        String::from_utf8_lossy(&native.text),
                        native.metadata,
                    ));
                }
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
