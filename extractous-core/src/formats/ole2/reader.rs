//! POI's `LittleEndianByteArrayInputStream` over a document's bytes, and the reads `IOUtils`
//! bounds by the largest length a record may have.

/// What POI throws where a read cannot go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Thrown {
    /// A runtime exception: a read past the end, a position outside the bytes, or a length
    /// below zero or above a record's largest.
    Runtime,
    /// `EOFException`: fewer bytes left than a length asks for.
    EndOfFile,
}

/// The bytes of a document, read from a position up to a count, as
/// `LittleEndianByteArrayInputStream` reads them.
pub(super) struct Reader<'a> {
    data: &'a [u8],
    position: i64,
    count: i64,
}

impl<'a> Reader<'a> {
    /// A reader at `position` of all of `data`.
    pub(super) fn new(data: &'a [u8], position: usize) -> Self {
        Reader {
            data,
            position: position as i64,
            count: data.len() as i64,
        }
    }

    pub(super) fn position(&self) -> i64 {
        self.position
    }

    /// `setReadIndex`, and `reset` to a position `getReadIndex` gave.
    pub(super) fn set_position(&mut self, position: i64) -> Result<(), Thrown> {
        if position < 0 || position >= self.count {
            return Err(Thrown::Runtime);
        }
        self.position = position;
        Ok(())
    }

    /// `limit`: reads end `size` bytes into the data, or at its end.
    pub(super) fn limit(&mut self, size: i64) {
        self.count = size.min(self.data.len() as i64);
    }

    /// `readFully`: the next `length` bytes, failing where fewer are left.
    pub(super) fn take(&mut self, length: usize) -> Result<&'a [u8], Thrown> {
        if length as i64 > self.count - self.position {
            return Err(Thrown::Runtime);
        }
        let start = self.position as usize;
        self.position += length as i64;
        Ok(&self.data[start..start + length])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Thrown> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }

    pub(super) fn read_i8(&mut self) -> Result<i8, Thrown> {
        Ok(i8::from_le_bytes(self.array()?))
    }

    pub(super) fn read_u8(&mut self) -> Result<u8, Thrown> {
        Ok(u8::from_le_bytes(self.array()?))
    }

    pub(super) fn read_i16(&mut self) -> Result<i16, Thrown> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    pub(super) fn read_u16(&mut self) -> Result<u16, Thrown> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(super) fn read_i32(&mut self) -> Result<i32, Thrown> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub(super) fn read_u32(&mut self) -> Result<u32, Thrown> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(super) fn read_i64(&mut self) -> Result<i64, Thrown> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    /// `ByteArrayInputStream.read(buffer, 0, length)`: up to `length` bytes, as many as are left.
    fn read_up_to(&mut self, length: usize) -> &'a [u8] {
        let left = (self.count - self.position).max(0) as usize;
        let length = length.min(left);
        let start = self.position.min(self.count).max(0) as usize;
        self.position += length as i64;
        &self.data[start..start + length]
    }

    /// `IOUtils.skipFully`: up to `length` bytes skipped, as many as are left.
    pub(super) fn skip(&mut self, length: usize) {
        self.read_up_to(length);
    }

    /// `TypedPropertyValue.skipPadding`: the zero bytes up to the next multiple of four of the
    /// position, stopping at any other byte.
    pub(super) fn skip_padding(&mut self) {
        let padding = (4 - (self.position & 3)) & 3;
        for _ in 0..padding {
            if self.position >= self.count || self.data[self.position as usize] != 0 {
                break;
            }
            self.position += 1;
        }
    }

    /// `IOUtils.toByteArray(stream, length, maxLength)`: `length` bytes, or with a negative
    /// length as many as are left up to `max_length`.
    pub(super) fn read_bounded(&mut self, length: i32, max_length: i32) -> Result<Vec<u8>, Thrown> {
        if length > max_length {
            return Err(Thrown::Runtime);
        }
        let wanted = if length >= 0 { length } else { max_length };
        let bytes = self.read_up_to(wanted as usize);
        if length >= 0 && bytes.len() < length as usize {
            return Err(Thrown::EndOfFile);
        }
        Ok(bytes.to_vec())
    }
}

/// `IOUtils.safelyAllocate`'s check: the length, if it lies between zero and `max_length`.
pub(super) fn allocation(length: i64, max_length: i32) -> Result<usize, Thrown> {
    if length < 0 || length > i64::from(max_length) {
        return Err(Thrown::Runtime);
    }
    Ok(length as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_as_poi_does() {
        let data = [1, 0, 0, 0, 0xFF, 0xFF, 0, 0, 7];
        let mut reader = Reader::new(&data, 0);
        assert_eq!(reader.read_i32(), Ok(1));
        assert_eq!(reader.read_i16(), Ok(-1));
        reader.skip_padding();
        assert_eq!(reader.position(), 8);
        assert_eq!(reader.read_i32(), Err(Thrown::Runtime));
        assert_eq!(reader.read_bounded(2, 10), Err(Thrown::EndOfFile));
        let mut reader = Reader::new(&data, 6);
        assert_eq!(reader.read_bounded(-1, 10), Ok(vec![0, 0, 7]));
        assert_eq!(reader.read_bounded(11, 10), Err(Thrown::Runtime));
        assert_eq!(allocation(-1, 10), Err(Thrown::Runtime));
        assert_eq!(allocation(10, 10), Ok(10));
    }

    #[test]
    fn stops_padding_at_other_bytes() {
        let data = [9, 0, 5, 0];
        let mut reader = Reader::new(&data, 1);
        reader.skip_padding();
        assert_eq!(reader.position(), 2);
    }
}
