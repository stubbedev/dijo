//! dijo's native parsers of audio and video files: MP3, MP4 and QuickTime, FLV, Ogg (Vorbis,
//! Opus, FLAC), MIDI and the formats of Java's audio system.

use dijo_base::*;

/// The tests count allocations as the command line does, so that budgets are enforced in them,
/// and measure the peak of what each thread holds.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<dijo_core::large::Peak<std::alloc::System>> =
    memory::CountingAllocator(dijo_core::large::Peak(std::alloc::System));

/// The parsers, at the paths they have in `dijo-core`.
pub mod formats {
    pub(crate) use dijo_base::formats::*;

    pub mod media;
}
