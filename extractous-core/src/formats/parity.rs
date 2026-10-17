//! Compares a native parser with Tika's path, which it replaces, on the same documents: the
//! streamed text and XHTML, their final metadata, and the text and XHTML of a string extraction
//! cut by a small length limit. The only differences allowed are the parser's name in
//! `tk:parsed-by`, which the native output is compared under Tika's, and the values of metadata
//! a test names as departures, which the native parser gives otherwise on purpose where Tika's
//! are wrong, each with the issue that tells why. Documents Tika's path fails
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
//! Where a native parser departs from Tika's path on purpose, fixing one of Tika's quirks, a
//! test names the departure, and exactly that is taken out before the results are compared:
//! see [`Departure`], [`NativeRewrite`] and [`Adjustments`].
//!
//! What OCR reads is not compared, the quality corpus scores it instead: the text in each div of
//! class `ocr`, in the XHTML and where it stands in the plain text, and the values of the
//! reports of how each image was read. The rest of a document OCR reads is compared exactly, in
//! its streamed UTF-8 text and XHTML, and the native parser must read text in each div Tika's
//! path reads text in. Its string extractions are not compared, since where the length limit
//! cuts them depends on the length of what OCR reads, nor its other character sets, which OCR
//! has no part in.

#[cfg(feature = "tika")]
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::config::ParserConfigs;
#[cfg(feature = "tika")]
use crate::config::Password;
#[cfg(feature = "tika")]
use crate::formats::embedded::{EXCEPTION_WARNING, ZERO_BYTES};
#[cfg(any(test, feature = "tika"))]
use crate::formats::Output;
#[cfg(any(feature = "parity", feature = "tika"))]
pub use crate::formats::Routed;
#[cfg(feature = "tika")]
use crate::formats::StreamOptions;
use crate::formats::{self, Parser};
/// The native path's routing and string extraction, for the tests that drive a parser through them.
#[cfg(feature = "parity")]
pub use crate::formats::{parse_to_string, route_file};

/// Installs the routes of the documents embedded in those native parsers read, which routing a
/// document installs, for the tests that write a document with its parser directly.
#[cfg(feature = "parity")]
pub fn install_routes() {
    formats::install_routes(&formats::Registry);
}
#[cfg(feature = "tika")]
use crate::input::Source;
#[cfg(feature = "tika")]
use crate::isolate::isolate;
#[cfg(feature = "tika")]
use crate::tika;
#[cfg(any(test, feature = "tika"))]
use crate::CharSet;
#[cfg(feature = "tika")]
use crate::Metadata;
#[cfg(feature = "tika")]
pub use extractous_base::formats::parity::*;

#[cfg(feature = "tika")]
const SMALL_LIMIT: usize = 100;

/// Why the native path extracts a Photoshop image Tika's path fails on: with OCR on, our Java
/// OCR path converts the image's colours with AWT, whose colour management throws
/// `CMMException: Invalid profile` on its ICC profile; the native path prepares it without.
#[cfg(feature = "tika")]
pub const PSD_PROFILE_AWT_REJECTS: &str = "Tika's OCR path throws CMMException on the \
                                                  image's colour profile; the native path reads \
                                                  it";

/// Why the native path extracts Zstandard files Tika's path leaves empty: Tika's native image
/// lacks zstd-jni, so its path parses them with `EmptyParser`.
pub const ZSTD_WITHOUT_ZSTD_JNI: &str = "Tika's path parses Zstandard with EmptyParser, as \
                                                its native image lacks zstd-jni; the native path \
                                                decodes it";

/// One extraction's result, as the two paths are compared on it.
#[cfg(feature = "tika")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub text: Vec<u8>,
    pub metadata: Vec<(String, Vec<String>)>,
}

/// An extraction's result, or why it failed.
#[cfg(feature = "tika")]
pub type Outcome = Result<Extracted, String>;

#[cfg(feature = "tika")]
pub fn sorted(metadata: Metadata) -> Vec<(String, Vec<String>)> {
    let mut sorted: Vec<_> = metadata.into_iter().collect();
    sorted.sort();
    sorted
}

#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
fn routed(path: &str) -> Routed<'_> {
    formats::route_file(path)
        .unwrap_or_else(|unrouted| panic!("{path} has no native parser: {unrouted:?}"))
}

#[cfg(feature = "tika")]
fn deferred(path: &str) -> String {
    format!("the native parser leaves {path} to Tika")
}

#[cfg(feature = "tika")]
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
        deadline: None,
        spool: None,
    };
    let (mut stream, _) = formats::parse_to_stream(
        routed.parser,
        routed.input.to_static(),
        routed.detected,
        configs.clone(),
        options,
    )
    .map_err(|e| e.to_string())?
    .unwrap_or_else(|unsupported| panic!("{}: {unsupported}", deferred(path)));
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
#[cfg(feature = "tika")]
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
    .unwrap_or_else(|unsupported| panic!("{}: {unsupported}", deferred(path)));
    Ok(Extracted {
        text: text.into_bytes(),
        metadata: sorted(metadata),
    })
}

/// The native names of the parsers of a document paired with Tika's: the parser `native_name`
/// Tika's path names `tika_name`, its `delegates`, then every native parser and the parsers it
/// delegates to, which its embedded documents may record.
#[cfg(feature = "tika")]
pub fn tika_names<'n>(
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
#[cfg(feature = "tika")]
pub fn as_tika(native: Extracted, names: &[(&str, &str)], char_set: CharSet) -> Extracted {
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

#[cfg(feature = "tika")]
const PARSED_BY: &str = "tk:parsed-by";
#[cfg(feature = "tika")]
const PARSED_BY_FULL_SET: &str = "tk:parsed-by-full-set";
#[cfg(feature = "tika")]
const EMBEDDED_EXCEPTION: &str = "tk:exception:embedded-exception";
#[cfg(feature = "tika")]
const EMBEDDED_STREAM_EXCEPTION: &str = "tk:exception:embedded-stream-exception";
/// Where the XHTML head writes a recorded warning.
#[cfg(any(test, feature = "tika"))]
const WARNING_META: &str = "<meta name=\"tk:exception:warn\" content=\"";
/// The first frame below an embedded document's parse in Tika's path, where the embedding
/// parser hands it to the embedded document extractor.
#[cfg(any(test, feature = "tika"))]
const TIKA_EMBEDDING_FRAME: &str = "\tat org.apache.tika.parser.DelegatingParser.parse(";
/// The first frame below an embedded document's parse in the native path.
#[cfg(any(test, feature = "tika"))]
const NATIVE_EMBEDDING_FRAME: &str = "\tat ai.yobix.TikaNativeMain.parseEmbedded(";

/// An extraction, its text in `char_set`, as the two paths are compared on it: each parser of
/// the full set once, the failures of embedded documents without the frames that embedded them,
/// that of an empty one without any frames, which the native path does not record (#68),
/// and the exceptions a parser records without their frames, which a native parser does not
/// have.
#[cfg(feature = "tika")]
pub fn comparable(mut extracted: Extracted, char_set: CharSet) -> Extracted {
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
                *value = if value.starts_with(ZERO_BYTES) {
                    without_frames(value)
                } else {
                    without_temporary_names(&without_embedding_frames(value))
                };
            }
        } else if name == EMBEDDED_STREAM_EXCEPTION || name == EXCEPTION_WARNING {
            for value in values.iter_mut() {
                *value = without_frames(value);
            }
        }
    }
    extracted
}

/// The OCR enricher our Java path hands Tika's image parsers.
#[cfg(feature = "tika")]
const PREPARING_OCR_PARSER: &str = "ai.yobix.PreparingOcrParser";

/// Tika's extraction, its text in `char_set`, under `configs`, and whether its XHTML head held
/// what is left out: where OCR is skipped, [`PREPARING_OCR_PARSER`] among the parsers it
/// records, in its metadata and in the XHTML head, which Tika's path records though it reads
/// nothing and the native path does not (#114).
#[cfg(feature = "tika")]
fn without_skipped_ocr_parser(
    mut tika: Extracted,
    char_set: CharSet,
    configs: &ParserConfigs,
) -> (Extracted, bool) {
    if !configs.ocr.skips_ocr() {
        return (tika, false);
    }
    for (name, values) in &mut tika.metadata {
        if name == PARSED_BY || name == PARSED_BY_FULL_SET {
            values.retain(|value| value != PREPARING_OCR_PARSER);
        }
    }
    tika.metadata.retain(|(_, values)| !values.is_empty());
    let Some(mut text) = decoded(&tika.text, char_set) else {
        return (tika, false);
    };
    let mut in_head = false;
    for name in [PARSED_BY, PARSED_BY_FULL_SET] {
        let meta = format!("<meta name=\"{name}\" content=\"{PREPARING_OCR_PARSER}\" />\n");
        in_head |= text.contains(&meta);
        text = text.replace(&meta, "");
    }
    tika.text.clear();
    formats::encode(&text, char_set, &mut tika.text);
    (tika, in_head)
}

/// A native string extraction, its text in `char_set`, with its head cut as Tika's is, where
/// the length limit stopped Tika's in its head and the two agree up to there: Tika's head held
/// an element left out of it since, so its limit stopped it sooner.
#[cfg(feature = "tika")]
fn with_the_head_cut_as_tikas(
    mut native: Extracted,
    tika: &Extracted,
    char_set: CharSet,
) -> Extracted {
    let (Some(tika_text), Some(native_text)) = (
        decoded(&tika.text, char_set),
        decoded(&native.text, char_set),
    ) else {
        return native;
    };
    let Some(before) = tika_text.strip_suffix(CUT_HEAD) else {
        return native;
    };
    if native_text.starts_with(before) && native_text[before.len()..].starts_with("<meta ") {
        native.text = tika.text.clone();
    }
    native
}

/// The parser of our Java path whose document nests the document of the parser it hands an
/// image to (#114).
#[cfg(feature = "tika")]
const RECOGNIZED_IMAGE_PARSER: &str = "ai.yobix.RecognizedImageParser";

/// An extraction of [`RECOGNIZED_IMAGE_PARSER`] in `output`, written in `char_set`, without the
/// whole document that our Java `RecognizedImageParser` writes into its body, and the native
/// parser does not: its `html` element in XHTML, and the line breaks leading the plain text.
#[cfg(any(test, feature = "tika"))]
fn without_nested_document(text: Vec<u8>, output: Output, char_set: CharSet) -> Vec<u8> {
    let Some(decoded) = decoded(&text, char_set) else {
        return text;
    };
    let kept = match output {
        Output::Xml => {
            let Some(start) = decoded.find("<body><html>").map(|at| at + "<body>".len()) else {
                return text;
            };
            let Some(end) = decoded[start..]
                .find("</html>")
                .map(|at| start + at + "</html>".len())
            else {
                return text;
            };
            [&decoded[..start], &decoded[end..]]
                .concat()
                .replace("<body></body>", "<body />")
        }
        _ => decoded.trim_start_matches('\n').to_owned(),
    };
    let mut encoded = Vec::with_capacity(kept.len());
    formats::encode(&kept, char_set, &mut encoded);
    encoded
}

/// What both paths' failures of an embedded document a native parser failed on are compared
/// as: the native path records a line of its own (#68) where Tika's records the stack trace of
/// its parser's exception.
#[cfg(feature = "tika")]
const NATIVE_FAILURE: &str = "<a failure the native path records itself (#68)>";

/// The two results with each failure of an embedded document the native path records itself,
/// and Tika's failure in its place, compared as [`NATIVE_FAILURE`].
#[cfg(feature = "tika")]
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

/// A native XHTML string extraction cut by the length limit with the end tags Tika's ends with
/// after it, when they close its `body`: an HTML document embedded in Tika's, or parsed inline,
/// that the limit stops in its body ends its own document, whose `</body>` leaks into the
/// embedding document and closes the elements open there (#113). The native path leaves them
/// open, as a cut does anywhere else.
#[cfg(any(test, feature = "tika"))]
fn with_the_body_end_tika_leaks(native: Vec<u8>, tika: &[u8]) -> Vec<u8> {
    let Some(tail) = tika.strip_prefix(native.as_slice()) else {
        return native;
    };
    let Ok(tail) = std::str::from_utf8(tail) else {
        return native;
    };
    let only_end_tags = tail.split_inclusive('>').all(|tag| {
        tag.strip_prefix("</")
            .and_then(|tag| tag.strip_suffix('>'))
            .is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()))
    });
    if only_end_tags && tail.contains("</body>") {
        tika.to_vec()
    } else {
        native
    }
}

/// A string extraction's native result with the embedded documents' parsers and failures that
/// follow Tika's left out: the native path parses an embedded document whole, while Tika's stops
/// at the length limit, before the parsers and failures of what comes after it.
#[cfg(feature = "tika")]
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

/// A string extraction's results with both texts cut where they part, when what follows in
/// one of them is only end tags and white space: the native text was cut at the length limit
/// before a [`NativeRewrite`] rewrote it, and what the rewrite undoes counts towards the limit,
/// so the text ends earlier or later than Tika's, its open elements then ended.
#[cfg(feature = "tika")]
fn cut_texts(mut tika: Extracted, mut native: Extracted) -> (Extracted, Extracted) {
    let common = tika
        .text
        .iter()
        .zip(&native.text)
        .take_while(|(tika, native)| tika == native)
        .count();
    let only_end_tags = |rest: &[u8]| {
        let mut rest = std::str::from_utf8(rest).unwrap_or("x").trim_start();
        while let Some(tag) = rest.strip_prefix("</") {
            let Some(end) = tag.find('>') else {
                return false;
            };
            rest = tag[end + 1..].trim_start();
        }
        rest.is_empty()
    };
    if only_end_tags(&tika.text[common..]) || only_end_tags(&native.text[common..]) {
        tika.text.truncate(common);
        native.text.truncate(common);
    }
    (tika, native)
}

/// Two string extractions, Tika's path's and the native parser's, without departures, cut to
/// the length of the shorter where there are departures: the length limit cut each before what
/// departed was left out, so one may run on further than the other.
#[cfg(feature = "tika")]
fn cut_alike(
    mut tika: Extracted,
    mut native: Extracted,
    departures: Departures,
) -> (Extracted, Extracted) {
    if !departures.is_empty() {
        let length = tika.text.len().min(native.text.len());
        tika.text.truncate(length);
        native.text.truncate(length);
    }
    (tika, native)
}

/// A Java stack trace without its frames, its lines ended by `\n`: Java ends them with the
/// platform's line separator, the native parsers with `\n` everywhere (#108).
#[cfg(any(test, feature = "tika"))]
fn without_frames(trace: &str) -> String {
    trace
        .split_inclusive('\n')
        .filter(|line| !line.starts_with('\t'))
        .collect::<String>()
        .replace("\r\n", "\n")
}

/// Text written in `char_set`, decoded, unless it is not valid in it.
#[cfg(any(test, feature = "tika"))]
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

/// The line breaks Tika's text output has for the `meta` elements of the metadata `departures`
/// leave unset, in Tika's streamed UTF-8 XHTML `tika`: those of the heads nested in its body,
/// which a parser that writes whole documents into its own writes, each followed by a line break
/// that the text output keeps.
#[cfg(feature = "tika")]
fn nested_head_breaks(tika: &str, departures: Departures) -> usize {
    let Some(body) = tika.find("<body") else {
        return 0;
    };
    departures
        .iter()
        .filter_map(|departure| match departure {
            Departure::Unset(name, _) => Some(
                tika[body..]
                    .matches(&format!("<meta name=\"{name}\" content=\""))
                    .count(),
            ),
            _ => None,
        })
        .sum()
}

/// Tika's text output without the `breaks` line breaks of the nested `meta` elements of metadata
/// the native parser leaves unset, where they are all that sets it apart from the native one:
/// exactly that many, or, where the length limit `cut` both, up to that many, Tika's then taking
/// the native text's end, which those line breaks had left no room for.
#[cfg(feature = "tika")]
fn without_nested_head_breaks(
    mut tika: Extracted,
    native: &Extracted,
    char_set: CharSet,
    cut: Option<Output>,
    breaks: usize,
) -> Extracted {
    let (Some(tika_text), Some(native_text)) = (
        decoded(&tika.text, char_set),
        decoded(&native.text, char_set),
    ) else {
        return tika;
    };
    if let Some(text) = without_line_breaks(&tika_text, &native_text, cut, breaks, 0) {
        tika.text = encoded_as(&text, char_set);
    }
    tika
}

/// The text of an extraction, in `char_set`, with the recorded warnings the XHTML head writes
/// without their frames.
#[cfg(any(test, feature = "tika"))]
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
#[cfg(any(test, feature = "tika"))]
fn in_defined_order(text: Vec<u8>, char_set: CharSet) -> Vec<u8> {
    let Some(decoded) = decoded(&text, char_set).filter(|decoded| decoded.contains('<')) else {
        return text;
    };
    let mut ordered = String::with_capacity(decoded.len());
    let mut rest = decoded.as_str();
    let mut metas: Vec<(&str, &str, bool)> = Vec::new();
    while let Some(at) = rest.find('<') {
        let end = tag_end(&rest[at..]).map_or(rest.len(), |end| at + end);
        let tag = &rest[at..end];
        if at > 0 {
            flush_metas(&mut metas, &mut ordered);
            ordered.push_str(&rest[..at]);
        }
        if tag.starts_with("<meta ") {
            let line_ended = rest[end..].starts_with('\n');
            metas.push((attribute(tag, "name").unwrap_or_default(), tag, line_ended));
            rest = &rest[end + usize::from(line_ended)..];
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

/// Writes a run of `meta` elements, each with whether a line break follows it, sorted by name,
/// stably. A run some element of which no line break follows is one a string extraction's
/// length limit, which counts the line breaks, stopped: which elements the head has then
/// depends on their order, so the run is written as [`CUT_HEAD`].
#[cfg(any(test, feature = "tika"))]
fn flush_metas(metas: &mut Vec<(&str, &str, bool)>, ordered: &mut String) {
    if metas.iter().any(|(_, _, line_ended)| !line_ended) {
        metas.clear();
        ordered.push_str(CUT_HEAD);
        return;
    }
    metas.sort_by_key(|(name, _, _)| *name);
    for (_, element, _) in metas.drain(..) {
        ordered.push_str(element);
        ordered.push('\n');
    }
}

/// What a run of `meta` elements the length limit stopped in is compared as (#64).
#[cfg(any(test, feature = "tika"))]
const CUT_HEAD: &str =
    "<!-- a head the length limit stopped, its meta elements in its own order -->";

/// Where the tag `text` starts with ends, after its `>`: attribute values hold no `"`, which
/// both paths escape.
#[cfg(any(test, feature = "tika"))]
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
#[cfg(any(test, feature = "tika"))]
fn attribute<'t>(tag: &'t str, name: &str) -> Option<&'t str> {
    let marker = format!(" {name}=\"");
    let start = tag.find(&marker)? + marker.len();
    let end = start + tag[start..].find('"')?;
    Some(&tag[start..end])
}

/// A start tag with the namespace declarations it ends with sorted by prefix.
#[cfg(any(test, feature = "tika"))]
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
#[cfg(any(test, feature = "tika"))]
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

/// A Java stack trace without, in each of its exceptions, the frames from the first one that
/// embedded the document on, and without the counts of frames a cause shares with its effect:
/// the frames of the failing document's own parse. Below them are those of the documents it is
/// embedded in, Java frames only where Tika's path parsed those, which a native parser replaces.
#[cfg(any(test, feature = "tika"))]
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
            .position(|line| {
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

#[cfg(feature = "tika")]
const TOTAL_TIME: &str = "extended-properties:TotalTime";
/// What an OLE2 document's edit time comes to in Tika's path: its file time in 100-nanosecond
/// units, a minute being this many.
#[cfg(feature = "tika")]
const FILE_TIME_MINUTE: i64 = 600_000_000;

/// Tika's extraction with the edit time of an OLE2 document's summary information as the native
/// parsers write it, where the two agree on it (#106): Tika writes the raw file time, and `0`
/// where the document records none, the native parsers the whole minutes, and nothing where there
/// is none. The XHTML head is rewritten alike, its other metadata in the native head's order where
/// the edit time goes, which changes the order of Java's `HashMap` of them. Where Tika's
/// extraction was rewritten, by how many characters its edit time was longer, which a length
/// limit counts.
#[cfg(feature = "tika")]
fn with_native_edit_time(
    mut tika: Extracted,
    native: &Extracted,
    char_set: CharSet,
) -> (Extracted, Option<usize>) {
    let value = |extracted: &Extracted| {
        extracted
            .metadata
            .iter()
            .find(|(name, _)| name == TOTAL_TIME)
            .and_then(|(_, values)| values.first().cloned())
    };
    let (Some(raw), native_value) = (value(&tika), value(native)) else {
        return (tika, None);
    };
    let Ok(file_time) = raw.parse::<i64>() else {
        return (tika, None);
    };
    let minutes = (file_time != 0).then(|| (file_time / FILE_TIME_MINUTE).to_string());
    if native_value.as_ref() == Some(&raw) || native_value != minutes {
        return (tika, None);
    }
    let longer = raw.len() - minutes.as_ref().map_or(0, String::len);
    if let (Some(text), Some(native_text)) = (
        decoded(&tika.text, char_set),
        decoded(&native.text, char_set),
    ) {
        let mut encoded = Vec::new();
        formats::encode(
            &with_head_edit_time(&text, &native_text, &raw, minutes.as_deref()),
            char_set,
            &mut encoded,
        );
        tika.text = encoded;
    }
    match minutes {
        Some(minutes) => {
            for (name, values) in &mut tika.metadata {
                if name == TOTAL_TIME {
                    *values = vec![minutes.clone()];
                }
            }
        }
        None => tika.metadata.retain(|(name, _)| name != TOTAL_TIME),
    }
    (tika, Some(longer))
}

/// The XHTML `text` with the edit time `raw` of its head as `minutes`, or without it, its other
/// metadata then in the order of the native head `native`, as a `HashMap` without the edit time
/// orders them, where the two name the same metadata.
#[cfg(feature = "tika")]
fn with_head_edit_time(text: &str, native: &str, raw: &str, minutes: Option<&str>) -> String {
    let element = |content: &str| format!("<meta name=\"{TOTAL_TIME}\" content=\"{content}\" />");
    let Some(minutes) = minutes else {
        let Some((start, end)) = head_span(text) else {
            return text.to_owned();
        };
        let lines: Vec<(&str, &str)> = head_lines(&text[start..end])
            .into_iter()
            .filter(|&(name, line)| name != TOTAL_TIME || line.trim_end() != element(raw))
            .collect();
        let mut names: Vec<&str> = Vec::new();
        for (name, _) in &lines {
            if !names.contains(name) {
                names.push(name);
            }
        }
        let mut order: Vec<&str> = Vec::new();
        if let Some((native_start, native_end)) = head_span(native) {
            for (name, _) in head_lines(&native[native_start..native_end]) {
                if !order.contains(&name) {
                    order.push(name);
                }
            }
        }
        let mut sorted_names = names.clone();
        let mut sorted_order = order.clone();
        sorted_names.sort_unstable();
        sorted_order.sort_unstable();
        if sorted_names == sorted_order {
            names = order;
        }
        let head: String = names
            .iter()
            .flat_map(|&name| {
                lines
                    .iter()
                    .filter(move |(line_name, _)| *line_name == name)
                    .map(|(_, line)| *line)
            })
            .collect();
        return format!("{}{head}{}", &text[..start], &text[end..]);
    };
    text.replacen(&element(raw), &element(minutes), 1)
}

/// Where the metadata of an XHTML head start and end.
#[cfg(feature = "tika")]
fn head_span(text: &str) -> Option<(usize, usize)> {
    let start = text.find("<head>\n")? + "<head>\n".len();
    let end = text[start..].find("<title>")? + start;
    Some((start, end))
}

/// The lines of the metadata of an XHTML head, each with its name.
#[cfg(feature = "tika")]
fn head_lines(head: &str) -> Vec<(&str, &str)> {
    head.split_inclusive('\n')
        .map(|line| {
            let name = line
                .strip_prefix("<meta name=\"")
                .and_then(|line| line.split_once('"'))
                .map_or("", |(name, _)| name);
            (name, line)
        })
        .collect()
}

/// The native text of a string extraction as Tika's is, where the two differ only in what the
/// native one goes on to past where Tika's length limit cut it sooner, as Tika's edit time,
/// longer by `longer` characters, counted towards it (#106): at one place, at most `longer`
/// characters of text and the markup around them.
#[cfg(feature = "tika")]
fn cut_sooner(native: Vec<u8>, tika: &[u8], longer: usize) -> Vec<u8> {
    let (Ok(native_text), Ok(tika_text)) =
        (std::str::from_utf8(&native), std::str::from_utf8(tika))
    else {
        return native;
    };
    let Some(inserted) = native_text.len().checked_sub(tika_text.len()) else {
        return native;
    };
    let common = native_text
        .char_indices()
        .zip(tika_text.chars())
        .find(|((_, native), tika)| native != tika)
        .map_or(tika_text.len(), |((at, _), _)| at);
    let Some(more) = native_text.get(common..common + inserted) else {
        return native;
    };
    let mut in_markup = false;
    let text = more
        .chars()
        .filter(|&c| {
            match c {
                '<' => in_markup = true,
                '>' => {
                    in_markup = false;
                    return false;
                }
                _ => {}
            }
            !in_markup
        })
        .count();
    if text <= longer && native_text[common + inserted..] == tika_text[common..] {
        return tika.to_vec();
    }
    native
}

#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
#[derive(Debug, Clone, Copy)]
enum Expected<'a> {
    /// Both extract it identically, or both fail on it in one of the ways, as a parser that
    /// rethrows the length limit of a string extraction fails it.
    Same(Naming<'a>),
    /// Both fail on it.
    Failure,
}

/// How Tika's path names the parsers of a document the native parser extracts.
#[cfg(feature = "tika")]
#[derive(Debug, Clone, Copy)]
struct Naming<'a> {
    /// The class name of the parser of the document.
    tika_name: &'a str,
    /// The Tika names of the parsers it delegates to, each paired after its native name.
    delegates: &'a [(&'a str, &'a str)],
    /// The metadata the native parser gives other values than Tika's path on purpose.
    departures: Departures<'a>,
}

/// The divs native parsers write around the parts Tika's path reads with a handler that never
/// writes the div it is handed: those of a presentation's layouts, notes, masters, diagrams and
/// charts (#102), and of a workbook's diagrams and charts (#93). Wherever Tika's XHTML of a
/// document has none of one of them, the native XHTML's, of the document and of every document
/// it embeds, are taken out, with the line break that ends each.
#[cfg(feature = "tika")]
pub const UNWRITTEN_DIVS: [&str; 7] = [
    "<div class=\"slide-master-content\">",
    "<div class=\"slide-notes\">",
    "<div class=\"slide-notes-master\">",
    "<div class=\"slide-master\">",
    "<div class=\"slide-handout-master\">",
    "<div class=\"diagram-data\">",
    "<div class=\"chart\">",
];

/// The native XHTML `native` without the [`UNWRITTEN_DIVS`] that Tika's XHTML `tika` does not
/// have where the native one does, found by reading both alike up to each, each with the line
/// breaks that end it; and how many line breaks that takes out.
#[cfg(feature = "tika")]
fn without_unwritten(native: &str, tika: &str) -> (String, usize) {
    let unwritten = |at: usize| {
        UNWRITTEN_DIVS.iter().copied().find(|div| {
            let rest = &native[at..];
            rest.starts_with(div)
                || rest.starts_with(div.trim_end_matches('>'))
                    && rest[div.len() - 1..].starts_with(" />")
        })
    };
    let (native_bytes, tika_bytes) = (native.as_bytes(), tika.as_bytes());
    let mut kept = Vec::with_capacity(native.len());
    let (mut at, mut tika_at, mut line_breaks) = (0, 0, 0);
    let mut ends: Vec<(usize, usize)> = Vec::new();
    let mut stripped = false;
    while at < native_bytes.len() {
        if let Some(index) = ends.iter().position(|&(start, _)| start == at) {
            at = ends.swap_remove(index).1;
            stripped = true;
            continue;
        }
        let byte = native_bytes[at];
        let div = (byte == b'<').then(|| unwritten(at)).flatten();
        let tika_has = |div: &str| {
            tika.get(tika_at..)
                .is_some_and(|rest| rest.starts_with(div.trim_end_matches('>')))
        };
        if let Some(div) = div.filter(|div| !tika_has(div)) {
            match element_end(native, at, div) {
                Some((end, None)) => at = end,
                Some((end, Some(close))) => {
                    ends.push((close, end));
                    at += div.len();
                }
                None => at += div.len(),
            }
            stripped = true;
            continue;
        }
        if tika_bytes.get(tika_at) == Some(&byte) {
            kept.push(byte);
            (at, tika_at, stripped) = (at + 1, tika_at + 1, false);
            continue;
        }
        if stripped && byte == b'\n' {
            at += 1;
            line_breaks += 1;
            continue;
        }
        kept.push(byte);
        at += 1;
        stripped = false;
    }
    let kept = String::from_utf8(kept).expect("whole characters of the native XHTML");
    (kept, line_breaks)
}

/// What stands for a value a departure leaves uncompared.
#[cfg(feature = "tika")]
const DEPARTED: &str = "<departed>";

/// An extraction, its text in `char_set`, without what `departures` leave uncompared: each
/// value of a metadata name, in the metadata and as the content of each `meta` element of the
/// XHTML head, up to the end of a string extraction the length limit cuts in it; and, for
/// line breaks, every CRLF of the text an LF.
#[cfg(feature = "tika")]
fn without_departures(
    mut extracted: Extracted,
    char_set: CharSet,
    departures: Departures,
) -> Extracted {
    if departures.is_empty() {
        return extracted;
    }
    let names: Vec<&str> = departures
        .iter()
        .filter_map(|departure| match departure {
            Departure::Values(name, _) => Some(*name),
            Departure::Unset(..) | Departure::LineBreaks(_) | Departure::PngImages(_) => None,
        })
        .collect();
    let unset: Vec<&str> = departures
        .iter()
        .filter_map(|departure| match departure {
            Departure::Unset(name, _) => Some(*name),
            _ => None,
        })
        .collect();
    for (name, values) in &mut extracted.metadata {
        if names.contains(&name.as_str()) {
            values
                .iter_mut()
                .for_each(|value| *value = DEPARTED.to_owned());
        }
    }
    let Some(mut text) = decoded(&extracted.text, char_set) else {
        return extracted;
    };
    for name in unset {
        without_metadata(&mut text, &mut extracted.metadata, name);
    }
    if departures
        .iter()
        .any(|departure| matches!(departure, Departure::LineBreaks(_)))
    {
        text = text.replace("\r\n", "\n");
        // A CRLF the length limit of a string extraction cuts after its CR.
        if text.ends_with('\r') {
            text.pop();
            text.push('\n');
        }
    }
    for name in names {
        let meta = format!("<meta name=\"{name}\" content=\"");
        let mut kept = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(start) = rest.find(&meta) {
            let inside = start + meta.len();
            kept.push_str(&rest[..inside]);
            kept.push_str(DEPARTED);
            rest = &rest[inside..];
            rest = &rest[rest.find('"').unwrap_or(rest.len())..];
        }
        kept.push_str(rest);
        text = kept;
    }
    let mut encoded = Vec::with_capacity(extracted.text.len());
    formats::encode(&text, char_set, &mut encoded);
    extracted.text = encoded;
    extracted
}

/// Asserts that the native parser of each document, which Tika's path parses with
/// `tika_name`, extracts exactly what Tika's path does under Tika's default settings.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika(documents: &[&Path], tika_name: &str) {
    assert_same_as_tika_with(documents, tika_name, &ParserConfigs::default());
}

/// Asserts that the native parser of each document, which Tika's path parses with
/// `tika_name`, extracts exactly what Tika's path does under the settings `configs`.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_with(documents: &[&Path], tika_name: &str, configs: &ParserConfigs) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates: &[],
        departures: &[],
    });
    compare(documents, configs, expected, None, Adjustments::default());
}

/// What a native parser writes differently from Tika's path on purpose, where it fixes one of
/// Tika's quirks, which a comparison takes out of the native result before comparing it.
#[cfg(feature = "tika")]
#[derive(Debug, Clone, Copy, Default)]
pub struct Adjustments {
    /// Counts, in Tika's and the native parser's streamed UTF-8 XHTML of a document, the line
    /// breaks the native parser adds after blocks that Tika's path does not end as blocks or
    /// does not write. Up to that many line breaks where Tika's text and XHTML have none are
    /// taken out of the native ones, exactly that many where the output is not cut by the
    /// length limit, and nothing else.
    pub line_breaks: Option<fn(&str, &str) -> usize>,
    /// Metadata the native parser sets where Tika's path sets none: left out of the native
    /// metadata, and its `meta` element out of the XHTML head, where Tika's has no value under it.
    pub metadata: &'static [&'static str],
    /// The start tags of elements the native parser wraps around what Tika's path writes
    /// unwrapped: taken out of the native XHTML with their end tags.
    pub wrappers: &'static [&'static str],
    /// The starts of empty elements Tika's path writes more often than the native parser, which
    /// writes each once: as many of each as Tika's XHTML has more are taken out of it.
    pub repeated: &'static [&'static str],
    /// Metadata the native parser has its final value of when it writes the XHTML head, where
    /// Tika's path writes the head with a value it changes later: Tika's `meta` element of it
    /// is compared with its final value, and where the length limit cut the output before Tika
    /// changed it, Tika's value is taken as the native one.
    pub final_in_head: &'static [&'static str],
}

/// Asserts that the native parser of each document, which Tika's path parses with
/// `tika_name`, extracts exactly what Tika's path does under the settings `configs`, but for
/// its `adjustments`.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_adjusting(
    documents: &[&Path],
    tika_name: &str,
    configs: &ParserConfigs,
    adjustments: Adjustments,
) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates: &[],
        departures: &[],
    });
    compare(documents, configs, expected, None, adjustments);
}

/// [`assert_same_as_tika_adjusting`], with each native result rewritten by `rewrite` before
/// it is compared.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_adjusting_rewriting(
    documents: &[&Path],
    tika_name: &str,
    configs: &ParserConfigs,
    adjustments: Adjustments,
    rewrite: NativeRewrite,
) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates: &[],
        departures: &[],
    });
    compare(
        documents,
        configs,
        expected,
        Some((rewrite, None)),
        adjustments,
    );
}

/// Tika's and the native result without their `adjustments` from each other: the metadata
/// Tika's path leaves unset, with its `meta` element in the XHTML head, the native wrappers,
/// Tika's repeated elements, and `line_breaks`, those the XHTML says the native parser adds,
/// where they are its only other difference; and the [`UNWRITTEN_DIVS`] of the native XHTML,
/// or, in the text of a document with `unwritten` line breaks after them, the line breaks it
/// has more than Tika's, any of them where the length limit cut it. The text ends a block with
/// line breaks of its own, as many as the XHTML or more, so their count is not the XHTML's.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "tika")]
fn without_adjustments(
    mut tika: Extracted,
    mut native: Extracted,
    char_set: CharSet,
    output: Output,
    cut: Option<Output>,
    line_breaks: usize,
    unwritten: usize,
    adjustments: Adjustments,
) -> (Extracted, Extracted) {
    let unset: Vec<&str> = adjustments
        .metadata
        .iter()
        .copied()
        .filter(|name| !tika.metadata.iter().any(|(tika_name, _)| tika_name == name))
        .collect();
    native
        .metadata
        .retain(|(name, _)| !unset.contains(&name.as_str()));
    let (Some(mut native_text), Some(mut tika_text)) = (
        decoded(&native.text, char_set),
        decoded(&tika.text, char_set),
    ) else {
        return (tika, native);
    };
    for wrapper in adjustments.wrappers {
        native_text = without_wrapper(&native_text, wrapper);
    }
    for name in adjustments.final_in_head {
        // Cut by the length limit, Tika's value is the one it had where the limit stopped it.
        let native_values = native
            .metadata
            .iter()
            .find(|(native_name, _)| native_name == name)
            .map(|(_, values)| values.clone());
        if let (Some(_), Some(values)) = (cut, native_values) {
            if let Some((_, tika_values)) = tika
                .metadata
                .iter_mut()
                .find(|(tika_name, _)| tika_name == name)
            {
                *tika_values = values;
            }
        }
        let Some((_, values)) = tika
            .metadata
            .iter()
            .find(|(tika_name, _)| tika_name == name)
        else {
            continue;
        };
        let [value] = values.as_slice() else {
            continue;
        };
        let element = format!("<meta name=\"{name}\" content=\"");
        if let Some(start) = tika_text.find(&element).map(|at| at + element.len()) {
            if let Some(end) = tika_text[start..].find('"') {
                tika_text.replace_range(start..start + end, value);
            }
        }
    }
    for start in adjustments.repeated {
        tika_text = without_repeats(&tika_text, &native_text, start);
    }
    tika.text = encoded_as(&tika_text, char_set);
    // Each `meta` element is followed by a line break, which the length limit counts.
    let mut spent = 0;
    for name in unset {
        let element = format!("<meta name=\"{name}\" content=\"");
        if let Some(start) = native_text.find(&element) {
            if let Some(end) = native_text[start..].find("/>\n") {
                native_text.replace_range(start..start + end + "/>\n".len(), "");
                spent += 1;
            }
        }
    }
    let line_breaks = match output {
        Output::Xml => {
            let (stripped, unwritten) = without_unwritten(&native_text, &tika_text);
            native_text = stripped;
            spent += unwritten;
            line_breaks
        }
        Output::Text if unwritten > 0 => {
            let native_breaks = native_text.matches('\n').count();
            match cut {
                Some(_) => native_breaks,
                None => {
                    line_breaks.max(native_breaks.saturating_sub(tika_text.matches('\n').count()))
                }
            }
        }
        Output::Text => line_breaks,
    };
    let text = without_line_breaks(&native_text, &tika_text, cut, line_breaks, spent)
        .unwrap_or(native_text);
    native.text = encoded_as(&text, char_set);
    (tika, native)
}

/// `text` written in `char_set`.
#[cfg(feature = "tika")]
fn encoded_as(text: &str, char_set: CharSet) -> Vec<u8> {
    match char_set {
        CharSet::UTF_16BE => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        CharSet::UTF_8 | CharSet::US_ASCII => text.as_bytes().to_vec(),
    }
}

/// Where the element that starts with `start` at `at` of `xhtml` ends: past its empty tag, or
/// past the end tag that matches its start tag, with the start of that end tag; none where the
/// length limit cut it.
#[cfg(feature = "tika")]
fn element_end(xhtml: &str, at: usize, start: &str) -> Option<(usize, Option<usize>)> {
    if let Some(attributes) = start.strip_suffix('>') {
        let empty = format!("{attributes} />");
        if xhtml[at..].starts_with(&empty) {
            return Some((at + empty.len(), None));
        }
    }
    let name: String = start
        .trim_start_matches('<')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    let (open, close) = (format!("<{name}"), format!("</{name}>"));
    let mut depth = 0;
    let mut index = at + start.len();
    loop {
        let next_open = xhtml[index..].find(&open).map(|found| index + found);
        let next_close = xhtml[index..].find(&close).map(|found| index + found)?;
        match next_open {
            Some(next_open) if next_open < next_close => {
                let tag_end = xhtml[next_open..]
                    .find('>')
                    .map(|found| next_open + found)?;
                if !xhtml[..tag_end].ends_with('/') {
                    depth += 1;
                }
                index = tag_end + 1;
            }
            _ if depth > 0 => {
                depth -= 1;
                index = next_close + close.len();
            }
            _ => return Some((next_close + close.len(), Some(next_close))),
        }
    }
}

/// `xhtml` without each element that starts with `start`, its content kept: the start tag and
/// the end tag that matches it, or the start tag alone where the length limit cut the element,
/// and the whole element where it is empty.
#[cfg(feature = "tika")]
fn without_wrapper(xhtml: &str, start: &str) -> String {
    let empty = start
        .strip_suffix('>')
        .map(|attributes| format!("{attributes} />"));
    let mut text = xhtml.to_owned();
    let mut from = 0;
    while let Some(at) = text[from..]
        .find(start.trim_end_matches('>'))
        .map(|at| from + at)
    {
        let is_empty = empty
            .as_ref()
            .is_some_and(|empty| text[at..].starts_with(empty));
        if !is_empty && !text[at..].starts_with(start) {
            from = at + 1;
            continue;
        }
        match element_end(&text, at, start) {
            Some((end, None)) => text.replace_range(at..end, ""),
            Some((end, Some(close))) => {
                text.replace_range(close..end, "");
                text.replace_range(at..at + start.len(), "");
            }
            None => text.replace_range(at..at + start.len(), ""),
        }
        from = at;
    }
    text
}

/// `tika` without as many of each empty element starting with `start` as it has more than
/// `native`, the first of them.
#[cfg(feature = "tika")]
fn without_repeats(tika: &str, native: &str, start: &str) -> String {
    let elements = |xhtml: &str| -> Vec<String> {
        let mut found = Vec::new();
        let mut rest = xhtml;
        while let Some(at) = rest.find(start) {
            let Some(end) = rest[at..].find("/>") else {
                break;
            };
            found.push(rest[at..at + end + 2].to_owned());
            rest = &rest[at + end + 2..];
        }
        found
    };
    let native_elements = elements(native);
    let mut text = tika.to_owned();
    let mut seen: Vec<String> = Vec::new();
    for element in elements(tika) {
        if seen.contains(&element) {
            continue;
        }
        let in_native = native_elements.iter().filter(|e| **e == element).count();
        let more = tika
            .matches(element.as_str())
            .count()
            .saturating_sub(in_native);
        for _ in 0..more {
            if let Some(at) = text.find(element.as_str()) {
                text.replace_range(at..at + element.len(), "");
            }
        }
        seen.push(element);
    }
    text
}

/// The native text without the `line_breaks` it has more than Tika's, when they are all that
/// differs: exactly that many, or, where the length limit `cut` the output, up to that many. A
/// cut native text ends early by as many characters as it has line breaks more, and `spent`
/// more, which went to departing metadata: where it ends, Tika's may go on with as many
/// characters, which are taken as Tika's, while the native one only ends its open elements.
#[cfg(feature = "tika")]
fn without_line_breaks(
    native: &str,
    tika: &str,
    cut: Option<Output>,
    line_breaks: usize,
    spent: usize,
) -> Option<String> {
    if line_breaks == 0 && (cut.is_none() || spent == 0) {
        return None;
    }
    let mut kept = String::with_capacity(native.len());
    let mut tika_chars = tika.char_indices().peekable();
    let mut removed = 0;
    let mut native_chars = native.char_indices().peekable();
    while let Some(&(at, c)) = native_chars.peek() {
        match tika_chars.peek() {
            Some(&(_, t)) if t == c => {
                kept.push(c);
                tika_chars.next();
                native_chars.next();
            }
            _ if c == '\n' && removed < line_breaks => {
                removed += 1;
                native_chars.next();
            }
            _ if cut.is_some() && is_closing_tags(&native[at..]) => break,
            _ => return None,
        }
    }
    let left = tika_chars.peek().map_or("", |&(at, _)| &tika[at..]);
    let aligned = match cut {
        Some(output) => {
            let mut in_tag = false;
            let text = left.chars().filter(|&c| {
                let markup = output == Output::Xml && (in_tag || c == '<');
                in_tag = markup && c != '>';
                !markup
            });
            text.count() <= removed + spent
        }
        None => left.is_empty() && removed == line_breaks,
    };
    aligned.then(|| kept + left)
}

/// Whether `text` holds nothing but end tags.
#[cfg(feature = "tika")]
fn is_closing_tags(text: &str) -> bool {
    let mut rest = text;
    while !rest.is_empty() {
        let Some(tag) = rest.strip_prefix("</") else {
            return false;
        };
        let Some(end) = tag.find('>') else {
            return false;
        };
        if tag[..end].contains('<') {
            return false;
        }
        rest = &tag[end + 1..];
    }
    true
}

/// [`assert_same_as_tika`], with each native result rewritten by `rewrite` before it is
/// compared.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_rewriting(documents: &[&Path], tika_name: &str, rewrite: NativeRewrite) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates: &[],
        departures: &[],
    });
    compare(
        documents,
        &ParserConfigs::default(),
        expected,
        Some((rewrite, None)),
        Adjustments::default(),
    );
}

/// [`assert_same_as_tika_rewriting`], with both texts then rewritten by `normalise`.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_normalising(
    documents: &[&Path],
    tika_name: &str,
    rewrite: NativeRewrite,
    normalise: Normalisation,
) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates: &[],
        departures: &[],
    });
    compare(
        documents,
        &ParserConfigs::default(),
        expected,
        Some((rewrite, Some(normalise))),
        Adjustments::default(),
    );
}

/// The result `extracted`, its text in `char_set`, with the text rewritten by `normalise`.
#[cfg(feature = "tika")]
fn normalised(mut extracted: Extracted, char_set: CharSet, normalise: Normalisation) -> Extracted {
    let mut text = decoded(&extracted.text, char_set).expect("text in its character set");
    normalise(&mut text);
    extracted.text.clear();
    formats::encode(&text, char_set, &mut extracted.text);
    extracted
}

/// The native result `native`, its text in `char_set`, rewritten by `rewrite` beside Tika's
/// result `tika`.
#[cfg(feature = "tika")]
fn rewritten(
    mut native: Extracted,
    tika: &Extracted,
    char_set: CharSet,
    rewrite: NativeRewrite,
) -> Extracted {
    let mut text = decoded(&native.text, char_set).expect("text the native path wrote");
    rewrite(&mut text, &mut native.metadata, &tika.metadata);
    native.text.clear();
    formats::encode(&text, char_set, &mut native.text);
    native
}

/// Both results, the native one rewritten and both then normalised as `rewrite` says.
#[cfg(feature = "tika")]
fn with_rewrite(
    tika: Extracted,
    native: Extracted,
    char_set: CharSet,
    rewrite: Option<(NativeRewrite, Option<Normalisation>)>,
) -> (Extracted, Extracted) {
    let Some((rewrite, normalise)) = rewrite else {
        return (tika, native);
    };
    let native = rewritten(native, &tika, char_set, rewrite);
    match normalise {
        Some(normalise) => (
            normalised(tika, char_set, normalise),
            normalised(native, char_set, normalise),
        ),
        None => (tika, native),
    }
}

/// [`assert_same_as_tika`], but for the values of the metadata `departures` names, which the
/// native parser gives on purpose otherwise than Tika's path.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_departing(documents: &[&Path], tika_name: &str, departures: Departures) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates: &[],
        departures,
    });
    compare(
        documents,
        &ParserConfigs::default(),
        expected,
        None,
        Adjustments::default(),
    );
}

/// What a native parser does with a document a wrong password was given for.
#[cfg(feature = "tika")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrongPassword {
    /// It fails as Tika's path fails, with the same error.
    FailsAsTika,
    /// It leaves the document to Tika's path, whose decoders fail on the garbled data with
    /// errors of their own.
    Defers,
}

/// Asserts that of `documents`, each one `passwords` names, by file name with the password that
/// opens it, fails without a password and with a wrong one as Tika's path fails on it, with the
/// same error, and extracts with its password as Tika's path extracts it; and that the rest
/// extract as Tika's path extracts them.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_with_passwords(
    documents: &[PathBuf],
    tika_name: &str,
    passwords: &[(&str, &str)],
) {
    assert_same_as_tika_with_passwords_but(
        documents,
        tika_name,
        passwords,
        WrongPassword::FailsAsTika,
    );
}

/// [`assert_same_as_tika_with_passwords`], each document a wrong password is given for handled
/// as `wrong` says.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_with_passwords_but(
    documents: &[PathBuf],
    tika_name: &str,
    passwords: &[(&str, &str)],
    wrong_password: WrongPassword,
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
        let failing = match wrong_password {
            WrongPassword::FailsAsTika => vec![ParserConfigs::default(), wrong.clone()],
            WrongPassword::Defers => {
                let routed = routed(document.to_str().unwrap());
                assert!(
                    routed
                        .parser
                        .defers(
                            &routed.input,
                            &routed.detected.metadata_for(routed.parser),
                            &wrong
                        )
                        .is_some(),
                    "{document:?} with a wrong password"
                );
                vec![ParserConfigs::default()]
            }
        };
        for configs in failing {
            assert_eq!(
                format!("{:?}", native_error(document, &configs)),
                format!("{:?}", tika_error(document, &configs)),
                "{document:?}"
            );
        }
    }
}

/// The error the native parser fails with on the document at `path`.
#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_delegating(
    documents: &[&Path],
    tika_name: &str,
    delegates: &[(&str, &str)],
) {
    assert_same_as_tika_delegating_with(documents, tika_name, delegates, &ParserConfigs::default());
}

/// [`assert_same_as_tika_delegating`] under the settings `configs`.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_delegating_with(
    documents: &[&Path],
    tika_name: &str,
    delegates: &[(&str, &str)],
    configs: &ParserConfigs,
) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates,
        departures: &[],
    });
    compare(documents, configs, expected, None, Adjustments::default());
}

/// [`assert_same_as_tika_delegating_with`], but for the values of the metadata `departures`
/// names, which the native parser gives on purpose otherwise than Tika's path.
#[cfg(feature = "tika")]
pub fn assert_same_as_tika_delegating_departing_with(
    documents: &[&Path],
    tika_name: &str,
    delegates: &[(&str, &str)],
    departures: Departures,
    configs: &ParserConfigs,
) {
    let expected = Expected::Same(Naming {
        tika_name,
        delegates,
        departures,
    });
    compare(documents, configs, expected, None, Adjustments::default());
}

/// Where a div of class `ocr` starts, the text OCR read in it following.
#[cfg(feature = "tika")]
const OCR_DIV: &str = "<div class=\"ocr\">";
/// A div of class `ocr` OCR read nothing in.
#[cfg(feature = "tika")]
const EMPTY_OCR_DIV: &str = "<div class=\"ocr\" />";

/// The text OCR read in each div of class `ocr` of an XHTML extraction, in order.
#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
type OcrTexts = (Vec<String>, Vec<String>);

/// An extraction in `output`, written in `char_set`, without what OCR read: the text in each
/// div of class `ocr` of XHTML, the `texts` read where they stand in plain text, and the
/// values of the reports of how each image was read.
#[cfg(feature = "tika")]
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

/// Tika's XHTML of a protected OOXML package, which Tika's path writes through two XHTML
/// handlers, without the second line break after each block element and the second tab before
/// each indented one, which the native path writes once (#119).
#[cfg(feature = "tika")]
pub fn without_doubled_breaks(xhtml: &str) -> String {
    use crate::formats::xhtml::{ENDLINE, INDENT};
    let block = ENDLINE.join("|");
    let breaks = regex::Regex::new(&format!(r"(</(?:{block})>|<(?:{block})(?: [^>]*)? />)\n\n"))
        .expect("a regex");
    let tabs =
        regex::Regex::new(&format!(r"\t\t(<(?:{})[ />])", INDENT.join("|"))).expect("a regex");
    let once = breaks.replace_all(xhtml, "$1\n");
    tabs.replace_all(&once, "\t$1").into_owned()
}

/// Asserts that the native parser of each document, which Tika's path parses with
/// `tika_name`, streams the UTF-8 XHTML and metadata Tika's path does under the settings
/// `configs`, once `tika_as_native` has made Tika's XHTML what the native path writes on
/// purpose, and the native XHTML is without the [`UNWRITTEN_DIVS`] Tika's has not. Its other
/// outputs are not compared.
#[cfg(feature = "tika")]
pub fn assert_same_xhtml_as_tika_with(
    documents: &[&Path],
    tika_name: &str,
    configs: &ParserConfigs,
    tika_as_native: fn(&str) -> String,
) {
    let mut differences = Vec::new();
    for document in documents {
        let path = document.to_str().unwrap();
        let names = tika_names(routed(path).parser.name(), tika_name, &[]);
        let native = native_stream(path, configs, Output::Xml, CharSet::UTF_8)
            .map(|native| comparable(as_tika(native, &names, CharSet::UTF_8), CharSet::UTF_8));
        let tika = tika_stream(path, configs, Output::Xml, CharSet::UTF_8).map(|tika| {
            let mut tika = comparable(tika, CharSet::UTF_8);
            tika.text =
                tika_as_native(std::str::from_utf8(&tika.text).expect("UTF-8")).into_bytes();
            match &native {
                Ok(native) => with_native_edit_time(tika, native, CharSet::UTF_8).0,
                Err(_) => tika,
            }
        });
        let native = match (native, &tika) {
            (Ok(mut native), Ok(tika)) => {
                let (native_text, tika_text) = (
                    std::str::from_utf8(&native.text).expect("UTF-8"),
                    std::str::from_utf8(&tika.text).expect("UTF-8"),
                );
                native.text = without_unwritten(native_text, tika_text).0.into_bytes();
                Ok(native)
            }
            (native, _) => native,
        };
        if tika != native {
            differences.push(format!(
                "{path}:\n  tika   {}\n  native {}",
                describe(&tika),
                describe(&native)
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

/// The error each path fails with on the document at `path` under `configs`, the native
/// path's first.
#[cfg(feature = "tika")]
pub fn errors(path: &Path, configs: &ParserConfigs) -> (String, String) {
    (
        format!("{:?}", native_error(path, configs)),
        format!("{:?}", tika_error(path, configs)),
    )
}

/// Asserts that Tika's path fails on each document, and that the native parser fails too.
#[cfg(feature = "tika")]
pub fn assert_fails_as_tika(documents: &[&Path]) {
    assert_fails_as_tika_with(documents, &ParserConfigs::default());
}

/// [`assert_fails_as_tika`] under the settings `configs`.
#[cfg(feature = "tika")]
pub fn assert_fails_as_tika_with(documents: &[&Path], configs: &ParserConfigs) {
    compare(
        documents,
        configs,
        Expected::Failure,
        None,
        Adjustments::default(),
    );
}

#[cfg(feature = "tika")]
fn compare(
    documents: &[&Path],
    configs: &ParserConfigs,
    expected: Expected,
    rewrite: Option<(NativeRewrite, Option<Normalisation>)>,
    adjustments: Adjustments,
) {
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
        let (line_breaks, unwritten) = match (&comparisons[0].3, &comparisons[0].4) {
            (Ok(tika), Ok(native)) => {
                let (tika, native) = (
                    String::from_utf8_lossy(&tika.text),
                    String::from_utf8_lossy(&native.text),
                );
                let adjusted = adjustments
                    .line_breaks
                    .map_or(0, |line_breaks| line_breaks(&tika, &native));
                (adjusted, without_unwritten(&native, &tika).1)
            }
            _ => (0, 0),
        };
        let unset_breaks = match (&expected, &comparisons[0].3) {
            (Expected::Same(naming), Ok(tika)) => {
                nested_head_breaks(&String::from_utf8_lossy(&tika.text), naming.departures)
            }
            _ => 0,
        };
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
                    let mut native = without_departures(
                        comparable(as_tika(native, &names, char_set), char_set),
                        char_set,
                        naming.departures,
                    );
                    let (tika, skipped_ocr_parser) =
                        without_skipped_ocr_parser(tika, char_set, configs);
                    let (mut tika, edit_time_longer) = with_native_edit_time(
                        without_departures(comparable(tika, char_set), char_set, naming.departures),
                        &native,
                        char_set,
                    );
                    with_native_failures_masked(&mut tika, &mut native);
                    if naming.tika_name == RECOGNIZED_IMAGE_PARSER {
                        tika.text = without_nested_document(tika.text, output, char_set);
                        native.text = without_nested_document(native.text, output, char_set);
                    }
                    let (tika, native) = with_rewrite(tika, native, char_set, rewrite);
                    let (tika, native) = match &ocr {
                        Some((tika_read, native_read)) => (
                            without_ocr(tika, output, char_set, tika_read),
                            without_ocr(native, output, char_set, native_read),
                        ),
                        None if entry == "string" => {
                            let mut native = cut_to(native, &tika);
                            if skipped_ocr_parser {
                                native = with_the_head_cut_as_tikas(native, &tika, char_set);
                            }
                            if let Some(longer) = edit_time_longer {
                                native.text = cut_sooner(native.text, &tika.text, longer);
                            }
                            native.text = with_the_body_end_tika_leaks(native.text, &tika.text);
                            let (tika, native) = cut_alike(tika, native, naming.departures);
                            let unset = naming
                                .departures
                                .iter()
                                .any(|departure| matches!(departure, Departure::Unset(..)));
                            if rewrite.is_some() || unset {
                                cut_texts(tika, native)
                            } else {
                                (tika, native)
                            }
                        }
                        None => (tika, native),
                    };
                    let tika = match output {
                        Output::Text if unset_breaks > 0 => without_nested_head_breaks(
                            tika,
                            &native,
                            char_set,
                            (entry == "string").then_some(output),
                            unset_breaks,
                        ),
                        _ => tika,
                    };
                    let (tika, native) = without_adjustments(
                        tika,
                        native,
                        char_set,
                        output,
                        (entry == "string").then_some(output),
                        line_breaks,
                        unwritten,
                        adjustments,
                    );
                    if tika == native {
                        continue;
                    }
                    (Ok(tika), Ok(native))
                }
                (Expected::Failure | Expected::Same(_), Err(_), Err(_)) => continue,
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
#[cfg(feature = "tika")]
type Comparison = (&'static str, Output, CharSet, Outcome, Outcome);

/// A document extracted both ways, as a string or a stream, in an output and a character set.
#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
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

#[cfg(feature = "tika")]
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
pub fn deferral(parser: &dyn Parser, path: &Path) -> Option<formats::Deferral> {
    let name = path.to_str().unwrap();
    let input = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
    let detected = formats::Detected::detect(
        &formats::OpenedFrom::File(name.into()),
        &input,
        crate::mime::Intent::Parse,
    )
    .unwrap_or_else(|e| panic!("{name} is not detected: {e:?}"));
    parser.defers(
        &input,
        &detected.metadata_for(parser),
        &ParserConfigs::default(),
    )
}

/// The non-empty files under `roots` whose detected type routing gives `parser`, split into
/// those it parses and those it defers to Tika.
pub fn documents(parser: &dyn Parser, roots: &[&Path]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    documents_with(parser, roots, &ParserConfigs::default())
}

/// [`documents`], deferred under the settings `configs`.
pub fn documents_with(
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
        let detected = formats::Detected::detect(
            &formats::OpenedFrom::File(path.into()),
            &input,
            crate::mime::Intent::Parse,
        )
        .unwrap_or_else(|e| panic!("{path} is not detected: {e:?}"));
        let routed = formats::parser_for(&detected.media_type).is_ok_and(|routed| {
            routed.name() == parser.name() && routed.media_types() == parser.media_types()
        });
        if !routed {
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
pub fn conformance_corpus() -> PathBuf {
    PathBuf::from(std::env::var_os("CONFORMANCE_CORPUS").expect(
        "CONFORMANCE_CORPUS names Tika's test documents: eval \"$(conformance/prepare.sh target/conformance)\"",
    ))
}

/// A directory for the documents a test writes, removed with it. Each is a directory of its
/// own, so tests running at once never share one.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(test: &str) -> Self {
        static CREATED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let number = CREATED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("extractous-{test}-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }

    /// The directory the documents are written in.
    pub fn directory(&self) -> &Path {
        &self.0
    }

    /// Writes `bytes` as the document `name`, returning its path.
    pub fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
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
#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
const COMPOUND_FILE_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// The files the native path spooled, with each embedded OLE storage that Tika's path spooled
/// empty taken as Tika's: Tika hands `EmbeddedSpooler` the storage as an opened container, whose
/// stream holds no bytes, while the native path writes the storage as a compound file, or
/// nothing but a cut where the budget is spent (#116). The bytes those take of the budget move
/// where it cuts the files after them: a file the native path cut sooner, by no more than those
/// bytes left over, is taken as Tika's where Tika's starts with it.
#[cfg(feature = "tika")]
fn as_tika_spools(native: Vec<Spooled>, tika: &[Spooled]) -> Vec<Spooled> {
    let mut moved = 0u64;
    native
        .into_iter()
        .enumerate()
        .map(|(index, native)| {
            let Some(tika) = tika.get(index) else {
                return native;
            };
            let same_file = tika.name == native.name
                && tika.content_type == native.content_type
                && tika.file == native.file;
            let storage = tika.size == 0
                && !tika.truncated
                && (native.content.starts_with(&COMPOUND_FILE_MAGIC)
                    || (native.truncated && native.content.is_empty()));
            if same_file && storage {
                moved += native.size;
                return Spooled {
                    size: 0,
                    truncated: false,
                    content: Vec::new(),
                    ..native
                };
            }
            let cut_sooner = native.truncated
                && tika.size > native.size
                && tika.size - native.size <= moved
                && tika.content.starts_with(&native.content);
            if same_file && cut_sooner {
                moved -= tika.size - native.size;
                return Spooled {
                    size: tika.size,
                    truncated: tika.truncated,
                    content: tika.content.clone(),
                    ..native
                };
            }
            native
        })
        .collect()
}

/// The spooled files, where a [`Departure::PngImages`] applies, with each whole PNG file the
/// image it holds, and no size: its dimensions, colour type, bit depth and resolution, then its
/// pixels.
#[cfg(feature = "tika")]
fn with_png_images(spooled: Vec<Spooled>, png_images: bool) -> Vec<Spooled> {
    if !png_images {
        return spooled;
    }
    spooled
        .into_iter()
        .map(|file| {
            if file.truncated || !file.content.starts_with(b"\x89PNG\r\n\x1a\n") {
                return file;
            }
            let mut reader = png::Decoder::new(std::io::Cursor::new(&file.content))
                .read_info()
                .unwrap_or_else(|e| panic!("{} is no PNG file: {e}", file.name));
            let info = reader.info();
            let mut content = format!(
                "{}x{} {:?} {:?} {:?}\n",
                info.width,
                info.height,
                info.color_type,
                info.bit_depth,
                info.pixel_dims
                    .map(|dims| (dims.xppu, dims.yppu, dims.unit == png::Unit::Meter)),
            )
            .into_bytes();
            let mut pixels = vec![0; reader.output_buffer_size().expect("an image in memory")];
            reader
                .next_frame(&mut pixels)
                .unwrap_or_else(|e| panic!("{} does not decode: {e}", file.name));
            content.extend(pixels);
            Spooled {
                size: 0,
                content,
                ..file
            }
        })
        .collect()
}

/// The spooled files, where a [`Departure::LineBreaks`] applies, with each CRLF of the whole
/// ones an LF, as their text is compared: the line breaks of a part the departure decodes
/// otherwise are in its bytes too. Where the budget cuts the spool, the departure moves the cut,
/// which no comparison of the cut files can take out.
#[cfg(feature = "tika")]
fn with_line_feeds(spooled: Vec<Spooled>, line_breaks: bool) -> Vec<Spooled> {
    if !line_breaks {
        return spooled;
    }
    spooled
        .into_iter()
        .map(|file| {
            if file.truncated {
                return file;
            }
            let content = replace_bytes(&file.content, b"\r\n", b"\n");
            Spooled {
                size: content.len() as u64,
                content,
                ..file
            }
        })
        .collect()
}

#[cfg(feature = "tika")]
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
#[cfg(feature = "tika")]
pub fn spools_as_tika(path: &Path, budget: u64) -> Result<usize, String> {
    spools_as_tika_rewriting(path, budget, None, &[])
}

/// [`spools_as_tika`], with the native result rewritten by `rewrite`, and both texts then by
/// its normalisation, if any, and without the values `departures` name.
#[cfg(feature = "tika")]
pub fn spools_as_tika_rewriting(
    path: &Path,
    budget: u64,
    rewrite: Option<(NativeRewrite, Option<Normalisation>)>,
    departures: Departures,
) -> Result<usize, String> {
    spools_as_tika_under(path, budget, rewrite, departures, &ParserConfigs::default())
}

/// [`spools_as_tika`] under the settings `configs`, but for what `departures` name.
#[cfg(feature = "tika")]
pub fn spools_as_tika_departing_with(
    path: &Path,
    budget: u64,
    departures: Departures,
    configs: &ParserConfigs,
) -> Result<usize, String> {
    spools_as_tika_under(path, budget, None, departures, configs)
}

#[cfg(feature = "tika")]
fn spools_as_tika_under(
    path: &Path,
    budget: u64,
    rewrite: Option<(NativeRewrite, Option<Normalisation>)>,
    departures: Departures,
    configs: &ParserConfigs,
) -> Result<usize, String> {
    let path = path.to_str().unwrap();
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
        configs,
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
        deadline: None,
        spool: Some(crate::EmbeddedSpool {
            dir: native_spool.directory().to_path_buf(),
            byte_budget: budget,
        }),
    };
    let native = formats::parse_to_stream(
        routed.parser,
        routed.input.to_static(),
        routed.detected,
        configs.clone(),
        options,
    )
    .map(|prepared| {
        prepared.unwrap_or_else(|unsupported| panic!("{}: {unsupported}", deferred(path)))
    })
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
            let (mut tika_extracted, mut native_extracted) = with_rewrite(
                without_departures(tika_extracted, CharSet::UTF_8, departures),
                without_departures(native_extracted, CharSet::UTF_8, departures),
                CharSet::UTF_8,
                rewrite,
            );
            if tika_name == RECOGNIZED_IMAGE_PARSER {
                tika_extracted.text =
                    without_nested_document(tika_extracted.text, Output::Xml, CharSet::UTF_8);
                native_extracted.text =
                    without_nested_document(native_extracted.text, Output::Xml, CharSet::UTF_8);
            }
            let (tika_extracted, _) =
                with_native_edit_time(tika_extracted, &native_extracted, CharSet::UTF_8);
            let (mut tika_extracted, mut native_extracted) =
                without_ocr_both(tika_extracted, native_extracted);
            with_native_failures_masked(&mut tika_extracted, &mut native_extracted);
            native_extracted.text = without_unwritten(
                &String::from_utf8_lossy(&native_extracted.text),
                &String::from_utf8_lossy(&tika_extracted.text),
            )
            .0
            .into_bytes();
            let line_breaks = departures
                .iter()
                .any(|departure| matches!(departure, Departure::LineBreaks(_)));
            let png_images = departures
                .iter()
                .any(|departure| matches!(departure, Departure::PngImages(_)));
            let tika = (
                tika_extracted,
                with_png_images(
                    with_line_feeds(spooled(embedded, tika_spool.directory()), line_breaks),
                    png_images,
                ),
            );
            let native = (
                native_extracted,
                as_tika_spools(
                    with_png_images(
                        with_line_feeds(
                            spooled(native_embedded, native_spool.directory()),
                            line_breaks,
                        ),
                        png_images,
                    ),
                    &tika.1,
                ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_recognized_images_without_their_nested_document() {
        let tika = "<head>\n<title></title>\n</head>\n<body><html>\n\n<meta name=\"a\" \
                    content=\"b\" />\n<title></title>\n\n<body /></html></body></html>";
        let ocr = "<body><html>\n<title></title>\n<body /></html><div class=\"ocr\">x</div></body>";
        for (char_set, tika, native) in [
            (
                CharSet::UTF_8,
                tika,
                "<head>\n<title></title>\n</head>\n<body /></html>",
            ),
            (
                CharSet::UTF_16BE,
                ocr,
                "<body><div class=\"ocr\">x</div></body>",
            ),
        ] {
            let encoded = |text: &str| {
                let mut bytes = Vec::new();
                formats::encode(text, char_set, &mut bytes);
                bytes
            };
            assert_eq!(
                without_nested_document(encoded(tika), Output::Xml, char_set),
                encoded(native)
            );
        }
        assert_eq!(
            without_nested_document(b"\n\n\nx\n".to_vec(), Output::Text, CharSet::UTF_8),
            b"x\n"
        );
    }

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
        let tika = "<html xmlns=\"x\">\n<head>\n<meta name=\"b\" content=\"1\" />\n<meta name=\"a\" content=\"2\n/>\" />\n<meta name=\"b\" content=\"0\" />\n<title></title>\n</head>\n<body><div xmlns:z=\"u:z\" xmlns:a=\"u:a\"><p>t</p></div>\n<meta name=\"d\" content=\"\" />\n<meta name=\"c\" content=\"\" />\nx<meta name=\"a\" content=\"\" />\n</body></html>";
        let native = "<html xmlns=\"x\">\n<head>\n<meta name=\"a\" content=\"2\n/>\" />\n<meta name=\"b\" content=\"1\" />\n<meta name=\"b\" content=\"0\" />\n<title></title>\n</head>\n<body><div xmlns:a=\"u:a\" xmlns:z=\"u:z\"><p>t</p></div>\n<meta name=\"c\" content=\"\" />\n<meta name=\"d\" content=\"\" />\nx<meta name=\"a\" content=\"\" />\n</body></html>";
        for char_set in [CharSet::UTF_8, CharSet::UTF_16BE] {
            let encoded = |text: &str| {
                let mut bytes = Vec::new();
                formats::encode(text, char_set, &mut bytes);
                bytes
            };
            assert_eq!(in_defined_order(encoded(tika), char_set), encoded(native));
            assert_eq!(in_defined_order(encoded(native), char_set), encoded(native));
        }
        let cut = |first: &str, second: &str| {
            format!("<head>\n<meta name=\"{first}\" content=\"\" />\n<meta name=\"{second}\" content=\"\" />")
        };
        let ordered = |text: String| in_defined_order(text.into_bytes(), CharSet::UTF_8);
        assert_eq!(ordered(cut("b", "c")), ordered(cut("a", "d")));
        assert_eq!(
            ordered(cut("b", "c")),
            format!("<head>\n{CUT_HEAD}").into_bytes()
        );
    }

    #[test]
    fn takes_the_end_tags_tikas_leaked_body_end_writes_after_a_cut() {
        let native = b"<body><table><tr><td>Thank".to_vec();
        let tika = b"<body><table><tr><td>Thank</td></tr></table></body></html>";
        assert_eq!(with_the_body_end_tika_leaks(native.clone(), tika), tika);
        let closed = b"<body><table><tr><td>Thank</td></tr></table>";
        assert_eq!(with_the_body_end_tika_leaks(native.clone(), closed), native);
        let more = b"<body><table><tr><td>Thank you</td></tr></table></body>";
        assert_eq!(with_the_body_end_tika_leaks(native.clone(), more), native);
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
