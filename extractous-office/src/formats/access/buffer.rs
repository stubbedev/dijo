//! Java's little-endian `ByteBuffer` as Jackcess reads pages through it: relative reads that
//! throw past the limit, absolute reads that throw outside it, and a position that cannot pass
//! it; and what Jackcess throws reading a database.

/// Why reading a database stops: an exception Jackcess throws, which ends the parse; something
/// the port leaves to Tika, which it then leaves the database to; or Office encryption, which
/// needs a password. The port reads no usage map but a table's owned pages, which hold its rows:
/// Jackcess reads the others too, and fails the parse on any it cannot read but a column's
/// (#122).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Thrown {
    IllegalState(String),
    Other(String),
    Unported(String),
    /// The database is encrypted with Office encryption.
    Encrypted,
}

impl Thrown {
    pub(super) fn illegal_state(message: impl Into<String>) -> Self {
        Thrown::IllegalState(message.into())
    }

    pub(super) fn other(message: impl Into<String>) -> Self {
        Thrown::Other(message.into())
    }

    /// What the port does not read.
    pub(super) fn unported(what: impl Into<String>) -> Self {
        Thrown::Unported(what.into())
    }

    /// Why the port leaves the database to Tika.
    pub(super) fn reason(self) -> String {
        match self {
            Thrown::IllegalState(message) => format!(
                "an Access database Jackcess throws reading: java.lang.IllegalStateException: \
                 {message}"
            ),
            Thrown::Other(message) => {
                format!("an Access database Jackcess throws reading: {message}")
            }
            Thrown::Unported(what) => format!("{what}, which the port does not read"),
            Thrown::Encrypted => {
                "an encrypted database, which the port does not decrypt".to_owned()
            }
        }
    }
}

fn index_out_of_bounds(index: i64, size: usize) -> Thrown {
    Thrown::other(format!(
        "java.lang.IndexOutOfBoundsException: index {index}, size {size}"
    ))
}

fn underflow() -> Thrown {
    Thrown::other("java.nio.BufferUnderflowException")
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
            _ => Err(Thrown::other(format!(
                "java.lang.IllegalArgumentException: newPosition > limit: ({position} > {})",
                self.limit
            ))),
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
            _ => Err(Thrown::other(format!(
                "java.lang.IllegalArgumentException: newLimit > capacity: ({limit} > {})",
                self.data.len()
            ))),
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
            _ => Err(index_out_of_bounds(index, self.limit)),
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
            .map_err(|_| Thrown::other(format!("java.lang.NegativeArraySizeException: {count}")))?;
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
