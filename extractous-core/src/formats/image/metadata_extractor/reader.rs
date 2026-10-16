//! metadata-extractor's readers of bytes: `ByteArrayReader`, read at any index, and
//! `SequentialByteArrayReader`, read from a moving position, both in Motorola (big-endian) or
//! Intel byte order, failing as they fail on a read beyond the data.

use crate::text::charset::Charset;

use super::StringValue;

/// A read beyond the data (`BufferBoundsException` or `EOFException`), with its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bounds(pub(crate) String);

pub(crate) type Read<T> = Result<T, Bounds>;

/// `ByteArrayReader`: reads at an index, which may be computed out of range.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pub(crate) motorola: bool,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            motorola: true,
        }
    }

    pub(crate) fn len(&self) -> i64 {
        self.data.len() as i64
    }

    /// `validateIndex`.
    fn validate(&self, index: i64, count: i64) -> Read<usize> {
        if index < 0 {
            return Err(Bounds(format!(
                "Attempt to read from buffer using a negative index ({index})"
            )));
        }
        if count < 0 || index + count > self.len() {
            return Err(Bounds(format!(
                "Attempt to read from beyond end of underlying data source (requested index: {index}, requested count: {count}, max index: {})",
                self.len() - 1
            )));
        }
        Ok(index as usize)
    }

    fn array<const N: usize>(&self, index: i64) -> Read<[u8; N]> {
        let start = self.validate(index, N as i64)?;
        let mut bytes: [u8; N] = self.data[start..start + N]
            .try_into()
            .expect("the slice has N bytes");
        if !self.motorola {
            bytes.reverse();
        }
        Ok(bytes)
    }

    pub(crate) fn u8(&self, index: i64) -> Read<u8> {
        Ok(self.data[self.validate(index, 1)?])
    }

    pub(crate) fn i8(&self, index: i64) -> Read<i8> {
        Ok(self.u8(index)? as i8)
    }

    pub(crate) fn u16(&self, index: i64) -> Read<u16> {
        Ok(u16::from_be_bytes(self.array(index)?))
    }

    pub(crate) fn i16(&self, index: i64) -> Read<i16> {
        Ok(i16::from_be_bytes(self.array(index)?))
    }

    pub(crate) fn u32(&self, index: i64) -> Read<u32> {
        Ok(u32::from_be_bytes(self.array(index)?))
    }

    pub(crate) fn i32(&self, index: i64) -> Read<i32> {
        Ok(i32::from_be_bytes(self.array(index)?))
    }

    pub(crate) fn i64(&self, index: i64) -> Read<i64> {
        Ok(i64::from_be_bytes(self.array(index)?))
    }

    pub(crate) fn f32(&self, index: i64) -> Read<f32> {
        Ok(f32::from_bits(self.u32(index)?))
    }

    pub(crate) fn f64(&self, index: i64) -> Read<f64> {
        Ok(f64::from_bits(self.i64(index)? as u64))
    }

    /// `getS15Fixed16`: the integer part read unsigned, as metadata-extractor reads it.
    pub(crate) fn s15_fixed16(&self, index: i64) -> Read<f32> {
        let [high, low, fraction_high, fraction_low] = self.array::<4>(index)?;
        let (integer, fraction) = if self.motorola {
            (
                u32::from(high) << 8 | u32::from(low),
                u32::from(fraction_high) << 8 | u32::from(fraction_low),
            )
        } else {
            (
                u32::from(fraction_low) << 8 | u32::from(fraction_high),
                u32::from(low) << 8 | u32::from(high),
            )
        };
        Ok((f64::from(integer as f32) + f64::from(fraction) / 65536.0) as f32)
    }

    pub(crate) fn bytes(&self, index: i64, count: i64) -> Read<&'a [u8]> {
        let start = self.validate(index, count)?;
        Ok(&self.data[start..start + count as usize])
    }

    /// `getNullTerminatedBytes`: `max` bytes are read, and those before the first zero kept.
    pub(crate) fn null_terminated_bytes(&self, index: i64, max: i64) -> Read<&'a [u8]> {
        let buffer = self.bytes(index, max)?;
        let length = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        Ok(&buffer[..length])
    }

    pub(crate) fn null_terminated_string_value(
        &self,
        index: i64,
        max: i64,
        charset: Option<Charset>,
    ) -> Read<StringValue> {
        Ok(StringValue {
            bytes: self.null_terminated_bytes(index, max)?.to_vec(),
            charset,
        })
    }
}

/// `SequentialByteArrayReader`.
#[derive(Debug, Clone)]
pub(crate) struct Sequential<'a> {
    data: &'a [u8],
    position: usize,
    pub(crate) motorola: bool,
}

fn end_of_data() -> Bounds {
    Bounds("End of data reached.".to_owned())
}

impl<'a> Sequential<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            motorola: true,
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn available(&self) -> usize {
        self.data.len() - self.position
    }

    pub(crate) fn bytes(&mut self, count: usize) -> Read<&'a [u8]> {
        if self.position + count > self.data.len() {
            return Err(end_of_data());
        }
        let bytes = &self.data[self.position..self.position + count];
        self.position += count;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Read<[u8; N]> {
        let mut bytes: [u8; N] = self.bytes(N)?.try_into().expect("N bytes were read");
        if !self.motorola {
            bytes.reverse();
        }
        Ok(bytes)
    }

    pub(crate) fn skip(&mut self, count: usize) -> Read<()> {
        self.bytes(count).map(|_| ())
    }

    /// `trySkip`: false, at the end, when there are fewer bytes left.
    pub(crate) fn try_skip(&mut self, count: usize) -> bool {
        if self.position + count > self.data.len() {
            self.position = self.data.len();
            return false;
        }
        self.position += count;
        true
    }

    pub(crate) fn u8(&mut self) -> Read<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub(crate) fn i8(&mut self) -> Read<i8> {
        Ok(self.u8()? as i8)
    }

    pub(crate) fn u16(&mut self) -> Read<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Read<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    pub(crate) fn i32(&mut self) -> Read<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }
}
