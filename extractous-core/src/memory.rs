//! A memory budget for native extraction. A program that installs [`CountingAllocator`] as its
//! global allocator has every allocation a native parse makes counted against that
//! extraction's budget. The extraction checks the budget as it writes and fails with
//! [`crate::Error::MemoryLimit`] once it is overrun, so one extraction fails cleanly and the next
//! runs with a budget of its own. One allocation may overrun the budget before the check that
//! fails the extraction.
//!
//! An allocation of 4 KiB or more, the kind that overruns a budget, keeps the budget
//! it was charged to just before its memory, and is credited to that budget when it is freed, on
//! whichever thread. A smaller allocation is passed straight to the inner allocator: it is
//! charged to the budget of the thread that makes it and credited to the budget of the thread
//! that frees it, if any, so a small allocation freed on another thread stays charged to its
//! extraction, and one made before an extraction and freed within it lowers that extraction's
//! count.
//!
//! Without the counting allocator nothing is counted, and an extraction with a budget is left to
//! Tika's path, whose heap cap bounds it.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// The size from which an allocation keeps its budget, for exact accounting on any thread.
const LARGE: usize = 4096;

/// The memory one extraction may hold, and what its large allocations hold: their bytes, and one
/// more while the extraction runs. The budget is freed once both are gone, by whichever thread
/// frees the last of them, so a large allocation may outlive the extraction that made it.
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

/// The budget a thread's allocations are charged to, and the net bytes of the small allocations
/// the thread made and freed while it was current.
struct Charged {
    budget: Cell<*const Budget>,
    small: Cell<i64>,
}

thread_local! {
    static CHARGED: Charged = const {
        Charged {
            budget: Cell::new(ptr::null()),
            small: Cell::new(0),
        }
    };
}

/// How many extractions run with a budget, on any thread: while none does, an allocation is
/// charged to none without looking up its thread's budget.
static BUDGETED: AtomicUsize = AtomicUsize::new(0);

fn current() -> *const Budget {
    if BUDGETED.load(Ordering::Relaxed) == 0 {
        return ptr::null();
    }
    CHARGED
        .try_with(|charged| charged.budget.get())
        .unwrap_or(ptr::null())
}

/// Adds `bytes`, negative for a free, to the small allocations of this thread's budget, if any.
fn charge_small(bytes: i64) {
    if BUDGETED.load(Ordering::Relaxed) == 0 {
        return;
    }
    let _ = CHARGED.try_with(|charged| {
        if !charged.budget.get().is_null() {
            charged.small.set(charged.small.get() + bytes);
        }
    });
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
        previous_small: i64,
    }
    impl Drop for Run {
        fn drop(&mut self) {
            CHARGED.with(|charged| {
                charged.budget.set(self.previous);
                charged.small.set(self.previous_small);
            });
            BUDGETED.fetch_sub(1, Ordering::Relaxed);
            unsafe { Budget::credit(self.budget, RUNNING) };
        }
    }
    let budget = Box::into_raw(Box::new(Budget {
        limit,
        held: AtomicU64::new(RUNNING),
    }));
    let (previous, previous_small) =
        CHARGED.with(|charged| (charged.budget.replace(budget), charged.small.replace(0)));
    BUDGETED.fetch_add(1, Ordering::Relaxed);
    let _run = Run {
        budget,
        previous,
        previous_small,
    };
    work()
}

/// The limit of the budget this thread's allocations are charged to, if it is overrun.
pub(crate) fn overrun() -> Option<u64> {
    let budget = current();
    if budget.is_null() {
        return None;
    }
    let budget = unsafe { &*budget };
    let large = budget.held.load(Ordering::Relaxed) - RUNNING;
    let small = CHARGED.with(|charged| charged.small.get());
    let allocated = i128::from(large) + i128::from(small);
    (allocated > i128::from(budget.limit)).then_some(budget.limit)
}

/// A global allocator that counts what each extraction allocates against its budget, around
/// the allocator `A` that allocates. Each allocation of 4 KiB or more keeps the budget
/// it was charged to just before the memory it returns.
///
/// Its functions are never inlined: LLVM takes the memory an allocation returns for the whole
/// object, so where link-time optimisation inlines `dealloc` into the code that allocated, its
/// read of the budget before that memory is out of bounds, and LLVM compiled a `String`'s drop
/// in `formats::image` into a panic.
pub struct CountingAllocator<A>(pub A);

/// Whether an allocation of `size` bytes keeps its budget.
fn is_large(size: usize) -> bool {
    size >= LARGE
}

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

fn signed(size: usize) -> i64 {
    i64::try_from(size).unwrap_or(i64::MAX)
}

fn note_counting() {
    if !COUNTING.load(Ordering::Relaxed) {
        COUNTING.store(true, Ordering::Relaxed);
    }
}

impl<A: GlobalAlloc> CountingAllocator<A> {
    /// Keeps the budget of this thread, if any, for the memory `offset` into the block at
    /// `base`, and charges it `size` bytes.
    ///
    /// # Safety
    /// `base` is null or a block of at least `offset` bytes before the memory.
    unsafe fn register(&self, base: *mut u8, offset: usize, size: usize) -> *mut u8 {
        if base.is_null() {
            return base;
        }
        note_counting();
        let memory = unsafe { base.add(offset) };
        let budget = current();
        if !budget.is_null() {
            unsafe { (*budget).charge(bytes(size)) };
        }
        unsafe { slot(memory).write(budget) };
        memory
    }

    /// Charges a small allocation at `memory` to this thread's budget, if it was made.
    fn small(memory: *mut u8, size: usize) -> *mut u8 {
        if !memory.is_null() {
            note_counting();
            charge_small(signed(size));
        }
        memory
    }

    /// Moves the allocation at `memory` of `layout` to a new one of `new_size` bytes, where one
    /// of the two is large and the other is not.
    ///
    /// # Safety
    /// As [`GlobalAlloc::realloc`].
    unsafe fn relocate(&self, memory: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        let moved = unsafe { self.alloc(new_layout) };
        if !moved.is_null() {
            unsafe {
                ptr::copy_nonoverlapping(memory, moved, layout.size().min(new_size));
                self.dealloc(memory, layout);
            }
        }
        moved
    }
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
    #[inline(never)]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !is_large(layout.size()) {
            return Self::small(unsafe { self.0.alloc(layout) }, layout.size());
        }
        let Some((offset, block)) = block(layout) else {
            return ptr::null_mut();
        };
        unsafe { self.register(self.0.alloc(block), offset, layout.size()) }
    }

    #[inline(never)]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !is_large(layout.size()) {
            return Self::small(unsafe { self.0.alloc_zeroed(layout) }, layout.size());
        }
        let Some((offset, block)) = block(layout) else {
            return ptr::null_mut();
        };
        unsafe { self.register(self.0.alloc_zeroed(block), offset, layout.size()) }
    }

    #[inline(never)]
    unsafe fn dealloc(&self, memory: *mut u8, layout: Layout) {
        if !is_large(layout.size()) {
            unsafe { self.0.dealloc(memory, layout) };
            charge_small(-signed(layout.size()));
            return;
        }
        let (offset, block) = block(layout).expect("the layout was allocated");
        let budget = unsafe { slot(memory).read() };
        unsafe { self.0.dealloc(memory.sub(offset), block) };
        if !budget.is_null() {
            unsafe { Budget::credit(budget, bytes(layout.size())) };
        }
    }

    #[inline(never)]
    unsafe fn realloc(&self, memory: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        match (is_large(layout.size()), is_large(new_size)) {
            (false, false) => {
                let moved = unsafe { self.0.realloc(memory, layout, new_size) };
                if !moved.is_null() {
                    charge_small(signed(new_size) - signed(layout.size()));
                }
                moved
            }
            (true, true) => unsafe { self.realloc_large(memory, layout, new_size) },
            _ => unsafe { self.relocate(memory, layout, new_size) },
        }
    }
}

impl<A: GlobalAlloc> CountingAllocator<A> {
    /// [`GlobalAlloc::realloc`] of a large allocation to a large size, in place in its block.
    ///
    /// # Safety
    /// As [`GlobalAlloc::realloc`], with both sizes large.
    unsafe fn realloc_large(&self, memory: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
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
    fn allocated() -> i128 {
        let budget = current();
        let large = unsafe { (*budget).held.load(Ordering::Relaxed) - RUNNING };
        i128::from(large) + i128::from(CHARGED.with(|charged| charged.small.get()))
    }

    /// What the large allocations of this thread's budget hold, which spawning a thread leaves
    /// alone, unlike the small ones.
    fn large() -> u64 {
        unsafe { (*current()).held.load(Ordering::Relaxed) - RUNNING }
    }

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size, 16).unwrap()
    }

    #[test]
    fn counts_what_an_extraction_holds_until_it_is_freed() {
        let allocator = CountingAllocator(System);
        let limit = bytes(LARGE + 100);
        let (grown, second) = within(limit, || unsafe {
            let first = allocator.alloc(layout(64));
            let second = allocator.alloc_zeroed(layout(LARGE));
            assert_eq!(first as usize % 16, 0);
            assert_eq!(second as usize % 16, 0);
            assert_eq!(*second, 0);
            assert_eq!(allocated(), 64 + LARGE as i128);
            assert_eq!(overrun(), None);
            let third = allocator.alloc(layout(64));
            assert_eq!(overrun(), Some(limit));
            allocator.dealloc(third, layout(64));
            let grown = allocator.realloc(first, layout(64), 20);
            assert_eq!(allocated(), 20 + LARGE as i128);
            assert_eq!(overrun(), None);
            (grown, second)
        });
        assert_eq!(overrun(), None);
        unsafe {
            allocator.dealloc(grown, layout(20));
            allocator.dealloc(second, layout(LARGE));
        }
    }

    #[test]
    fn credits_a_large_allocation_to_its_budget_on_any_thread() {
        let allocator = CountingAllocator(System);
        within(1, || {
            let memory = unsafe { allocator.alloc(layout(LARGE)) } as usize;
            assert_eq!(large(), LARGE as u64);
            std::thread::scope(|scope| {
                scope.spawn(|| unsafe { allocator.dealloc(memory as *mut u8, layout(LARGE)) });
            });
            assert_eq!(large(), 0);
        });
    }

    #[test]
    fn frees_a_budget_when_its_last_large_allocation_outlives_it() {
        let allocator = CountingAllocator(System);
        let memory = within(1, || unsafe { allocator.alloc(layout(LARGE)) });
        unsafe { allocator.dealloc(memory, layout(LARGE)) };
    }

    #[test]
    fn leaves_large_allocations_outside_an_extraction_uncounted() {
        let allocator = CountingAllocator(System);
        let memory = unsafe { allocator.alloc(layout(2 * LARGE)) };
        within(10, || {
            unsafe { allocator.dealloc(memory, layout(2 * LARGE)) };
            assert_eq!(allocated(), 0);
        });
        assert!(counting());
    }

    #[test]
    fn moves_an_allocation_across_the_large_size_with_its_bytes() {
        let allocator = CountingAllocator(System);
        within(u64::MAX, || unsafe {
            let memory = allocator.alloc(layout(16));
            memory.copy_from_nonoverlapping(b"0123456789abcdef".as_ptr(), 16);
            let grown = allocator.realloc(memory, layout(16), 2 * LARGE);
            assert_eq!(std::slice::from_raw_parts(grown, 16), b"0123456789abcdef");
            assert_eq!(allocated(), 2 * LARGE as i128);
            let shrunk = allocator.realloc(grown, layout(2 * LARGE), 8);
            assert_eq!(std::slice::from_raw_parts(shrunk, 8), b"01234567");
            assert_eq!(allocated(), 8);
            allocator.dealloc(shrunk, layout(8));
            assert_eq!(allocated(), 0);
        });
    }

    #[test]
    fn restores_the_outer_budget_after_a_nested_one() {
        let allocator = CountingAllocator(System);
        within(u64::MAX, || unsafe {
            let outer = allocator.alloc(layout(32));
            within(u64::MAX, || assert_eq!(allocated(), 0));
            assert_eq!(allocated(), 32);
            allocator.dealloc(outer, layout(32));
        });
    }
}
