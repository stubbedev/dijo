//! What the ports of POI's readers share: the Java exception POI throws reading a document,
//! which fails Tika's parse, and little-endian reads that fail where Java's array bounds do;
//! and how a parser that reads a document twice, once to decide whether it extracts it and once
//! as it writes it, stops.

use std::borrow::Cow;
use std::convert::Infallible;

use crate::errors::Error;
use crate::formats::thrown::{Exception, Thrown as Rethrown, TIKA_EXCEPTION};
use crate::formats::{Decline, Difference, Failure, Unported};

/// The parser Tika reads most of POI's formats with, which lets what POI throws through.
pub(crate) const OFFICE_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";
/// POI's `RecordFormatException`.
pub(crate) const RECORD_FORMAT: &str = "org.apache.poi.util.RecordFormatException";
pub(crate) const INDEX_OUT_OF_BOUNDS: &str = "java.lang.IndexOutOfBoundsException";
pub(crate) const ARRAY_INDEX_OUT_OF_BOUNDS: &str = "java.lang.ArrayIndexOutOfBoundsException";
pub(crate) const ILLEGAL_ARGUMENT: &str = "java.lang.IllegalArgumentException";
pub(crate) const ILLEGAL_STATE: &str = "java.lang.IllegalStateException";

const EOF: &str = "java.io.EOFException";
const ZIP_EXCEPTION: &str = "java.util.zip.ZipException";
const UNEXPECTED_ZLIB_END: &str = "Unexpected end of ZLIB input stream";

/// A Java exception POI throws reading a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thrown {
    /// As Java throws it, where the port reads the document as POI does up to the throw, which
    /// Tika's parser lets through.
    Java(Rethrown),
    /// As the port tells it.
    Told(String),
}

impl Thrown {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Thrown::Told(reason.into())
    }

    /// A `RuntimeException` of `class`, with `message`.
    pub(crate) fn runtime(class: &'static str, message: impl Into<String>) -> Self {
        Thrown::Java(Rethrown::Runtime(Exception::new(class, message)))
    }

    /// An `IOException` of `class`, with `message`.
    pub(crate) fn io(class: &'static str, message: impl Into<String>) -> Self {
        Thrown::Java(Rethrown::Io(Exception::new(class, message)))
    }

    /// A `TikaException` with `message`, which Tika's parser throws over what POI reads.
    pub(crate) fn tika(message: impl Into<String>) -> Self {
        Thrown::Java(Rethrown::Tika(Exception::new(TIKA_EXCEPTION, message)))
    }

    /// POI's `RecordFormatException`, with `message`.
    pub(crate) fn record_format(message: impl Into<String>) -> Self {
        Thrown::runtime(RECORD_FORMAT, message)
    }

    /// The exception met reading `context`: as Java throws it, or told within its context.
    pub(crate) fn within(self, context: &str) -> Self {
        match self {
            Thrown::Told(reason) => Thrown::Told(format!("{context}: {reason}")),
            java @ Thrown::Java(_) => java,
        }
    }

    /// What the parse fails with where POI throws this in Tika's parser `tika_name`: as Tika's
    /// path fails, where the port knows the exception as Java throws it, else with the port's
    /// account of it.
    pub(crate) fn failure(self, tika_name: &str) -> Failure {
        match self {
            Thrown::Java(rethrown) => Failure::Extraction(rethrown.into_error(tika_name)),
            Thrown::Told(reason) => {
                Failure::Malformed(format!("POI throws reading the document: {reason}"))
            }
        }
    }
}

impl std::fmt::Display for Thrown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Thrown::Java(rethrown) => write!(f, "{rethrown}"),
            Thrown::Told(reason) => f.write_str(reason),
        }
    }
}

pub(crate) type Read<T> = Result<T, Thrown>;

/// What a reader that may meet what the port does not read reads.
pub(crate) type Reading<T> = Result<T, Unread>;

pub(crate) fn thrown<T>(reason: impl Into<String>) -> Read<T> {
    Err(Thrown::new(reason))
}

/// [`thrown`] of POI's `RecordFormatException`.
pub(crate) fn record_format<T>(message: impl Into<String>) -> Read<T> {
    Err(Thrown::record_format(message))
}

/// Why a native parser of POI's formats does not read a document as Tika's parser does: POI
/// throws on it, which fails Tika's parse, or it has what the port does not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// POI throws reading the document.
    Thrown(Thrown),
    Unported(Unported),
}

impl From<Thrown> for Unread {
    fn from(thrown: Thrown) -> Self {
        Unread::Thrown(thrown)
    }
}

impl From<Unported> for Unread {
    fn from(unported: Unported) -> Self {
        Unread::Unported(unported)
    }
}

impl From<Thrown> for Decline<'_> {
    fn from(thrown: Thrown) -> Self {
        Unread::from(thrown).declined()
    }
}

impl From<Unread> for Decline<'_> {
    fn from(unread: Unread) -> Self {
        unread.declined()
    }
}

impl Unread {
    /// What [`OFFICE_PARSER`] declines the document with: see [`Unread::declined_by`].
    pub fn declined<'a>(self) -> Decline<'a> {
        self.declined_by(OFFICE_PARSER)
    }

    /// What a parser declines a document with that Tika reads with its parser `tika_name`: what
    /// the port does not read is left unread, and a document POI throws on fails, as it
    /// fails Tika's parse: see [`Thrown::failure`]. The failure is a reading that may differ,
    /// as the port may throw where POI does not.
    pub fn declined_by<'a>(self, tika_name: &'static str) -> Decline<'a> {
        match self {
            Unread::Unported(unported) => unported.into(),
            Unread::Thrown(thrown) => Decline::Differs {
                difference: Difference(format!(
                    "POI throws on the document, which fails Tika's parse with POI's exception: \
                     {thrown}"
                )),
                reading: Box::new(move |_| Err(thrown.failure(tika_name))),
            },
        }
    }
}

/// Why a parser reading a document as it hands its content on stops: it declines the
/// document, or handing its content on failed.
#[derive(Debug)]
pub(crate) enum Halt<E> {
    Declined(Decline<'static>),
    Written(E),
}

impl<E> From<Decline<'static>> for Halt<E> {
    fn from(declined: Decline<'static>) -> Self {
        Halt::Declined(declined)
    }
}

impl<E> From<Unread> for Halt<E> {
    fn from(unread: Unread) -> Self {
        Halt::Declined(unread.into())
    }
}

impl<E> From<Thrown> for Halt<E> {
    fn from(thrown: Thrown) -> Self {
        Halt::Declined(thrown.into())
    }
}

impl<E> From<Unported> for Halt<E> {
    fn from(unported: Unported) -> Self {
        Halt::Declined(unported.into())
    }
}

/// What reading a document as far as deciding whether it is extracted, its content let go,
/// came to.
pub(crate) fn planned<T>(read: Result<T, Halt<Infallible>>) -> Result<T, Decline<'static>> {
    read.map_err(|halt| match halt {
        Halt::Declined(declined) => declined,
        Halt::Written(never) => match never {},
    })
}

/// What reading a document again as it is written came to: where it declines the document,
/// which reading it before did not, it fails.
pub(crate) fn written<T>(read: Result<T, Halt<Failure>>) -> Result<T, Failure> {
    read.map_err(|halt| match halt {
        Halt::Written(failure) => failure,
        Halt::Declined(declined) => Failure::Extraction(Error::ParseError(format!(
            "the document reads otherwise the second time it is read: {declined:?}"
        ))),
    })
}

/// The part `part` takes of bytes, in place where they are borrowed.
pub(crate) fn narrowed<'a>(bytes: &Cow<'a, [u8]>, part: impl Fn(&[u8]) -> &[u8]) -> Cow<'a, [u8]> {
    match *bytes {
        Cow::Borrowed(bytes) => Cow::Borrowed(part(bytes)),
        Cow::Owned(ref bytes) => Cow::Owned(part(bytes).to_vec()),
    }
}

/// `length` bytes from `at`, read one after the other as `LittleEndian` reads them: past the
/// end, Java's array access throws on the first index outside the data.
fn slice(data: &[u8], at: usize, length: usize) -> Read<&[u8]> {
    at.checked_add(length)
        .and_then(|end| data.get(at..end))
        .ok_or_else(|| out_of_bounds(at.max(data.len()), data.len()))
}

/// Java's `ArrayIndexOutOfBoundsException` on `index` of an array of `length`.
pub(crate) fn out_of_bounds(index: usize, length: usize) -> Thrown {
    Thrown::runtime(
        ARRAY_INDEX_OUT_OF_BOUNDS,
        format!("Index {index} out of bounds for length {length}"),
    )
}

/// `IOUtils.safelyAllocateCheck(length, max)`: POI's limit on the arrays it allocates.
pub(crate) fn safely_allocate_check(length: i64, max: i64) -> Read<()> {
    if length < 0 {
        return record_format(format!(
            "Can't allocate an array of length < 0, but had {length} and {max}"
        ));
    }
    if length > i64::from(i32::MAX) {
        return record_format(format!("Can't allocate an array > {}", i32::MAX));
    }
    if length > max {
        return record_format(format!(
            "Tried to allocate an array of length {}, but the maximum length for this record \
             type is {}.\nIf the file is not corrupt and not large, please open an issue on \
             bugzilla to request \nincreasing the maximum allowable size for this record \
             type.\nYou can set a higher override value with IOUtils.setByteArrayMaxOverride()",
            grouped(length),
            grouped(max)
        ));
    }
    Ok(())
}

/// A number as Java's `%,d` writes it in the root locale: its thousands grouped with commas.
fn grouped(number: i64) -> String {
    let digits = number.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if number < 0 {
        out.push('-');
    }
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub(crate) fn u8_at(data: &[u8], at: usize) -> Read<u8> {
    Ok(slice(data, at, 1)?[0])
}

/// `LittleEndian.getShort`.
pub(crate) fn i16_at(data: &[u8], at: usize) -> Read<i16> {
    let bytes = slice(data, at, 2)?;
    Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
}

/// `LittleEndian.getUShort`.
pub(crate) fn u16_at(data: &[u8], at: usize) -> Read<u16> {
    Ok(i16_at(data, at)? as u16)
}

/// `LittleEndian.getInt`.
pub(crate) fn i32_at(data: &[u8], at: usize) -> Read<i32> {
    let bytes = slice(data, at, 4)?;
    Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// `IOUtils.safelyClone(data, offset, length, max)`: the bytes from `offset`, as many as there
/// are up to `length`.
pub(crate) fn safely_clone(data: &[u8], offset: i64, length: i64, max: i64) -> Read<Vec<u8>> {
    safely_borrow(data, offset, length, max).map(<[u8]>::to_vec)
}

/// The bytes [`safely_clone`] copies, where they are. A negative offset, length or maximum
/// throws POI's `RecordFormatException`, its `length` spelt right where POI misspells it (#185).
pub(crate) fn safely_borrow(data: &[u8], offset: i64, length: i64, max: i64) -> Read<&[u8]> {
    if offset < 0 || length < 0 || max < 0 {
        return record_format(format!(
            "Invalid offset/length specified: offset: {offset}, length: {length}, maxLength: \
             {max}"
        ));
    }
    let real = (data.len() as i64 - offset).min(length);
    safely_allocate_check(real, max)?;
    let start = offset as usize;
    Ok(&data[start..start + real as usize])
}

/// `InflaterInputStream` read to its end, or to `limit` bytes where a `BoundedInputStream` stops
/// it there: the inflated data, or the exception Java's stream throws first, the zlib stream
/// broken or ending early, as the first line of its stack trace.
pub(crate) fn inflate(data: &[u8], limit: Option<usize>) -> Read<Vec<u8>> {
    let mut inflater = flate2::Decompress::new(true);
    let mut out = Vec::new();
    loop {
        let written = out.len();
        if limit.is_some_and(|limit| written >= limit) {
            return Ok(out);
        }
        let room = written.max(data.len().saturating_mul(4)).max(64);
        let end = limit.map_or(written + room, |limit| limit.min(written + room));
        out.resize(end, 0);
        let consumed = inflater.total_in() as usize;
        let before = inflater.total_out();
        let status = inflater.decompress(
            &data[consumed..],
            &mut out[written..],
            flate2::FlushDecompress::None,
        );
        let produced = (inflater.total_out() - before) as usize;
        out.truncate(written + produced);
        match status {
            Ok(flate2::Status::StreamEnd) => return Ok(out),
            Ok(_) if inflater.total_in() as usize == data.len() && produced == 0 => {
                return Err(Thrown::io(EOF, UNEXPECTED_ZLIB_END));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(Thrown::io(
                    ZIP_EXCEPTION,
                    error.message().unwrap_or("Invalid ZLIB data format"),
                ))
            }
        }
    }
}

/// `InflaterInputStream` with `new Inflater(true)`: raw deflate data read from `input` as it is
/// inflated, to the end of the deflate stream, what follows it unread. What Java's stream
/// throws, the deflate stream broken or ending early, is the error's [`Thrown`], which
/// [`Thrown::of`] takes back.
pub(crate) struct RawInflater<R> {
    input: R,
    inflater: flate2::Decompress,
    buffer: Box<[u8; INFLATER_BUFFER]>,
    start: usize,
    end: usize,
    input_ended: bool,
    stream_ended: bool,
}

/// How many bytes of its input [`RawInflater`] reads at a time.
const INFLATER_BUFFER: usize = 8192;

impl<R: std::io::Read> RawInflater<R> {
    pub(crate) fn new(input: R) -> Self {
        Self {
            input,
            inflater: flate2::Decompress::new(false),
            buffer: Box::new([0; INFLATER_BUFFER]),
            start: 0,
            end: 0,
            input_ended: false,
            stream_ended: false,
        }
    }
}

impl<R: std::io::Read> std::io::Read for RawInflater<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.stream_ended || out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.start == self.end && !self.input_ended {
                self.end = self.input.read(&mut self.buffer[..])?;
                self.start = 0;
                self.input_ended = self.end == 0;
            }
            let (read_before, written_before) =
                (self.inflater.total_in(), self.inflater.total_out());
            let status = self
                .inflater
                .decompress(
                    &self.buffer[self.start..self.end],
                    out,
                    flate2::FlushDecompress::None,
                )
                .map_err(|error| {
                    std::io::Error::other(Thrown::io(
                        ZIP_EXCEPTION,
                        error.message().unwrap_or("Invalid ZLIB data format"),
                    ))
                })?;
            self.start += (self.inflater.total_in() - read_before) as usize;
            let produced = (self.inflater.total_out() - written_before) as usize;
            if status == flate2::Status::StreamEnd {
                self.stream_ended = true;
                return Ok(produced);
            }
            if produced > 0 {
                return Ok(produced);
            }
            if self.input_ended && self.start == self.end {
                return Err(std::io::Error::other(Thrown::io(EOF, UNEXPECTED_ZLIB_END)));
            }
        }
    }
}

impl std::error::Error for Thrown {}

impl Thrown {
    /// The exception a reader's error is: the [`Thrown`] it carries, else an `IOException` with
    /// its message.
    pub(crate) fn of(error: std::io::Error) -> Self {
        match error.downcast::<Thrown>() {
            Ok(thrown) => thrown,
            Err(error) => Thrown::io("java.io.IOException", error.to_string()),
        }
    }
}

/// How a subclass of POI's `LZWDecompresser` reads its data.
pub(crate) struct Lzw {
    /// Whether a set flag bit marks a dictionary reference, not a byte.
    pub(crate) mask_means_compressed: bool,
    /// How much longer than its length field a reference copies.
    pub(crate) code_length_increase: usize,
    /// Whether a reference's position is its first byte and the high half of its second.
    pub(crate) position_is_big_endian: bool,
    /// `populateDictionary`: what the dictionary starts with.
    pub(crate) preload: &'static [u8],
    /// `adjustDictionaryOffset`.
    pub(crate) adjust: fn(usize) -> usize,
}

/// `LZWDecompresser`'s dictionary of the last 4096 bytes.
const LZW_DICTIONARY_SIZE: usize = 0x1000;
const LZW_DICTIONARY_MASK: usize = 0xFFF;

impl Lzw {
    /// `LZWDecompresser.decompress`: flags of eight items, each a byte or a reference into the
    /// dictionary of what came before, until the data ends.
    pub(crate) fn decompress(&self, data: &[u8]) -> Vec<u8> {
        let mut dictionary = [0u8; LZW_DICTIONARY_SIZE];
        dictionary[..self.preload.len()].copy_from_slice(self.preload);
        let mut position = self.preload.len();
        let mut out = Vec::with_capacity(data.len() * 2);
        let mut input = data.iter().copied();
        while let Some(flag) = input.next() {
            for bit in 0..8 {
                if (flag & (1 << bit) != 0) != self.mask_means_compressed {
                    if let Some(byte) = input.next() {
                        dictionary[position & LZW_DICTIONARY_MASK] = byte;
                        position += 1;
                        out.push(byte);
                    }
                    continue;
                }
                let (Some(first), Some(second)) = (input.next(), input.next()) else {
                    break;
                };
                let length = usize::from(second & 0x0F) + self.code_length_increase;
                let pointer = if self.position_is_big_endian {
                    (usize::from(first) << 4) + (usize::from(second) >> 4)
                } else {
                    usize::from(first) + (usize::from(second & 0xF0) << 4)
                };
                let pointer = (self.adjust)(pointer);
                for i in 0..length {
                    let byte = dictionary[(pointer + i) & LZW_DICTIONARY_MASK];
                    dictionary[(position + i) & LZW_DICTIONARY_MASK] = byte;
                    out.push(byte);
                }
                position += length;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throws_on_the_first_index_past_the_data_as_java_does() {
        assert_eq!(i32_at(&[1, 2, 3], 1), Err(out_of_bounds(3, 3)),);
        assert_eq!(
            u16_at(&[1, 2, 3], 7).unwrap_err().to_string(),
            "java.lang.ArrayIndexOutOfBoundsException: Index 7 out of bounds for length 3"
        );
    }

    /// `safelyClone` refuses a negative offset, length or maximum as POI does, with `length`
    /// spelt right where POI writes `lenght` (#185).
    #[test]
    fn refuses_negative_ranges_with_their_message_spelt_right() {
        assert_eq!(
            safely_clone(&[1, 2, 3], -1, 2, 512).unwrap_err(),
            Thrown::record_format(
                "Invalid offset/length specified: offset: -1, length: 2, maxLength: 512"
            )
        );
    }

    #[test]
    fn refuses_allocations_as_ioutils_does() {
        assert_eq!(safely_allocate_check(100, 100), Ok(()));
        assert_eq!(
            safely_allocate_check(1_234_567, 100_000)
                .unwrap_err()
                .to_string(),
            "org.apache.poi.util.RecordFormatException: Tried to allocate an array of length \
             1,234,567, but the maximum length for this record type is 100,000.\nIf the file is \
             not corrupt and not large, please open an issue on bugzilla to request \
             \nincreasing the maximum allowable size for this record type.\nYou can set a \
             higher override value with IOUtils.setByteArrayMaxOverride()"
        );
        assert_eq!(
            safely_allocate_check(-1, 10).unwrap_err().to_string(),
            "org.apache.poi.util.RecordFormatException: Can't allocate an array of length < 0, \
             but had -1 and 10"
        );
    }

    #[test]
    fn fails_with_tikas_wording_where_the_exception_is_javas() {
        let failure = Thrown::record_format("Bad special value code (9)").failure(OFFICE_PARSER);
        let Failure::Extraction(error) = failure else {
            panic!("an extraction failure: {failure:?}");
        };
        assert_eq!(
            error.to_string(),
            "Parse error occurred : org.apache.tika.exception.TikaException: Unexpected \
             RuntimeException from org.apache.tika.parser.microsoft.OfficeParser <- caused by \
             org.apache.poi.util.RecordFormatException: Bad special value code (9)"
        );
        assert!(matches!(
            Thrown::new("the port's account").failure(OFFICE_PARSER),
            Failure::Malformed(reason) if reason.ends_with("the port's account")
        ));
    }

    #[test]
    fn inflates_only_a_complete_stream_or_as_far_as_a_limit() {
        use std::io::Write;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"metafile").unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(inflate(&compressed, None), Ok(b"metafile".to_vec()));
        assert_eq!(
            inflate(&compressed[..compressed.len() - 5], None),
            Err(Thrown::io(EOF, UNEXPECTED_ZLIB_END))
        );
        assert_eq!(
            inflate(b"not zlib", None),
            Err(Thrown::io(ZIP_EXCEPTION, "incorrect header check"))
        );
        let mut bad_check = compressed.clone();
        *bad_check.last_mut().unwrap() ^= 1;
        assert_eq!(
            inflate(&bad_check, None),
            Err(Thrown::io(ZIP_EXCEPTION, "incorrect data check"))
        );
        assert_eq!(inflate(&bad_check, Some(4)), Ok(b"meta".to_vec()));
        assert_eq!(inflate(&compressed, Some(20)), Ok(b"metafile".to_vec()));
    }

    #[test]
    fn inflates_raw_deflate_as_read_to_its_end_leaving_what_follows() {
        use std::io::{Read as _, Write as _};
        let inflated = |data: &[u8]| {
            let mut out = Vec::new();
            RawInflater::new(data)
                .read_to_end(&mut out)
                .map(|_| out)
                .map_err(Thrown::of)
        };
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&b"section".repeat(5000)).unwrap();
        let mut compressed = encoder.finish().unwrap();
        assert_eq!(
            inflated(&compressed[..compressed.len() - 2]),
            Err(Thrown::io(EOF, UNEXPECTED_ZLIB_END))
        );
        compressed.extend(b"padding");
        assert_eq!(inflated(&compressed), Ok(b"section".repeat(5000)));
        assert!(matches!(
            inflated(&[0xFF; 16]),
            Err(Thrown::Java(thrown)) if thrown.to_string().starts_with(ZIP_EXCEPTION)
        ));
    }
}
