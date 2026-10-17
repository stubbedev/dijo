#[cfg(feature = "tika")]
use std::fmt;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(feature = "tika")]
use jni::{Env, JavaVM};

use crate::errors::{Error, ExtractResult};
#[cfg(feature = "tika")]
use crate::tika::{self, native};

/// The GraalVM isolate Tika runs in. There is one per process: the JDK's AWT native libraries,
/// which PDF page rendering for OCR needs, keep process-wide state tied to the first isolate
/// that loads them, so in any further isolate rendering silently produces empty page images.
#[cfg(feature = "tika")]
pub(crate) struct Isolate {
    vm: JavaVM,
}

#[cfg(feature = "tika")]
impl Isolate {
    /// Runs `f` on the current thread attached to the isolate, in a local frame of its own: the
    /// local references `f` creates are freed when it returns. A thread attached only for `f` is
    /// detached again.
    pub(crate) fn with_env<T>(
        &self,
        f: impl FnOnce(&mut Env<'_>) -> ExtractResult<T>,
    ) -> ExtractResult<T> {
        self.vm.attach_current_thread_for_scope(f)
    }
}

#[cfg(feature = "tika")]
impl fmt::Debug for Isolate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Isolate").finish_non_exhaustive()
    }
}

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
    pub(crate) fn min_heap_bytes(&self) -> u64 {
        self.max_heap_bytes
            .map_or(MIN_HEAP_BYTES, |max| max.get().min(MIN_HEAP_BYTES))
    }
}

#[cfg(feature = "tika")]
static ISOLATE: OnceLock<Result<Isolate, String>> = OnceLock::new();
static SETTINGS: OnceLock<IsolateSettings> = OnceLock::new();

/// Where temporary files go: the directory the settings name, or the system's.
pub(crate) fn temp_dir() -> PathBuf {
    SETTINGS
        .get()
        .and_then(|settings| settings.temp_dir.clone())
        .unwrap_or_else(std::env::temp_dir)
}

/// The process-wide isolate, created on first use.
#[cfg(feature = "tika")]
pub(crate) fn isolate() -> ExtractResult<&'static Isolate> {
    ISOLATE
        .get_or_init(|| {
            native::create_isolate(SETTINGS.get_or_init(IsolateSettings::default))
                .map(|vm| Isolate { vm })
        })
        .as_ref()
        .map_err(|message| Error::NativeLibrary(message.clone()))
}

/// The cap on the memory a native extraction may hold, and the Java heap may take, if one is
/// set.
pub(crate) fn heap_cap() -> Option<u64> {
    SETTINGS
        .get()
        .and_then(|settings| settings.max_heap_bytes)
        .map(NonZeroU64::get)
}

/// The processors this process may use: its share of the machine, or all of the machine's, or
/// one when the machine does not tell how many it has.
pub(crate) fn processors() -> usize {
    SETTINGS
        .get()
        .and_then(|settings| settings.processors)
        .or_else(|| std::thread::available_parallelism().ok())
        .map_or(1, NonZeroUsize::get)
}

/// Whether the process has created its isolate, which nothing but Tika's path needs.
#[cfg(all(test, feature = "tika"))]
pub(crate) fn isolate_created() -> bool {
    ISOLATE.get().is_some()
}

/// Sets up the process; only once, and before the first extraction starts the isolate.
pub fn configure_isolate(settings: IsolateSettings) -> ExtractResult<()> {
    #[cfg(feature = "tika")]
    if ISOLATE.get().is_some() {
        return Err(Error::Unknown(
            "The isolate must be configured before the first extraction".to_string(),
        ));
    }
    SETTINGS
        .set(settings)
        .map_err(|_| Error::Unknown("The isolate was already configured".to_string()))
}

/// Waits, for at most `timeout`, until every extraction on Tika's path has ended, including those
/// whose reader stopped early: they are cancelled but still wind down and kill the tesseract
/// they started. Call it before the process exits. Returns whether they all ended; at once, and
/// true, when no extraction ever started the isolate, as in a build without Tika.
pub fn wait_for_extractions(timeout: Duration) -> ExtractResult<bool> {
    #[cfg(feature = "tika")]
    if let Some(Ok(isolate)) = ISOLATE.get() {
        return tika::await_idle_parses(isolate, timeout);
    }
    #[cfg(not(feature = "tika"))]
    let _ = timeout;
    Ok(true)
}

/// Attaches the current thread to the isolate until the thread exits, so extractions on it skip
/// attaching and detaching each time. Worth it for long-lived worker threads.
#[cfg(feature = "tika")]
pub fn keep_current_thread_attached() -> ExtractResult<()> {
    isolate()?.vm.attach_current_thread(|_| Ok::<_, Error>(()))
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
