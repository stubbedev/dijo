//! extractous's native parsers of audio and video files: MP3, MP4 and QuickTime, FLV, Ogg (Vorbis,
//! Opus, FLAC), MIDI and the formats of Java's audio system.

use extractous_base::*;
#[cfg(all(test, feature = "tika"))]
use extractous_core::Extractor;

/// The tests count allocations as the command line does, so that budgets are enforced in them.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<std::alloc::System> =
    memory::CountingAllocator(std::alloc::System);

/// The parsers, at the paths they have in `extractous-core`.
pub mod formats {
    pub(crate) use extractous_base::formats::*;
    #[cfg(all(test, feature = "tika"))]
    pub(crate) use extractous_core::parity;

    pub mod media;
}
