use std::fmt;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::OnceLock;

use jni::{AttachGuard, JavaVM};

use crate::errors::{Error, ExtractResult};
use crate::tika::native;

/// The GraalVM isolate Tika runs in. There is one per process: the JDK's AWT native libraries,
/// which PDF page rendering for OCR needs, keep process-wide state tied to the first isolate
/// that loads them, so in any further isolate rendering silently produces empty page images.
pub(crate) struct Isolate {
    vm: JavaVM,
}

impl Isolate {
    pub(crate) fn attach(&self) -> ExtractResult<AttachGuard<'_>> {
        Ok(self.vm.attach_current_thread()?)
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
    /// The most memory the Java heap may take. Beyond it an extraction fails with an
    /// OutOfMemoryError, and the isolate stays usable for the next one. By default the heap may
    /// grow to a large share of physical memory.
    pub max_heap_bytes: Option<NonZeroU64>,
}

static ISOLATE: OnceLock<Result<Isolate, String>> = OnceLock::new();
static SETTINGS: OnceLock<IsolateSettings> = OnceLock::new();

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

/// Attaches the current thread to the isolate until the thread exits, so extractions on it skip
/// attaching and detaching each time. Worth it for long-lived worker threads.
pub fn keep_current_thread_attached() -> ExtractResult<()> {
    isolate()?.vm.attach_current_thread_permanently()?;
    Ok(())
}
