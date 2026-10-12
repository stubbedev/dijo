use std::io;
use std::str::Utf8Error;

/// Represent errors returned by extractous
#[derive(thiserror::Error, Debug)]
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
    Utf8Error(#[from] Utf8Error),

    #[error("{0}")]
    JniError(#[from] jni::errors::Error),

    #[error("{0}")]
    JniEnvCall(&'static str),

    #[error("Java exception: {0}")]
    JavaException(String),

    #[error("Native library error: {0}")]
    NativeLibrary(String),

    #[error("The final metadata is only available once the stream has been read to the end")]
    StreamNotFinished,

    #[error("The extraction exceeded its time limit")]
    Timeout,
}

impl From<Error> for io::Error {
    fn from(err: Error) -> Self {
        io::Error::other(err)
    }
}

/// Result that is a wrapper of Result<T, extractous_core::Error>
pub type ExtractResult<T> = Result<T, Error>;
