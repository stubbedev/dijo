use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use dijo_core::{EmbeddedSpool, Extractor, SpooledEmbedded};
use serde::{Deserialize, Serialize};

use crate::cache::{Cache, OutputSettings};
use crate::cli::Format;
use crate::render::{self, Opened, Outcome, RenderOptions};
use crate::schema::{ErrorCategory, ExtractionError, Source, Status, SCHEMA_VERSION};

/// Bounds on how far, and how much, extraction descends into embedded documents.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Limits {
    pub max_depth: usize,
    pub max_documents: usize,
    pub max_expanded_bytes: u64,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        max_depth: 16,
        max_documents: 10_000,
        max_expanded_bytes: 4 << 30,
    };
}

/// A document of a tree as the walk hands it to its [`Records`].
pub struct Document<'a> {
    pub path: &'a Path,
    /// Its file name, as its container names it.
    pub name: &'a str,
    pub source: &'a Source,
    /// 0 for the input, then each embedded document numbered in the order it is found.
    pub id: usize,
    pub depth: usize,
}

/// What a walk writes of the documents it finds.
pub trait Records {
    /// Writes a document, whose extraction `open` starts, and returns its outcome; the walk
    /// descends into the documents spooled in its `embedded`.
    fn document(
        &mut self,
        document: &Document<'_>,
        open: &dyn Fn() -> Opened,
    ) -> io::Result<Outcome>;
    /// Writes a document, `depth` levels down, that a limit skipped.
    fn skipped(&mut self, source: &Source, error: &ExtractionError, depth: usize)
        -> io::Result<()>;
}

/// Extracts a document and then, one at a time, every document embedded in it, recursively,
/// each as a document in its own right that records where it came from, and hands each to
/// `records`.
pub fn render_tree(
    extractor: &Extractor,
    path: &Path,
    display: &str,
    limits: Limits,
    scratch: &Path,
    records: &mut dyn Records,
) -> io::Result<Outcome> {
    let mut walk = Walk {
        extractor,
        display,
        limits,
        scratch,
        records,
        documents: 0,
        expanded_bytes: 0,
        spools: 0,
        child_errors: Vec::new(),
        children_complete: true,
    };
    let root = walk.document(path, display, None, 0, 0)?;
    let status = match (root.status, walk.children_complete) {
        (Status::Complete, false) => Status::Partial,
        (status, _) => status,
    };
    Ok(Outcome {
        status,
        errors: root.errors.into_iter().chain(walk.child_errors).collect(),
        metadata: root.metadata,
        embedded: Vec::new(),
    })
}

/// dijo's own records: each document in the output format, extracted with `options`. An
/// embedded document whose content was already extracted in this input is written as a
/// reference to the first one. With a cache, each document without embedded documents of its
/// own is looked up and stored by its own content, wherever it is embedded.
pub struct DijoRecords<'a> {
    options: RenderOptions,
    cache: Option<(&'a Cache, OutputSettings<'a>)>,
    sink: &'a mut dyn Write,
    /// The embedded path each content hash was first extracted at.
    seen: HashMap<String, String>,
}

impl<'a> DijoRecords<'a> {
    pub fn new(
        options: RenderOptions,
        cache: Option<(&'a Cache, OutputSettings<'a>)>,
        sink: &'a mut dyn Write,
    ) -> Self {
        DijoRecords {
            options,
            cache,
            sink,
            seen: HashMap::new(),
        }
    }

    fn reference(&mut self, source: &Source, first: &str) -> io::Result<()> {
        match self.options.format {
            Format::Text | Format::Xhtml | Format::Markdown => {
                let embedded_path = source.embedded_path.as_deref().unwrap_or_default();
                write!(
                    self.sink,
                    "\n\n=== {embedded_path} (same as {first}) ===\n\n"
                )
            }
            Format::Json | Format::Pages | Format::Elements => {
                serde_json::to_writer(
                    &mut *self.sink,
                    &Reference {
                        schema_version: SCHEMA_VERSION,
                        kind: "document",
                        source,
                        duplicate_of: first,
                        status: Status::Complete,
                        errors: [],
                    },
                )?;
                self.sink.write_all(b"\n")
            }
        }
    }
}

struct Walk<'a> {
    extractor: &'a Extractor,
    display: &'a str,
    limits: Limits,
    scratch: &'a Path,
    records: &'a mut dyn Records,
    documents: usize,
    expanded_bytes: u64,
    spools: usize,
    child_errors: Vec<ExtractionError>,
    children_complete: bool,
}

#[derive(Serialize)]
struct Reference<'a> {
    schema_version: u32,
    kind: &'static str,
    source: &'a Source,
    duplicate_of: &'a str,
    status: Status,
    errors: [ExtractionError; 0],
}

#[derive(Serialize)]
struct Skipped<'a> {
    schema_version: u32,
    kind: &'static str,
    source: &'a Source,
    status: Status,
    errors: [&'a ExtractionError; 1],
}

impl Walk<'_> {
    /// Extracts the document at `path`, known by the file name `name`, then its embedded ones.
    fn document(
        &mut self,
        path: &Path,
        name: &str,
        embedded_path: Option<String>,
        id: usize,
        depth: usize,
    ) -> io::Result<Outcome> {
        let source = render::source(path, self.display, embedded_path.clone());
        let spool = EmbeddedSpool {
            dir: self.scratch.join(format!("embedded-{}", self.spools)),
            byte_budget: self.limits.max_expanded_bytes - self.expanded_bytes,
        };
        self.spools += 1;
        fs::create_dir(&spool.dir)?;
        let extractor = self.extractor;
        let open = || render::open_spooling(extractor, path, &spool);
        let document = Document {
            path,
            name,
            source: &source,
            id,
            depth,
        };
        let mut outcome = self.records.document(&document, &open)?;
        for child in std::mem::take(&mut outcome.embedded) {
            let child_path = match &embedded_path {
                Some(parent) => format!("{parent}/{}", child.name),
                None => child.name.clone(),
            };
            self.child(child, child_path, depth + 1)?;
        }
        fs::remove_dir_all(&spool.dir)?;
        Ok(outcome)
    }

    fn child(
        &mut self,
        child: SpooledEmbedded,
        embedded_path: String,
        depth: usize,
    ) -> io::Result<()> {
        self.documents += 1;
        self.expanded_bytes += child.size;
        let limit = if self.documents > self.limits.max_documents {
            Some(format!(
                "More than {} embedded documents; this one and the rest were skipped",
                self.limits.max_documents
            ))
        } else if depth > self.limits.max_depth {
            Some(format!(
                "Nested deeper than {} levels; this document was skipped",
                self.limits.max_depth
            ))
        } else if child.truncated {
            Some(format!(
                "The embedded documents expand to more than {} bytes; this one was skipped",
                self.limits.max_expanded_bytes
            ))
        } else {
            None
        };
        let Some(message) = limit else {
            let outcome = self.document(
                &child.path,
                &child.name,
                Some(embedded_path.clone()),
                self.documents,
                depth,
            )?;
            self.children_complete &= outcome.status == Status::Complete;
            self.child_errors
                .extend(outcome.errors.into_iter().map(|error| ExtractionError {
                    location: Some(embedded_path.clone()),
                    ..error
                }));
            return Ok(());
        };
        let error = ExtractionError {
            category: ErrorCategory::LimitExceeded,
            message,
            location: Some(embedded_path.clone()),
        };
        let source = Source {
            path: self.display.to_string(),
            embedded_path: Some(embedded_path),
            size: None,
            blake3: None,
        };
        self.records.skipped(&source, &error, depth)?;
        self.children_complete = false;
        self.child_errors.push(error);
        Ok(())
    }
}

impl Records for DijoRecords<'_> {
    fn document(
        &mut self,
        document: &Document<'_>,
        open: &dyn Fn() -> Opened,
    ) -> io::Result<Outcome> {
        if let Some(embedded_path) = &document.source.embedded_path {
            let first = document
                .source
                .blake3
                .as_ref()
                .and_then(|hash| self.seen.get(hash))
                .cloned();
            if let Some(first) = first {
                self.reference(document.source, &first)?;
                return Ok(Outcome {
                    status: Status::Complete,
                    errors: Vec::new(),
                    metadata: None,
                    embedded: Vec::new(),
                });
            }
            if let Some(hash) = &document.source.blake3 {
                self.seen.insert(hash.clone(), embedded_path.clone());
            }
            if matches!(self.options.format, Format::Text | Format::Markdown) {
                write!(self.sink, "\n\n=== {embedded_path} ===\n\n")?;
            }
        }
        let options = self.options;
        let render =
            |sink: &mut dyn Write| render::render_opened(open(), document.source, options, sink);
        match self.cache {
            Some((cache, settings)) => cache.render(
                document.path,
                document.name,
                document.source,
                settings,
                self.sink,
                render,
            ),
            None => render(self.sink),
        }
    }

    fn skipped(
        &mut self,
        source: &Source,
        error: &ExtractionError,
        _depth: usize,
    ) -> io::Result<()> {
        match self.options.format {
            Format::Text | Format::Xhtml | Format::Markdown => Ok(()),
            Format::Json | Format::Pages | Format::Elements => {
                serde_json::to_writer(
                    &mut *self.sink,
                    &Skipped {
                        schema_version: SCHEMA_VERSION,
                        kind: "document",
                        source,
                        status: Status::Failed,
                        errors: [error],
                    },
                )?;
                self.sink.write_all(b"\n")
            }
        }
    }
}
