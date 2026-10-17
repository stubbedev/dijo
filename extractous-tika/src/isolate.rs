#[cfg(feature = "tika")]
use std::fmt;
#[cfg(feature = "tika")]
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(feature = "tika")]
use jni::{Env, JavaVM};

#[cfg(feature = "tika")]
use crate::errors::Error;
use crate::errors::ExtractResult;
#[cfg(feature = "tika")]
use crate::tika::{self, native};
pub use extractous_base::isolate::*;

/// The GraalVM isolate Tika runs in. There is one per process: the JDK's AWT native libraries,
/// which PDF page rendering for OCR needs, keep process-wide state tied to the first isolate
/// that loads them, so in any further isolate rendering silently produces empty page images.
#[cfg(feature = "tika")]
pub struct Isolate {
    vm: JavaVM,
}

#[cfg(feature = "tika")]
impl Isolate {
    /// Runs `f` on the current thread attached to the isolate, in a local frame of its own: the
    /// local references `f` creates are freed when it returns. A thread attached only for `f` is
    /// detached again.
    pub fn with_env<T>(
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

#[cfg(feature = "tika")]
static ISOLATE: OnceLock<Result<Isolate, String>> = OnceLock::new();

/// The process-wide isolate, created on first use.
#[cfg(feature = "tika")]
pub fn isolate() -> ExtractResult<&'static Isolate> {
    ISOLATE
        .get_or_init(|| native::create_isolate(settings()).map(|vm| Isolate { vm }))
        .as_ref()
        .map_err(|message| Error::NativeLibrary(message.clone()))
}

/// Whether the process has created its isolate, which nothing but Tika's path needs.
#[cfg(feature = "tika")]
pub fn isolate_created() -> bool {
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
    configure(settings)
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
