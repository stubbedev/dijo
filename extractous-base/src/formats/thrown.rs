//! The exceptions a Tika parser throws out of its `parse`, as Tika's `CompositeParser` wraps
//! them, for the native parsers that fail where Tika's parsers throw.

use crate::errors::Error;

/// A Java exception: its class and its message, if it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exception {
    class: &'static str,
    message: Option<String>,
}

impl Exception {
    pub fn new(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            class,
            message: Some(message.into()),
        }
    }

    pub fn bare(class: &'static str) -> Self {
        Self {
            class,
            message: None,
        }
    }

    /// `Throwable.toString`: the class, then the message, if there is one.
    fn describe(&self) -> String {
        match &self.message {
            Some(message) => format!("{}: {message}", self.class),
            None => self.class.to_owned(),
        }
    }
}

/// What a Tika parser throws out of its `parse`, which `CompositeParser` wraps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thrown {
    /// An `IOException`, which it wraps as `TIKA-198`.
    Io(Exception),
    /// A `RuntimeException`.
    Runtime(Exception),
}

impl Thrown {
    /// The error Tika's path fails with when the parser `tika_name` throws this.
    pub fn into_error(self, tika_name: &str) -> Error {
        let (wrapped, cause) = match self {
            Self::Io(cause) => (
                format!("TIKA-198: Illegal IOException from {tika_name}"),
                cause,
            ),
            Self::Runtime(cause) => (
                format!("Unexpected RuntimeException from {tika_name}"),
                cause,
            ),
        };
        Error::ParseError(format!("{wrapped} <- caused by {}", cause.describe()))
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
            "TIKA-198: Illegal IOException from org.example.Parser <- caused by \
             java.io.EOFException: short"
        );
        let runtime = Thrown::Runtime(Exception::bare("java.lang.NullPointerException"));
        assert_eq!(
            runtime.into_error("org.example.Parser").to_string(),
            "Unexpected RuntimeException from org.example.Parser <- caused by \
             java.lang.NullPointerException"
        );
    }
}
