//! Compares a native parser with Tika's path, which it replaces, on the same documents: the
//! streamed text and XHTML, their final metadata, and the text and XHTML of a string extraction
//! cut by a small length limit. The only difference allowed is the parser's name in
//! `tk:parsed-by`, which the native output is compared under Tika's. Documents Tika's path fails
//! on are compared on their outcome alone: the native parser must fail too. A native parser that
//! records the parsers its Tika counterpart delegates to is compared under their Tika names too.
//!
//! Three things the native path writes in a defined order or chain where Tika's does not are
//! compared as both can give them: each run of `meta` elements sorted by name, which Tika writes
//! in the order of its metadata's `HashMap` (#64); the namespace declarations of each start tag
//! sorted, which Tika writes in the order of a `HashMap` of pending mappings (#87); and the
//! native string extraction written through Tika's string chain, which leaves anchored blocks
//! where they cut their paragraph, where the native one holds them back as a stream does (#63).
//!
//! Two things of a document's embedded documents, which Tika parses either way, are compared as
//! they can be: the parsers that read them, each once as Tika's record keeps it, once the native
//! parser goes by Tika's name; and the stack traces of their failures without the Java frames
//! below the embedded document's parse, the embedding parser's in Tika's path and the native
//! bridge's in the other, which no native parser runs, and with the random numbers of Tika's
//! temporary files, and the suffixes the native bridge gives them, masked. A failure the parser
//! records itself (`embedded-stream-exception`), or a warning it records (`warn`), is compared
//! without its frames, all of them the Java parser's. In a string extraction, the native path,
//! which parses an embedded document whole, may go on to more parsers and failures than Tika's,
//! which stops at the length limit; Tika's must lead the native ones.
//!
//! What OCR reads is not compared, the quality corpus scores it instead: the text in each div of
//! class `ocr`, in the XHTML and where it stands in the plain text, and the values of the
//! reports of how each image was read. The rest of a document OCR reads is compared exactly, in
//! its streamed UTF-8 text and XHTML, and the native parser must read text in each div Tika's
//! path reads text in. Its string extractions are not compared, since where the length limit
//! cuts them depends on the length of what OCR reads, nor its other character sets, which OCR
//! has no part in.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::config::{ParserConfigs, Password};
use crate::formats::embedded::EXCEPTION_WARNING;
use crate::formats::{self, Output, Parser, Routed, StreamOptions};
use crate::isolate::isolate;
use crate::tika::{self, Source};
use crate::{CharSet, Metadata};

const SMALL_LIMIT: usize = 100;

/// Why the native path extracts a Photoshop image Tika's path fails on: with OCR on, our Java
/// OCR path converts the image's colours with AWT, whose colour management throws
/// `CMMException: Invalid profile` on its ICC profile; the native path prepares it without.
pub(crate) const PSD_PROFILE_AWT_REJECTS: &str = "Tika's OCR path throws CMMException on the \
                                                  image's colour profile; the native path reads \
                                                  it";

/// Why the native path extracts Zstandard files Tika's path leaves empty: Tika's native image
/// lacks zstd-jni, so its path parses them with `EmptyParser`.
pub(crate) const ZSTD_WITHOUT_ZSTD_JNI: &str = "Tika's path parses Zstandard with EmptyParser, as \
                                                its native image lacks zstd-jni; the native path \
                                                decodes it";

/// One extraction's result, as the two paths are compared on it.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    let (mut reader, _) =
        tika::parse_to_stream(isolate().unwrap(), Source::File(path), configs, &options)
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
        output == Output::Xml,
    )
    .map_err(|e| e.to_string())?;
    Ok(Extracted {
        text: text.into_bytes(),
        metadata: sorted(metadata),
    })
}

/// The error Tika's path fails with on the document at `path`.
fn tika_error(path: &Path, configs: &ParserConfigs) -> crate::Error {
    tika::parse_to_string(
        isolate().unwrap(),
        Source::File(path.to_str().unwrap()),
        i32::try_from(SMALL_LIMIT).unwrap(),
        configs,
        false,
    )
    .map(|_| ())
    .expect_err("Tika's path fails on the document")
}

fn routed(path: &str) -> Routed<'_> {
    formats::route(Source::File(path))
        .unwrap()
        .unwrap_or_else(|| panic!("{path} has no native parser"))
}

fn deferred(path: &str) -> String {
    format!("the native parser leaves {path} to Tika")
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
        spool: None,
    };
    let (mut stream, _) = formats::parse_to_stream(
        routed.parser,
        routed.input.into_owned(),
        routed.detected,
        configs.clone(),
        options,
    )
    .map_err(|e| e.to_string())?
    .unwrap_or_else(|| panic!("{}", deferred(path)));
    let mut text = Vec::new();
    stream
        .read_to_end(&mut text)
        .map_err(|e| format!("reading failed: {e}"))?;
    Ok(Extracted {
        text,
        metadata: sorted(stream.metadata().map_err(|e| e.to_string())?),
    })
}

/// The native string extraction, through Tika's string chain, which leaves anchored blocks
/// where they are (#63): the native one holds them back as a stream does, which the comparisons
/// of the streams cover.
fn native_string(path: &str, configs: &ParserConfigs, output: Output) -> Outcome {
    let routed = routed(path);
    let (text, metadata) = formats::parse_to_string_through(
        routed.parser,
        &routed.input,
        &routed.detected,
        configs,
        formats::Chain::TikaString {
            output,
            max_length: SMALL_LIMIT,
        },
    )
    .map_err(|e| e.to_string())?
    .unwrap_or_else(|| panic!("{}", deferred(path)));
    Ok(Extracted {
        text: text.into_bytes(),
        metadata: sorted(metadata),
    })
}

/// The native names of the parsers of a document paired with Tika's: the parser `native_name`
/// Tika's path names `tika_name`, its `delegates`, then every native parser and the parsers it
/// delegates to, which its embedded documents may record.
fn tika_names<'n>(
    native_name: &'n str,
    tika_name: &'n str,
    delegates: &[(&'n str, &'n str)],
) -> Vec<(&'n str, &'n str)> {
    [(native_name, tika_name)]
        .into_iter()
        .chain(delegates.iter().copied())
        .chain(formats::PARSERS.iter().flat_map(|parser| {
            std::iter::once((parser.name(), parser.tika_name()))
                .chain(parser.tika_delegates().iter().copied())
        }))
        .collect()
}

/// The native result as Tika's path would name its parsers: each native name of `names` as the
/// Tika name paired with it.
fn as_tika(native: Extracted, names: &[(&str, &str)], char_set: CharSet) -> Extracted {
    let mut text = native.text;
    for (native_name, tika_name) in names {
        let mut native_bytes = Vec::new();
        let mut tika_bytes = Vec::new();
        formats::encode(native_name, char_set, &mut native_bytes);
        formats::encode(tika_name, char_set, &mut tika_bytes);
        text = replace_bytes(&text, &native_bytes, &tika_bytes);
    }
    Extracted {
        text,
        metadata: native
            .metadata
            .into_iter()
            .map(|(name, values)| {
                let values = values
                    .into_iter()
                    .map(|value| {
                        names
                            .iter()
                            .find(|(native_name, _)| *native_name == value)
                            .map_or(value, |(_, tika_name)| (*tika_name).to_owned())
                    })
                    .collect();
                (name, values)
            })
            .collect(),
    }
}

const PARSED_BY_FULL_SET: &str = "tk:parsed-by-full-set";
const EMBEDDED_EXCEPTION: &str = "tk:exception:embedded-exception";
const EMBEDDED_STREAM_EXCEPTION: &str = "tk:exception:embedded-stream-exception";
/// Where the XHTML head writes a recorded warning.
const WARNING_META: &str = "<meta name=\"tk:exception:warn\" content=\"";
/// The first frame below an embedded document's parse in Tika's path, where the embedding
/// parser hands it to the embedded document extractor.
const TIKA_EMBEDDING_FRAME: &str = "\tat org.apache.tika.parser.DelegatingParser.parse(";
/// The first frame below an embedded document's parse in the native path.
const NATIVE_EMBEDDING_FRAME: &str = "\tat ai.yobix.TikaNativeMain.parseEmbedded(";

/// An extraction, its text in `char_set`, as the two paths are compared on it: each parser of
/// the full set once, the failures of embedded documents without the frames that embedded them,
/// and the exceptions a parser records without their frames, which a native parser does not
/// have.
fn comparable(mut extracted: Extracted, char_set: CharSet) -> Extracted {
    extracted.text = without_warning_frames(extracted.text, char_set);
    extracted.text = in_defined_order(extracted.text, char_set);
    for (name, values) in &mut extracted.metadata {
        if name == PARSED_BY_FULL_SET {
            let mut once: Vec<String> = Vec::with_capacity(values.len());
            for value in values.drain(..) {
                if !once.contains(&value) {
                    once.push(value);
                }
            }
            *values = once;
        } else if name == EMBEDDED_EXCEPTION {
            for value in values.iter_mut() {
                *value = without_temporary_names(&without_embedding_frames(value));
            }
        } else if name == EMBEDDED_STREAM_EXCEPTION || name == EXCEPTION_WARNING {
            for value in values.iter_mut() {
                *value = without_frames(value);
            }
        }
    }
    extracted
}

/// What both paths' failures of an embedded document a native parser failed on are compared
/// as: the native path records a line of its own (#68) where Tika's records the stack trace of
/// its parser's exception.
const NATIVE_FAILURE: &str = "<a failure the native path records itself (#68)>";

/// The two results with each failure of an embedded document the native path records itself,
/// and Tika's failure in its place, compared as [`NATIVE_FAILURE`].
fn with_native_failures_masked(tika: &mut Extracted, native: &mut Extracted) {
    let Some((_, natives)) = native
        .metadata
        .iter_mut()
        .find(|(name, _)| name == EMBEDDED_EXCEPTION)
    else {
        return;
    };
    let Some((_, tikas)) = tika
        .metadata
        .iter_mut()
        .find(|(name, _)| name == EMBEDDED_EXCEPTION)
    else {
        return;
    };
    for (native, tika) in natives.iter_mut().zip(tikas.iter_mut()) {
        let recorded = native.starts_with("extractous::")
            && native.contains(" could not parse ")
            && !native.contains('\n');
        if recorded && tika.contains('\n') {
            NATIVE_FAILURE.clone_into(native);
            NATIVE_FAILURE.clone_into(tika);
        }
    }
}

/// A string extraction's native result with the embedded documents' parsers and failures that
/// follow Tika's left out: the native path parses an embedded document whole, while Tika's stops
/// at the length limit, before the parsers and failures of what comes after it.
fn cut_to(mut native: Extracted, tika: &Extracted) -> Extracted {
    for (name, values) in &mut native.metadata {
        if ![
            PARSED_BY_FULL_SET,
            EMBEDDED_EXCEPTION,
            EMBEDDED_STREAM_EXCEPTION,
        ]
        .contains(&name.as_str())
        {
            continue;
        }
        let expected = tika
            .metadata
            .iter()
            .find(|(tika_name, _)| tika_name == name)
            .map_or(&[][..], |(_, values)| values.as_slice());
        if values.starts_with(expected) {
            values.truncate(expected.len());
        }
    }
    native.metadata.retain(|(_, values)| !values.is_empty());
    native
}

/// A Java stack trace without its frames, its lines ended by `\n`: Java ends them with the
/// platform's line separator, the native parsers with `\n` everywhere (#108).
fn without_frames(trace: &str) -> String {
    trace
        .split_inclusive('\n')
        .filter(|line| !line.starts_with('\t'))
        .collect::<String>()
        .replace("\r\n", "\n")
}

/// Text written in `char_set`, decoded, unless it is not valid in it.
fn decoded(text: &[u8], char_set: CharSet) -> Option<String> {
    match char_set {
        CharSet::UTF_8 | CharSet::US_ASCII => String::from_utf8(text.to_vec()).ok(),
        CharSet::UTF_16BE => {
            let (pairs, rest) = text.as_chunks::<2>();
            let units: Vec<u16> = pairs.iter().map(|&pair| u16::from_be_bytes(pair)).collect();
            rest.is_empty()
                .then(|| String::from_utf16(&units).ok())
                .flatten()
        }
    }
}

/// The text of an extraction, in `char_set`, with the recorded warnings the XHTML head writes
/// without their frames.
fn without_warning_frames(text: Vec<u8>, char_set: CharSet) -> Vec<u8> {
    let Some(decoded) = decoded(&text, char_set).filter(|decoded| decoded.contains(WARNING_META))
    else {
        return text;
    };
    let mut kept = String::with_capacity(decoded.len());
    let mut rest = decoded.as_str();
    while let Some(at) = rest.find(WARNING_META) {
        let start = at + WARNING_META.len();
        let end = rest[start..]
            .find('"')
            .map_or(rest.len(), |end| start + end);
        kept.push_str(&rest[..start]);
        kept.push_str(&without_frames(&rest[start..end]));
        rest = &rest[end..];
    }
    kept.push_str(rest);
    let mut encoded = Vec::with_capacity(text.len());
    formats::encode(&kept, char_set, &mut encoded);
    encoded
}

/// The text of an extraction, in `char_set`, with what Tika writes in the order of a
/// `java.util.HashMap` put in the order the native path writes it: each run of `meta` elements,
/// which Tika writes in the order of its metadata's map and the native head sorted by name
/// (#64), sorted by name, the values of one name kept in their order; and the namespace
/// declarations of each start tag, which Tika writes in the order of a map of its pending
/// mappings and the native path by prefix (#87), sorted by prefix.
fn in_defined_order(text: Vec<u8>, char_set: CharSet) -> Vec<u8> {
    let Some(decoded) = decoded(&text, char_set).filter(|decoded| decoded.contains('<')) else {
        return text;
    };
    let mut ordered = String::with_capacity(decoded.len());
    let mut rest = decoded.as_str();
    let mut metas: Vec<(&str, &str)> = Vec::new();
    while let Some(at) = rest.find('<') {
        let end = tag_end(&rest[at..]).map_or(rest.len(), |end| at + end);
        let tag = &rest[at..end];
        if at > 0 {
            flush_metas(&mut metas, &mut ordered);
            ordered.push_str(&rest[..at]);
        }
        if tag.starts_with("<meta ") {
            let line = if rest[end..].starts_with('\n') {
                end + 1
            } else {
                end
            };
            metas.push((attribute(tag, "name").unwrap_or_default(), &rest[at..line]));
            rest = &rest[line..];
        } else {
            flush_metas(&mut metas, &mut ordered);
            ordered.push_str(&with_sorted_declarations(tag));
            rest = &rest[end..];
        }
    }
    flush_metas(&mut metas, &mut ordered);
    ordered.push_str(rest);
    let mut encoded = Vec::with_capacity(text.len());
    formats::encode(&ordered, char_set, &mut encoded);
    encoded
}

/// Text written in `char_set` with `departure` applied to it, unless it is not valid in it.
fn departed(text: Vec<u8>, char_set: CharSet, departure: fn(&str) -> String) -> Vec<u8> {
    let Some(decoded) = decoded(&text, char_set) else {
        return text;
    };
    let mut encoded = Vec::with_capacity(text.len());
    formats::encode(&departure(&decoded), char_set, &mut encoded);
    encoded
}

/// Writes a run of `meta` elements sorted by name, stably.
fn flush_metas(metas: &mut Vec<(&str, &str)>, ordered: &mut String) {
    metas.sort_by_key(|(name, _)| *name);
    for (_, element) in metas.drain(..) {
        ordered.push_str(element);
    }
}

/// Where the tag `text` starts with ends, after its `>`: attribute values hold no `"`, which
/// both paths escape.
fn tag_end(text: &str) -> Option<usize> {
    let mut quoted = false;
    for (index, character) in text.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '>' if !quoted => return Some(index + 1),
            _ => {}
        }
    }
    None
}

/// The value of the attribute `name` of a start tag, as written.
fn attribute<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
    let marker = format!(" {name}=\"");
    let start = tag.find(&marker)? + marker.len();
    let end = start + tag[start..].find('"')?;
    Some(&tag[start..end])
}

/// A start tag with the namespace declarations it ends with sorted by prefix.
fn with_sorted_declarations(tag: &str) -> String {
    let Some(first) = tag.find(" xmlns") else {
        return tag.to_owned();
    };
    let mut declarations: Vec<&str> = Vec::new();
    let mut at = first;
    while tag[at..].starts_with(" xmlns") {
        let Some(end) = tag[at..].find("=\"").and_then(|open| {
            tag[at + open + 2..]
                .find('"')
                .map(|shut| at + open + 3 + shut)
        }) else {
            break;
        };
        declarations.push(&tag[at..end]);
        at = end;
    }
    let close = at;
    declarations.sort_unstable();
    format!(
        "{}{}{}",
        &tag[..first],
        declarations.concat(),
        &tag[close..]
    )
}

/// The text with the random number and the suffix of each of Tika's temporary files,
/// `apache-tika-<digits>.<suffix>`, masked: the suffix is the extension of the name the
/// embedded document's stream was made with, which the native bridge gives it and Tika's path,
/// making the stream from bytes alone, does not.
fn without_temporary_names(text: &str) -> String {
    const PREFIX: &str = "apache-tika-";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        out.push_str(&rest[..at + PREFIX.len()]);
        rest =
            rest[at + PREFIX.len()..].trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
        if let Some(suffix) = rest.strip_prefix('.') {
            rest = suffix.trim_start_matches(|c: char| c.is_ascii_alphanumeric());
        }
        out.push('N');
    }
    out.push_str(rest);
    out
}

/// A Java stack trace without, in each of its exceptions, the frames from the last one that
/// embedded the document on, and without the counts of frames a cause shares with its effect.
fn without_embedding_frames(trace: &str) -> String {
    let lines: Vec<&str> = trace.split_inclusive('\n').collect();
    let mut kept = String::with_capacity(trace.len());
    let mut block_start = 0;
    while block_start < lines.len() {
        let mut block_end = block_start + 1;
        while block_end < lines.len() && lines[block_end].starts_with('\t') {
            block_end += 1;
        }
        let block = &lines[block_start..block_end];
        let cut = block
            .iter()
            .rposition(|line| {
                line.starts_with(TIKA_EMBEDDING_FRAME) || line.starts_with(NATIVE_EMBEDDING_FRAME)
            })
            .unwrap_or(block.len());
        for line in &block[..cut] {
            let shared = line.starts_with("\t... ") && line.trim_end().ends_with(" more");
            if !shared {
                kept.push_str(line);
            }
        }
        block_start = block_end;
    }
    kept
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
#[derive(Debug, Clone, Copy)]
enum Expected<'a> {
    /// Both extract it identically.
    Same(Naming<'a>),
    /// Both fail on it.
    Failure,
}

/// How Tika's path names the parsers of a document the native parser extracts.
#[derive(Debug, Clone, Copy)]
struct Naming<'a> {
    /// The class name of the parser of the document.
    tika_name: &'a str,
    /// The Tika names of the parsers it delegates to, each paired after its native name.
    delegates: &'a [(&'a str, &'a str)],
    /// A departure of the native parser from Tika's path that a quirk issue names, if the
    /// comparison expects one.
    departure: Option<Departure>,
}

/// Where a native parser departs from Tika's path on purpose, as a comparison undoes it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Departure {
    /// What undoes the departure in a native text.
    pub(crate) undo: fn(&str) -> String,
    /// Whether the departure moves where a string extraction's length limit cuts the text, as
    /// one in the head does, whose line breaks the limit counts: a string extraction the
    /// departure changes is then not compared.
    pub(crate) moves_the_cut: bool,
}

/// Asserts that the native parser of each document, which Tika's path parses with
/// `tika_name`, extracts what Tika's path does under Tika's default settings once `departure`
/// is undone in each native text.
pub(crate) fn assert_same_as_tika_departing(
    documents: &[&Path],
    tika_name: &str,
    departure: Departure,
) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates: &[],
        departure: Some(departure),
    });
    compare(documents, &ParserConfigs::default(), expected);
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
    let expected = Expected::Same(Naming {
        tika_name,
        delegates: &[],
        departure: None,
    });
    compare(documents, configs, expected);
}

/// Asserts that of `documents`, each one `passwords` names, by file name with the password that
/// opens it, fails without a password and with a wrong one as Tika's path fails on it, with the
/// same error, and extracts with its password as Tika's path extracts it; and that the rest
/// extract as Tika's path extracts them.
pub(crate) fn assert_same_as_tika_with_passwords(
    documents: &[PathBuf],
    tika_name: &str,
    passwords: &[(&str, &str)],
) {
    let password_of = |document: &Path| {
        let name = document.file_name().unwrap().to_string_lossy();
        passwords
            .iter()
            .find(|(encrypted, _)| *encrypted == name)
            .map(|(_, password)| Password::new(*password))
    };
    let (encrypted, plain): (Vec<&Path>, Vec<&Path>) = documents
        .iter()
        .map(PathBuf::as_path)
        .partition(|document| password_of(document).is_some());
    assert_same_as_tika_with(&plain, tika_name, &ParserConfigs::default());
    assert_fails_as_tika(&encrypted);
    let wrong = ParserConfigs::default().with_password(Some(&Password::new("wrong")));
    for document in encrypted {
        let password = password_of(document).expect("an encrypted document");
        let configs = ParserConfigs::default().with_password(Some(&password));
        assert_same_as_tika_with(&[document], tika_name, &configs);
        for configs in [ParserConfigs::default(), wrong.clone()] {
            assert_eq!(
                format!("{:?}", native_error(document, &configs)),
                format!("{:?}", tika_error(document, &configs)),
                "{document:?}"
            );
        }
    }
}

/// The error the native parser fails with on the document at `path`.
fn native_error(path: &Path, configs: &ParserConfigs) -> crate::Error {
    let routed = routed(path.to_str().unwrap());
    formats::parse_to_string(
        routed.parser,
        &routed.input,
        &routed.detected,
        configs,
        SMALL_LIMIT,
        Output::Text,
    )
    .map(|_| ())
    .expect_err("the native parser fails on the document")
}

/// Asserts that the native parser of each document, which Tika's path parses with
/// `tika_name`, extracts exactly what Tika's path does under Tika's default settings, the
/// parsers it delegates to named as `delegates` pairs their native names with Tika's.
pub(crate) fn assert_same_as_tika_delegating(
    documents: &[&Path],
    tika_name: &str,
    delegates: &[(&str, &str)],
) {
    assert_same_as_tika_delegating_with(documents, tika_name, delegates, &ParserConfigs::default());
}

/// [`assert_same_as_tika_delegating`] under the settings `configs`.
pub(crate) fn assert_same_as_tika_delegating_with(
    documents: &[&Path],
    tika_name: &str,
    delegates: &[(&str, &str)],
    configs: &ParserConfigs,
) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates,
        departure: None,
    });
    compare(documents, configs, expected);
}

/// Where a div of class `ocr` starts, the text OCR read in it following.
const OCR_DIV: &str = "<div class=\"ocr\">";
/// A div of class `ocr` OCR read nothing in.
const EMPTY_OCR_DIV: &str = "<div class=\"ocr\" />";

/// The text OCR read in each div of class `ocr` of an XHTML extraction, in order.
fn ocr_texts(xhtml: &str) -> Vec<String> {
    let xhtml = xhtml.replace(EMPTY_OCR_DIV, "<div class=\"ocr\"></div>");
    xhtml
        .split(OCR_DIV)
        .skip(1)
        .map(|read| {
            let text = &read[..read.find("</div>").unwrap_or(read.len())];
            text.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&amp;", "&")
        })
        .collect()
}

/// The text OCR read in each div of class `ocr`, by Tika's path and by the native parser, as
/// their streamed UTF-8 XHTML has it, when either read any.
type OcrTexts = (Vec<String>, Vec<String>);

/// An extraction in `output`, written in `char_set`, without what OCR read: the text in each
/// div of class `ocr` of XHTML, the `texts` read where they stand in plain text, and the
/// values of the reports of how each image was read.
fn without_ocr(
    mut extracted: Extracted,
    output: Output,
    char_set: CharSet,
    texts: &[String],
) -> Extracted {
    let Some(text) = decoded(&extracted.text, char_set) else {
        return extracted;
    };
    let mut kept = String::with_capacity(text.len());
    match output {
        Output::Xml => {
            let text = text.replace(EMPTY_OCR_DIV, "<div class=\"ocr\"></div>");
            let mut rest = text.as_str();
            while let Some(start) = rest.find(OCR_DIV) {
                let inside = start + OCR_DIV.len();
                kept.push_str(&rest[..inside]);
                rest = &rest[inside + rest[inside..].find("</div>").unwrap_or(0)..];
            }
            kept.push_str(rest);
            let report = format!("<meta name=\"{}\" content=\"", crate::ocr::REPORT);
            let unreported = std::mem::take(&mut kept);
            let mut rest = unreported.as_str();
            while let Some(start) = rest.find(&report) {
                let inside = start + report.len();
                kept.push_str(&rest[..inside]);
                rest = &rest[inside..];
                rest = &rest[rest.find('"').unwrap_or(rest.len())..];
            }
            kept.push_str(rest);
        }
        _ => {
            let mut rest = text.as_str();
            for read in texts.iter().filter(|read| !read.is_empty()) {
                let mut written = Vec::new();
                formats::encode(read, char_set, &mut written);
                let written = decoded(&written, char_set).expect("text it encoded itself");
                if let Some(at) = rest.find(&written) {
                    kept.push_str(&rest[..at]);
                    rest = &rest[at + written.len()..];
                }
            }
            kept.push_str(rest);
        }
    }
    extracted.text = kept.into_bytes();
    for (name, values) in &mut extracted.metadata {
        if name == crate::ocr::REPORT {
            values
                .iter_mut()
                .for_each(|value| *value = "<report>".to_owned());
        }
    }
    extracted
}

/// Asserts that Tika's path fails on each document, and that the native parser fails too.
pub(crate) fn assert_fails_as_tika(documents: &[&Path]) {
    assert_fails_as_tika_with(documents, &ParserConfigs::default());
}

/// [`assert_fails_as_tika`] under the settings `configs`.
pub(crate) fn assert_fails_as_tika_with(documents: &[&Path], configs: &ParserConfigs) {
    compare(documents, configs, Expected::Failure);
}

fn compare(documents: &[&Path], configs: &ParserConfigs, expected: Expected) {
    let mut differences = Vec::new();
    for document in documents {
        let path = document.to_str().unwrap();
        let native_name = routed(path).parser.name();
        let first = ("stream", Output::Xml, CharSet::UTF_8);
        let mut comparisons = vec![extracted(path, configs, first)];
        let ocr = read_by_ocr(&comparisons[0]);
        let rest = match ocr {
            Some(_) => vec![("stream", Output::Text, CharSet::UTF_8)],
            None => [Output::Text, Output::Xml]
                .into_iter()
                .flat_map(|output| {
                    [CharSet::UTF_8, CharSet::US_ASCII, CharSet::UTF_16BE]
                        .map(|char_set| ("stream", output, char_set))
                        .into_iter()
                        .chain([("string", output, CharSet::UTF_8)])
                })
                .filter(|way| *way != first)
                .collect(),
        };
        comparisons.extend(rest.into_iter().map(|way| extracted(path, configs, way)));
        if let Some((tika, native)) = &ocr {
            for (tika, native) in tika.iter().zip(native) {
                if !tika.trim().is_empty() && native.trim().is_empty() {
                    differences.push(format!(
                        "{path}: the native parser's OCR read nothing where Tika's path read \
                         {tika:?}"
                    ));
                }
            }
        }
        for (entry, output, char_set, tika, native) in comparisons {
            let (tika, native) = match (expected, tika, native) {
                (Expected::Same(naming), Ok(tika), Ok(native)) => {
                    let names = tika_names(native_name, naming.tika_name, naming.delegates);
                    let mut tika = comparable(tika, char_set);
                    let mut native = comparable(as_tika(native, &names, char_set), char_set);
                    with_native_failures_masked(&mut tika, &mut native);
                    if let Some(departure) = naming.departure {
                        let text = departed(native.text.clone(), char_set, departure.undo);
                        if departure.moves_the_cut && entry == "string" && text != native.text {
                            continue;
                        }
                        native.text = text;
                    }
                    let (tika, native) = match &ocr {
                        Some((tika_read, native_read)) => (
                            without_ocr(tika, output, char_set, tika_read),
                            without_ocr(native, output, char_set, native_read),
                        ),
                        None if entry == "string" => {
                            let native = cut_to(native, &tika);
                            (tika, native)
                        }
                        None => (tika, native),
                    };
                    if tika == native {
                        continue;
                    }
                    (Ok(tika), Ok(native))
                }
                (Expected::Failure, Err(_), Err(_)) => continue,
                (_, tika, native) => (tika, native),
            };
            differences.push(format!(
                "{path} ({entry}, {output:?}, {char_set}), expected {expected:?}:\n  tika   {}\n  native {}",
                describe(&tika),
                describe(&native),
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// One output of a document, Tika's path's and the native parser's.
type Comparison = (&'static str, Output, CharSet, Outcome, Outcome);

/// A document extracted both ways, as a string or a stream, in an output and a character set.
fn extracted(
    path: &str,
    configs: &ParserConfigs,
    (entry, output, char_set): (&'static str, Output, CharSet),
) -> Comparison {
    let (tika, native) = match entry {
        "string" => (
            tika_string(path, configs, output),
            native_string(path, configs, output),
        ),
        _ => (
            tika_stream(path, configs, output, char_set),
            native_stream(path, configs, output, char_set),
        ),
    };
    (entry, output, char_set, tika, native)
}

/// What OCR read in a document, by each path, as their streamed UTF-8 XHTML has it, when either
/// read any.
/// Two streamed UTF-8 XHTML extractions, Tika's path's and the native parser's, without what
/// OCR read in either.
fn without_ocr_both(tika: Extracted, native: Extracted) -> (Extracted, Extracted) {
    let texts = |extracted: &Extracted| {
        ocr_texts(std::str::from_utf8(&extracted.text).expect("XHTML in UTF-8"))
    };
    let (tika_read, native_read) = (texts(&tika), texts(&native));
    if tika_read.is_empty() && native_read.is_empty() {
        return (tika, native);
    }
    (
        without_ocr(tika, Output::Xml, CharSet::UTF_8, &tika_read),
        without_ocr(native, Output::Xml, CharSet::UTF_8, &native_read),
    )
}

fn read_by_ocr(xhtml: &Comparison) -> Option<OcrTexts> {
    let (_, _, _, Ok(tika), Ok(native)) = xhtml else {
        return None;
    };
    let texts = |extracted: &Extracted| {
        ocr_texts(std::str::from_utf8(&extracted.text).expect("XHTML in UTF-8"))
    };
    let (tika, native) = (texts(tika), texts(native));
    (!tika.is_empty() || !native.is_empty()).then_some((tika, native))
}

/// Why `parser` defers the document at `path` to Tika, given the metadata the detection
/// starts it from, if it does.
pub(crate) fn deferral(parser: &dyn Parser, path: &Path) -> Option<formats::Deferral> {
    let name = path.to_str().unwrap();
    let input = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
    let detected = formats::Detected::detect(Source::File(name), &input)
        .unwrap_or_else(|e| panic!("{name} is not detected: {e:?}"));
    parser.defers(
        &input,
        &detected.metadata_for(parser),
        &ParserConfigs::default(),
    )
}

/// The non-empty files under `roots` whose detected type `parser` reads, split into
/// those it parses and those it defers to Tika.
pub(crate) fn documents(parser: &dyn Parser, roots: &[&Path]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    documents_with(parser, roots, &ParserConfigs::default())
}

/// [`documents`], deferred under the settings `configs`.
pub(crate) fn documents_with(
    parser: &dyn Parser,
    roots: &[&Path],
    configs: &ParserConfigs,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut pending: Vec<PathBuf> = roots.iter().map(|root| root.to_path_buf()).collect();
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
        let input = std::fs::read(&file).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
        if input.is_empty() {
            continue;
        }
        let detected = formats::Detected::detect(Source::File(path), &input)
            .unwrap_or_else(|e| panic!("{path} is not detected: {e:?}"));
        let base = detected
            .media_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim();
        if !parser.media_types().contains(&base) {
            continue;
        }
        if parser
            .defers(&input, &detected.metadata_for(parser), configs)
            .is_some()
        {
            deferred.push(file);
        } else {
            parsed.push(file);
        }
    }
    (parsed, deferred)
}

/// Tika's own test documents, which `conformance/prepare.sh` checks out and names in
/// `CONFORMANCE_CORPUS`, for the ignored tests that compare a parser on all of them.
pub(crate) fn conformance_corpus() -> PathBuf {
    PathBuf::from(std::env::var_os("CONFORMANCE_CORPUS").expect(
        "CONFORMANCE_CORPUS names Tika's test documents: eval \"$(conformance/prepare.sh target/conformance)\"",
    ))
}

/// A directory for the documents a test writes, removed with it. Each is a directory of its
/// own, so tests running at once never share one.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    pub(crate) fn new(test: &str) -> Self {
        static CREATED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let number = CREATED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("extractous-{test}-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }

    /// The directory the documents are written in.
    pub(crate) fn directory(&self) -> &Path {
        &self.0
    }

    /// Writes `bytes` as the document `name`, returning its path.
    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

/// A spooled embedded document as the two paths are compared on it: everything but where the
/// spool is, and the bytes copied.
#[derive(Debug, PartialEq, Eq)]
struct Spooled {
    name: String,
    content_type: Option<String>,
    file: String,
    size: u64,
    truncated: bool,
    content: Vec<u8>,
}

/// The bytes a compound file (OLE2) starts with.
const COMPOUND_FILE_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// The files the native path spooled, with each embedded OLE storage that Tika's path spooled
/// empty taken as Tika's: Tika hands `EmbeddedSpooler` the storage as an opened container, whose
/// stream holds no bytes, while the native path writes the storage as a compound file, or
/// nothing but a cut where the budget is spent.
fn as_tika_spools(native: Vec<Spooled>, tika: &[Spooled]) -> Vec<Spooled> {
    native
        .into_iter()
        .enumerate()
        .map(|(index, native)| match tika.get(index) {
            Some(tika)
                if tika.size == 0
                    && !tika.truncated
                    && tika.name == native.name
                    && tika.content_type == native.content_type
                    && tika.file == native.file
                    && (native.content.starts_with(&COMPOUND_FILE_MAGIC)
                        || (native.truncated && native.content.is_empty())) =>
            {
                Spooled {
                    size: 0,
                    truncated: false,
                    content: Vec::new(),
                    ..native
                }
            }
            _ => native,
        })
        .collect()
}

fn spooled(embedded: Vec<crate::SpooledEmbedded>, spool: &Path) -> Vec<Spooled> {
    embedded
        .into_iter()
        .map(|document| Spooled {
            name: document.name,
            content_type: document.content_type,
            file: document
                .path
                .strip_prefix(spool)
                .unwrap_or_else(|_| panic!("{} is outside the spool", document.path.display()))
                .display()
                .to_string(),
            size: document.size,
            truncated: document.truncated,
            content: std::fs::read(&document.path).unwrap(),
        })
        .collect()
}

/// How many embedded documents Tika's path spools from the document at `path` under a budget
/// of `budget` bytes in all, once the native parser has spooled them alike: the same XHTML and
/// metadata, and the same files, named, typed, numbered and cut alike. Where the two differ, or
/// one path fails and the other does not, what differs.
pub(crate) fn spools_as_tika(path: &Path, budget: u64) -> Result<usize, String> {
    let path = path.to_str().unwrap();
    let configs = ParserConfigs::default();
    let tika_spool = Scratch::new("spool-tika");
    let tika_options = tika::StreamOptions {
        char_set: CharSet::UTF_8,
        as_xml: true,
        timeout: None,
        embedded: Some(&crate::EmbeddedSpool {
            dir: tika_spool.directory().to_path_buf(),
            byte_budget: budget,
        }),
    };
    let tika = tika::parse_to_stream(
        isolate().unwrap(),
        Source::File(path),
        &configs,
        &tika_options,
    )
    .and_then(|(mut reader, _)| {
        let mut text = Vec::new();
        reader
            .read_to_end(&mut text)
            .map_err(|e| crate::Error::IoError(e.to_string()))?;
        let metadata = sorted(reader.metadata()?);
        Ok((text, metadata, reader.embedded()?))
    });

    let native_spool = Scratch::new("spool-native");
    let routed = routed(path);
    let native_name = routed.parser.name();
    let tika_name = routed.parser.tika_name();
    let options = StreamOptions {
        char_set: CharSet::UTF_8,
        output: Output::Xml,
        timeout: None,
        spool: Some(crate::EmbeddedSpool {
            dir: native_spool.directory().to_path_buf(),
            byte_budget: budget,
        }),
    };
    let native = formats::parse_to_stream(
        routed.parser,
        routed.input.into_owned(),
        routed.detected,
        configs.clone(),
        options,
    )
    .map(|prepared| prepared.unwrap_or_else(|| panic!("{}", deferred(path))))
    .and_then(|(mut stream, _)| {
        let mut text = Vec::new();
        stream
            .read_to_end(&mut text)
            .map_err(|e| crate::Error::IoError(e.to_string()))?;
        let metadata = sorted(stream.metadata()?);
        Ok((text, metadata, stream.embedded()?))
    });

    match (tika, native) {
        (Ok((text, metadata, embedded)), Ok((native_text, native_metadata, native_embedded))) => {
            let names = tika_names(native_name, tika_name, &[]);
            let tika_extracted = comparable(Extracted { text, metadata }, CharSet::UTF_8);
            let native_extracted = comparable(
                as_tika(
                    Extracted {
                        text: native_text,
                        metadata: native_metadata,
                    },
                    &names,
                    CharSet::UTF_8,
                ),
                CharSet::UTF_8,
            );
            let (mut tika_extracted, mut native_extracted) =
                without_ocr_both(tika_extracted, native_extracted);
            with_native_failures_masked(&mut tika_extracted, &mut native_extracted);
            let tika = (tika_extracted, spooled(embedded, tika_spool.directory()));
            let native = (
                native_extracted,
                as_tika_spools(spooled(native_embedded, native_spool.directory()), &tika.1),
            );
            if native == tika {
                Ok(tika.1.len())
            } else {
                let show = |(extracted, spooled): &(Extracted, Vec<Spooled>)| {
                    let files: Vec<_> = spooled
                        .iter()
                        .map(|file| {
                            (
                                &file.name,
                                &file.content_type,
                                &file.file,
                                file.size,
                                file.truncated,
                                file.content.len(),
                            )
                        })
                        .collect();
                    format!(
                        "{}\n         {:?}\n         {files:?}",
                        String::from_utf8_lossy(&extracted.text),
                        extracted.metadata
                    )
                };
                let differing: Vec<String> = tika
                    .1
                    .iter()
                    .zip(&native.1)
                    .filter_map(|(tika, native)| {
                        let at = tika
                            .content
                            .iter()
                            .zip(&native.content)
                            .position(|(tika, native)| tika != native)?;
                        Some(format!("{} from byte {at}", tika.name))
                    })
                    .collect();
                Err(format!(
                    "{path} with a budget of {budget} bytes\n  tika   {}\n  native {}\n  contents differ: {}",
                    show(&tika),
                    show(&native),
                    differing.join(", ")
                ))
            }
        }
        (Err(_), Err(_)) => Ok(0),
        (tika, native) => Err(format!(
            "{path} with a budget of {budget} bytes: tika {:?}, native {:?}",
            tika.map(|_| ()),
            native.map(|_| ())
        )),
    }
}

mod tests {
    use super::*;

    #[test]
    fn compares_recorded_warnings_without_their_frames() {
        let tika = "<head>\n<meta name=\"tk:exception:warn\" content=\"java.io.EOFException: End of data reached.\n\tat R.getByte(R.java:62)\n\tat P.parse(P.java:139)\n\" />\n<meta name=\"dc:title\" content=\"a\n\tb\" />\n</head>";
        let native = "<head>\n<meta name=\"tk:exception:warn\" content=\"java.io.EOFException: End of data reached.\n\" />\n<meta name=\"dc:title\" content=\"a\n\tb\" />\n</head>";
        for char_set in [CharSet::UTF_8, CharSet::US_ASCII, CharSet::UTF_16BE] {
            let encoded = |text: &str| {
                let mut bytes = Vec::new();
                formats::encode(text, char_set, &mut bytes);
                bytes
            };
            assert_eq!(
                without_warning_frames(encoded(tika), char_set),
                encoded(native)
            );
        }
        assert_eq!(
            without_frames("E: m\n\tat P.parse(P.java:1)\nCaused by: C\n\t... 2 more\n"),
            "E: m\nCaused by: C\n"
        );
    }

    #[test]
    fn compares_embedded_failures_without_the_embedding_frames() {
        let tika = "E: no\n\tat P.parse(P.java:1)\n\tat org.apache.tika.parser.AutoDetectParser.parse(A.java:2)\n\tat org.apache.tika.parser.DelegatingParser.parse(D.java:71)\n\tat M.handle(M.java:3)\nCaused by: C: why\n\tat Q.q(Q.java:4)\n\t... 6 more\n";
        let native = "E: no\n\tat P.parse(P.java:1)\n\tat org.apache.tika.parser.AutoDetectParser.parse(A.java:2)\n\tat ai.yobix.TikaNativeMain.parseEmbedded(T.java:431)\n\tat W.run(W.java:79)\nCaused by: C: why\n\tat Q.q(Q.java:4)\n\t... 3 more\n";
        assert_eq!(
            without_embedding_frames(tika),
            without_embedding_frames(native)
        );
        assert_eq!(
            without_embedding_frames(native),
            "E: no\n\tat P.parse(P.java:1)\n\tat org.apache.tika.parser.AutoDetectParser.parse(A.java:2)\nCaused by: C: why\n\tat Q.q(Q.java:4)\n"
        );
    }

    #[test]
    fn puts_metas_and_namespace_declarations_in_a_defined_order() {
        let tika = "<html xmlns=\"x\">\n<head>\n<meta name=\"b\" content=\"1\" />\n<meta name=\"a\" content=\"2\n/>\" />\n<meta name=\"b\" content=\"0\" />\n<title></title>\n</head>\n<body><div xmlns:z=\"u:z\" xmlns:a=\"u:a\"><p>t</p></div>\n<meta name=\"d\" content=\"\" />\n<meta name=\"c\" content=\"\" />\nx<meta name=\"a\" content=\"\" /></body></html>";
        let native = "<html xmlns=\"x\">\n<head>\n<meta name=\"a\" content=\"2\n/>\" />\n<meta name=\"b\" content=\"1\" />\n<meta name=\"b\" content=\"0\" />\n<title></title>\n</head>\n<body><div xmlns:a=\"u:a\" xmlns:z=\"u:z\"><p>t</p></div>\n<meta name=\"c\" content=\"\" />\n<meta name=\"d\" content=\"\" />\nx<meta name=\"a\" content=\"\" /></body></html>";
        for char_set in [CharSet::UTF_8, CharSet::UTF_16BE] {
            let encoded = |text: &str| {
                let mut bytes = Vec::new();
                formats::encode(text, char_set, &mut bytes);
                bytes
            };
            assert_eq!(in_defined_order(encoded(tika), char_set), encoded(native));
            assert_eq!(in_defined_order(encoded(native), char_set), encoded(native));
        }
    }

    #[test]
    fn masks_the_numbers_and_suffixes_of_temporary_files() {
        assert_eq!(
            without_temporary_names("Reading /tmp/apache-tika-18224193047354770268.bmp failed"),
            without_temporary_names("Reading /tmp/apache-tika-1899816013292051053.tmp failed"),
        );
        assert_eq!(
            without_temporary_names("/tmp/apache-tika-12-34.pdf: x"),
            "/tmp/apache-tika-N: x"
        );
    }
}
