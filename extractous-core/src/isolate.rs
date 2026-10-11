use std::fmt;
use std::sync::{Arc, OnceLock};
use std::thread;

use jni::{AttachGuard, JavaVM};

use crate::errors::{Error, ExtractResult};
use crate::tika::native;

/// A GraalVM isolate: a separate Java heap in which Tika runs. Extractions in different
/// isolates cannot exhaust or corrupt each other's heap. An isolate is destroyed when the last
/// [`Extractor`](crate::Extractor) and stream using it are dropped.
pub struct Isolate {
    vm: JavaVM,
}

impl Isolate {
    pub fn new() -> ExtractResult<Arc<Self>> {
        native::create_isolate()
            .map(|vm| Arc::new(Self { vm }))
            .map_err(Error::NativeLibrary)
    }

    /// Attaches the current thread to this isolate until the thread exits, so extractions on
    /// it skip attaching and detaching each time. A thread stays permanently attached to one
    /// isolate at a time: attaching it to another detaches it from this one.
    pub fn keep_current_thread_attached(&self) -> ExtractResult<()> {
        self.vm.attach_current_thread_permanently()?;
        Ok(())
    }

    pub(crate) fn attach(&self) -> ExtractResult<AttachGuard<'_>> {
        Ok(self.vm.attach_current_thread()?)
    }
}

impl Drop for Isolate {
    /// DestroyJavaVM waits for every non-daemon Java thread in the isolate to end, and some
    /// parsers leave such threads behind, so the isolate is torn down on a detached thread: its
    /// memory is reclaimed once those threads end, and neither the caller nor process exit waits.
    fn drop(&mut self) {
        let vm = unsafe { JavaVM::from_raw(self.vm.get_java_vm_pointer()) };
        let destroy = move || {
            if let Ok(vm) = vm {
                let _ = unsafe { vm.destroy() };
            }
        };
        if thread::Builder::new()
            .name("extractous isolate teardown".to_string())
            .spawn(destroy)
            .is_err()
        {
            eprintln!("extractous: could not start a thread to tear down an isolate; it stays");
        }
    }
}

impl fmt::Debug for Isolate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Isolate").finish_non_exhaustive()
    }
}

/// The isolate used by extractors that were not given one, created on first use.
pub(crate) fn default_isolate() -> ExtractResult<Arc<Isolate>> {
    static DEFAULT: OnceLock<Result<Arc<Isolate>, String>> = OnceLock::new();
    DEFAULT
        .get_or_init(|| native::create_isolate().map(|vm| Arc::new(Isolate { vm })))
        .clone()
        .map_err(Error::NativeLibrary)
}
