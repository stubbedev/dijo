//! Compares a native parser with Tika's path, which it replaces, on the same documents: the
//! streamed text and XHTML, their final metadata, and the text and XHTML of a string extraction
//! cut by a small length limit. The only difference allowed is the parser's name in
//! `tk:parsed-by`, which the native output is compared under Tika's. Documents Tika's path fails
//! on are compared on their outcome alone: the native parser must fail too. A native parser that
//! records the parsers its Tika counterpart delegates to is compared under their Tika names too.
//!
//! Two things of a document's embedded documents, which Tika parses either way, are compared as
//! they can be: the parsers that read them, each once as Tika's record keeps it, once the native
//! parser goes by Tika's name; and the stack traces of their failures without the Java frames
//! below the embedded document's parse, the embedding parser's in Tika's path and the native
//! bridge's in the other, which no native parser runs, and with the random numbers of Tika's
//! temporary files masked. A failure the parser records itself (`embedded-stream-exception`) is
//! compared without its frames, all of them the Java parser's. In a string extraction, the
//! native path, which parses an embedded document whole, may go on to more parsers and failures
//! than Tika's, which stops at the length limit; Tika's must lead the native ones.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::config::ParserConfigs;
use crate::formats::{self, Output, Parser, Routed, StreamOptions};
use crate::isolate::isolate;
use crate::tika::{self, Source};
use crate::{CharSet, Metadata};

const SMALL_LIMIT: usize = 100;

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
    .map_err(|e| e.to_string())?
    .unwrap_or_else(|| panic!("{}", deferred(path)));
    Ok(Extracted {
        text: text.into_bytes(),
        metadata: sorted(metadata),
    })
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
/// The first frame below an embedded document's parse in Tika's path, where the embedding
/// parser hands it to the embedded document extractor.
const TIKA_EMBEDDING_FRAME: &str = "\tat org.apache.tika.parser.DelegatingParser.parse(";
/// The first frame below an embedded document's parse in the native path.
const NATIVE_EMBEDDING_FRAME: &str = "\tat ai.yobix.TikaNativeMain.parseEmbedded(";

/// An extraction as the two paths are compared on it: each parser of the full set once, and
/// the failures of embedded documents without the frames that embedded them.
fn comparable(mut extracted: Extracted) -> Extracted {
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
        } else if name == EMBEDDED_STREAM_EXCEPTION {
            for value in values.iter_mut() {
                *value = value
                    .split_inclusive('\n')
                    .filter(|line| !line.starts_with('\t'))
                    .collect();
            }
        }
    }
    extracted
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

/// The text with the random number of each of Tika's temporary files, `apache-tika-<digits>`,
/// masked.
fn without_temporary_names(text: &str) -> String {
    const PREFIX: &str = "apache-tika-";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        out.push_str(&rest[..at + PREFIX.len()]);
        rest =
            rest[at + PREFIX.len()..].trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected<'a> {
    /// Both extract it identically.
    Same(Naming<'a>),
    /// Both fail on it.
    Failure,
    /// Both extract it identically but for the XML string cut at the write limit inside an HTML
    /// part of an embedded message: there Tika's path also writes the `</body>` that the HTML
    /// parse it aborts leaks through the body filter, which the native replay of the embedded
    /// document, parsed whole without the limit, never sees.
    LeaksBodyEndAtLimit(Naming<'a>),
}

/// How Tika's path names the parsers of a document the native parser extracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Naming<'a> {
    /// The class name of the parser of the document.
    tika_name: &'a str,
    /// The Tika names of the parsers it delegates to, each paired after its native name.
    delegates: &'a [(&'a str, &'a str)],
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
    });
    compare(documents, configs, expected);
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
    });
    compare(documents, configs, expected);
}

/// Asserts that Tika's path fails on each document, and that the native parser fails too.
pub(crate) fn assert_fails_as_tika(documents: &[&Path]) {
    assert_fails_as_tika_with(documents, &ParserConfigs::default());
}

/// [`assert_fails_as_tika`] under the settings `configs`.
pub(crate) fn assert_fails_as_tika_with(documents: &[&Path], configs: &ParserConfigs) {
    compare(documents, configs, Expected::Failure);
}

/// Asserts what [`assert_same_as_tika`] does, but that Tika's XML string, cut at the write limit
/// inside an HTML part of an embedded message, ends in the `</body>` its aborted HTML parse leaks
/// ([`Expected::LeaksBodyEndAtLimit`]).
pub(crate) fn assert_same_as_tika_but_body_end_leaked_at_limit(
    documents: &[&Path],
    tika_name: &str,
) {
    compare(
        documents,
        &ParserConfigs::default(),
        Expected::LeaksBodyEndAtLimit(Naming {
            tika_name,
            delegates: &[],
        }),
    );
}

fn compare(documents: &[&Path], configs: &ParserConfigs, expected: Expected) {
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
                let as_expected = match (expected, &tika, &native) {
                    (
                        Expected::Same(naming) | Expected::LeaksBodyEndAtLimit(naming),
                        Ok(tika),
                        Ok(native),
                    ) => {
                        let names: Vec<(&str, &str)> = [(native_name, naming.tika_name)]
                            .into_iter()
                            .chain(naming.delegates.iter().copied())
                            .collect();
                        let tika = comparable(tika.clone());
                        let mut native = comparable(as_tika(native.clone(), &names, char_set));
                        if matches!(expected, Expected::LeaksBodyEndAtLimit(_))
                            && entry == "string"
                            && output == Output::Xml
                        {
                            native.text.extend_from_slice(b"</body>");
                        }
                        if entry == "string" {
                            tika == cut_to(native, &tika)
                        } else {
                            tika == native
                        }
                    }
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
        None,
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
            let parsed_by = |metadata: &[(String, Vec<String>)]| -> Vec<String> {
                metadata
                    .iter()
                    .find(|(name, _)| name == "tk:parsed-by")
                    .map(|(_, values)| values.clone())
                    .unwrap_or_else(|| panic!("{path:?}: an extraction without tk:parsed-by"))
            };
            let native_parsers = parsed_by(&native_metadata);
            let tika_parsers = parsed_by(&metadata);
            let names: Vec<(&str, &str)> = native_parsers
                .iter()
                .zip(&tika_parsers)
                .map(|(native, tika)| (native.as_str(), tika.as_str()))
                .collect();
            let tika = (
                comparable(Extracted { text, metadata }),
                spooled(embedded, tika_spool.directory()),
            );
            let native = (
                comparable(as_tika(
                    Extracted {
                        text: native_text,
                        metadata: native_metadata,
                    },
                    &names,
                    CharSet::UTF_8,
                )),
                spooled(native_embedded, native_spool.directory()),
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
                Err(format!(
                    "{path} with a budget of {budget} bytes\n  tika   {}\n  native {}",
                    show(&tika),
                    show(&native)
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
}
