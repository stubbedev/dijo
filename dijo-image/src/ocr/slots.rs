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

    fn give_back(&self, count: usize) {
        *self.free() += count;
        self.released.notify_all();
    }

    fn take_free(&self, most: usize) -> usize {
        let mut free = self.free();
        let taken = (*free).min(most);
        *free -= taken;
        taken
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
            SLOTS.give_back(1);
        }
    }
}

/// The slots free at once, up to `most`, taken for threads that help the reading this thread
/// runs, and given back when dropped; none for a reading charged to a memory budget, which runs on
/// one thread.
pub(crate) struct Helpers {
    count: usize,
}

impl Helpers {
    pub(crate) fn count(&self) -> usize {
        self.count
    }
}

pub(crate) fn helpers(most: usize) -> Helpers {
    let count = match crate::memory::budgeted() {
        true => 0,
        false => SLOTS.take_free(most),
    };
    Helpers { count }
}

impl Drop for Helpers {
    fn drop(&mut self) {
        if self.count > 0 {
            SLOTS.give_back(self.count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn helpers_take_only_the_slots_free_and_give_them_back() {
        let processors = crate::process::processors();
        let free = || *SLOTS.free();
        let _reading = slot();
        assert_eq!(free(), processors - 1);
        {
            let helpers = helpers(processors);
            assert_eq!(helpers.count(), processors - 1);
            assert_eq!(free(), 0);
            assert_eq!(super::helpers(1).count(), 0);
        }
        assert_eq!(free(), processors - 1);
        assert_eq!(helpers(1).count(), (processors - 1).min(1));
        assert_eq!(free(), processors - 1);
    }

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
