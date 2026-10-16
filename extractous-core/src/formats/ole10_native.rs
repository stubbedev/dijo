//! POI's `Ole10Native`: the record an OLE 1.0 object keeps its document in, the content of its
//! `\1Ole10Native` stream, read as `new Ole10Native(data, 0)` reads it and failing where it fails.

use crate::java;

/// `Ole10Native`'s default `MAX_RECORD_LENGTH`.
pub(crate) const MAX_RECORD_LENGTH: i32 = 100_000_000;
const MAX_STRING_LENGTH: i32 = 1024;

/// An OLE 1.0 object's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ole10Native {
    /// The name the object is shown with; only a record in the parsed form has one.
    pub(crate) label: Option<String>,
    /// The path of the embedded file; only a record in the parsed form has one.
    pub(crate) file_name: Option<String>,
    /// The command that opens the embedded file; only a record in the parsed form has one.
    pub(crate) command: Option<String>,
    /// The embedded document.
    pub(crate) data: Vec<u8>,
}

/// Why POI reads no record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unread {
    /// An `Ole10NativeException`: the record is not one POI understands.
    Invalid,
    /// Another exception, with its message: the record overruns the stream, or claims more than
    /// POI allocates.
    Failed(String),
}

impl Ole10Native {
    /// `new Ole10Native(data, 0)`: the record in its parsed form when its flags are 2 and its
    /// label starts with a printable byte, in its compact form when they are 2 and it does not,
    /// and unparsed otherwise.
    pub(crate) fn parse(data: &[u8]) -> Result<Ole10Native, Unread> {
        let mut stream = Stream::new(data);
        let total_size = stream.read_int()?;
        stream.limit(total_size.wrapping_add(4));
        stream.mark();
        let flags = stream.read_short()?;
        if flags != 2 {
            stream.reset();
            return Ok(Self::unparsed(
                stream.read_array(total_size, MAX_RECORD_LENGTH)?,
            ));
        }
        stream.mark();
        let first = stream.read_byte()?;
        stream.reset();
        if is_iso_control(first) {
            let data = stream.read_array(total_size.wrapping_sub(2), MAX_RECORD_LENGTH)?;
            return Ok(Self::unparsed(data));
        }
        let label = read_ascii_z(&mut stream)?;
        let file_name = read_ascii_z(&mut stream)?;
        stream.read_short()?;
        stream.read_short()?;
        let command = read_ascii_len(&mut stream)?;
        let length = stream.read_int()?;
        let data = stream.read_array(length, MAX_RECORD_LENGTH)?.to_vec();
        stream.mark();
        if stream.read_short()? != 0 {
            stream.reset();
            for _ in 0..3 {
                read_utf16(&mut stream)?;
            }
        }
        Ok(Ole10Native {
            label: Some(label),
            file_name: Some(file_name),
            command: Some(command),
            data,
        })
    }

    fn unparsed(data: &[u8]) -> Ole10Native {
        Ole10Native {
            label: None,
            file_name: None,
            command: None,
            data: data.to_vec(),
        }
    }
}

/// `Character.isISOControl` of a byte Java widens to an `int`, sign and all.
fn is_iso_control(byte: i8) -> bool {
    (0..=0x1F).contains(&byte) || byte == 0x7F
}

/// `readAsciiZ`: the UTF-8 text before a zero byte within the first 1024.
fn read_ascii_z(stream: &mut Stream<'_>) -> Result<String, Unread> {
    let start = stream.pos;
    for _ in 0..MAX_STRING_LENGTH {
        if stream.read_byte()? == 0 {
            let bytes = &stream.buf[as_index(start)..as_index(stream.pos - 1)];
            return Ok(java::utf8(bytes));
        }
    }
    Err(Unread::Invalid)
}

/// `readAsciiLen`: ISO-8859-1 text of a length that counts a terminating byte it drops.
fn read_ascii_len(stream: &mut Stream<'_>) -> Result<String, Unread> {
    let size = stream.read_int()?;
    let bytes = stream.read_array(size, MAX_STRING_LENGTH)?;
    if bytes.is_empty() {
        return Ok(String::new());
    }
    let length = size
        .wrapping_sub(1)
        .min(i32::try_from(bytes.len()).expect("at most 1024"));
    let length = usize::try_from(length).map_err(|_| {
        Unread::Failed(format!(
            "Illegal length {length} of a string of {} bytes",
            bytes.len()
        ))
    })?;
    Ok(bytes[..length]
        .iter()
        .map(|&byte| char::from(byte))
        .collect())
}

/// `readUtf16`: a UTF-16LE string after its length in units, read to check that POI reads it.
fn read_utf16(stream: &mut Stream<'_>) -> Result<(), Unread> {
    let size = stream.read_int()?;
    let bytes = stream.read_array(size.wrapping_mul(2), MAX_STRING_LENGTH)?;
    if size == 0 {
        return Ok(());
    }
    if bytes.is_empty() {
        return Err(Unread::Failed(
            "Illegal offset 0 (String data is of length 0)".to_owned(),
        ));
    }
    if size < 0 || bytes.len() / 2 < as_index(i64::from(size)) {
        return Err(Unread::Failed(format!("Illegal length {size}")));
    }
    Ok(())
}

fn as_index(position: i64) -> usize {
    usize::try_from(position).expect("a position within the record")
}

/// `LittleEndianByteArrayInputStream` over a record: a `ByteArrayInputStream` whose `count` its
/// `limit` may set below its position, even below zero.
struct Stream<'a> {
    buf: &'a [u8],
    pos: i64,
    count: i64,
    mark: i64,
}

impl<'a> Stream<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Stream {
            buf,
            pos: 0,
            count: buf.len() as i64,
            mark: 0,
        }
    }

    fn limit(&mut self, size: i32) {
        self.count = i64::from(size).min(self.buf.len() as i64);
    }

    fn mark(&mut self) {
        self.mark = self.pos;
    }

    fn reset(&mut self) {
        self.pos = self.mark;
    }

    /// `checkPosition` and the read: `n` bytes, or the overrun POI throws for.
    fn take(&mut self, n: i64) -> Result<&'a [u8], Unread> {
        if n > self.count - self.pos {
            return Err(Unread::Failed(format!(
                "Buffer overrun, having {} bytes in the stream and position is at {}, but trying \
                 to increment position by {n}",
                self.count, self.pos
            )));
        }
        let bytes = &self.buf[as_index(self.pos)..as_index(self.pos + n)];
        self.pos += n;
        Ok(bytes)
    }

    fn read_byte(&mut self) -> Result<i8, Unread> {
        Ok(i8::from_le_bytes([self.take(1)?[0]]))
    }

    fn read_short(&mut self) -> Result<i16, Unread> {
        let bytes = self.take(2)?;
        Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_int(&mut self) -> Result<i32, Unread> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// `IOUtils.toByteArray(stream, length, max_length)`: `length` bytes, or the rest of the
    /// stream up to `max_length` for a negative length. A length above `max_length` is refused,
    /// and a stream ending before `length` bytes is an `EOFException`, which the constructor
    /// turns into an `Ole10NativeException`.
    fn read_array(&mut self, length: i32, max_length: i32) -> Result<&'a [u8], Unread> {
        if length > max_length {
            return Err(Unread::Failed(format!(
                "Tried to allocate an array of length {length}, but the maximum length for this \
                 record type is {max_length}."
            )));
        }
        let wanted = i64::from(if length >= 0 { length } else { max_length });
        let read = wanted.min((self.count - self.pos).max(0));
        let bytes = &self.buf[as_index(self.pos)..as_index(self.pos + read)];
        self.pos += read;
        if length >= 0 && read < wanted {
            return Err(Unread::Invalid);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(body: &[u8]) -> Vec<u8> {
        let mut record = i32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
        record.extend_from_slice(body);
        record
    }

    fn parsed(data: &[u8], trailer: &[u8]) -> Vec<u8> {
        let mut body = vec![2, 0];
        body.extend_from_slice(b"label.txt\0C:\\dir\\file.txt\0");
        body.extend_from_slice(&[0, 0, 3, 0]);
        body.extend_from_slice(&5i32.to_le_bytes());
        body.extend_from_slice(b"C:\\x\0");
        body.extend_from_slice(&i32::try_from(data.len()).unwrap().to_le_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(trailer);
        record(&body)
    }

    #[test]
    fn reads_the_names_and_data_of_the_parsed_form() {
        let native = Ole10Native::parse(&parsed(b"hello", &[0, 0])).unwrap();
        assert_eq!(native.label.as_deref(), Some("label.txt"));
        assert_eq!(native.file_name.as_deref(), Some("C:\\dir\\file.txt"));
        assert_eq!(native.command.as_deref(), Some("C:\\x"));
        assert_eq!(native.data, b"hello");
    }

    #[test]
    fn checks_the_unicode_names_after_the_data() {
        let mut trailer = Vec::new();
        for name in ["a", "bc", "d"] {
            trailer.extend_from_slice(&i32::try_from(name.len()).unwrap().to_le_bytes());
            for unit in name.encode_utf16() {
                trailer.extend_from_slice(&unit.to_le_bytes());
            }
        }
        assert!(Ole10Native::parse(&parsed(b"x", &trailer)).is_ok());
        let unterminated = [&b"\x01\0\0\0a\0"[..], &(-1i32).to_le_bytes()].concat();
        assert_eq!(
            Ole10Native::parse(&parsed(b"x", &unterminated)),
            Err(Unread::Failed(
                "Illegal offset 0 (String data is of length 0)".to_owned()
            ))
        );
    }

    #[test]
    fn fails_as_poi_fails_where_the_record_ends_after_its_data() {
        assert!(matches!(
            Ole10Native::parse(&parsed(b"hello", &[])),
            Err(Unread::Failed(message)) if message.starts_with("Buffer overrun")
        ));
    }

    #[test]
    fn reads_the_compact_form_after_its_flags() {
        let native = Ole10Native::parse(&record(&[2, 0, 1, 2, 3])).unwrap();
        assert_eq!(native.label, None);
        assert_eq!(native.data, [1, 2, 3]);
    }

    #[test]
    fn reads_the_unparsed_form_whole() {
        let native = Ole10Native::parse(&record(&[7, 0, 1, 2])).unwrap();
        assert_eq!(native.command, None);
        assert_eq!(native.data, [7, 0, 1, 2]);
    }

    #[test]
    fn is_invalid_where_the_data_is_shorter_than_its_length() {
        let mut data = parsed(b"hello", &[0, 0]);
        data.truncate(data.len() - 4);
        let length = i32::try_from(data.len() - 4).unwrap();
        data[..4].copy_from_slice(&length.to_le_bytes());
        assert_eq!(Ole10Native::parse(&data), Err(Unread::Invalid));
    }

    #[test]
    fn is_invalid_where_a_label_runs_past_1024_bytes() {
        let mut body = vec![2, 0];
        body.extend(std::iter::repeat_n(b'a', 1100));
        assert_eq!(Ole10Native::parse(&record(&body)), Err(Unread::Invalid));
    }

    #[test]
    fn refuses_what_poi_will_not_allocate() {
        let body = [&[2, 0, 1][..], &[0; 8]].concat();
        let mut data = record(&body);
        data[..4].copy_from_slice(&(MAX_RECORD_LENGTH + 10).to_le_bytes());
        assert!(matches!(
            Ole10Native::parse(&data),
            Err(Unread::Failed(message)) if message.starts_with("Tried to allocate")
        ));
    }
}
