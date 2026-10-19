//! The ground dijo's native parsers stand on: errors and settings, ports of Java's
//! semantics and of Tika's models of text, the XML reader, the containers and codecs several
//! formats share, and the common ground of the parsers themselves, the [`formats::Parser`] they
//! implement and the [`formats::xhtml::Xhtml`] they write through. `dijo-core` gathers the
//! parsers, which live in the crates of their formats, and is the API; this one is internal and
//! changes freely.

pub mod bplist;
pub mod compress;
pub mod config;
pub mod embedded;
pub mod errors;
pub mod extracted;
pub mod formats;
pub mod hash;
pub mod input;
pub mod java;
pub mod memory;
pub mod ole2;
pub mod ordered_map;
pub mod process;
pub mod text;
pub mod unicode;
pub mod xml;

pub use config::*;
pub use embedded::*;
pub use errors::*;
pub use extracted::*;

/// The tests count allocations as the command line does, so that budgets are enforced in them.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<std::alloc::System> =
    memory::CountingAllocator(std::alloc::System);
