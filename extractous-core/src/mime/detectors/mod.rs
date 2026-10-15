//! The detectors Tika's `DefaultDetector` loads from the class path besides `MimeTypes`.

pub(crate) mod bplist;
pub(crate) mod gzip;
pub(crate) mod matroska;
pub(crate) mod ogg;
pub(crate) mod ole;
pub(crate) mod raw_tiff;
pub(crate) mod zip;

/// What a detector throws out of Tika's detection, which then fails as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Thrown {
    /// An `IOException`: `TikaNativeMain` reports the input as one it could not open.
    Io,
    /// A `RuntimeException`, which `TikaNativeMain` does not catch.
    Runtime,
}
