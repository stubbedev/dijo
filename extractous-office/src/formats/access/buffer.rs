//! Java's little-endian `ByteBuffer` as Jackcess reads pages through it: relative reads that
//! throw past the limit, absolute reads that throw outside it, and a position that cannot pass
//! it; and what Jackcess throws reading a database.

use crate::formats::thrown;

pub(super) const IO_EXCEPTION: &str = "java.io.IOException";
pub(super) const ILLEGAL_ARGUMENT: &str = "java.lang.IllegalArgumentException";
pub(super) const CLASS_CAST: &str = "java.lang.ClassCastException";
pub(super) const NEGATIVE_ARRAY_SIZE: &str = "java.lang.NegativeArraySizeException";
pub(super) const ARRAY_INDEX_OUT_OF_BOUNDS: &str = "java.lang.ArrayIndexOutOfBoundsException";
pub(super) const BUFFER_UNDERFLOW: &str = "java.nio.BufferUnderflowException";
pub(super) const INDEX_OUT_OF_BOUNDS: &str = "java.lang.IndexOutOfBoundsException";
const ILLEGAL_STATE: &str = "java.lang.IllegalStateException";
const NULL_POINTER: &str = "java.lang.NullPointerException";

/// The branch of Java's exceptions an exception is of, which decides how Tika's
/// `JackcessParser` rethrows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// An `IOException`.
    Io,
    /// An `IndexOutOfBoundsException`.
    IndexOutOfBounds,
    /// An `IllegalStateException`.
    IllegalState,
    /// Any other `RuntimeException`.
    Runtime,
}

/// What Jackcess's `withErrorContext` names after an exception's message: the database, and the
/// table, column or index of it, or the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Context {
    Database,
    Table(String),
    Column { table: String, column: String },
    Index { table: String, index: String },
    Query(String),
}

/// An exception Jackcess, or the JVM under it, throws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Exception {
    pub(super) kind: Kind,
    class: &'static str,
    message: Option<String>,
    context: Option<Context>,
}

impl Exception {
    /// The class of the exception.
    pub(super) fn class(&self) -> &'static str {
        self.class
    }

    /// The message, with its context in the database Jackcess names `database`.
    pub(super) fn message(&self, database: &str) -> Option<String> {
        let message = self.message.as_deref()?;
        Some(match &self.context {
            None => message.to_owned(),
            Some(Context::Database) => format!("{message} (Db={database})"),
            Some(Context::Table(table)) => format!("{message} (Db={database};Table={table})"),
            Some(Context::Column { table, column }) => {
                format!("{message} (Db={database};Table={table};Column={column})")
            }
            Some(Context::Index { table, index }) => {
                format!("{message} (Db={database};Table={table};Index={index})")
            }
            Some(Context::Query(query)) => format!("{message} (Query: {query})"),
        })
    }

    /// The exception as Java's, in the database Jackcess names `database`.
    pub(super) fn java(&self, database: &str) -> thrown::Exception {
        match self.message(database) {
            Some(message) => thrown::Exception::new(self.class, message),
            None => thrown::Exception::bare(self.class),
        }
    }
}

/// Why reading a database stops: an exception Jackcess throws, which ends the parse, something the
/// port does not read, which it then leaves the database unread for, or what the port cannot read
/// right where Jackcess reads it wrongly, which fails the parse. The port reads no usage map but a
/// table's owned pages, which hold its rows: Jackcess reads the others too, and fails the parse on
/// any it cannot read but a column's (#122).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Thrown {
    Exception(Exception),
    Unported(String),
    Unreadable(String),
}

impl Thrown {
    fn exception(kind: Kind, class: &'static str, message: Option<String>) -> Self {
        Thrown::Exception(Exception {
            kind,
            class,
            message,
            context: None,
        })
    }

    /// A `java.io.IOException`.
    pub(super) fn io(message: impl Into<String>) -> Self {
        Self::exception(Kind::Io, IO_EXCEPTION, Some(message.into()))
    }

    /// A `java.lang.IllegalStateException`, or an exception of `class` that extends it.
    pub(super) fn illegal_state_of(class: &'static str, message: impl Into<String>) -> Self {
        Self::exception(Kind::IllegalState, class, Some(message.into()))
    }

    pub(super) fn illegal_state(message: impl Into<String>) -> Self {
        Self::illegal_state_of(ILLEGAL_STATE, message)
    }

    /// An `IndexOutOfBoundsException` of `class`, with its message if it has one.
    pub(super) fn index_out_of_bounds(class: &'static str, message: Option<String>) -> Self {
        Self::exception(Kind::IndexOutOfBounds, class, message)
    }

    /// Another `RuntimeException`, of `class`.
    pub(super) fn runtime(class: &'static str, message: impl Into<String>) -> Self {
        Self::exception(Kind::Runtime, class, Some(message.into()))
    }

    /// Another `RuntimeException`, of `class`, without a message.
    pub(super) fn bare(class: &'static str) -> Self {
        Self::exception(Kind::Runtime, class, None)
    }

    /// The `NullPointerException` the JVM throws dereferencing null.
    pub(super) fn null_pointer() -> Self {
        Self::bare(NULL_POINTER)
    }

    /// What the port does not read.
    pub(super) fn unported(what: impl Into<String>) -> Self {
        Thrown::Unported(what.into())
    }

    /// The exception with the context Jackcess names after its message.
    pub(super) fn in_context(self, context: Context) -> Self {
        match self {
            Thrown::Exception(exception) => Thrown::Exception(Exception {
                context: Some(context),
                ..exception
            }),
            other @ (Thrown::Unported(_) | Thrown::Unreadable(_)) => other,
        }
    }

    /// The exception with `context`, where it is an `IOException` without one: Jackcess names
    /// the context of every `IOException` a column throws reading its value, the JVM's
    /// exceptions without one.
    pub(super) fn io_in_context(self, context: Context) -> Self {
        match self {
            Thrown::Exception(
                exception @ Exception {
                    kind: Kind::Io,
                    context: None,
                    ..
                },
            ) => Thrown::Exception(Exception {
                context: Some(context),
                ..exception
            }),
            other => other,
        }
    }
}

fn index_out_of_bounds() -> Thrown {
    Thrown::index_out_of_bounds(INDEX_OUT_OF_BOUNDS, None)
}

fn underflow() -> Thrown {
    Thrown::bare(BUFFER_UNDERFLOW)
}

/// A `ByteBuffer` over borrowed bytes, its capacity theirs.
#[derive(Debug, Clone, Copy)]
pub(super) struct Buffer<'a> {
    data: &'a [u8],
    position: usize,
    limit: usize,
}

impl<'a> Buffer<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            limit: data.len(),
        }
    }

    pub(super) fn bytes(&self) -> &'a [u8] {
        self.data
    }

    pub(super) fn position(&self) -> usize {
        self.position
    }

    pub(super) fn limit(&self) -> usize {
        self.limit
    }

    pub(super) fn remaining(&self) -> usize {
        self.limit - self.position
    }

    pub(super) fn has_remaining(&self) -> bool {
        self.position < self.limit
    }

    /// `position(newPosition)`, which throws outside `0..=limit`.
    pub(super) fn set_position(&mut self, position: i64) -> Result<(), Thrown> {
        match usize::try_from(position) {
            Ok(position) if position <= self.limit => {
                self.position = position;
                Ok(())
            }
            _ if position < 0 => Err(Thrown::runtime(
                ILLEGAL_ARGUMENT,
                format!("newPosition < 0: ({position} < 0)"),
            )),
            _ => Err(Thrown::runtime(
                ILLEGAL_ARGUMENT,
                format!("newPosition > limit: ({position} > {})", self.limit),
            )),
        }
    }

    /// `limit(newLimit)`, which throws outside `0..=capacity` and pulls the position back to it.
    pub(super) fn set_limit(&mut self, limit: i64) -> Result<(), Thrown> {
        match usize::try_from(limit) {
            Ok(limit) if limit <= self.data.len() => {
                self.limit = limit;
                self.position = self.position.min(limit);
                Ok(())
            }
            _ if limit < 0 => Err(Thrown::runtime(
                ILLEGAL_ARGUMENT,
                format!("newLimit < 0: ({limit} < 0)"),
            )),
            _ => Err(Thrown::runtime(
                ILLEGAL_ARGUMENT,
                format!("newLimit > capacity: ({limit} > {})", self.data.len()),
            )),
        }
    }

    /// `ByteUtil.forward`.
    pub(super) fn forward(&mut self, count: usize) -> Result<(), Thrown> {
        self.set_position((self.position + count) as i64)
    }

    /// The `size` bytes at `index`, as an absolute read takes them.
    fn at(&self, index: i64, size: usize) -> Result<&'a [u8], Thrown> {
        match usize::try_from(index) {
            Ok(start) if start + size <= self.limit => Ok(&self.data[start..start + size]),
            _ => Err(index_out_of_bounds()),
        }
    }

    pub(super) fn u8_at(&self, index: i64) -> Result<u8, Thrown> {
        Ok(self.at(index, 1)?[0])
    }

    pub(super) fn i16_at(&self, index: i64) -> Result<i16, Thrown> {
        Ok(i16::from_le_bytes(
            self.at(index, 2)?.try_into().expect("two bytes"),
        ))
    }

    pub(super) fn i32_at(&self, index: i64) -> Result<i32, Thrown> {
        Ok(i32::from_le_bytes(
            self.at(index, 4)?.try_into().expect("four bytes"),
        ))
    }

    /// `ByteUtil.get3ByteInt` at `index`.
    pub(super) fn u24_at(&self, index: i64) -> Result<i32, Thrown> {
        let bytes = self.at(index, 3)?;
        Ok(i32::from(bytes[0]) | i32::from(bytes[1]) << 8 | i32::from(bytes[2]) << 16)
    }

    /// The next `count` bytes, as `ByteUtil.getBytes` takes them: a negative count throws as
    /// `new byte[count]` does, too many as the read does.
    pub(super) fn take(&mut self, count: i64) -> Result<&'a [u8], Thrown> {
        let count = usize::try_from(count)
            .map_err(|_| Thrown::runtime(NEGATIVE_ARRAY_SIZE, count.to_string()))?;
        if count > self.remaining() {
            return Err(underflow());
        }
        let bytes = &self.data[self.position..self.position + count];
        self.position += count;
        Ok(bytes)
    }

    pub(super) fn u8(&mut self) -> Result<u8, Thrown> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn i16(&mut self) -> Result<i16, Thrown> {
        Ok(i16::from_le_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }

    pub(super) fn i32(&mut self) -> Result<i32, Thrown> {
        Ok(i32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    /// `ByteUtil.get3ByteInt`.
    pub(super) fn u24(&mut self) -> Result<i32, Thrown> {
        let value = self.u24_at(self.position as i64).map_err(|_| underflow())?;
        self.position += 3;
        Ok(value)
    }

    /// `ByteUtil.getUnsignedVarInt` of `size` bytes at `index`.
    pub(super) fn unsigned_at(&self, index: i64, size: usize) -> Result<i32, Thrown> {
        match size {
            1 => Ok(i32::from(self.u8_at(index)?)),
            2 => Ok(i32::from(self.i16_at(index)? as u16)),
            _ => unreachable!("Jackcess reads names and column counts of one or two bytes"),
        }
    }

    /// `ByteUtil.getUnsignedVarInt` of `size` bytes.
    pub(super) fn unsigned(&mut self, size: usize) -> Result<i32, Thrown> {
        let value = self.unsigned_at(self.position as i64, size)?;
        self.position += size;
        Ok(value)
    }
}
