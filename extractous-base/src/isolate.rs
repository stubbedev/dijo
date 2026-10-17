use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::errors::{Error, ExtractResult};

/// How the process extracts: where its temporary files go, the memory an extraction may take
/// and the processors it may use. In a build with Tika, the process-wide isolate Tika runs in is
/// set up with them too, and reads them once, when it starts.
#[derive(Debug, Clone, Default)]
pub struct IsolateSettings {
    /// Where temporary files go (spooled streams, decoded images, and Tika's PDF scratch files)
    /// instead of the system temporary directory.
    pub temp_dir: Option<PathBuf>,
    /// The most memory a native extraction may hold, and the Java heap may take. Beyond it an
    /// extraction fails, with [`crate::Error::MemoryLimit`] on the native path and an
    /// OutOfMemoryError on Tika's, and the next one runs normally. By default the heap may grow
    /// to a large share of physical memory. The heap is sized to at least 512 MiB, or the cap
    /// when it is lower. A native extraction is held to the cap only in a program that counts
    /// allocations with [`crate::memory::CountingAllocator`]; in any other, while a cap is set,
    /// every document is extracted on Tika's path, and in a build without Tika every extraction
    /// fails with [`crate::Unsupported::UncountedMemoryCap`].
    pub max_heap_bytes: Option<NonZeroU64>,
    /// The processors this process may use, when it shares the machine with other extracting
    /// processes: OCR divides them among the readings running at once. All of the machine's by
    /// default.
    pub processors: Option<NonZeroUsize>,
}

/// The heap the isolate keeps at least: GraalVM CE collects only with its serial collector, which
/// with its default sizing collects the young generation every few dozen megabytes, a sixth of
/// the time spent on office documents.
#[cfg(feature = "tika")]
const MIN_HEAP_BYTES: u64 = 512 << 20;

#[cfg(feature = "tika")]
impl IsolateSettings {
    /// The heap the isolate keeps at least, which may not exceed the cap.
    pub fn min_heap_bytes(&self) -> u64 {
        self.max_heap_bytes
            .map_or(MIN_HEAP_BYTES, |max| max.get().min(MIN_HEAP_BYTES))
    }
}

static SETTINGS: OnceLock<IsolateSettings> = OnceLock::new();

/// The process's settings: those [`configure`] set, or the defaults, which then stay.
pub fn settings() -> &'static IsolateSettings {
    SETTINGS.get_or_init(IsolateSettings::default)
}

/// Sets up the process; only once, and before anything reads the settings.
pub fn configure(settings: IsolateSettings) -> ExtractResult<()> {
    SETTINGS
        .set(settings)
        .map_err(|_| Error::Unknown("The isolate was already configured".to_string()))
}

/// Where temporary files go: the directory the settings name, or the system's.
pub fn temp_dir() -> PathBuf {
    SETTINGS
        .get()
        .and_then(|settings| settings.temp_dir.clone())
        .unwrap_or_else(std::env::temp_dir)
}

/// The cap on the memory a native extraction may hold, and the Java heap may take, if one is
/// set.
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

#[cfg(all(test, feature = "tika"))]
mod tests {
    use super::*;

    fn settings(max_heap_bytes: Option<u64>) -> IsolateSettings {
        IsolateSettings {
            max_heap_bytes: max_heap_bytes.map(|bytes| NonZeroU64::new(bytes).unwrap()),
            ..IsolateSettings::default()
        }
    }

    #[test]
    fn the_minimum_heap_never_exceeds_the_cap() {
        assert_eq!(settings(None).min_heap_bytes(), MIN_HEAP_BYTES);
        assert_eq!(settings(Some(64 << 20)).min_heap_bytes(), 64 << 20);
        assert_eq!(settings(Some(4 << 30)).min_heap_bytes(), MIN_HEAP_BYTES);
    }
}
