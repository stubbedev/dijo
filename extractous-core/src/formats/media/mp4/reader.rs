//! metadata-extractor's big-endian sequential readers, `SequentialByteArrayReader` over a box's
//! payload and `StreamReader` over the file, and the `IOException`s they and Tika's box readers
//! throw, which Tika records as warnings.

/// An `IOException` thrown while reading the boxes, which Tika records and reads on past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Thrown {
    /// `EOFException("End of data reached.")`.
    EndOfData,
    /// `StreamReader.skip`'s `EOFException` for a skip past the end of the file.
    SkippedPastEnd { requested: u64, remained: u64 },
    /// An `IOException` with this message.
    Malformed(String),
}

impl Thrown {
    /// `class: message`, the first line of the stack trace Tika records.
    pub(super) fn exception(&self) -> String {
        match self {
            Self::EndOfData => "java.io.EOFException: End of data reached.".to_owned(),
            Self::SkippedPastEnd {
                requested,
                remained,
            } => format!(
                "java.io.EOFException: Unable to skip. Requested {requested} bytes but only \
                 {remained} remained."
            ),
            Self::Malformed(message) => format!("java.io.IOException: {message}"),
        }
    }
}

pub(super) type Read<T> = Result<T, Thrown>;

/// `EndianUtils.getUIntBE` and Tika's `readUInt32`: the big-endian unsigned 32 bits at `at`,
/// which the caller has checked lie within `bytes`.
pub(super) fn u32_at(bytes: &[u8], at: usize) -> u64 {
    let field: [u8; 4] = bytes[at..at + 4]
        .try_into()
        .expect("the range holds four bytes");
    u64::from(u32::from_be_bytes(field))
}

/// `SequentialByteArrayReader`: a payload read from its start, every read past its end an
/// `EOFException`.
pub(super) struct Bytes<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Bytes<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub(super) fn position(&self) -> u64 {
        self.at as u64
    }

    pub(super) fn available(&self) -> u64 {
        (self.bytes.len() - self.at) as u64
    }

    pub(super) fn take(&mut self, count: u64) -> Read<&'a [u8]> {
        let end = usize::try_from(count)
            .ok()
            .and_then(|count| self.at.checked_add(count))
            .filter(|&end| end <= self.bytes.len())
            .ok_or(Thrown::EndOfData)?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    pub(super) fn skip(&mut self, count: u64) -> Read<()> {
        self.take(count).map(|_| ())
    }

    fn array<const N: usize>(&mut self) -> Read<[u8; N]> {
        Ok(self
            .take(N as u64)?
            .try_into()
            .expect("took as many bytes as the array holds"))
    }

    pub(super) fn u8(&mut self) -> Read<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub(super) fn i8(&mut self) -> Read<i8> {
        Ok(i8::from_be_bytes(self.array()?))
    }

    pub(super) fn u16(&mut self) -> Read<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(super) fn i16(&mut self) -> Read<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    pub(super) fn u32(&mut self) -> Read<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(super) fn i32(&mut self) -> Read<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    pub(super) fn i64(&mut self) -> Read<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    /// `getNullTerminatedBytes(maxLength, false)`: the bytes up to a zero byte or `max_length`,
    /// read one by one.
    pub(super) fn null_terminated(&mut self, max_length: u64) -> Read<&'a [u8]> {
        let start = self.at;
        for _ in 0..max_length {
            if self.u8()? == 0 {
                return Ok(&self.bytes[start..self.at - 1]);
            }
        }
        Ok(&self.bytes[start..self.at])
    }
}

/// `StreamReader` over the whole file: the position it reports, which a read that fails part
/// way leaves behind what it consumed, and the bytes it consumed.
pub(super) struct Stream<'a> {
    input: &'a [u8],
    consumed: usize,
    position: u64,
}

impl<'a> Stream<'a> {
    pub(super) fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            consumed: 0,
            position: 0,
        }
    }

    pub(super) fn position(&self) -> u64 {
        self.position
    }

    /// `getByte`, which the number readers read byte by byte.
    fn byte(&mut self) -> Read<u8> {
        let byte = *self.input.get(self.consumed).ok_or(Thrown::EndOfData)?;
        self.consumed += 1;
        self.position += 1;
        Ok(byte)
    }

    pub(super) fn u32(&mut self) -> Read<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            value = value << 8 | u32::from(self.byte()?);
        }
        Ok(value)
    }

    pub(super) fn i64(&mut self) -> Read<i64> {
        let mut value = 0u64;
        for _ in 0..8 {
            value = value << 8 | u64::from(self.byte()?);
        }
        Ok(value as i64)
    }

    /// `getBytes`: past the end it consumes the rest without counting it.
    pub(super) fn take(&mut self, count: u64) -> Read<&'a [u8]> {
        let rest = &self.input[self.consumed..];
        let Some(taken) = usize::try_from(count)
            .ok()
            .and_then(|count| rest.get(..count))
        else {
            self.consumed = self.input.len();
            return Err(Thrown::EndOfData);
        };
        self.consumed += taken.len();
        self.position += count;
        Ok(taken)
    }

    /// `skip`: as much as is left, and an `EOFException` where that is less than asked.
    pub(super) fn skip(&mut self, count: u64) -> Read<()> {
        let remained = (self.input.len() - self.consumed) as u64;
        let skipped = count.min(remained);
        self.consumed += usize::try_from(skipped).expect("skipped within the input");
        self.position += skipped;
        if skipped == count {
            Ok(())
        } else {
            Err(Thrown::SkippedPastEnd {
                requested: count,
                remained: skipped,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_payloads_as_sequential_byte_array_reader_does() {
        let mut bytes = Bytes::new(b"\x00\x00\x01\x02ab\x00cd");
        assert_eq!(bytes.u32(), Ok(0x102));
        assert_eq!(bytes.null_terminated(4), Ok(&b"ab"[..]));
        assert_eq!(bytes.available(), 2);
        assert_eq!(bytes.null_terminated(2), Ok(&b"cd"[..]));
        assert_eq!(bytes.skip(1), Err(Thrown::EndOfData));
        assert_eq!(Bytes::new(b"ab").null_terminated(3), Err(Thrown::EndOfData));
    }

    #[test]
    fn reads_files_as_stream_reader_does() {
        let mut stream = Stream::new(b"\x00\x00\x00\x10ftypab");
        assert_eq!(stream.u32(), Ok(16));
        assert_eq!(stream.take(4), Ok(&b"ftyp"[..]));
        assert_eq!(stream.take(4), Err(Thrown::EndOfData));
        assert_eq!(stream.position(), 8);
        assert_eq!(stream.u32(), Err(Thrown::EndOfData));
        let mut stream = Stream::new(b"abc");
        assert_eq!(
            stream.skip(5),
            Err(Thrown::SkippedPastEnd {
                requested: 5,
                remained: 3
            })
        );
        assert_eq!(stream.position(), 3);
        assert_eq!(
            Thrown::SkippedPastEnd {
                requested: 100,
                remained: 30
            }
            .exception(),
            "java.io.EOFException: Unable to skip. Requested 100 bytes but only 30 remained."
        );
    }
}
