use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use extractous_core::Metadata;

use crate::cli::Ocr;
use crate::render::{Outcome, RenderOptions};
use crate::schema::Status;

const TRAILER_LENGTH_BYTES: usize = 8;
const TEMPORARY_PREFIX: &str = ".partial-";

/// Rendered output of complete extractions, keyed by everything that determines it: the input's
/// content, the options that shape the output, this exact build of extractous with Tika inside
/// it, and the tesseract that runs OCR.
pub struct Cache {
    dir: PathBuf,
    max_bytes: Option<u64>,
    settings: blake3::Hash,
    next_temporary: AtomicU64,
}

impl Cache {
    pub fn open(
        dir: &Path,
        max_bytes: Option<u64>,
        options: RenderOptions,
        ocr: Ocr,
        ocr_lang: Option<&str>,
    ) -> Result<Cache, String> {
        fs::create_dir_all(dir).map_err(|e| {
            format!(
                "Failed to create the cache directory {}: {e}",
                dir.display()
            )
        })?;
        let executable = std::env::current_exe()
            .map_err(|e| format!("Failed to locate the running executable: {e}"))?;
        let build = hash_file(&executable)
            .map_err(|e| format!("Failed to hash {}: {e}", executable.display()))?;
        let mut settings = blake3::Hasher::new();
        settings.update(build.as_bytes());
        settings.update(format!("{options:?}\0{ocr:?}\0{ocr_lang:?}\0").as_bytes());
        if ocr != Ocr::Never {
            settings.update(tesseract_version().as_bytes());
        }
        Ok(Cache {
            dir: dir.to_path_buf(),
            max_bytes,
            settings: settings.finalize(),
            next_temporary: AtomicU64::new(0),
        })
    }

    /// Writes the cached output for the input at `path` into `sink`, or renders it with `render`
    /// and stores the result when it is complete.
    pub fn render(
        &self,
        path: &Path,
        display: &str,
        sink: &mut dyn Write,
        render: impl FnOnce(&mut dyn Write) -> io::Result<Outcome>,
    ) -> io::Result<Outcome> {
        let entry = self.entry(path, display)?;
        match OpenOptions::new().read(true).write(true).open(&entry) {
            Ok(file) => return replay(file, &entry, sink),
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "Failed to open the cache entry {}: {error}",
                        entry.display()
                    ),
                ))
            }
            Err(_) => {}
        }
        let shard = entry.parent().expect("entries live in a shard directory");
        fs::create_dir_all(shard)?;
        let temporary = shard.join(format!(
            "{TEMPORARY_PREFIX}{}-{}",
            std::process::id(),
            self.next_temporary.fetch_add(1, Ordering::Relaxed)
        ));
        let stored = store(&temporary, &entry, sink, render);
        if stored.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        stored
    }

    /// Removes the least recently used entries until the cache fits in its size limit.
    pub fn evict(&self) -> io::Result<()> {
        let Some(max_bytes) = self.max_bytes else {
            return Ok(());
        };
        let mut entries = Vec::new();
        for shard in fs::read_dir(&self.dir)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
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
        Ok(())
    }

    fn entry(&self, path: &Path, display: &str) -> io::Result<PathBuf> {
        let mut key = blake3::Hasher::new();
        key.update(self.settings.as_bytes());
        key.update(display.as_bytes());
        key.update(b"\0");
        key.update(hash_file(path)?.as_bytes());
        let key = key.finalize().to_hex();
        Ok(self.dir.join(&key[..2]).join(key.as_str()))
    }
}

/// Renders into the sink and a temporary file at once, and moves the file into place as the
/// entry when the extraction was complete.
fn store(
    temporary: &Path,
    entry: &Path,
    sink: &mut dyn Write,
    render: impl FnOnce(&mut dyn Write) -> io::Result<Outcome>,
) -> io::Result<Outcome> {
    let mut copy = BufWriter::new(File::create_new(temporary)?);
    let outcome = render(&mut Tee {
        sink,
        copy: &mut copy,
    })?;
    if outcome.status != Status::Complete {
        drop(copy);
        fs::remove_file(temporary)?;
        return Ok(outcome);
    }
    let trailer = serde_json::to_vec(&outcome.metadata)?;
    copy.write_all(&trailer)?;
    copy.write_all(&(trailer.len() as u64).to_le_bytes())?;
    copy.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    fs::rename(temporary, entry)?;
    Ok(outcome)
}

fn replay(mut file: File, entry: &Path, sink: &mut dyn Write) -> io::Result<Outcome> {
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
    let metadata: Option<Metadata> =
        serde_json::from_slice(&trailer).map_err(|e| corrupt(&e.to_string()))?;
    file.seek(SeekFrom::Start(0))?;
    io::copy(&mut BufReader::new(&mut file).take(output_length), sink)?;
    file.set_modified(SystemTime::now())?;
    Ok(Outcome {
        status: Status::Complete,
        errors: Vec::new(),
        metadata,
        embedded: Vec::new(),
    })
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
