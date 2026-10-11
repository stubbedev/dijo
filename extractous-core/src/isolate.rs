use std::fmt;
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

/// The process-wide isolate, created on first use.
pub(crate) fn isolate() -> ExtractResult<&'static Isolate> {
    static ISOLATE: OnceLock<Result<Isolate, String>> = OnceLock::new();
    ISOLATE
        .get_or_init(|| native::create_isolate().map(|vm| Isolate { vm }))
        .as_ref()
        .map_err(|message| Error::NativeLibrary(message.clone()))
}

/// Attaches the current thread to the isolate until the thread exits, so extractions on it skip
/// attaching and detaching each time. Worth it for long-lived worker threads.
pub fn keep_current_thread_attached() -> ExtractResult<()> {
    isolate()?.vm.attach_current_thread_permanently()?;
    Ok(())
}
