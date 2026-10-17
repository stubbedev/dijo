//! What Tika's `RTFEmbObjHandler` and `RTFObjDataParser` do with the pictures (`\pict`) and
//! objects (`\objdata`) of an RTF document, as far as they decide the document's own output:
//! the bytes they collect, and whether a finished one is handed to the embedded document
//! extractor, which parses it recursively, or makes Tika record a failure in the document's
//! metadata. Either defers the document to Tika.

use super::input::Input;

/// `RTFParser`'s default limit on the bytes of one picture or object, 20 MB.
const MEMORY_LIMIT: u64 = 20 * 1024 * 1024;
const PACKAGE: &[u8] = b"package";
const EMBEDDED_OBJECT: u32 = 2;
const EMBEDDED_PACKAGE: u16 = 3;

/// Why the native parser leaves a document to Tika.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Deferral {
    /// A finished picture, which Tika parses as an embedded document.
    Picture,
    /// A finished embedded object, which Tika parses as an embedded document.
    Object,
    /// A picture or object Tika fails to read, recording the failure with its stack trace in
    /// the document's metadata.
    RecordedFailure,
    /// Text in a code page the JDK decodes and this port does not.
    Charset,
}

impl Deferral {
    pub(super) fn reason(self) -> &'static str {
        match self {
            Deferral::Picture => "an embedded picture, which Tika parses as a document of its own",
            Deferral::Object => "an embedded object, which Tika parses as a document of its own",
            Deferral::RecordedFailure => {
                "an embedded picture or object Tika fails to read, recording the failure"
            }
            Deferral::Charset => "text in a code page this port does not decode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Nothing,
    Picture,
    Object,
}

/// The picture or object being collected.
pub(super) struct Embedded {
    state: State,
    bytes: Vec<u8>,
    high: Option<u8>,
}

impl Embedded {
    pub(super) fn new() -> Self {
        Self {
            state: State::Nothing,
            bytes: Vec::new(),
            high: None,
        }
    }

    pub(super) fn start_picture(&mut self) {
        self.state = State::Picture;
    }

    pub(super) fn start_object(&mut self) {
        self.state = State::Object;
    }

    /// Takes a byte of hex data; anything other than a hex digit, such as a line break, is
    /// skipped.
    pub(super) fn write_hex_char(&mut self, byte: u8) {
        let Some(value) = hex_value(byte) else {
            return;
        };
        match self.high.take() {
            None => self.high = Some(16 * value),
            Some(high) => self.bytes.push(high + value),
        }
    }

    /// Takes `length` bytes of `\binN` data from `input`. Data over the memory limit, or cut
    /// off, is a failure Tika records.
    pub(super) fn write_bytes(&mut self, input: &mut Input, length: u32) -> Result<(), Deferral> {
        if u64::from(length) > MEMORY_LIMIT {
            return Err(Deferral::RecordedFailure);
        }
        let data = input
            .take(length as usize)
            .ok_or(Deferral::RecordedFailure)?;
        self.bytes.extend_from_slice(&data);
        Ok(())
    }

    /// `handleCompletedObject`: a picture is handed to the embedded document extractor; an
    /// object is, unless `RTFObjDataParser` finds no embedded document in it.
    pub(super) fn complete(&mut self) -> Result<(), Deferral> {
        match self.state {
            State::Nothing => {}
            State::Picture => return Err(Deferral::Picture),
            State::Object => object(&self.bytes)?,
        }
        self.state = State::Nothing;
        self.bytes.clear();
        self.high = None;
        Ok(())
    }
}

pub(super) fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Why `RTFObjDataParser` stopped reading an object.
enum Stop {
    /// It found no embedded document: a link, or a package holding a link.
    Nothing,
    /// The object ends early, an `IOException` Tika records in the embedded document's own
    /// metadata, which is dropped with it.
    Truncated,
    /// A length over the memory limit, a `TikaMemoryLimitException` Tika records in the
    /// document's metadata.
    OverLimit,
}

/// `RTFObjDataParser.parse` as far as it decides whether the object reaches the embedded
/// document extractor. An object other than a package always does, its bytes or the document
/// an OLE2 container wraps.
fn object(bytes: &[u8]) -> Result<(), Deferral> {
    match read_object(&mut Reader { bytes, at: 0 }) {
        Ok(()) => Err(Deferral::Object),
        Err(Stop::Nothing | Stop::Truncated) => Ok(()),
        Err(Stop::OverLimit) => Err(Deferral::RecordedFailure),
    }
}

fn read_object(reader: &mut Reader) -> Result<(), Stop> {
    reader.u32()?;
    if reader.u32()? != EMBEDDED_OBJECT {
        return Err(Stop::Nothing);
    }
    let class = reader.length_prefixed()?;
    reader.length_prefixed()?;
    reader.length_prefixed()?;
    let data = reader.length_prefixed()?;
    if !trim_java(class).eq_ignore_ascii_case(PACKAGE) {
        return Ok(());
    }
    read_package(&mut Reader { bytes: data, at: 0 })
}

/// `RTFObjDataParser.handlePackage`, up to the embedded file's bytes.
fn read_package(reader: &mut Reader) -> Result<(), Stop> {
    reader.u16()?;
    reader.ansi_string()?;
    reader.ansi_string()?;
    reader.u16()?;
    if reader.u16()? != EMBEDDED_PACKAGE {
        return Err(Stop::Nothing);
    }
    reader.u32()?;
    reader.ansi_string()?;
    let length = reader.u32()?;
    reader.take(length)?;
    Ok(())
}

/// Java's `String.trim` on text one byte per character: bytes up to a space go from both ends.
fn trim_java(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|&b| b > b' ').unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| b > b' ')
        .map_or(start, |end| end + 1);
    &bytes[start..end]
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], Stop> {
        let bytes = self
            .bytes
            .get(self.at..self.at + N)
            .ok_or(Stop::Truncated)?;
        self.at += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    fn u16(&mut self) -> Result<u16, Stop> {
        self.fixed().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, Stop> {
        self.fixed().map(u32::from_le_bytes)
    }

    /// `initByteArray` and `readFully`: a length over the limit fails before the bytes are read.
    fn take(&mut self, length: u32) -> Result<&'a [u8], Stop> {
        if u64::from(length) > MEMORY_LIMIT {
            return Err(Stop::OverLimit);
        }
        let end = self.at + length as usize;
        let bytes = self.bytes.get(self.at..end).ok_or(Stop::Truncated)?;
        self.at = end;
        Ok(bytes)
    }

    fn length_prefixed(&mut self) -> Result<&'a [u8], Stop> {
        let length = self.u32()?;
        self.take(length)
    }

    /// Bytes up to a zero byte, which must come before the end.
    fn ansi_string(&mut self) -> Result<&'a [u8], Stop> {
        let rest = &self.bytes[self.at..];
        let end = rest.iter().position(|&b| b == 0).ok_or(Stop::Truncated)?;
        self.at += end + 1;
        Ok(&rest[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_bytes(class: &[u8], data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0501u32.to_le_bytes());
        bytes.extend_from_slice(&EMBEDDED_OBJECT.to_le_bytes());
        for part in [class, b"", b"", data] {
            bytes.extend_from_slice(&(part.len() as u32).to_le_bytes());
            bytes.extend_from_slice(part);
        }
        bytes
    }

    fn package(kind: u16) -> Vec<u8> {
        let mut bytes = vec![2, 0];
        bytes.extend_from_slice(b"name.txt\0C:\\icon\0");
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&9u32.to_le_bytes());
        bytes.extend_from_slice(b"C:\\a.txt\0");
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(b"hi");
        bytes
    }

    #[test]
    fn hands_objects_with_documents_to_the_extractor() {
        assert_eq!(
            object(&object_bytes(b"Word.Document.8\0", b"data")),
            Err(Deferral::Object)
        );
        assert_eq!(
            object(&object_bytes(b" Package\0", &package(3))),
            Err(Deferral::Object)
        );
        assert_eq!(object(&object_bytes(b"PBrush", b"")), Err(Deferral::Object));
    }

    #[test]
    fn keeps_links_and_cut_objects() {
        let mut link = object_bytes(b"Word.Document.8", b"");
        link[4] = 1;
        assert_eq!(object(&link), Ok(()));
        assert_eq!(object(&object_bytes(b"Package", &package(1))), Ok(()));
        assert_eq!(object(&object_bytes(b"Package", &package(3)[..20])), Ok(()));
        assert_eq!(object(&[1, 0, 0]), Ok(()));
        let mut cut = object_bytes(b"Word.Document.8", b"data");
        cut.truncate(cut.len() - 1);
        assert_eq!(object(&cut), Ok(()));
    }

    #[test]
    fn records_lengths_over_the_limit() {
        let mut bytes = object_bytes(b"Word.Document.8", b"");
        let at = bytes.len() - 4;
        bytes[at..].copy_from_slice(&(MEMORY_LIMIT as u32 + 1).to_le_bytes());
        assert_eq!(object(&bytes), Err(Deferral::RecordedFailure));
        let mut embedded = Embedded::new();
        let mut input = Input::new(b"abc");
        assert_eq!(
            embedded.write_bytes(&mut input, MEMORY_LIMIT as u32 + 1),
            Err(Deferral::RecordedFailure)
        );
        assert_eq!(
            embedded.write_bytes(&mut input, 4),
            Err(Deferral::RecordedFailure)
        );
        assert_eq!(embedded.write_bytes(&mut input, 2), Ok(()));
        assert_eq!(embedded.bytes, b"ab");
    }

    #[test]
    fn hands_finished_pictures_to_the_extractor() {
        let mut embedded = Embedded::new();
        assert_eq!(embedded.complete(), Ok(()));
        embedded.start_picture();
        embedded.write_hex_char(b'f');
        embedded.write_hex_char(b'\n');
        embedded.write_hex_char(b'F');
        assert_eq!(embedded.bytes, [0xff]);
        assert_eq!(embedded.complete(), Err(Deferral::Picture));
    }
}
