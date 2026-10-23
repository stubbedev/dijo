//! POI's `RecordInputStream` over a workbook stream: BIFF records of a type and a length, read
//! by records that run on into the `CONTINUE` records after them where POI's reads do, and the
//! exceptions POI throws on records that end early or late. The stream is read in place, a
//! record at a time, and an encrypted workbook's records are decrypted as they are read.

use std::borrow::Cow;

use super::factory::Decrypting;
use crate::formats::poi::{
    record_format, safely_allocate_check, thrown, Read, Thrown, ILLEGAL_ARGUMENT, ILLEGAL_STATE,
};
use crate::ole2::Content;

/// The type of a `CONTINUE` record.
pub(super) const CONTINUE: u16 = 0x003C;
/// The longest record POI reads.
const MAX_RECORD_DATA_SIZE: i64 = 8224;
/// `HSSFWorkbook`'s largest record.
pub(super) const MAX_RECORD_LENGTH: i64 = 100_000;
/// What `LittleEndianInputStream` throws reading past the end of its stream.
const UNEXPECTED_END: &str = "Unexpected end-of-file";

fn illegal_state(message: &str) -> Thrown {
    Thrown::runtime(ILLEGAL_STATE, message)
}
/// `StringUtil`'s largest record.
pub(super) const MAX_STRING_LENGTH: i64 = 10_000_000;

/// Where a record stream is: in a record, by the start of its header and how far into its data
/// it has read, or between records, by the start of the next one's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mark {
    In { header: u32, offset: u16 },
    Between { header: u32 },
}

/// The records of a workbook stream, as `RecordInputStream` reads them.
#[derive(Clone)]
pub(super) struct RecordStream<'a> {
    content: Content<'a>,
    /// What decrypts the records from where the encryption starts, in an encrypted workbook.
    decrypting: Option<Decrypting>,
    /// Where the next byte is read: in a record, the start of its data plus the offset.
    position: usize,
    sid: u16,
    /// The length of the current record, `None` before it is read.
    length: Option<i64>,
    offset: i64,
    /// The current record's data, as far as the stream holds it, decrypted.
    data: Cow<'a, [u8]>,
    /// The type of the next record, `None` at the end of the stream.
    next_sid: Option<u16>,
}

impl<'a> RecordStream<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self::of(Content::from(data))
    }

    pub(super) fn of(content: Content<'a>) -> Self {
        let mut stream = RecordStream {
            content,
            decrypting: None,
            position: 0,
            sid: 0,
            length: None,
            offset: 0,
            data: Cow::Borrowed(&[]),
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

    /// Decrypts the records read from now on as `decrypting` says.
    pub(super) fn decrypt_with(&mut self, decrypting: Decrypting) {
        self.decrypting = Some(decrypting);
    }

    /// Where the stream is.
    pub(super) fn mark(&self) -> Mark {
        let header =
            |at: usize| u32::try_from(at).expect("a stream of a compound document is under 4 GiB");
        match self.length {
            Some(_) => Mark::In {
                header: header(self.position - self.offset as usize - 4),
                offset: u16::try_from(self.offset).expect("a record holds at most 8224 bytes"),
            },
            None => Mark::Between {
                header: header(self.position - 2),
            },
        }
    }

    /// Where the current record starts.
    pub(super) fn record_start(&self) -> Mark {
        Mark::In {
            header: u32::try_from(self.position - self.offset as usize - 4)
                .expect("a stream of a compound document is under 4 GiB"),
            offset: 0,
        }
    }

    /// Goes back or on to where the stream was at `mark`.
    pub(super) fn seek(&mut self, mark: Mark) -> Read<()> {
        let (Mark::In { header, .. } | Mark::Between { header }) = mark;
        self.position = header as usize;
        self.next_sid = self.read_next_sid();
        if let Mark::In { offset, .. } = mark {
            self.next_record()?;
            self.offset = i64::from(offset);
            self.position += usize::from(offset);
        }
        Ok(())
    }

    /// `getNextSid`.
    pub(super) fn next_sid(&self) -> Option<u16> {
        self.next_sid
    }

    fn u16_at(&self, at: usize) -> Option<u16> {
        let bytes = self.content.range(at, 2)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_next_sid(&mut self) -> Option<u16> {
        if self.content.len() - self.position < 4 {
            return None;
        }
        let sid = self.u16_at(self.position)?;
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
            return Err(illegal_state("EOF - next record not available"));
        };
        if self.length.is_some() {
            return Err(illegal_state(
                "Cannot call nextRecord() without checking hasNextRecord() first",
            ));
        }
        self.sid = sid;
        self.offset = 0;
        let length = i64::from(
            self.u16_at(self.position)
                .ok_or_else(|| illegal_state(UNEXPECTED_END))?,
        );
        self.position += 2;
        self.length = Some(length);
        if length > MAX_RECORD_DATA_SIZE {
            return record_format(format!(
                "The content of an excel record cannot exceed {MAX_RECORD_DATA_SIZE} bytes, but \
                 had: {length} for record with sid: {sid}"
            ));
        }
        let held = (self.content.len() - self.position).min(length as usize);
        let mut data = self
            .content
            .range(self.position, held)
            .expect("the data of the record the stream holds");
        if let Some(decrypting) = &mut self.decrypting {
            decrypting.decrypt(sid, self.position - 4, length as usize, &mut data);
        }
        self.data = data;
        Ok(())
    }

    /// `read(bytes, 0, length)`: as many of `length` bytes as the record holds, never into the
    /// records after it.
    pub(super) fn read_up_to(&mut self, length: usize) -> Read<Vec<u8>> {
        let available = usize::try_from(self.remaining()).unwrap_or_default();
        let length = length.min(available);
        if length == 0 {
            return Ok(Vec::new());
        }
        self.read_fully(length)
    }

    /// The next byte of the record, left to be read again (`mark` and `reset`), failing where
    /// the record has none.
    pub(super) fn peek_u8(&self) -> Read<u8> {
        if self.remaining() < 1 {
            return thrown("a byte past the end of its record, read on into the next");
        }
        self.data
            .get(self.offset as usize)
            .copied()
            .ok_or_else(|| illegal_state(UNEXPECTED_END))
    }

    /// `remaining`.
    pub(super) fn remaining(&self) -> i64 {
        self.length.map_or(0, |length| length - self.offset)
    }

    fn is_continue_next(&mut self) -> Read<bool> {
        if self.length.is_some_and(|length| length != self.offset) {
            return Err(illegal_state(
                "Should never be called before end of current record",
            ));
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
        record_format(format!(
            "Not enough data ({available}) to read requested ({required}) bytes"
        ))
    }

    /// `length` bytes of the record's data from where the stream is, failing past the end of
    /// the stream.
    fn take(&mut self, length: usize) -> Read<&[u8]> {
        let at = self.offset as usize;
        self.offset += length as i64;
        self.position += length;
        self.data
            .get(at..at + length)
            .ok_or_else(|| illegal_state(UNEXPECTED_END))
    }

    fn read<const N: usize>(&mut self) -> Read<[u8; N]> {
        self.check_record_position(N as i64)?;
        Ok(self.take(N)?.try_into().expect("N bytes"))
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
        self.read_fully_with(length, |chunk| bytes.extend_from_slice(chunk))?;
        Ok(bytes)
    }

    /// [`RecordStream::read_fully`], the bytes passed over.
    pub(super) fn skip_fully(&mut self, length: usize) -> Read<()> {
        self.read_fully_with(length, |_| {})
    }

    fn read_fully_with(&mut self, length: usize, mut read: impl FnMut(&[u8])) -> Read<()> {
        let mut left = length as i64;
        while left > 0 {
            let mut chunk = self.remaining().min(left);
            if chunk == 0 {
                if !self.has_next_record()? {
                    return record_format(format!(
                        "Can't read the remaining {left} bytes of the requested {length} bytes. \
                         No further record exists."
                    ));
                }
                self.next_record()?;
                chunk = self.remaining().min(left);
                if chunk <= 0 {
                    return record_format(format!(
                        "Need to have a valid next chunk, but had: {chunk} with len: {left} and \
                         available: {}",
                        self.remaining()
                    ));
                }
            }
            self.check_record_position(chunk)?;
            read(self.take(chunk as usize)?);
            left -= chunk;
        }
        Ok(())
    }

    /// `readCompressedUnicode`.
    pub(super) fn read_compressed(&mut self, length: i32) -> Read<Vec<u16>> {
        self.read_string(length, true)
    }

    /// `readUnicodeLEString`.
    pub(super) fn read_unicode(&mut self, length: i32) -> Read<Vec<u16>> {
        self.read_string(length, false)
    }

    fn read_string(&mut self, length: i32, compressed: bool) -> Read<Vec<u16>> {
        let length = string_length(length)?;
        let mut units = Vec::with_capacity(length);
        self.read_string_with(length, compressed, |bytes, compressed| {
            if compressed {
                units.extend(bytes.iter().map(|&byte| u16::from(byte)));
            } else {
                units.extend(
                    bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&pair| u16::from_le_bytes(pair)),
                );
            }
        })?;
        Ok(units)
    }

    /// [`RecordStream::read_compressed`] or [`RecordStream::read_unicode`], the characters
    /// passed over.
    pub(super) fn skip_string(&mut self, length: i32, compressed: bool) -> Read<()> {
        let length = string_length(length)?;
        self.read_string_with(length, compressed, |_, _| {})
    }

    /// `readStringCommon`: the bytes of `length` characters, read on into `CONTINUE` records,
    /// each of which starts with a byte saying whether its characters are compressed, passed
    /// to `read` with whether they are.
    fn read_string_with(
        &mut self,
        length: usize,
        compressed: bool,
        mut read: impl FnMut(&[u8], bool),
    ) -> Read<()> {
        let mut done = 0;
        let mut compressed = compressed;
        loop {
            let available = if compressed {
                self.remaining()
            } else {
                self.remaining() / 2
            };
            let wanted = (length - done) as i64;
            let take = if wanted <= available {
                wanted
            } else {
                available
            };
            let count = take.max(0) as usize;
            read(
                self.take_units(count, if compressed { 1 } else { 2 })?,
                compressed,
            );
            done += count;
            if wanted <= available {
                return Ok(());
            }
            if !self.is_continue_next()? {
                return record_format(format!(
                    "Expected to find a ContinueRecord in order to read remaining {} of {length} \
                     chars",
                    length - done
                ));
            }
            if self.remaining() != 0 {
                return record_format(format!(
                    "Odd number of bytes({}) left behind",
                    self.remaining()
                ));
            }
            self.next_record()?;
            let flag = self.read_u8()? as i8;
            if flag != 0 && flag != 1 {
                return record_format(format!("Invalid compressFlag: {flag}"));
            }
            compressed = flag == 0;
        }
    }

    /// `count` units of `width` bytes the record holds, read as one at a time would: where the
    /// stream ends early, the stream moves past the first unit it cannot read and fails.
    fn take_units(&mut self, count: usize, width: usize) -> Read<&[u8]> {
        if count == 0 {
            return Ok(&[]);
        }
        let length = count * width;
        let held = self.data.len().saturating_sub(self.offset as usize);
        if held < length {
            let past = (held / width + 1) * width;
            self.offset += past as i64;
            self.position += past;
            return Err(illegal_state(UNEXPECTED_END));
        }
        self.take(length)
    }

    /// `readRemainder`: the rest of the record.
    pub(super) fn read_remainder(&mut self) -> Read<Vec<u8>> {
        let size = self.remainder_size()?;
        self.read_fully(size)
    }

    /// [`RecordStream::read_remainder`], the bytes passed over.
    pub(super) fn skip_remainder(&mut self) -> Read<()> {
        let size = self.remainder_size()?;
        self.skip_fully(size)
    }

    fn remainder_size(&self) -> Read<usize> {
        let size = self.remaining();
        if size == 0 {
            return Ok(0);
        }
        safely_allocate_check(size, MAX_RECORD_LENGTH)?;
        Ok(size as usize)
    }
}

/// `readStringCommon`'s check of the length of a string.
fn string_length(length: i32) -> Read<usize> {
    if !(0..=0x10_0000).contains(&length) {
        return Err(Thrown::runtime(
            ILLEGAL_ARGUMENT,
            format!("Bad requested string length ({length})"),
        ));
    }
    Ok(length as usize)
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

    fn read_string_per_char(
        stream: &mut RecordStream,
        length: i32,
        compressed: bool,
    ) -> Read<Vec<u16>> {
        let length = length as usize;
        let mut units = Vec::new();
        let mut compressed = compressed;
        loop {
            let available = if compressed {
                stream.remaining()
            } else {
                stream.remaining() / 2
            };
            let wanted = (length - units.len()) as i64;
            for _ in 0..wanted.min(available).max(0) {
                units.push(if compressed {
                    u16::from(stream.read_u8()?)
                } else {
                    stream.read_i16()? as u16
                });
            }
            if wanted <= available {
                return Ok(units);
            }
            if !stream.is_continue_next()? {
                return record_format("no continue");
            }
            stream.next_record()?;
            compressed = stream.read_u8()? == 0;
        }
    }

    #[test]
    fn reads_strings_as_char_by_char() {
        let mut continued = record(0x0204, &[b'a', 0, 0x3D, 0xD8, b'b']);
        continued.extend(record(CONTINUE, &[0, b'c', b'd', b'e']));
        continued.extend(record(CONTINUE, &[1, 0x00, 0xDC, b'f', 0]));
        let mut truncated = record(0x0204, &[b'a', 0, b'b']);
        truncated[2] = 9;
        let mut empty_truncated = record(0x0204, &[]);
        empty_truncated[2] = 4;
        for data in [continued, truncated, empty_truncated] {
            for compressed in [true, false] {
                for length in 0..12 {
                    let mut fast = RecordStream::new(&data);
                    fast.has_next_record().unwrap();
                    fast.next_record().unwrap();
                    let mut slow = fast.clone();
                    let read = fast.read_string(length, compressed);
                    let expected = read_string_per_char(&mut slow, length, compressed);
                    assert_eq!(read.is_ok(), expected.is_ok());
                    if let (Ok(read), Ok(expected)) = (&read, &expected) {
                        assert_eq!(read, expected);
                    }
                    if read.as_ref().is_err_and(|error| {
                        *error == Thrown::runtime(ILLEGAL_STATE, UNEXPECTED_END)
                    }) {
                        assert_eq!(read, expected);
                    }
                    assert_eq!(fast.position(), slow.position());
                    assert_eq!(fast.remaining(), slow.remaining());
                }
            }
        }
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
