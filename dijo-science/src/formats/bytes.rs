//! In-place reads of a file's bytes, each failing with what it read where: the headers of a
//! scientific file are read from the mapped file as they lie, never copied whole.

use crate::errors::Error;
use crate::formats::Decline;

/// Why a native parser does not read a scientific file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unread {
    /// The file breaks its format, as the reason says where.
    Malformed(String),
    /// The file holds a construct the native reader does not read, as the reason names it.
    Unported(String),
}

impl From<Unread> for Decline<'_> {
    fn from(unread: Unread) -> Self {
        match unread {
            Unread::Malformed(reason) => Decline::Fails(Error::ParseError(reason)),
            Unread::Unported(reason) => Decline::Unread(reason),
        }
    }
}

/// What a read gives, or why it fails.
pub(crate) type Read<T> = Result<T, Unread>;

pub(crate) fn malformed(reason: impl Into<String>) -> Unread {
    Unread::Malformed(reason.into())
}

pub(crate) fn unported(reason: impl Into<String>) -> Unread {
    Unread::Unported(reason.into())
}

/// The order of the bytes of the numbers a format writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    Big,
    Little,
}

/// A file's bytes, read in place.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bytes<'a> {
    bytes: &'a [u8],
}

impl<'a> Bytes<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub(crate) fn len(self) -> u64 {
        self.bytes.len() as u64
    }

    /// The `length` bytes at `offset`, which hold `what`.
    pub(crate) fn slice(self, offset: u64, length: u64, what: &str) -> Read<&'a [u8]> {
        let end = offset.checked_add(length);
        let range = usize::try_from(offset)
            .ok()
            .zip(end.and_then(|end| usize::try_from(end).ok()));
        range
            .and_then(|(start, end)| self.bytes.get(start..end))
            .ok_or_else(|| {
                malformed(format!(
                    "{what} needs {length} bytes at offset {offset}, where the file of {} bytes \
                     ends",
                    self.bytes.len()
                ))
            })
    }

    /// The bytes from `offset` to the end, which hold `what`.
    pub(crate) fn from(self, offset: u64, what: &str) -> Read<&'a [u8]> {
        let length = self.len().checked_sub(offset).ok_or_else(|| {
            malformed(format!(
                "{what} starts at offset {offset}, past the end of the file of {} bytes",
                self.bytes.len()
            ))
        })?;
        self.slice(offset, length, what)
    }

    /// A cursor on the `length` bytes at `offset`, which hold `what`.
    pub(crate) fn cursor(
        self,
        offset: u64,
        length: u64,
        order: Order,
        what: &'static str,
    ) -> Read<Cursor<'a>> {
        Ok(Cursor::new(
            self.slice(offset, length, what)?,
            offset,
            order,
            what,
        ))
    }

    /// A cursor on the bytes from `offset` to the end, which hold `what`.
    pub(crate) fn cursor_from(
        self,
        offset: u64,
        order: Order,
        what: &'static str,
    ) -> Read<Cursor<'a>> {
        Ok(Cursor::new(self.from(offset, what)?, offset, order, what))
    }
}

/// A reading position in bytes that hold one structure of a file, `what`, which starts at
/// `origin` in the file.
#[derive(Debug, Clone)]
pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
    origin: u64,
    order: Order,
    what: &'static str,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(bytes: &'a [u8], origin: u64, order: Order, what: &'static str) -> Self {
        Self {
            bytes,
            at: 0,
            origin,
            order,
            what,
        }
    }

    /// Where the cursor stands, from the start of its bytes.
    pub(crate) fn position(&self) -> usize {
        self.at
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    /// The bytes not read yet.
    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }

    /// Moves to `position`, from the start of its bytes.
    pub(crate) fn seek(&mut self, position: usize) -> Read<()> {
        if position > self.bytes.len() {
            return Err(self.short(position - self.at));
        }
        self.at = position;
        Ok(())
    }

    fn short(&self, needed: usize) -> Unread {
        malformed(format!(
            "{} at offset {} needs {needed} more bytes at offset {}, where it holds {}",
            self.what,
            self.origin,
            self.origin + self.at as u64,
            self.remaining()
        ))
    }

    /// The next `length` bytes.
    pub(crate) fn take(&mut self, length: usize) -> Read<&'a [u8]> {
        let taken = self
            .bytes
            .get(self.at..self.at.saturating_add(length))
            .ok_or_else(|| self.short(length))?;
        self.at += length;
        Ok(taken)
    }

    pub(crate) fn skip(&mut self, length: usize) -> Read<()> {
        self.take(length).map(|_| ())
    }

    /// Skips to the next multiple of `alignment` from the start of its bytes.
    pub(crate) fn align(&mut self, alignment: usize) -> Read<()> {
        let padding = (alignment - self.at % alignment) % alignment;
        self.skip(padding)
    }

    pub(crate) fn u8(&mut self) -> Read<u8> {
        Ok(self.take(1)?[0])
    }

    /// An unsigned number of `size` bytes, up to 8, in the cursor's order.
    pub(crate) fn uint(&mut self, size: usize) -> Read<u64> {
        let bytes = self.take(size)?;
        Ok(uint(bytes, self.order))
    }

    pub(crate) fn u16(&mut self) -> Read<u16> {
        Ok(self.uint(2)? as u16)
    }

    pub(crate) fn u32(&mut self) -> Read<u32> {
        Ok(self.uint(4)? as u32)
    }

    /// A number of `size` bytes that must fit in a `usize`, for a count or a length.
    pub(crate) fn size(&mut self, size: usize, what: &str) -> Read<usize> {
        let value = self.uint(size)?;
        usize::try_from(value).map_err(|_| {
            malformed(format!(
                "{} at offset {}: {what} of {value} is beyond what this machine addresses",
                self.what, self.origin
            ))
        })
    }

    /// Fails as malformed `what` in the cursor's structure, with `reason`.
    pub(crate) fn malformed(&self, reason: impl std::fmt::Display) -> Unread {
        malformed(format!(
            "{} at offset {}: {reason} (at offset {})",
            self.what,
            self.origin,
            self.origin + self.at as u64
        ))
    }
}

/// An unsigned number of up to 8 `bytes` in `order`.
pub(crate) fn uint(bytes: &[u8], order: Order) -> u64 {
    match order {
        Order::Big => bytes
            .iter()
            .fold(0, |value, &byte| (value << 8) | u64::from(byte)),
        Order::Little => bytes
            .iter()
            .rev()
            .fold(0, |value, &byte| (value << 8) | u64::from(byte)),
    }
}

/// The text of `bytes` up to the first NUL, as UTF-8, each malformed sequence replaced by one
/// U+FFFD.
pub(crate) fn text_to_nul(bytes: &[u8]) -> String {
    let end = memchr::memchr(0, bytes).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_in_either_order_and_fails_where_bytes_run_out() {
        let bytes = [0x01, 0x02, 0x03, 0x04, 0x05];
        let mut big = Cursor::new(&bytes, 100, Order::Big, "a test structure");
        assert_eq!(big.u16().unwrap(), 0x0102);
        assert_eq!(big.u16().unwrap(), 0x0304);
        let short = big.u16().unwrap_err();
        assert_eq!(
            short,
            Unread::Malformed(
                "a test structure at offset 100 needs 2 more bytes at offset 104, where it holds 1"
                    .to_owned()
            )
        );
        let mut little = Cursor::new(&bytes, 0, Order::Little, "a test structure");
        assert_eq!(little.u32().unwrap(), 0x0403_0201);
        assert_eq!(text_to_nul(b"name\0rest"), "name");
        assert!(Bytes::new(&bytes).slice(4, 2, "a field").is_err());
        assert_eq!(Bytes::new(&bytes).slice(3, 2, "a field").unwrap(), &[4, 5]);
    }
}
