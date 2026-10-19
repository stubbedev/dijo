use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::errors::{Error, ExtractResult};

/// How the process extracts: where its temporary files go, the memory an extraction may take
/// and the processors it may use.
#[derive(Debug, Clone, Default)]
pub struct ProcessSettings {
    /// Where temporary files go (spooled streams and decoded images) instead of the system
    /// temporary directory.
    pub temp_dir: Option<PathBuf>,
    /// The most memory an extraction may hold. Beyond it an extraction fails with
    /// [`crate::Error::MemoryLimit`], and the next one runs normally. An extraction is held to
    /// the cap only in a program that counts allocations with
    /// [`crate::memory::CountingAllocator`]; in any other, while a cap is set, every extraction
    /// fails with [`crate::Unsupported::UncountedMemoryCap`].
    pub max_heap_bytes: Option<NonZeroU64>,
    /// The processors this process may use, when it shares the machine with other extracting
    /// processes: OCR divides them among the readings running at once. All of the machine's by
    /// default.
    pub processors: Option<NonZeroUsize>,
}

static SETTINGS: OnceLock<ProcessSettings> = OnceLock::new();

/// The process's settings: those [`configure`] set, or the defaults, which then stay.
pub fn settings() -> &'static ProcessSettings {
    SETTINGS.get_or_init(ProcessSettings::default)
}

/// Sets up the process; only once, and before anything reads the settings.
pub fn configure(settings: ProcessSettings) -> ExtractResult<()> {
    SETTINGS
        .set(settings)
        .map_err(|_| Error::Unknown("The process was already configured".to_string()))
}

/// Where temporary files go: the directory the settings name, or the system's.
pub fn temp_dir() -> PathBuf {
    SETTINGS
        .get()
        .and_then(|settings| settings.temp_dir.clone())
        .unwrap_or_else(std::env::temp_dir)
}

/// The cap on the memory an extraction may hold, if one is set.
pub fn heap_cap() -> Option<u64> {
    SETTINGS
        .get()
        .and_then(|settings| settings.max_heap_bytes)
        .map(NonZeroU64::get)
}

/// The processors this process may use: its share of the machine, or all of the machine's, or
/// one when the machine does not tell how many it has.
pub fn processors() -> usize {
    SETTINGS
        .get()
        .and_then(|settings| settings.processors)
        .or_else(|| std::thread::available_parallelism().ok())
        .map_or(1, NonZeroUsize::get)
}
