use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use extractous_core::{EmbeddedSpool, Extractor, SpooledEmbedded};
use serde::{Deserialize, Serialize};

use crate::cache::{Cache, OutputSettings};
use crate::cli::Format;
use crate::render::{self, Outcome, RenderOptions};
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

/// Extracts a document and then, one at a time, every document embedded in it, recursively,
/// each as a document in its own right that records where it came from. A document whose
/// content was already extracted in this input is reported as a reference to the first one.
/// With a cache, each document without embedded documents of its own is looked up and stored
/// by its own content, wherever it is embedded.
#[allow(clippy::too_many_arguments)]
pub fn render_tree(
    extractor: &Extractor,
    path: &Path,
    display: &str,
    options: RenderOptions,
    limits: Limits,
    scratch: &Path,
    cache: Option<(&Cache, OutputSettings)>,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    let mut walk = Walk {
        extractor,
        display,
        options,
        limits,
        scratch,
        cache,
        seen: HashMap::new(),
        documents: 0,
        expanded_bytes: 0,
        spools: 0,
        child_errors: Vec::new(),
        children_complete: true,
    };
    let root = walk.document(path, display, None, 0, sink)?;
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

struct Walk<'a> {
    extractor: &'a Extractor,
    display: &'a str,
    options: RenderOptions,
    limits: Limits,
    scratch: &'a Path,
    cache: Option<(&'a Cache, OutputSettings<'a>)>,
    seen: HashMap<String, String>,
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
        depth: usize,
        sink: &mut dyn Write,
    ) -> io::Result<Outcome> {
        let source = render::source(path, self.display, embedded_path.clone());
        if let Some(embedded_path) = &embedded_path {
            if let Some(first) = source.blake3.as_ref().and_then(|hash| self.seen.get(hash)) {
                let first = first.clone();
                self.write_reference(&source, &first, sink)?;
                return Ok(Outcome {
                    status: Status::Complete,
                    errors: Vec::new(),
                    metadata: None,
                    embedded: Vec::new(),
                });
            }
            if self.options.format == Format::Text {
                write!(sink, "\n\n=== {embedded_path} ===\n\n")?;
            }
        }
        if let (Some(hash), Some(embedded_path)) = (&source.blake3, &embedded_path) {
            self.seen.insert(hash.clone(), embedded_path.clone());
        }

        let spool = EmbeddedSpool {
            dir: self.scratch.join(format!("embedded-{}", self.spools)),
            byte_budget: self.limits.max_expanded_bytes - self.expanded_bytes,
        };
        self.spools += 1;
        fs::create_dir(&spool.dir)?;
        let extractor = self.extractor;
        let options = self.options;
        let render = |sink: &mut dyn Write| {
            let opened = render::open_spooling(extractor, path, &spool);
            render::render_opened(opened, &source, options, sink)
        };
        let mut outcome = match self.cache {
            Some((cache, settings)) => cache.render(path, name, &source, settings, sink, render)?,
            None => render(sink)?,
        };
        for child in std::mem::take(&mut outcome.embedded) {
            let child_path = match &embedded_path {
                Some(parent) => format!("{parent}/{}", child.name),
                None => child.name.clone(),
            };
            self.child(child, child_path, depth + 1, sink)?;
        }
        fs::remove_dir_all(&spool.dir)?;
        Ok(outcome)
    }

    fn child(
        &mut self,
        child: SpooledEmbedded,
        embedded_path: String,
        depth: usize,
        sink: &mut dyn Write,
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
                depth,
                sink,
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
        self.write_skipped(&source, &error, sink)?;
        self.children_complete = false;
        self.child_errors.push(error);
        Ok(())
    }

    fn write_reference(
        &self,
        source: &Source,
        first: &str,
        sink: &mut dyn Write,
    ) -> io::Result<()> {
        match self.options.format {
            Format::Text | Format::Xhtml => {
                let embedded_path = source.embedded_path.as_deref().unwrap_or_default();
                write!(sink, "\n\n=== {embedded_path} (same as {first}) ===\n\n")
            }
            Format::Json | Format::Pages | Format::Elements => {
                serde_json::to_writer(
                    &mut *sink,
                    &Reference {
                        schema_version: SCHEMA_VERSION,
                        kind: "document",
                        source,
                        duplicate_of: first,
                        status: Status::Complete,
                        errors: [],
                    },
                )?;
                sink.write_all(b"\n")
            }
        }
    }

    fn write_skipped(
        &self,
        source: &Source,
        error: &ExtractionError,
        sink: &mut dyn Write,
    ) -> io::Result<()> {
        match self.options.format {
            Format::Text | Format::Xhtml => Ok(()),
            Format::Json | Format::Pages | Format::Elements => {
                serde_json::to_writer(
                    &mut *sink,
                    &Skipped {
                        schema_version: SCHEMA_VERSION,
                        kind: "document",
                        source,
                        status: Status::Failed,
                        errors: [error],
                    },
                )?;
                sink.write_all(b"\n")
            }
        }
    }
}
