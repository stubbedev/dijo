//! Compares a native parser with Tika's path, which it replaces, on the same documents: the
//! streamed text and XHTML, their final metadata, and the text and XHTML of a string extraction
//! cut by a small length limit. The only difference allowed is the parser's name in
//! `tk:parsed-by`, which the native output is compared under Tika's. Documents Tika's path fails
//! on are compared on their outcome alone: the native parser must fail too.

use std::io::Read;
use std::path::Path;

use crate::config::ParserConfigs;
use crate::formats::{self, Output, Routed, StreamOptions};
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

/// An extraction's result, or why it failed.
type Outcome = Result<Extracted, String>;

fn sorted(metadata: Metadata) -> Vec<(String, Vec<String>)> {
    let mut sorted: Vec<_> = metadata.into_iter().collect();
    sorted.sort();
    sorted
}

fn tika_stream(path: &str, configs: &ParserConfigs, output: Output, char_set: CharSet) -> Outcome {
    let options = tika::StreamOptions {
        char_set,
        as_xml: output == Output::Xml,
        timeout: None,
        embedded: None,
    };
    let (mut reader, _) = tika::parse_to_stream(
        isolate().unwrap(),
        Source::File(path),
        configs,
        None,
        &options,
    )
    .map_err(|e| e.to_string())?;
    let mut text = Vec::new();
    reader
        .read_to_end(&mut text)
        .map_err(|e| format!("reading failed: {e}"))?;
    Ok(Extracted {
        text,
        metadata: sorted(reader.metadata().map_err(|e| e.to_string())?),
    })
}

fn tika_string(path: &str, configs: &ParserConfigs, output: Output) -> Outcome {
    let (text, metadata) = tika::parse_to_string(
        isolate().unwrap(),
        Source::File(path),
        i32::try_from(SMALL_LIMIT).unwrap(),
        configs,
        None,
        output == Output::Xml,
    )
    .map_err(|e| e.to_string())?;
    Ok(Extracted {
        text: text.into_bytes(),
        metadata: sorted(metadata),
    })
}

fn routed(path: &str) -> Routed<'_> {
    formats::route(Source::File(path), false)
        .unwrap()
        .unwrap_or_else(|| panic!("{path} has no native parser, or its parser defers it to Tika"))
}

fn native_stream(
    path: &str,
    configs: &ParserConfigs,
    output: Output,
    char_set: CharSet,
) -> Outcome {
    let routed = routed(path);
    let options = StreamOptions {
        char_set,
        output,
        timeout: None,
    };
    let mut stream = formats::parse_to_stream(
        routed.parser,
        routed.input.into_owned(),
        routed.detected,
        configs.clone(),
        options,
    )
    .map_err(|e| e.to_string())?;
    let mut text = Vec::new();
    stream
        .read_to_end(&mut text)
        .map_err(|e| format!("reading failed: {e}"))?;
    Ok(Extracted {
        text,
        metadata: sorted(stream.metadata().map_err(|e| e.to_string())?),
    })
}

fn native_string(path: &str, configs: &ParserConfigs, output: Output) -> Outcome {
    let routed = routed(path);
    let (text, metadata) = formats::parse_to_string(
        routed.parser,
        &routed.input,
        &routed.detected,
        configs,
        SMALL_LIMIT,
        output,
    )
    .map_err(|e| e.to_string())?;
    Ok(Extracted {
        text: text.into_bytes(),
        metadata: sorted(metadata),
    })
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

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Ok(extracted) => format!(
            "{}\n         {:?}",
            String::from_utf8_lossy(&extracted.text),
            extracted.metadata
        ),
        Err(error) => format!("failed: {error}"),
    }
}

/// What Tika's path and the native parser are expected to do with a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// Both extract it, identically.
    Same,
    /// Both fail on it.
    Failure,
}

/// Asserts that the native parser of each document, which Tika's path parses with
/// `tika_name`, extracts exactly what Tika's path does under Tika's default settings.
pub(crate) fn assert_same_as_tika(documents: &[&Path], tika_name: &str) {
    assert_same_as_tika_with(documents, tika_name, &ParserConfigs::default());
}

/// Asserts that the native parser of each document, which Tika's path parses with
/// `tika_name`, extracts exactly what Tika's path does under the settings `configs`.
pub(crate) fn assert_same_as_tika_with(
    documents: &[&Path],
    tika_name: &str,
    configs: &ParserConfigs,
) {
    compare(documents, tika_name, configs, Expected::Same);
}

/// Asserts that Tika's path fails on each document, which it parses with `tika_name`, and that
/// the native parser fails too.
pub(crate) fn assert_fails_as_tika(documents: &[&Path], tika_name: &str) {
    compare(
        documents,
        tika_name,
        &ParserConfigs::default(),
        Expected::Failure,
    );
}

fn compare(documents: &[&Path], tika_name: &str, configs: &ParserConfigs, expected: Expected) {
    let mut differences = Vec::new();
    for document in documents {
        let path = document.to_str().unwrap();
        let native_name = routed(path).parser.name();
        for output in [Output::Text, Output::Xml] {
            let mut comparisons = vec![(
                "string",
                CharSet::UTF_8,
                tika_string(path, configs, output),
                native_string(path, configs, output),
            )];
            for char_set in [CharSet::UTF_8, CharSet::US_ASCII, CharSet::UTF_16BE] {
                comparisons.push((
                    "stream",
                    char_set,
                    tika_stream(path, configs, output, char_set),
                    native_stream(path, configs, output, char_set),
                ));
            }
            for (entry, char_set, tika, native) in comparisons {
                let native = native.map(|native| as_tika(native, native_name, tika_name, char_set));
                let as_expected = match (expected, &tika, &native) {
                    (Expected::Same, Ok(tika), Ok(native)) => tika == native,
                    (Expected::Failure, Err(_), Err(_)) => true,
                    _ => false,
                };
                if !as_expected {
                    differences.push(format!(
                        "{path} ({entry}, {output:?}, {char_set}), expected {expected:?}:\n  tika   {}\n  native {}",
                        describe(&tika),
                        describe(&native),
                    ));
                }
            }
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
