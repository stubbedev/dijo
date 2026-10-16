//! POI's `RecordInputStream` over a workbook stream: BIFF records of a type and a length, read
//! by records that run on into the `CONTINUE` records after them where POI's reads do, and the
//! exceptions POI throws on records that end early or late.

use std::borrow::Cow;

use crate::formats::poi::{thrown, Read, Thrown};

/// The type of a `CONTINUE` record.
pub(super) const CONTINUE: u16 = 0x003C;
/// The longest record POI reads.
const MAX_RECORD_DATA_SIZE: i64 = 8224;
/// `HSSFWorkbook`'s largest record.
pub(super) const MAX_RECORD_LENGTH: i64 = 100_000;
/// `StringUtil`'s largest record.
pub(super) const MAX_STRING_LENGTH: i64 = 10_000_000;

/// The records of a workbook stream, as `RecordInputStream` reads them.
pub(super) struct RecordStream<'a> {
    data: Cow<'a, [u8]>,
    position: usize,
    sid: u16,
    /// The length of the current record, `None` before it is read.
    length: Option<i64>,
    offset: i64,
    /// The type of the next record, `None` at the end of the stream.
    next_sid: Option<u16>,
}

impl<'a> RecordStream<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        let mut stream = RecordStream {
            data: Cow::Borrowed(data),
            position: 0,
            sid: 0,
            length: None,
            offset: 0,
            next_sid: None,
        };
        stream.next_sid = stream.read_next_sid();
        stream
    }

    /// The type of the current record.
    pub(super) fn sid(&self) -> u16 {
        self.sid
    }

    /// Where the stream is: past the type of the next record once `has_next_record` read it.
    pub(super) fn position(&self) -> usize {
        self.position
    }

    /// The stream's bytes, to decrypt in place: a copy of those it was made with.
    pub(super) fn data_mut(&mut self) -> &mut [u8] {
        self.data.to_mut()
    }

    /// `getNextSid`.
    pub(super) fn next_sid(&self) -> Option<u16> {
        self.next_sid
    }

    fn read_next_sid(&mut self) -> Option<u16> {
        if self.data.len() - self.position < 4 {
            return None;
        }
        let sid = u16::from_le_bytes([self.data[self.position], self.data[self.position + 1]]);
        self.position += 2;
        self.length = None;
        Some(sid)
    }

    /// `hasNextRecord`: a record left partly read fails.
    pub(super) fn has_next_record(&mut self) -> Read<bool> {
        if let Some(length) = self.length {
            if length != self.offset {
                return thrown(format!(
                    "Initialisation of record 0x{:X} left {} bytes remaining still to be read",
                    self.sid,
                    length - self.offset
                ));
            }
            self.next_sid = self.read_next_sid();
        }
        Ok(self.next_sid.is_some())
    }

    /// `nextRecord`: moves to the next record, whose type `has_next_record` read.
    pub(super) fn next_record(&mut self) -> Read<()> {
        let Some(sid) = self.next_sid else {
            return thrown("EOF - next record not available");
        };
        if self.length.is_some() {
            return thrown("Cannot call nextRecord() without checking hasNextRecord() first");
        }
        self.sid = sid;
        self.offset = 0;
        let length = i64::from(u16::from_le_bytes(self.raw::<2>()?));
        self.length = Some(length);
        if length > MAX_RECORD_DATA_SIZE {
            return thrown(format!(
                "The content of an excel record cannot exceed 8224 bytes, but had: {length}"
            ));
        }
        Ok(())
    }

    /// The bytes after the position, read past record boundaries as the underlying stream is.
    fn raw<const N: usize>(&mut self) -> Read<[u8; N]> {
        let bytes = self
            .data
            .get(self.position..self.position + N)
            .ok_or_else(|| Thrown("Unexpected end-of-file".to_owned()))?;
        self.position += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    /// `remaining`.
    pub(super) fn remaining(&self) -> i64 {
        self.length.map_or(0, |length| length - self.offset)
    }

    fn is_continue_next(&mut self) -> Read<bool> {
        if self.length.is_some_and(|length| length != self.offset) {
            return thrown("Should never be called before end of current record");
        }
        Ok(self.has_next_record()? && self.next_sid == Some(CONTINUE))
    }

    fn check_record_position(&mut self, required: i64) -> Read<()> {
        let available = self.remaining();
        if available >= required {
            return Ok(());
        }
        if available == 0 && self.is_continue_next()? {
            return self.next_record();
        }
        thrown(format!(
            "Not enough data ({available}) to read requested ({required}) bytes"
        ))
    }

    fn read<const N: usize>(&mut self) -> Read<[u8; N]> {
        self.check_record_position(N as i64)?;
        self.offset += N as i64;
        self.raw::<N>()
    }

    pub(super) fn read_u8(&mut self) -> Read<u8> {
        Ok(self.read::<1>()?[0])
    }

    pub(super) fn read_i16(&mut self) -> Read<i16> {
        Ok(i16::from_le_bytes(self.read()?))
    }

    pub(super) fn read_u16(&mut self) -> Read<u16> {
        Ok(u16::from_le_bytes(self.read()?))
    }

    pub(super) fn read_i32(&mut self) -> Read<i32> {
        Ok(i32::from_le_bytes(self.read()?))
    }

    pub(super) fn read_i64(&mut self) -> Read<i64> {
        Ok(i64::from_le_bytes(self.read()?))
    }

    pub(super) fn read_f64(&mut self) -> Read<f64> {
        Ok(f64::from_bits(self.read_i64()? as u64))
    }

    /// `readFully`: bytes read on into the following records.
    pub(super) fn read_fully(&mut self, length: usize) -> Read<Vec<u8>> {
        let mut bytes = Vec::with_capacity(length);
        let mut left = length as i64;
        while left > 0 {
            let mut chunk = self.remaining().min(left);
            if chunk == 0 {
                if !self.has_next_record()? {
                    return thrown(format!(
                        "Can't read the remaining {left} bytes of the requested {length} bytes. \
                         No further record exists."
                    ));
                }
                self.next_record()?;
                chunk = self.remaining().min(left);
                if chunk <= 0 {
                    return thrown(format!("Need to have a valid next chunk, but had: {chunk}"));
                }
            }
            self.check_record_position(chunk)?;
            let piece = self
                .data
                .get(self.position..self.position + chunk as usize)
                .ok_or_else(|| Thrown("Unexpected end-of-file".to_owned()))?;
            bytes.extend_from_slice(piece);
            self.position += chunk as usize;
            self.offset += chunk;
            left -= chunk;
        }
        Ok(bytes)
    }

    /// `readCompressedUnicode`.
    pub(super) fn read_compressed(&mut self, length: i32) -> Read<Vec<u16>> {
        self.read_string(length, true)
    }

    /// `readUnicodeLEString`.
    pub(super) fn read_unicode(&mut self, length: i32) -> Read<Vec<u16>> {
        self.read_string(length, false)
    }

    /// `readStringCommon`: the characters, read on into `CONTINUE` records, each of which
    /// starts with a byte saying whether its characters are compressed.
    fn read_string(&mut self, length: i32, compressed: bool) -> Read<Vec<u16>> {
        if !(0..=0x10_0000).contains(&length) {
            return thrown(format!("Bad requested string length ({length})"));
        }
        let length = length as usize;
        let mut units = Vec::with_capacity(length);
        let mut compressed = compressed;
        loop {
            let available = if compressed {
                self.remaining()
            } else {
                self.remaining() / 2
            };
            let wanted = (length - units.len()) as i64;
            let take = if wanted <= available {
                wanted
            } else {
                available
            };
            for _ in 0..take.max(0) {
                units.push(self.read_char(compressed)?);
            }
            if wanted <= available {
                return Ok(units);
            }
            if !self.is_continue_next()? {
                return thrown(format!(
                    "Expected to find a ContinueRecord in order to read remaining {} of {length} \
                     chars",
                    length - units.len()
                ));
            }
            if self.remaining() != 0 {
                return thrown(format!(
                    "Odd number of bytes({}) left behind",
                    self.remaining()
                ));
            }
            self.next_record()?;
            let flag = self.read_u8()? as i8;
            if flag != 0 && flag != 1 {
                return thrown(format!("Invalid compressFlag: {flag}"));
            }
            compressed = flag == 0;
        }
    }

    fn read_char(&mut self, compressed: bool) -> Read<u16> {
        Ok(if compressed {
            u16::from(self.read_u8()?)
        } else {
            self.read_i16()? as u16
        })
    }

    /// `readRemainder`: the rest of the record.
    pub(super) fn read_remainder(&mut self) -> Read<Vec<u8>> {
        let size = self.remaining();
        if size == 0 {
            return Ok(Vec::new());
        }
        if !(0..=MAX_RECORD_LENGTH).contains(&size) {
            return thrown(format!("Tried to allocate an array of length {size}"));
        }
        self.read_fully(size as usize)
    }
}

/// A Java string of UTF-16 units, unpaired surrogates replaced.
pub(super) fn java_string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A record of type `sid` holding `data`.
    pub(in crate::formats::xls) fn record(sid: u16, data: &[u8]) -> Vec<u8> {
        let mut bytes = sid.to_le_bytes().to_vec();
        bytes.extend((data.len() as u16).to_le_bytes());
        bytes.extend(data);
        bytes
    }

    #[test]
    fn reads_strings_on_into_continue_records() {
        let mut data = record(0x0204, b"ab");
        data.extend(record(CONTINUE, &[1, b'c', 0, b'd', 0]));
        let mut stream = RecordStream::new(&data);
        assert!(stream.has_next_record().unwrap());
        stream.next_record().unwrap();
        assert_eq!(java_string(&stream.read_compressed(4).unwrap()), "abcd");
        assert!(!stream.has_next_record().unwrap());
    }

    #[test]
    fn fails_on_records_left_partly_read() {
        let data = record(0x0204, b"ab");
        let mut stream = RecordStream::new(&data);
        stream.has_next_record().unwrap();
        stream.next_record().unwrap();
        stream.read_u8().unwrap();
        assert!(stream.has_next_record().is_err());
        let mut stream = RecordStream::new(&data);
        stream.has_next_record().unwrap();
        stream.next_record().unwrap();
        assert!(stream.read_i32().is_err());
    }
}
