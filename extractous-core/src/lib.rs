#![doc = include_str!("../../README.md")]
/// Default buffer size - optimized for better performance
/// Increased from 32KB to 256KB for better throughput based on benchmarks
pub const DEFAULT_BUF_SIZE: usize = 262144; // 256KB

/// Large buffer size for memory-mapped operations
pub const LARGE_BUF_SIZE: usize = 1048576; // 1MB

/// Threshold for using memory-mapped I/O - lowered based on benchmarks
pub const MMAP_THRESHOLD: usize = 512 * 1024; // 512KB

// errors module
mod errors;
pub use errors::*;

// config module
mod config;
pub use config::*;

// embedded module for embedded document extraction
mod embedded;
pub use embedded::*;

// extractor module is the main public api interface
mod extractor;
pub use extractor::*;

// format detection module
mod format_detection;
pub use format_detection::*;

// pure rust parsers for performance optimization
mod pure_rust_parsers;
pub use pure_rust_parsers::*;

// SIMD-optimized text processing
mod simd_text;
pub use simd_text::*;

// tika module, not exposed outside this crate
mod tika {
    mod jni_utils;
    mod parse;
    mod parse_embedded;
    mod wrappers;
    pub use parse::*;
    pub use parse_embedded::*;
    pub use wrappers::JReaderInputStream;
}
