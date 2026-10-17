use std::fmt;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use jni::{Env, JavaVM};

use crate::errors::{Error, ExtractResult};
use crate::tika::{self, native};

/// The GraalVM isolate Tika runs in. There is one per process: the JDK's AWT native libraries,
/// which PDF page rendering for OCR needs, keep process-wide state tied to the first isolate
/// that loads them, so in any further isolate rendering silently produces empty page images.
pub(crate) struct Isolate {
    vm: JavaVM,
}

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

impl fmt::Debug for Isolate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Isolate").finish_non_exhaustive()
    }
}

/// How the process-wide isolate is set up. The isolate reads these once, when it starts.
#[derive(Debug, Clone, Default)]
pub struct IsolateSettings {
    /// Where Tika puts its temporary files (PDF scratch files, spooled streams) instead of the
    /// system temporary directory.
    pub temp_dir: Option<PathBuf>,
    /// The most memory the Java heap may take, and a native extraction may hold. Beyond it an
    /// extraction fails, with an OutOfMemoryError on Tika's path and
    /// [`crate::Error::MemoryLimit`] on the native one, and the next one runs normally. By
    /// default the heap may grow to a large share of physical memory. The heap is sized to at
    /// least 512 MiB, or the cap when it is lower. A native extraction is held to the cap only
    /// in a program that counts allocations with [`crate::memory::CountingAllocator`]; in any
    /// other, every document is extracted on Tika's path while a cap is set.
    pub max_heap_bytes: Option<NonZeroU64>,
    /// The processors this process may use, when it shares the machine with other extracting
    /// processes: OCR divides them among the readings running at once. All of the machine's by
    /// default.
    pub processors: Option<NonZeroUsize>,
}

/// The heap the isolate keeps at least: GraalVM CE collects only with its serial collector, which
/// with its default sizing collects the young generation every few dozen megabytes, a sixth of
/// the time spent on office documents.
const MIN_HEAP_BYTES: u64 = 512 << 20;

impl IsolateSettings {
    /// The heap the isolate keeps at least, which may not exceed the cap.
    pub(crate) fn min_heap_bytes(&self) -> u64 {
        self.max_heap_bytes
            .map_or(MIN_HEAP_BYTES, |max| max.get().min(MIN_HEAP_BYTES))
    }
}

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
pub(crate) fn isolate() -> ExtractResult<&'static Isolate> {
    ISOLATE
        .get_or_init(|| {
            native::create_isolate(SETTINGS.get_or_init(IsolateSettings::default))
                .map(|vm| Isolate { vm })
        })
        .as_ref()
        .map_err(|message| Error::NativeLibrary(message.clone()))
}

/// The cap on the Java heap, which a native extraction is held to as well, if one is set.
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
#[cfg(test)]
pub(crate) fn isolate_created() -> bool {
    ISOLATE.get().is_some()
}

/// Sets up the isolate; only once, and before the first extraction starts it.
pub fn configure_isolate(settings: IsolateSettings) -> ExtractResult<()> {
    if ISOLATE.get().is_some() {
        return Err(Error::Unknown(
            "The isolate must be configured before the first extraction".to_string(),
        ));
    }
    SETTINGS
        .set(settings)
        .map_err(|_| Error::Unknown("The isolate was already configured".to_string()))
}

/// Waits, for at most `timeout`, until every extraction has ended, including those whose reader
/// stopped early: they are cancelled but still wind down and kill the tesseract they started.
/// Call it before the process exits. Returns whether they all ended; at once, and true, when no
/// extraction ever started the isolate.
pub fn wait_for_extractions(timeout: Duration) -> ExtractResult<bool> {
    match ISOLATE.get() {
        Some(Ok(isolate)) => tika::await_idle_parses(isolate, timeout),
        Some(Err(_)) | None => Ok(true),
    }
}

/// Attaches the current thread to the isolate until the thread exits, so extractions on it skip
/// attaching and detaching each time. Worth it for long-lived worker threads.
pub fn keep_current_thread_attached() -> ExtractResult<()> {
    isolate()?.vm.attach_current_thread(|_| Ok::<_, Error>(()))
}

#[cfg(test)]
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
