use std::io;
use std::str::Utf8Error;

/// Represent errors returned by dijo
#[derive(thiserror::Error, Debug, Clone)]
pub enum Error {
    #[error("{0}")]
    Unknown(String),

    #[error("{0}")]
    IoError(String),

    #[error("{0}")]
    ParseError(String),

    #[error("{0}")]
    Encrypted(String),

    #[error("{0}")]
    LimitExceeded(String),

    #[error("{0}")]
    Utf8Error(#[from] Utf8Error),

    /// No native parser extracts the document.
    #[error("{0}")]
    Unsupported(#[from] Unsupported),

    #[error("The final metadata is only available once the stream has been read to the end")]
    StreamNotFinished,

    #[error("The extraction exceeded its time limit")]
    Timeout,

    #[error("The native extraction exceeded its memory budget of {0} bytes")]
    MemoryLimit(u64),
}

/// Why no native parser extracts a document, which the extraction fails with.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// No native parser reads documents of this media type.
    #[error("No native parser reads documents of type {0}")]
    Format(String),
    /// The native parser of this name does not read this document yet, for this reason.
    #[error("{parser} does not read this document natively yet: {reason}")]
    Unread {
        parser: &'static str,
        reason: String,
    },
    /// The native parser of this name reads its input whole, and the file is larger than
    /// `limit` bytes, the most a file is read whole.
    #[error("{parser} reads its input whole, and the file is larger than {limit} bytes")]
    TooLarge { parser: &'static str, limit: u64 },
    /// Tika's detection throws on the document, for this reason, as the native detection
    /// reproduces.
    #[error("The type of the document cannot be detected: {0}")]
    Undetected(String),
    /// A memory cap is set, which holds a native extraction only in a program that counts its
    /// allocations with [`crate::memory::CountingAllocator`].
    #[error(
        "A memory cap is set, which holds a native extraction only in a program that counts \
         its allocations"
    )]
    UncountedMemoryCap,
}

impl From<Error> for io::Error {
    fn from(err: Error) -> Self {
        io::Error::other(err)
    }
}

/// Result that is a wrapper of Result<T, dijo_core::Error>
pub type ExtractResult<T> = Result<T, Error>;
