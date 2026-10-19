//! extractous's media type detection: a port of what Tika's `DefaultDetector` answers, and what a
//! document is before its native parser runs.

use extractous_base::*;

pub mod mime;

/// The tests count allocations as the command line does, so that budgets are enforced in them.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<std::alloc::System> =
    memory::CountingAllocator(std::alloc::System);
