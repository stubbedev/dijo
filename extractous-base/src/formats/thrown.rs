//! The exceptions a Tika parser throws out of its `parse`, as Tika's `CompositeParser` wraps
//! them, for the native parsers that fail where Tika's parsers throw.

use crate::errors::Error;

/// Tika's `TikaException`.
pub const TIKA_EXCEPTION: &str = "org.apache.tika.exception.TikaException";

/// A Java exception: its class, its message, if it has one, and the exception it wraps, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exception {
    class: &'static str,
    message: Option<String>,
    cause: Option<Box<Exception>>,
}

impl Exception {
    pub fn new(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            message: Some(message.into()),
            cause: None,
        }
    }

    pub fn bare(class: &'static str) -> Self {
        Self {
            class,
            message: None,
            cause: None,
        }
    }

    /// An exception of `class` made of its `cause` alone, as `new IOException(cause)` takes the
    /// cause's `toString` for its message.
    pub fn wrapping(class: &'static str, cause: Exception) -> Self {
        Self {
            class,
            message: Some(cause.to_java_string()),
            cause: Some(Box::new(cause)),
        }
    }

    /// This exception with the exception `cause` it wraps.
    pub fn caused_by(self, cause: Exception) -> Self {
        Self {
            cause: Some(Box::new(cause)),
            ..self
        }
    }

    /// `Throwable.toString`: the class, then the message, if there is one.
    fn to_java_string(&self) -> String {
        match &self.message {
            Some(message) => format!("{}: {message}", self.class),
            None => self.class.to_owned(),
        }
    }

    /// The exception and the causes it wraps, as our Java reports them.
    fn describe(&self) -> String {
        match &self.cause {
            Some(cause) => format!(
                "{} <- caused by {}",
                self.to_java_string(),
                cause.describe()
            ),
            None => self.to_java_string(),
        }
    }
}

impl std::fmt::Display for Exception {
    /// `Throwable.toString`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_java_string())
    }
}

/// What a Tika parser throws out of its `parse`, which `CompositeParser` wraps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thrown {
    /// An `IOException`, which it wraps as `TIKA-198`.
    Io(Exception),
    /// A `RuntimeException`.
    Runtime(Exception),
    /// A `TikaException`, which it passes on as it is.
    Tika(Exception),
}

impl std::fmt::Display for Thrown {
    /// The exception's `Throwable.toString`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(exception) | Self::Runtime(exception) | Self::Tika(exception) => {
                write!(f, "{exception}")
            }
        }
    }
}

impl Thrown {
    /// The error Tika's path fails with when the parser `tika_name` throws this: the
    /// `TikaException` `CompositeParser` wraps it in, as our Java reports it.
    pub fn into_error(self, tika_name: &str) -> Error {
        let (wrapped, cause) = match self {
            Self::Tika(exception) => {
                return Error::ParseError(format!(
                    "Parse error occurred : {}",
                    exception.describe()
                ))
            }
            Self::Io(cause) => (
                format!("TIKA-198: Illegal IOException from {tika_name}"),
                cause,
            ),
            Self::Runtime(cause) => (
                format!("Unexpected RuntimeException from {tika_name}"),
                cause,
            ),
        };
        Error::ParseError(format!(
            "Parse error occurred : org.apache.tika.exception.TikaException: {wrapped} <- caused \
             by {}",
            cause.describe()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_exceptions_as_composite_parser_does() {
        let io = Thrown::Io(Exception::new("java.io.EOFException", "short"));
        assert_eq!(
            io.into_error("org.example.Parser").to_string(),
            "Parse error occurred : org.apache.tika.exception.TikaException: TIKA-198: Illegal \
             IOException from org.example.Parser <- caused by java.io.EOFException: short"
        );
        let runtime = Thrown::Runtime(Exception::bare("java.lang.NullPointerException"));
        assert_eq!(
            runtime.into_error("org.example.Parser").to_string(),
            "Parse error occurred : org.apache.tika.exception.TikaException: Unexpected \
             RuntimeException from org.example.Parser <- caused by \
             java.lang.NullPointerException"
        );
    }

    #[test]
    fn passes_tika_exceptions_on() {
        let cause = Exception::new("java.io.IOException", "Hit max recursion depth");
        let tika = Exception::new(
            "org.apache.tika.exception.TikaException",
            "Unable to extract",
        )
        .caused_by(cause);
        assert_eq!(
            Thrown::Tika(tika)
                .into_error("org.example.Parser")
                .to_string(),
            "Parse error occurred : org.apache.tika.exception.TikaException: Unable to extract \
             <- caused by java.io.IOException: Hit max recursion depth"
        );
    }

    #[test]
    fn reports_each_cause_an_exception_wraps() {
        let cause = Exception::new(
            "java.lang.IllegalArgumentException",
            "Input buffer too short",
        );
        let io = Thrown::Io(Exception::wrapping("java.io.IOException", cause));
        assert_eq!(
            io.into_error("org.example.Parser").to_string(),
            "Parse error occurred : org.apache.tika.exception.TikaException: TIKA-198: Illegal \
             IOException from org.example.Parser <- caused by java.io.IOException: \
             java.lang.IllegalArgumentException: Input buffer too short <- caused by \
             java.lang.IllegalArgumentException: Input buffer too short"
        );
    }
}
