//! Tika's `OneNoteDirectFileResource` over the bytes of a OneNote file: a position every read
//! moves, a single byte read as -1 past the end, and a buffer read that fails where the file
//! ends before it is full; and the exceptions the parser throws, by their Java names.

/// A Java exception the OneNote parser throws or catches, as `Throwable.toString()` names
/// it: its class and its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Thrown(pub(super) String);

/// A `TikaException` with `message`.
pub(super) fn tika(message: impl AsRef<str>) -> Thrown {
    Thrown(format!(
        "org.apache.tika.exception.TikaException: {}",
        message.as_ref()
    ))
}

/// A `TikaMemoryLimitException` with `message`.
pub(super) fn memory_limit(message: impl AsRef<str>) -> Thrown {
    Thrown(format!(
        "org.apache.tika.exception.TikaMemoryLimitException: {}",
        message.as_ref()
    ))
}

/// An `IOException` with `message`.
pub(super) fn io(message: &str) -> Thrown {
    Thrown(format!("java.io.IOException: {message}"))
}

/// The bytes of the file and the position of its `RandomAccessFile`.
pub(super) struct Dif<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Dif<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Dif { data, position: 0 }
    }

    pub(super) fn size(&self) -> i64 {
        self.data.len() as i64
    }

    pub(super) fn position(&self) -> i64 {
        self.position as i64
    }

    /// `position(long)`: a seek, which fails past the end.
    pub(super) fn seek(&mut self, to: i64) -> Result<(), Thrown> {
        if to > self.size() {
            return Err(io("requesting seek past end of stream"));
        }
        self.position = usize::try_from(to).map_err(|_| io("Negative seek offset"))?;
        Ok(())
    }

    /// `read()`: the next byte, or -1 at the end.
    pub(super) fn read_byte(&mut self) -> i32 {
        match self.data.get(self.position) {
            Some(&byte) => {
                self.position += 1;
                i32::from(byte)
            }
            None => -1,
        }
    }

    /// `read(ByteBuffer)` of a buffer of `length` bytes, which fails where the file ends
    /// before the buffer is full, having read to the end.
    pub(super) fn read(&mut self, length: usize) -> Result<&'a [u8], Thrown> {
        let start = self.position;
        let Some(bytes) = start
            .checked_add(length)
            .and_then(|end| self.data.get(start..end))
        else {
            self.position = self.data.len();
            return Err(io("End of stream reached earlier than expected"));
        };
        self.position += length;
        Ok(bytes)
    }
}
