//! A memory budget for native extraction. A program that installs [`CountingAllocator`] as its
//! global allocator has every allocation a native parse makes counted against that
//! extraction's budget, until the memory is freed, on whichever thread. The extraction checks
//! the budget as it writes and fails with [`crate::Error::MemoryLimit`] once it is overrun, so
//! one extraction fails cleanly and the next runs with a budget of its own. One allocation may
//! overrun the budget before the check that fails the extraction.
//!
//! Without the counting allocator nothing is counted, and an extraction with a budget is left to
//! Tika's path, whose heap cap bounds it.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// The memory one extraction may hold, and what it holds: the bytes of its live allocations,
/// and one more while it runs. The budget is freed once both are gone, by whichever thread
/// frees the last of them, so an allocation may outlive the extraction that made it.
struct Budget {
    limit: u64,
    held: AtomicU64,
}

/// What the budget holds besides allocations while its extraction runs.
const RUNNING: u64 = 1;

impl Budget {
    /// Adds `bytes` to what the budget holds.
    fn charge(&self, bytes: u64) {
        self.held.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Takes `bytes` from what the budget at `budget` holds, and frees the budget when that was
    /// the last of it.
    ///
    /// # Safety
    /// `budget` is a live budget that holds at least `bytes`.
    unsafe fn credit(budget: *const Budget, bytes: u64) {
        if unsafe { (*budget).held.fetch_sub(bytes, Ordering::AcqRel) } == bytes {
            drop(unsafe { Box::from_raw(budget.cast_mut()) });
        }
    }
}

thread_local! {
    /// The budget the allocations of this thread are charged to, if any.
    static CURRENT: Cell<*const Budget> = const { Cell::new(ptr::null()) };
}

/// How many extractions run with a budget, on any thread: while none does, an allocation is
/// charged to none without looking up its thread's budget.
static BUDGETED: AtomicUsize = AtomicUsize::new(0);

fn current() -> *const Budget {
    if BUDGETED.load(Ordering::Relaxed) == 0 {
        return ptr::null();
    }
    CURRENT.try_with(Cell::get).unwrap_or(ptr::null())
}

/// Whether the program counts allocations, which it does once [`CountingAllocator`] has served
/// one.
static COUNTING: AtomicBool = AtomicBool::new(false);

pub(crate) fn counting() -> bool {
    COUNTING.load(Ordering::Relaxed)
}

/// Runs `work` with the allocations of this thread charged to a budget of `limit` bytes of its
/// own.
pub(crate) fn within<R>(limit: u64, work: impl FnOnce() -> R) -> R {
    struct Run {
        budget: *const Budget,
        previous: *const Budget,
    }
    impl Drop for Run {
        fn drop(&mut self) {
            CURRENT.with(|current| current.set(self.previous));
            BUDGETED.fetch_sub(1, Ordering::Relaxed);
            unsafe { Budget::credit(self.budget, RUNNING) };
        }
    }
    let budget = Box::into_raw(Box::new(Budget {
        limit,
        held: AtomicU64::new(RUNNING),
    }));
    let previous = CURRENT.with(|current| current.replace(budget));
    BUDGETED.fetch_add(1, Ordering::Relaxed);
    let _run = Run { budget, previous };
    work()
}

/// The limit of the budget this thread's allocations are charged to, if it is overrun.
pub(crate) fn overrun() -> Option<u64> {
    let budget = current();
    if budget.is_null() {
        return None;
    }
    let budget = unsafe { &*budget };
    let allocated = budget.held.load(Ordering::Relaxed) - RUNNING;
    (allocated > budget.limit).then_some(budget.limit)
}

/// A global allocator that counts what each extraction allocates against its budget, around
/// the allocator `A` that allocates. Each allocation keeps the budget it was charged to just
/// before the memory it returns.
pub struct CountingAllocator<A>(pub A);

/// Where the memory returned for `layout` starts in the block allocated for it, and the layout
/// of that block: room for the budget's pointer before the memory, at the memory's alignment.
fn block(layout: Layout) -> Option<(usize, Layout)> {
    let align = layout.align().max(size_of::<*const Budget>());
    let block = Layout::from_size_align(layout.size().checked_add(align)?, align).ok()?;
    Some((align, block))
}

/// Where the budget's pointer is kept for the memory at `memory`.
fn slot(memory: *mut u8) -> *mut *const Budget {
    memory
        .wrapping_sub(size_of::<*const Budget>())
        .cast::<*const Budget>()
}

fn bytes(size: usize) -> u64 {
    u64::try_from(size).unwrap_or(u64::MAX)
}

impl<A> CountingAllocator<A> {
    /// Keeps the budget of this thread, if any, for the memory `offset` into the block at
    /// `base`, and charges it `size` bytes.
    ///
    /// # Safety
    /// `base` is null or a block of at least `offset` bytes before the memory.
    unsafe fn register(&self, base: *mut u8, offset: usize, size: usize) -> *mut u8 {
        if base.is_null() {
            return base;
        }
        if !COUNTING.load(Ordering::Relaxed) {
            COUNTING.store(true, Ordering::Relaxed);
        }
        let memory = unsafe { base.add(offset) };
        let budget = current();
        if !budget.is_null() {
            unsafe { (*budget).charge(bytes(size)) };
        }
        unsafe { slot(memory).write(budget) };
        memory
    }
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some((offset, block)) = block(layout) else {
            return ptr::null_mut();
        };
        unsafe { self.register(self.0.alloc(block), offset, layout.size()) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let Some((offset, block)) = block(layout) else {
            return ptr::null_mut();
        };
        unsafe { self.register(self.0.alloc_zeroed(block), offset, layout.size()) }
    }

    unsafe fn dealloc(&self, memory: *mut u8, layout: Layout) {
        let (offset, block) = block(layout).expect("the layout was allocated");
        let budget = unsafe { slot(memory).read() };
        unsafe { self.0.dealloc(memory.sub(offset), block) };
        if !budget.is_null() {
            unsafe { Budget::credit(budget, bytes(layout.size())) };
        }
    }

    unsafe fn realloc(&self, memory: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let (offset, block) = block(layout).expect("the layout was allocated");
        let Some(new_block_size) = new_size.checked_add(offset) else {
            return ptr::null_mut();
        };
        let budget = unsafe { slot(memory).read() };
        if !budget.is_null() {
            unsafe { (*budget).charge(bytes(new_size)) };
        }
        let base = unsafe { self.0.realloc(memory.sub(offset), block, new_block_size) };
        if !budget.is_null() {
            let released = if base.is_null() {
                new_size
            } else {
                layout.size()
            };
            unsafe { Budget::credit(budget, bytes(released)) };
        }
        if base.is_null() {
            return base;
        }
        unsafe { base.add(offset) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::System;

    /// What the budget of this thread holds in allocations.
    fn allocated() -> u64 {
        let budget = current();
        unsafe { (*budget).held.load(Ordering::Relaxed) - RUNNING }
    }

    #[test]
    fn counts_what_an_extraction_holds_until_it_is_freed() {
        let allocator = CountingAllocator(System);
        let layout = Layout::from_size_align(64, 16).unwrap();
        let (grown, second) = within(100, || unsafe {
            let first = allocator.alloc(layout);
            let second = allocator.alloc_zeroed(layout);
            assert_eq!(first as usize % 16, 0);
            assert_eq!(*second, 0);
            assert_eq!(allocated(), 128);
            assert_eq!(overrun(), Some(100));
            let grown = allocator.realloc(first, layout, 20);
            assert_eq!(allocated(), 84);
            assert_eq!(overrun(), None);
            (grown, second)
        });
        assert_eq!(overrun(), None);
        unsafe {
            allocator.dealloc(grown, Layout::from_size_align(20, 16).unwrap());
            allocator.dealloc(second, layout);
        }
    }

    #[test]
    fn leaves_allocations_outside_an_extraction_uncounted() {
        let allocator = CountingAllocator(System);
        let layout = Layout::from_size_align(32, 1).unwrap();
        let memory = unsafe { allocator.alloc(layout) };
        within(10, || {
            unsafe { allocator.dealloc(memory, layout) };
            assert_eq!(allocated(), 0);
        });
        assert!(counting());
    }
}
