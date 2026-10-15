//! POI's `Ole10Native`: the `\1Ole10Native` stream of an embedded OLE 1.0 object, a file packaged
//! with its label, file name and command, or data alone in the compact and unparsed layouts.

use super::reader::{Reader, Thrown};

/// The longest data POI reads.
const MAX_RECORD_LENGTH: i32 = 100_000_000;
/// The longest string POI reads.
const MAX_STRING_LENGTH: usize = 1024;
const MAX_STRING_RECORD: i32 = 1024;

/// What `new Ole10Native(data, 0)` reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Ole10Native {
    pub(super) label: Option<String>,
    pub(super) file_name: Option<String>,
    pub(super) command: Option<String>,
    pub(super) data: Vec<u8>,
}

/// Why POI reads no `Ole10Native`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    /// `Ole10NativeException`, which Tika passes over.
    Invalid,
    /// A runtime exception, which Tika records.
    Thrown,
}

impl From<Thrown> for Refusal {
    fn from(thrown: Thrown) -> Self {
        match thrown {
            Thrown::EndOfFile => Refusal::Invalid,
            Thrown::Runtime => Refusal::Thrown,
        }
    }
}

impl Ole10Native {
    /// `new Ole10Native(data, 0)`.
    pub(super) fn read(data: &[u8]) -> Result<Ole10Native, Refusal> {
        let mut reader = Reader::new(data, 0);
        let total_size = reader.read_i32()?;
        reader.limit(i64::from(total_size.wrapping_add(4)));
        let start = reader.position();
        let flags = reader.read_i16()?;
        if flags != 2 {
            reader.set_position(start)?;
            return Ok(Ole10Native {
                data: reader.read_bounded(total_size, MAX_RECORD_LENGTH)?,
                ..Ole10Native::default()
            });
        }
        let after_flags = reader.position();
        let first = reader.read_i8()?;
        reader.set_position(after_flags)?;
        if is_iso_control(first) {
            return Ok(Ole10Native {
                data: reader.read_bounded(total_size.wrapping_sub(2), MAX_RECORD_LENGTH)?,
                ..Ole10Native::default()
            });
        }
        read_parsed(&mut reader)
    }
}

/// `Character.isISOControl` of a signed byte.
fn is_iso_control(byte: i8) -> bool {
    matches!(byte, 0..=0x1F | 0x7F)
}

fn read_parsed(reader: &mut Reader<'_>) -> Result<Ole10Native, Refusal> {
    let label = read_ascii_z(reader)?;
    let file_name = read_ascii_z(reader)?;
    reader.read_i16()?;
    reader.read_i16()?;
    let command = read_ascii_len(reader)?;
    let size = reader.read_i32()?;
    let data = reader.read_bounded(size, MAX_RECORD_LENGTH)?;
    let mark = reader.position();
    if reader.read_i16()? != 0 {
        reader.set_position(mark)?;
        for _ in 0..3 {
            read_utf16(reader)?;
        }
    }
    Ok(Ole10Native {
        label: Some(label),
        file_name: Some(file_name),
        command: Some(command),
        data,
    })
}

/// `readAsciiZ`: bytes up to a NUL, as UTF-8, failing past 1024 of them.
fn read_ascii_z(reader: &mut Reader<'_>) -> Result<String, Refusal> {
    let mut bytes = Vec::new();
    for _ in 0..MAX_STRING_LENGTH {
        let byte = reader.read_u8()?;
        if byte == 0 {
            return Ok(utf8(&bytes));
        }
        bytes.push(byte);
    }
    Err(Refusal::Invalid)
}

/// Java's UTF-8 decoding, malformed input replaced.
fn utf8(bytes: &[u8]) -> String {
    crate::text::charset::Charset::named("UTF-8")
        .decode(bytes)
        .expect("the JDK's UTF-8 decodes")
}

/// `readAsciiLen`: a length, then that many bytes of which all but the last are ISO-8859-1.
fn read_ascii_len(reader: &mut Reader<'_>) -> Result<String, Refusal> {
    let size = reader.read_i32()?;
    let bytes = reader.read_bounded(size, MAX_STRING_RECORD)?;
    if bytes.is_empty() {
        return Ok(String::new());
    }
    let length = usize::try_from(size.wrapping_sub(1)).map_err(|_| Refusal::Thrown)?;
    Ok(bytes[..length.min(bytes.len())]
        .iter()
        .map(|&byte| char::from(byte))
        .collect())
}

/// `readUtf16`: a length in characters, then that many UTF-16LE characters, which POI reads
/// and Tika does not use.
fn read_utf16(reader: &mut Reader<'_>) -> Result<(), Refusal> {
    let size = reader.read_i32()?;
    let bytes = reader.read_bounded(size.wrapping_mul(2), MAX_STRING_RECORD)?;
    if size == 0 {
        return Ok(());
    }
    if bytes.is_empty() || size < 0 || bytes.len() / 2 < size as usize {
        return Err(Refusal::Thrown);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(trailer: &[u8]) -> Vec<u8> {
        let mut body = 2i16.to_le_bytes().to_vec();
        body.extend(b"report.pdf\0C:\\report.pdf\0");
        body.extend([0, 0, 3, 0]);
        body.extend(9i32.to_le_bytes());
        body.extend(b"C:\\a.pdf\0");
        body.extend(3i32.to_le_bytes());
        body.extend(b"PDF");
        body.extend(trailer);
        let mut data = (body.len() as i32).to_le_bytes().to_vec();
        data.extend(body);
        data
    }

    #[test]
    fn reads_a_packaged_file_as_poi_does() {
        let native = Ole10Native::read(&parsed(&[0, 0])).unwrap();
        assert_eq!(native.label.as_deref(), Some("report.pdf"));
        assert_eq!(native.file_name.as_deref(), Some("C:\\report.pdf"));
        assert_eq!(native.command.as_deref(), Some("C:\\a.pdf"));
        assert_eq!(native.data, b"PDF");
    }

    #[test]
    fn throws_where_no_trailer_follows_the_data() {
        assert_eq!(Ole10Native::read(&parsed(&[])), Err(Refusal::Thrown));
    }

    #[test]
    fn reads_compact_and_unparsed_data() {
        let mut compact = 6i32.to_le_bytes().to_vec();
        compact.extend([2, 0, 1, 2, 3, 4]);
        let native = Ole10Native::read(&compact).unwrap();
        assert_eq!(native.label, None);
        assert_eq!(native.data, [1, 2, 3, 4]);
        let mut unparsed = 3i32.to_le_bytes().to_vec();
        unparsed.extend([7, 0, 9]);
        assert_eq!(Ole10Native::read(&unparsed).unwrap().data, [7, 0, 9]);
    }

    #[test]
    fn refuses_data_shorter_than_its_length() {
        let mut short = 10i32.to_le_bytes().to_vec();
        short.extend([7, 0, 9]);
        assert_eq!(Ole10Native::read(&short), Err(Refusal::Invalid));
    }
}
