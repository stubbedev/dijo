//! The readings by OCR run at once in this process: as many as the processors it may use, however
//! many documents are read side by side, so that the rasters, prepared images and tesseract
//! engines readings take do not multiply with the documents read at once (#305, #307).

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard};

/// How many readings may run at once.
struct Slots {
    free: Mutex<usize>,
    released: Condvar,
}

impl Slots {
    const fn new(capacity: usize) -> Self {
        Self {
            free: Mutex::new(capacity),
            released: Condvar::new(),
        }
    }

    fn free(&self) -> MutexGuard<'_, usize> {
        self.free.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn take(&self) {
        let mut free = self.free();
        while *free == 0 {
            free = self.released.wait(free).unwrap_or_else(|e| e.into_inner());
        }
        *free -= 1;
    }

    fn give_back(&self) {
        *self.free() += 1;
        self.released.notify_one();
    }
}

static SLOTS: LazyLock<Slots> = LazyLock::new(|| Slots::new(crate::process::processors()));

thread_local! {
    /// Whether this thread holds a slot, which the readings it runs meanwhile share.
    static HELD: Cell<bool> = const { Cell::new(false) };
}

/// A reading running on this thread, which frees its slot when dropped. It stays on the thread
/// that took it, since the readings that thread runs meanwhile share it.
pub(crate) struct Slot {
    owns: bool,
    on_this_thread: PhantomData<*const ()>,
}

/// A slot for a reading on this thread, once one is free: the one this thread holds when it holds
/// one, so that a reading run while another holds a slot does not wait for itself.
pub(crate) fn slot() -> Slot {
    let owns = !HELD.get();
    if owns {
        SLOTS.take();
        HELD.set(true);
    }
    Slot {
        owns,
        on_this_thread: PhantomData,
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if self.owns {
            HELD.set(false);
            SLOTS.give_back();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Readings on more threads than the processors run no more at once than the processors, each
    /// running a reading within it on the slot it holds.
    #[test]
    fn readings_run_no_more_at_once_than_the_processors_and_share_a_thread_s_slot() {
        let processors = crate::process::processors();
        let (running, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..4 * processors {
                scope.spawn(|| {
                    for _ in 0..3 {
                        let _outer = slot();
                        let _inner = slot();
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        running.fetch_sub(1, Ordering::SeqCst);
                    }
                });
            }
        });
        let most = most.load(Ordering::SeqCst);
        assert!(most <= processors, "{most} readings ran at once");
        assert_eq!(*SLOTS.free(), processors);
    }
}
