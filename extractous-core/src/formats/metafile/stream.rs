//! POI's `LittleEndianInputStream` over the bytes of a metafile, as POI's metafile readers read
//! it through a `TikaInputStream`: each read takes as many bytes as are left, a short read of a
//! value fails, and the read index counts the bytes read, which a mark and its reset restore.
//! Its reads fail with the exception POI throws, which fails the whole picture.

use crate::formats::poi::{thrown, Read, Thrown};

/// `IOUtils.DEFAULT_BUFFER_SIZE` and `UnsynchronizedByteArrayOutputStream`'s chunk: what
/// `IOUtils.toByteArray` and `IOUtils.copy` read at a time.
const BUFFER: usize = 4096;

/// `LittleEndianInputStream` over a `TikaInputStream` of the metafile, or over the stream a
/// record's bitmap is copied into.
#[derive(Debug, Clone)]
pub(super) struct Stream<'a> {
    data: &'a [u8],
    position: usize,
    /// `getReadIndex`: the bytes read, which a skip by `InputStream.skip` does not count.
    read_index: i64,
    /// The position and read index a `reset` returns to.
    mark: Option<(usize, i64)>,
}

impl<'a> Stream<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            read_index: 0,
            mark: None,
        }
    }

    pub(super) fn read_index(&self) -> i64 {
        self.read_index
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.position
    }

    /// `read(byte[], int, int)`: up to `length` bytes, none at the end, where Java reads -1.
    pub(super) fn read_some(&mut self, length: usize) -> Option<&'a [u8]> {
        if length > 0 && self.remaining() == 0 {
            return None;
        }
        let taken = length.min(self.remaining());
        let read = &self.data[self.position..self.position + taken];
        self.position += taken;
        self.read_index += taken as i64;
        Some(read)
    }

    /// `readFully`: exactly `length` bytes, or the end-of-file failure.
    pub(super) fn bytes(&mut self, length: usize) -> Read<&'a [u8]> {
        let read = self.read_some(length).unwrap_or_default();
        if read.len() != length {
            return thrown("IllegalStateException: Unexpected end-of-file");
        }
        Ok(read)
    }

    fn array<const N: usize>(&mut self) -> Read<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("N bytes"))
    }

    pub(super) fn u8(&mut self) -> Read<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub(super) fn i8(&mut self) -> Read<i8> {
        Ok(self.u8()? as i8)
    }

    pub(super) fn u16(&mut self) -> Read<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(super) fn i16(&mut self) -> Read<i16> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    pub(super) fn i32(&mut self) -> Read<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    /// `readUInt`.
    pub(super) fn u32(&mut self) -> Read<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// `readFloat`, widened to the `double` POI keeps it in.
    pub(super) fn f32(&mut self) -> Read<f64> {
        Ok(f64::from(f32::from_le_bytes(self.array()?)))
    }

    /// `LittleEndianInputStream.skipFully(int)`: exactly `length` bytes skipped, or a failure,
    /// also for a negative `length`.
    pub(super) fn skip_fully(&mut self, length: i32) -> Read<()> {
        if length == 0 {
            return Ok(());
        }
        if length < 0 {
            return thrown(format!(
                "IllegalArgumentException: Skip count must be non-negative, actual: {length}"
            ));
        }
        self.bytes(length as usize)
            .map(|_| ())
            .map_err(|_| Thrown::new("IllegalStateException: Unexpected end-of-file"))
    }

    /// `IOUtils.skipFully(stream, length)`: the bytes skipped, as many as there are up to
    /// `length`, or -1 where none are left.
    pub(super) fn skip_up_to(&mut self, length: i64) -> Read<i64> {
        if length < 0 {
            return thrown(format!(
                "IllegalArgumentException: Skip count must be non-negative, actual: {length}"
            ));
        }
        if length == 0 {
            return Ok(0);
        }
        let skipped = self
            .read_some(usize::try_from(length).unwrap_or(usize::MAX))
            .map_or(0, <[u8]>::len);
        Ok(if skipped == 0 { -1 } else { skipped as i64 })
    }

    /// `InputStream.skip`: as many bytes as there are up to `length`, which the read index does
    /// not count.
    pub(super) fn skip(&mut self, length: i64) {
        if length <= 0 {
            return;
        }
        let skipped = usize::try_from(length)
            .unwrap_or(usize::MAX)
            .min(self.remaining());
        self.position += skipped;
    }

    pub(super) fn mark(&mut self) {
        self.mark = Some((self.position, self.read_index));
    }

    pub(super) fn reset(&mut self) -> Read<()> {
        let (position, read_index) = self
            .mark
            .take()
            .ok_or_else(|| Thrown::new("IOException: Resetting to invalid mark"))?;
        self.position = position;
        self.read_index = read_index;
        Ok(())
    }

    /// `IOUtils.toByteArray(stream, length, maxLength)`: `length` bytes, at most `max`, or for
    /// a negative `length` as many as there are up to `max`.
    pub(super) fn byte_array(&mut self, length: i32, max: i32) -> Read<Vec<u8>> {
        if length > max {
            return thrown(format!(
                "RecordFormatException: Tried to allocate an array of length {length}, but the \
                 maximum length for this record type is {max}"
            ));
        }
        let known = length != i32::MAX;
        let wanted = if known && length >= 0 {
            length.min(max)
        } else {
            max
        } as usize;
        let mut read = Vec::new();
        let mut last = Some(0);
        while read.len() < wanted && last.is_some() {
            let chunk = self.read_some(BUFFER.min(wanted - read.len()));
            last = chunk.map(<[u8]>::len);
            read.extend_from_slice(chunk.unwrap_or_default());
        }
        if !known && last.is_some() && self.read_some(1).is_some_and(|byte| !byte.is_empty()) {
            return thrown(format!(
                "RecordFormatException: Tried to read data but the maximum length for this \
                 record type is {max}"
            ));
        }
        if length >= 0 && wanted != i32::MAX as usize && read.len() < wanted {
            return thrown(format!(
                "EOFException: unexpected EOF - expected len: {wanted} - actual len: {}",
                read.len()
            ));
        }
        Ok(read)
    }

    /// `IOUtils.copy(stream, out, limit)`: the bytes copied, up to `limit`, all that are left
    /// for -1, and one buffer's worth for any other negative `limit`.
    pub(super) fn copy(&mut self, limit: i64, out: &mut Vec<u8>) {
        let mut total = 0i64;
        loop {
            let todo = if limit < 0 {
                BUFFER as i64
            } else {
                (limit - total).min(BUFFER as i64)
            };
            if todo <= 0 {
                return;
            }
            let Some(chunk) = self.read_some(todo as usize) else {
                return;
            };
            out.extend_from_slice(chunk);
            total += chunk.len() as i64;
            if limit != -1 && total >= limit {
                return;
            }
        }
    }
}

/// `IOUtils.safelyAllocate(length, max)`: whether an array of `length` bytes may be allocated,
/// as a length.
pub(super) fn safely_allocate(length: i64, max: i32) -> Read<usize> {
    if length < 0 {
        return thrown(format!(
            "RecordFormatException: Can't allocate an array of length < 0, but had {length} and \
             {max}"
        ));
    }
    if length > i64::from(i32::MAX) {
        return thrown(format!(
            "RecordFormatException: Can't allocate an array > {}",
            i32::MAX
        ));
    }
    if length > i64::from(max) {
        return thrown(format!(
            "RecordFormatException: Tried to allocate an array of length {length}, but the \
             maximum length for this record type is {max}"
        ));
    }
    Ok(length as usize)
}

/// `Math.toIntExact`.
pub(super) fn to_int_exact(value: i64) -> Read<i32> {
    i32::try_from(value).map_err(|_| Thrown::new("ArithmeticException: integer overflow"))
}

/// A Java array of `length` elements, which a negative length fails to create.
pub(super) fn array_length(length: i64) -> Read<usize> {
    usize::try_from(length)
        .map_err(|_| Thrown::new(format!("NegativeArraySizeException: {length}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_values_and_fails_short_of_one() {
        let mut stream = Stream::new(&[1, 0, 0, 0, 0xFE, 0xFF, 7]);
        assert_eq!(stream.i32(), Ok(1));
        assert_eq!(stream.i16(), Ok(-2));
        assert_eq!(stream.read_index(), 6);
        assert!(stream.u16().is_err());
    }

    #[test]
    fn a_reset_returns_to_the_mark_with_its_read_index() {
        let mut stream = Stream::new(&[1, 2, 3, 4]);
        stream.u8().unwrap();
        stream.mark();
        stream.u16().unwrap();
        stream.reset().unwrap();
        assert_eq!(stream.read_index(), 1);
        assert_eq!(stream.u8(), Ok(2));
        assert!(stream.reset().is_err());
    }

    #[test]
    fn skips_as_poi_does() {
        let mut stream = Stream::new(&[0; 4]);
        assert!(stream.skip_fully(-1).is_err());
        assert!(stream.skip_fully(5).is_err());
        let mut stream = Stream::new(&[0; 4]);
        assert_eq!(stream.skip_up_to(3), Ok(3));
        assert_eq!(stream.skip_up_to(3), Ok(1));
        assert_eq!(stream.skip_up_to(3), Ok(-1));
        let mut stream = Stream::new(&[0; 4]);
        stream.skip(2);
        assert_eq!(stream.read_index(), 0);
        assert_eq!(stream.bytes(2).map(<[u8]>::len), Ok(2));
    }

    #[test]
    fn copies_to_a_byte_array_as_poi_does() {
        let data: Vec<u8> = (0..10).collect();
        assert_eq!(Stream::new(&data).byte_array(4, 100), Ok(vec![0, 1, 2, 3]));
        assert!(Stream::new(&data).byte_array(11, 100).is_err());
        assert!(Stream::new(&data).byte_array(101, 100).is_err());
        assert_eq!(Stream::new(&data).byte_array(-1, 4), Ok(vec![0, 1, 2, 3]));
        assert_eq!(Stream::new(&data).byte_array(-1, 100), Ok(data.clone()));
        let mut copied = Vec::new();
        Stream::new(&data).copy(3, &mut copied);
        assert_eq!(copied, vec![0, 1, 2]);
        copied.clear();
        Stream::new(&data).copy(-1, &mut copied);
        assert_eq!(copied, data);
    }

    #[test]
    fn allocates_as_poi_allows() {
        assert_eq!(safely_allocate(10, 10), Ok(10));
        assert!(safely_allocate(11, 10).is_err());
        assert!(safely_allocate(-1, 10).is_err());
        assert!(array_length(-1).is_err());
        assert!(to_int_exact(i64::from(i32::MAX) + 1).is_err());
    }
}
