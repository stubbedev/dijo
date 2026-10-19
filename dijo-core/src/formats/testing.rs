//! What the tests of the crates of the formats share: driving a parser through the native
//! path's routing and string extraction, finding the documents it reads, and scratch
//! directories for the documents they write.

use std::path::{Path, PathBuf};

use crate::config::ParserConfigs;
#[cfg(feature = "testing")]
pub use crate::formats::Routed;
use crate::formats::{self, Parser};
/// The native path's routing and string extraction, for the tests that drive a parser through them.
#[cfg(feature = "testing")]
pub use crate::formats::{parse_to_string, route_file};

/// Installs the routes of the documents embedded in those native parsers read, which routing a
/// document installs, for the tests that write a document with its parser directly.
#[cfg(feature = "testing")]
pub fn install_routes() {
    formats::install_routes(&formats::Registry);
}

/// Where `parser` falls short of Tika's parser on the document at `path`, given the metadata
/// the detection starts it from, if it does: what it does not read, or reads differently.
pub fn shortfall(parser: &dyn Parser, path: &Path) -> Option<String> {
    let name = path.to_str().unwrap();
    let input = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
    let detected = formats::Detected::detect(
        &formats::OpenedFrom::File(name.into()),
        &input,
        crate::mime::Intent::Parse,
    )
    .unwrap_or_else(|e| panic!("{name} is not detected: {e:?}"));
    parser.shortfall(
        &input,
        &detected.metadata_for(parser),
        &ParserConfigs::default(),
    )
}

/// Where `parser` reads the document at `path` differently from Tika's parser, given the
/// metadata the detection starts it from, and the XHTML of its reading, which the extraction
/// takes (#144), well-formed. Panics where the parser takes the document or cannot read it.
pub fn differing_reading(parser: &dyn Parser, path: &Path) -> (formats::Difference, String) {
    formats::install_routes(&formats::Registry);
    let name = path.to_str().unwrap();
    let input = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
    let detected = formats::Detected::detect(
        &formats::OpenedFrom::File(name.into()),
        &input,
        crate::mime::Intent::Parse,
    )
    .unwrap_or_else(|e| panic!("{name} is not detected: {e:?}"));
    let configs = ParserConfigs::default();
    let metadata = detected.metadata_for(parser);
    let (reading, difference) = match parser.prepare(&input, &metadata, &configs) {
        Err(formats::Decline::Differs {
            reading,
            difference,
        }) => (reading, difference),
        Ok(_) => panic!("{name}: {} reads it as Tika's parser does", parser.name()),
        Err(declined) => panic!("{name}: {} declines it: {declined:?}", parser.name()),
    };
    let mut out = formats::StringOut::default();
    let chain = formats::Chain::Stream {
        output: formats::Output::Xml,
        encoding: "UTF-8",
    };
    let (_, _, result) = chain.write_to(&mut out, |handler| {
        formats::xhtml::Xhtml::write(
            handler,
            metadata,
            &formats::ParseContext::new(&configs),
            reading,
        )
    });
    if let Err(failure) = result {
        panic!("{name}: the reading fails: {failure:?}");
    }
    (difference, out.0)
}

/// The non-empty files under `roots` whose detected type routing gives `parser`, split into
/// those it parses as Tika's parser does and those it falls short on ([`Parser::shortfall`]).
pub fn documents(parser: &dyn Parser, roots: &[&Path]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    documents_with(parser, roots, &ParserConfigs::default())
}

/// [`documents`], under the settings `configs`.
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
    let (mut parsed, mut shortfalls) = (Vec::new(), Vec::new());
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
            .shortfall(&input, &detected.metadata_for(parser), configs)
            .is_some()
        {
            shortfalls.push(file);
        } else {
            parsed.push(file);
        }
    }
    (parsed, shortfalls)
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
            std::env::temp_dir().join(format!("dijo-{test}-{}-{number}", std::process::id()));
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
