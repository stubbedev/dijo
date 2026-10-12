use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use extractous_core::Metadata;

use crate::cli::{CacheArgs, Embedded, Ocr};
use crate::render::{self, Outcome, RenderOptions};
use crate::schema::{ErrorCategory, ExtractionError, Source, Status};
use serde::{Deserialize, Serialize};

const TRAILER_LENGTH_BYTES: usize = 8;
const TEMPORARY_PREFIX: &str = ".partial-";
const PATHS_DIR: &str = "paths";

/// What, besides the input itself, decides the output of an extraction.
#[derive(Debug, Clone, Copy)]
pub struct OutputSettings<'a> {
    pub render: RenderOptions,
    pub ocr: Ocr,
    pub ocr_lang: Option<&'a str>,
    /// Whether the output holds the text of embedded documents, or each embedded document is
    /// an entry of its own.
    pub embedded: Embedded,
}

impl OutputSettings<'_> {
    /// The settings as part of a cache key. The key also covers the build, so the encoding only
    /// has to be stable within one build.
    fn key(&self) -> String {
        format!(
            "{:?}\0{:?}\0{:?}\0{:?}",
            self.render, self.ocr, self.ocr_lang, self.embedded
        )
    }
}

/// Rendered output of complete extractions, keyed by everything that determines it: the input's
/// content and name, the settings that shape the output, this exact build of extractous with
/// Tika inside it, and the tesseract that runs OCR. Where the input came from is not part of the
/// key: a replayed entry reports the provenance it is replayed for, so the same attachment in
/// many emails is extracted once. Safe to share between threads and processes.
pub struct Cache {
    dir: PathBuf,
    policy: CachePolicy,
    build: blake3::Hash,
    stored_bytes: AtomicU64,
    next_temporary: AtomicU64,
}

/// What the cache keeps, and how it recognises unchanged inputs.
#[derive(Debug, Clone, Copy, Default)]
pub struct CachePolicy {
    /// Evict least recently used entries once the cache holds more.
    pub max_bytes: Option<u64>,
    /// Keep partial results too, except those cut short by a timeout, which depend on the run
    /// rather than on the input.
    pub store_partial: bool,
    /// Take an input as unchanged when its path, size and modification time match the last
    /// time it was hashed, instead of hashing it again.
    pub trust_mtime: bool,
}

#[derive(Serialize, Deserialize)]
struct Trailer {
    status: Status,
    errors: Vec<ExtractionError>,
    metadata: Option<Metadata>,
    /// The provenance the output reports, to be replaced by the one it is replayed for.
    source: Source,
}

/// The result of looking an input up.
pub enum Lookup<'a> {
    Hit(Hit),
    Miss(Miss<'a>),
}

/// A stored entry, ready to be replayed.
pub struct Hit {
    file: File,
    entry: PathBuf,
}

/// Where the output of an extraction will be stored if it turns out complete.
pub struct Miss<'a> {
    cache: &'a Cache,
    entry: PathBuf,
}

impl Cache {
    /// The cache the command line asks for, if any.
    pub fn from_args(args: &CacheArgs, trust_mtime: bool) -> Result<Option<Cache>, String> {
        args.cache_dir
            .as_deref()
            .map(|dir| {
                let policy = CachePolicy {
                    max_bytes: args.cache_max_size,
                    store_partial: args.cache_partial,
                    trust_mtime,
                };
                Cache::open(dir, policy)
            })
            .transpose()
    }

    pub fn open(dir: &Path, policy: CachePolicy) -> Result<Cache, String> {
        fs::create_dir_all(dir).map_err(|e| {
            format!(
                "Failed to create the cache directory {}: {e}",
                dir.display()
            )
        })?;
        let executable = std::env::current_exe()
            .map_err(|e| format!("Failed to locate the running executable: {e}"))?;
        let mut build = blake3::Hasher::new();
        build.update(
            hash_file(&executable)
                .map_err(|e| format!("Failed to hash {}: {e}", executable.display()))?
                .as_bytes(),
        );
        build.update(tesseract_version().as_bytes());
        let cache = Cache {
            dir: dir.to_path_buf(),
            policy,
            build: build.finalize(),
            stored_bytes: AtomicU64::new(0),
            next_temporary: AtomicU64::new(0),
        };
        cache
            .evict()
            .map_err(|e| format!("Failed to bound the cache {}: {e}", dir.display()))?;
        Ok(cache)
    }

    /// Writes the cached output for the input at `path`, known as `name`, into `sink`, reporting
    /// `source` as its provenance, or renders it with `render` and stores the result when it is
    /// complete.
    pub fn render(
        &self,
        path: &Path,
        name: &str,
        source: &Source,
        settings: OutputSettings,
        sink: &mut dyn Write,
        render: impl FnOnce(&mut dyn Write) -> io::Result<Outcome>,
    ) -> io::Result<Outcome> {
        match self.lookup(path, name, settings)? {
            Lookup::Hit(hit) => hit.replay(sink, source),
            Lookup::Miss(miss) => miss.store(sink, source, render),
        }
    }

    pub fn lookup(
        &self,
        path: &Path,
        display: &str,
        settings: OutputSettings,
    ) -> io::Result<Lookup<'_>> {
        let entry = self.entry(path, display, settings)?;
        match OpenOptions::new().read(true).write(true).open(&entry) {
            Ok(file) => Ok(Lookup::Hit(Hit { file, entry })),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(Lookup::Miss(Miss { cache: self, entry }))
            }
            Err(error) => Err(io::Error::new(
                error.kind(),
                format!(
                    "Failed to open the cache entry {}: {error}",
                    entry.display()
                ),
            )),
        }
    }

    /// Removes the least recently used entries until the cache fits in its size limit.
    fn evict(&self) -> io::Result<()> {
        let Some(max_bytes) = self.policy.max_bytes else {
            return Ok(());
        };
        let mut entries = Vec::new();
        for shard in fs::read_dir(&self.dir)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() || shard.file_name() == PATHS_DIR {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(TEMPORARY_PREFIX)
                {
                    continue;
                }
                let metadata = entry.metadata()?;
                entries.push((metadata.modified()?, metadata.len(), entry.path()));
            }
        }
        let mut total: u64 = entries.iter().map(|(_, size, _)| size).sum();
        entries.sort();
        for (_, size, path) in entries {
            if total <= max_bytes {
                break;
            }
            match fs::remove_file(&path) {
                Ok(()) => total -= size,
                Err(error) if error.kind() == io::ErrorKind::NotFound => total -= size,
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("Failed to evict {}: {error}", path.display()),
                    ))
                }
            }
        }
        self.stored_bytes.store(total, Ordering::Relaxed);
        Ok(())
    }

    /// Counts a newly stored entry, and evicts once the cache has grown past its limit.
    fn stored(&self, bytes: u64) -> io::Result<()> {
        let total = self.stored_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if self.policy.max_bytes.is_some_and(|max| total > max) {
            self.evict()?;
        }
        Ok(())
    }

    fn entry(&self, path: &Path, display: &str, settings: OutputSettings) -> io::Result<PathBuf> {
        let mut key = blake3::Hasher::new();
        key.update(self.build.as_bytes());
        key.update(format!("{}\0{display}\0", settings.key()).as_bytes());
        key.update(self.content_hash(path)?.as_bytes());
        let key = key.finalize().to_hex();
        Ok(self.dir.join(&key[..2]).join(key.as_str()))
    }
}

impl Cache {
    /// The BLAKE3 hash of the input's content, from the record of its last hashing when the
    /// policy trusts modification times and the record still matches.
    fn content_hash(&self, path: &Path) -> io::Result<blake3::Hash> {
        if !self.policy.trust_mtime {
            return hash_file(path);
        }
        let metadata = fs::metadata(path)?;
        let modified = metadata
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| {
                io::Error::other(format!("{} was modified before 1970: {e}", path.display()))
            })?;
        let stamp = format!("{}:{}", metadata.len(), modified.as_nanos());
        let name = blake3::hash(fs::canonicalize(path)?.as_os_str().as_encoded_bytes()).to_hex();
        let records = self.dir.join(PATHS_DIR);
        let record = records.join(name.as_str());
        let recorded = match fs::read_to_string(&record) {
            Ok(contents) => contents
                .split_once(' ')
                .filter(|(recorded_stamp, _)| *recorded_stamp == stamp)
                .and_then(|(_, hash)| blake3::Hash::from_hex(hash.trim()).ok()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if let Some(hash) = recorded {
            return Ok(hash);
        }
        let hash = hash_file(path)?;
        fs::create_dir_all(&records)?;
        let temporary = records.join(format!(
            "{TEMPORARY_PREFIX}{}-{}",
            std::process::id(),
            self.next_temporary.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&temporary, format!("{stamp} {}", hash.to_hex()))?;
        fs::rename(&temporary, &record)?;
        Ok(hash)
    }

    /// Whether an outcome is worth keeping: complete, or partial when the policy says so, and
    /// with no embedded documents left to extract separately, since an entry holds only its own
    /// document's output.
    fn keeps(&self, outcome: &Outcome) -> bool {
        if !outcome.embedded.is_empty() {
            return false;
        }
        match outcome.status {
            Status::Complete => true,
            Status::Partial => {
                self.policy.store_partial
                    && outcome
                        .errors
                        .iter()
                        .all(|error| error.category != ErrorCategory::Timeout)
            }
            Status::Encrypted | Status::Failed => false,
        }
    }
}

impl Hit {
    /// Writes the stored output into `sink`, reporting `source` as its provenance.
    pub fn replay(self, sink: &mut dyn Write, source: &Source) -> io::Result<Outcome> {
        replay(self.file, &self.entry, sink, source)
    }
}

impl Miss<'_> {
    /// Renders into `sink`, keeping a copy that becomes the entry if the extraction was complete.
    pub fn store(
        self,
        sink: &mut dyn Write,
        source: &Source,
        render: impl FnOnce(&mut dyn Write) -> io::Result<Outcome>,
    ) -> io::Result<Outcome> {
        let shard = self
            .entry
            .parent()
            .expect("entries live in a shard directory");
        fs::create_dir_all(shard)?;
        let temporary = shard.join(format!(
            "{TEMPORARY_PREFIX}{}-{}",
            std::process::id(),
            self.cache.next_temporary.fetch_add(1, Ordering::Relaxed)
        ));
        let stored = store(&temporary, &self.entry, sink, source, render, |outcome| {
            self.cache.keeps(outcome)
        });
        match stored {
            Ok((outcome, Some(bytes))) => {
                self.cache.stored(bytes)?;
                Ok(outcome)
            }
            Ok((outcome, None)) => Ok(outcome),
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(error)
            }
        }
    }
}

/// Renders into the sink and a temporary file at once, and moves the file into place as the
/// entry when the extraction was complete.
fn store(
    temporary: &Path,
    entry: &Path,
    sink: &mut dyn Write,
    source: &Source,
    render: impl FnOnce(&mut dyn Write) -> io::Result<Outcome>,
    keep: impl FnOnce(&Outcome) -> bool,
) -> io::Result<(Outcome, Option<u64>)> {
    let mut copy = BufWriter::new(File::create_new(temporary)?);
    let outcome = render(&mut Tee {
        sink,
        copy: &mut copy,
    })?;
    if !keep(&outcome) {
        drop(copy);
        fs::remove_file(temporary)?;
        return Ok((outcome, None));
    }
    let trailer = serde_json::to_vec(&Trailer {
        status: outcome.status,
        errors: outcome.errors.clone(),
        metadata: outcome.metadata.clone(),
        source: source.clone(),
    })?;
    copy.write_all(&trailer)?;
    copy.write_all(&(trailer.len() as u64).to_le_bytes())?;
    let file = copy.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    let bytes = file.metadata()?.len();
    fs::rename(temporary, entry)?;
    Ok((outcome, Some(bytes)))
}

fn replay(
    mut file: File,
    entry: &Path,
    sink: &mut dyn Write,
    source: &Source,
) -> io::Result<Outcome> {
    let corrupt = |reason: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("The cache entry {} is corrupt: {reason}", entry.display()),
        )
    };
    let size = file.metadata()?.len();
    if size < TRAILER_LENGTH_BYTES as u64 {
        return Err(corrupt("it is too short"));
    }
    let mut length = [0; TRAILER_LENGTH_BYTES];
    file.seek(SeekFrom::Start(size - TRAILER_LENGTH_BYTES as u64))?;
    file.read_exact(&mut length)?;
    let trailer_length = u64::from_le_bytes(length);
    let output_length = (size - TRAILER_LENGTH_BYTES as u64)
        .checked_sub(trailer_length)
        .ok_or_else(|| corrupt("its trailer is longer than the entry"))?;
    let mut trailer = vec![0; trailer_length as usize];
    file.seek(SeekFrom::Start(output_length))?;
    file.read_exact(&mut trailer)?;
    let trailer: Trailer = serde_json::from_slice(&trailer).map_err(|e| corrupt(&e.to_string()))?;
    file.seek(SeekFrom::Start(0))?;
    let mut output = BufReader::new(&mut file).take(output_length);
    if trailer.source == *source {
        io::copy(&mut output, sink)?;
    } else {
        let mut rewrite = SourceRewrite::new(
            sink,
            render::serialized_sources(&trailer.source),
            render::serialized_sources(source),
        );
        io::copy(&mut output, &mut rewrite)?;
        rewrite.finish()?;
    }
    file.set_modified(SystemTime::now())?;
    Ok(Outcome {
        status: trailer.status,
        errors: trailer.errors,
        metadata: trailer.metadata,
        embedded: Vec::new(),
    })
}

/// Copies output into a sink with each occurrence of one serialized provenance replaced by
/// another. Records serialize their provenance as `"source":{...}`, which cannot occur inside a
/// JSON string, where quotes are escaped, so only the records' own provenance is replaced.
struct SourceRewrite<'a> {
    sink: &'a mut dyn Write,
    from: [Vec<u8>; 2],
    to: [Vec<u8>; 2],
    held: Vec<u8>,
}

impl<'a> SourceRewrite<'a> {
    fn new(sink: &'a mut dyn Write, from: [Vec<u8>; 2], to: [Vec<u8>; 2]) -> Self {
        Self {
            sink,
            from,
            to,
            held: Vec::new(),
        }
    }

    /// Rewrites and passes on the held bytes that no later byte can complete a match for, or all
    /// of them at the end.
    fn pass(&mut self, end: bool) -> io::Result<()> {
        let longest = self.from.iter().map(Vec::len).max().unwrap_or(0);
        let decided = if end {
            self.held.len()
        } else {
            self.held.len().saturating_sub(longest.saturating_sub(1))
        };
        let mut written = 0;
        let mut index = 0;
        while index < decided {
            let matched = self
                .from
                .iter()
                .position(|from| self.held[index..].starts_with(from));
            match matched {
                Some(which) => {
                    self.sink.write_all(&self.held[written..index])?;
                    self.sink.write_all(&self.to[which])?;
                    index += self.from[which].len();
                    written = index;
                }
                None => index += 1,
            }
        }
        let passed = index.max(written);
        self.sink.write_all(&self.held[written..passed])?;
        self.held.drain(..passed);
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.pass(true)
    }
}

impl Write for SourceRewrite<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.held.extend_from_slice(bytes);
        self.pass(false)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

struct Tee<'a> {
    sink: &'a mut dyn Write,
    copy: &'a mut BufWriter<File>,
}

impl Write for Tee<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.sink.write(bytes)?;
        self.copy.write_all(&bytes[..written])?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}

fn hash_file(path: &Path) -> io::Result<blake3::Hash> {
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(hasher.finalize())
}

/// The version tesseract reports, or why there is none, since OCR output depends on it.
fn tesseract_version() -> String {
    match Command::new("tesseract").arg("--version").output() {
        Ok(output) => format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => format!("tesseract unavailable: {}", error.kind()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(output: &str, chunk: usize) -> String {
        let source = |path: &str| Source {
            path: path.to_string(),
            embedded_path: Some(format!("{path}/a.pdf")),
            size: Some(3),
            blake3: Some("ab".to_string()),
        };
        let mut sink = Vec::new();
        let mut rewrite = SourceRewrite::new(
            &mut sink,
            render::serialized_sources(&source("one.eml")),
            render::serialized_sources(&source("two.eml")),
        );
        for bytes in output.as_bytes().chunks(chunk) {
            rewrite.write_all(bytes).unwrap();
        }
        rewrite.finish().unwrap();
        String::from_utf8(sink).unwrap()
    }

    #[test]
    fn replayed_records_report_where_they_are_replayed_from() {
        let output = concat!(
            r#"{"kind":"document","source":{"path":"one.eml","embedded_path":"one.eml/a.pdf","size":3,"blake3":"ab"},"text":"x"}"#,
            "\n",
            r#"{"kind":"page","source":{"path":"one.eml","embedded_path":"one.eml/a.pdf"},"text":"\"source\":{\"path\":\"one.eml\"}"}"#,
            "\n"
        );
        let expected = output.replace(
            r#""path":"one.eml","embedded_path":"one.eml"#,
            r#""path":"two.eml","embedded_path":"two.eml"#,
        );
        for chunk in [1, 2, 7, 64, output.len()] {
            assert_eq!(rewritten(output, chunk), expected, "chunks of {chunk}");
        }
    }
}
