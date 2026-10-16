//! A memory budget for native extraction. A program that installs [`CountingAllocator`] as its
//! global allocator has every allocation a native parse makes counted against that
//! extraction's budget. The extraction checks the budget as it writes and fails with
//! [`crate::Error::MemoryLimit`] once it is overrun, so one extraction fails cleanly and the next
//! runs with a budget of its own. One allocation may overrun the budget before the check that
//! fails the extraction.
//!
//! An allocation of 4 KiB or more made in an extraction, the kind that overruns a budget, is
//! recorded by its address with the budget it was charged to, in a table beside the allocator,
//! and is credited to that budget when it is freed, on whichever thread. A smaller allocation is passed straight to the inner allocator: it is
//! charged to the budget of the thread that makes it and credited to the budget of the thread
//! that frees it, if any, so a small allocation freed on another thread stays charged to its
//! extraction, and one made before an extraction and freed within it lowers that extraction's
//! count.
//!
//! Without the counting allocator nothing is counted, and an extraction with a budget is left to
//! Tika's path, whose heap cap bounds it.

use std::alloc::{handle_alloc_error, GlobalAlloc, Layout, System};
use std::cell::{Cell, UnsafeCell};
use std::hash::Hasher;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::hash::FastHasher;

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
/// the allocator `A` that allocates. It keeps nothing in or beside the memory it returns: the
/// budget of each large allocation made in an extraction is recorded in [`OWNERS`].
pub struct CountingAllocator<A>(pub A);

/// Whether an allocation of `size` bytes is credited to the budget it was charged to.
fn is_large(size: usize) -> bool {
    size >= LARGE
}

/// The large allocations made in an extraction, by address, with the budget each was charged
/// to: a hash table with linear probing, in memory of the system allocator, behind a spin lock.
struct Owners {
    locked: AtomicBool,
    table: UnsafeCell<Table>,
    /// How many allocations the table holds, read without the lock: while it is zero, freeing a
    /// large allocation looks nothing up.
    len: AtomicUsize,
}

/// The slots of [`Owners`]: a power of two of them, or none, at most half of them used.
struct Table {
    slots: *mut Slot,
    capacity: usize,
    len: usize,
}

/// A slot of [`Table`]: an allocation's address, zero in an empty slot, and its budget.
#[derive(Clone, Copy)]
struct Slot {
    address: usize,
    budget: *const Budget,
}

unsafe impl Sync for Owners {}

static OWNERS: Owners = Owners {
    locked: AtomicBool::new(false),
    table: UnsafeCell::new(Table {
        slots: ptr::null_mut(),
        capacity: 0,
        len: 0,
    }),
    len: AtomicUsize::new(0),
};

/// How many times waiting for [`OWNERS`] spins before it yields the processor.
const SPINS: u32 = 64;

impl Owners {
    /// Runs `work` on the table, holding the lock.
    fn with<R>(&self, work: impl FnOnce(&mut Table) -> R) -> R {
        let mut spins = 0;
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spins += 1;
            if spins < SPINS {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        let table = unsafe { &mut *self.table.get() };
        let result = work(table);
        self.len.store(table.len, Ordering::Relaxed);
        self.locked.store(false, Ordering::Release);
        result
    }

    /// Records that the allocation at `address` is charged to `budget`.
    fn insert(&self, address: usize, budget: *const Budget) {
        self.with(|table| table.insert(address, budget));
    }

    /// Forgets the allocation at `address`, and returns the budget it was charged to, if it was
    /// made in an extraction.
    fn remove(&self, address: usize) -> Option<*const Budget> {
        if self.len.load(Ordering::Relaxed) == 0 {
            return None;
        }
        self.with(|table| table.remove(address))
    }
}

impl Table {
    /// The slot an address is looked up from.
    fn home(&self, address: usize) -> usize {
        let mut hasher = FastHasher::default();
        hasher.write_usize(address);
        (hasher.finish() >> (u64::BITS - self.capacity.trailing_zeros())) as usize
    }

    fn slots(&mut self) -> &mut [Slot] {
        if self.capacity == 0 {
            return &mut [];
        }
        unsafe { std::slice::from_raw_parts_mut(self.slots, self.capacity) }
    }

    fn insert(&mut self, address: usize, budget: *const Budget) {
        if 2 * (self.len + 1) > self.capacity {
            self.grow();
        }
        let mask = self.capacity - 1;
        let mut index = self.home(address);
        let slots = self.slots();
        while slots[index].address != 0 {
            index = (index + 1) & mask;
        }
        slots[index] = Slot { address, budget };
        self.len += 1;
    }

    fn remove(&mut self, address: usize) -> Option<*const Budget> {
        if self.capacity == 0 {
            return None;
        }
        let mask = self.capacity - 1;
        let mut hole = self.home(address);
        while self.slots()[hole].address != address {
            if self.slots()[hole].address == 0 {
                return None;
            }
            hole = (hole + 1) & mask;
        }
        let budget = self.slots()[hole].budget;
        let mut next = (hole + 1) & mask;
        loop {
            let slot = self.slots()[next];
            if slot.address == 0 {
                break;
            }
            let home = self.home(slot.address);
            if next.wrapping_sub(home) & mask >= next.wrapping_sub(hole) & mask {
                self.slots()[hole] = slot;
                hole = next;
            }
            next = (next + 1) & mask;
        }
        self.slots()[hole].address = 0;
        self.len -= 1;
        Some(budget)
    }

    /// Doubles the slots, from the system allocator, which never calls back into this one.
    fn grow(&mut self) {
        let capacity = (2 * self.capacity).max(64);
        let layout = Layout::array::<Slot>(capacity).expect("the owners table fits in memory");
        let slots = unsafe { System.alloc_zeroed(layout) }.cast::<Slot>();
        if slots.is_null() {
            handle_alloc_error(layout);
        }
        let mut old = Table {
            slots: std::mem::replace(&mut self.slots, slots),
            capacity: std::mem::replace(&mut self.capacity, capacity),
            len: std::mem::replace(&mut self.len, 0),
        };
        for slot in old.slots().iter().filter(|slot| slot.address != 0) {
            self.insert(slot.address, slot.budget);
        }
    }
}

impl Drop for Table {
    fn drop(&mut self) {
        if self.capacity > 0 {
            let layout = Layout::array::<Slot>(self.capacity).expect("the table was allocated");
            unsafe { System.dealloc(self.slots.cast(), layout) };
        }
    }
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

/// Charges the new allocation of `size` bytes at `memory`, if it was made, to this thread's
/// budget, if any.
fn allocated(memory: *mut u8, size: usize) -> *mut u8 {
    if memory.is_null() {
        return memory;
    }
    note_counting();
    if !is_large(size) {
        charge_small(signed(size));
        return memory;
    }
    let budget = current();
    if !budget.is_null() {
        unsafe { (*budget).charge(bytes(size)) };
        OWNERS.insert(memory as usize, budget);
    }
    memory
}

/// Forgets the allocation of `size` bytes at `memory`, about to be freed, and returns the budget
/// to credit once it is, for a large one made in an extraction.
fn freeing(memory: *mut u8, size: usize) -> Option<*const Budget> {
    if is_large(size) {
        OWNERS.remove(memory as usize)
    } else {
        charge_small(-signed(size));
        None
    }
}

/// Credits `budget`, if any, with the freed `size` bytes.
fn freed(budget: Option<*const Budget>, size: usize) {
    if let Some(budget) = budget {
        unsafe { Budget::credit(budget, bytes(size)) };
    }
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated(unsafe { self.0.alloc(layout) }, layout.size())
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated(unsafe { self.0.alloc_zeroed(layout) }, layout.size())
    }

    unsafe fn dealloc(&self, memory: *mut u8, layout: Layout) {
        let budget = freeing(memory, layout.size());
        unsafe { self.0.dealloc(memory, layout) };
        freed(budget, layout.size());
    }

    unsafe fn realloc(&self, memory: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let size = layout.size();
        if !is_large(size) && !is_large(new_size) {
            let moved = unsafe { self.0.realloc(memory, layout, new_size) };
            if !moved.is_null() {
                charge_small(signed(new_size) - signed(size));
            }
            return moved;
        }
        if is_large(size) && is_large(new_size) {
            return unsafe { self.realloc_large(memory, layout, new_size) };
        }
        let budget = if is_large(size) {
            OWNERS.remove(memory as usize)
        } else {
            None
        };
        let moved = unsafe { self.0.realloc(memory, layout, new_size) };
        if moved.is_null() {
            if let Some(budget) = budget {
                OWNERS.insert(memory as usize, budget);
            }
            return moved;
        }
        if !is_large(size) {
            charge_small(-signed(size));
        }
        freed(budget, size);
        allocated(moved, new_size)
    }
}

impl<A: GlobalAlloc> CountingAllocator<A> {
    /// [`GlobalAlloc::realloc`] of a large allocation to a large size, which stays charged to
    /// the budget it was charged to, if any.
    ///
    /// # Safety
    /// As [`GlobalAlloc::realloc`], with both sizes large.
    unsafe fn realloc_large(&self, memory: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Some(budget) = OWNERS.remove(memory as usize) else {
            return unsafe { self.0.realloc(memory, layout, new_size) };
        };
        unsafe { (*budget).charge(bytes(new_size)) };
        let moved = unsafe { self.0.realloc(memory, layout, new_size) };
        let (kept, released) = if moved.is_null() {
            (memory, new_size)
        } else {
            (moved, layout.size())
        };
        OWNERS.insert(kept as usize, budget);
        freed(Some(budget), released);
        moved
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
    fn keeps_a_grown_large_allocation_charged_to_its_budget() {
        let allocator = CountingAllocator(System);
        let grown = within(u64::MAX, || unsafe {
            let memory = allocator.alloc(layout(LARGE));
            memory.write(7);
            let grown = allocator.realloc(memory, layout(LARGE), 64 * LARGE);
            assert_eq!(*grown, 7);
            assert_eq!(large(), 64 * LARGE as u64);
            grown as usize
        });
        within(u64::MAX, || {
            let shrunk = std::thread::scope(|scope| {
                scope
                    .spawn(|| unsafe {
                        allocator.realloc(grown as *mut u8, layout(64 * LARGE), 2 * LARGE) as usize
                    })
                    .join()
                    .unwrap()
            });
            unsafe { allocator.dealloc(shrunk as *mut u8, layout(2 * LARGE)) };
            assert_eq!(large(), 0);
        });
    }

    #[test]
    fn credits_many_large_allocations_freed_in_any_order() {
        let allocator = CountingAllocator(System);
        within(u64::MAX, || {
            let mut memories: Vec<usize> = Vec::with_capacity(1000);
            let before = large();
            memories.extend((0..1000).map(|_| unsafe { allocator.alloc(layout(LARGE)) } as usize));
            assert_eq!(large() - before, 1000 * LARGE as u64);
            memories.sort_unstable_by_key(|&memory| memory.wrapping_mul(31) % 997);
            let (first, second) = memories.split_at(500);
            let allocator = &allocator;
            std::thread::scope(|scope| {
                for half in [first, second] {
                    scope.spawn(move || {
                        for &memory in half {
                            unsafe { allocator.dealloc(memory as *mut u8, layout(LARGE)) };
                        }
                    });
                }
            });
            assert_eq!(large(), before);
        });
    }

    #[test]
    fn finds_every_address_a_table_holds_until_it_is_removed() {
        let mut table = Table {
            slots: ptr::null_mut(),
            capacity: 0,
            len: 0,
        };
        let budget = |n: usize| n as *const Budget;
        let addresses: Vec<usize> = (1..=3000).map(|n| n * 4096).collect();
        for &address in &addresses {
            table.insert(address, budget(address / 4096));
        }
        assert_eq!(table.remove(1), None);
        for &address in addresses.iter().step_by(2) {
            assert_eq!(table.remove(address), Some(budget(address / 4096)));
        }
        for (index, &address) in addresses.iter().enumerate() {
            let expected = (index % 2 == 1).then(|| budget(address / 4096));
            assert_eq!(table.remove(address), expected);
        }
        assert_eq!(table.len, 0);
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
