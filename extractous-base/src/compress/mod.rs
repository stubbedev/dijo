//! Ports of the parts of Apache commons-compress 1.28 that Tika reads archives and compressed
//! files with, failing and recovering where commons-compress does.
//!
//! A compressed stream is decoded whole before the native parsers hand its content on, to a
//! [`Spool`]: in memory as far as a file is read whole, beyond that to a temporary file that is
//! mapped, so that what a decoder holds stays bounded whatever the stream decodes into. Each
//! decoder answers with all of it, or the first [`decode_limit`] bytes of it, past which the
//! stream is taken for a bomb ([`Limited`]), or with why commons-compress would not give all of
//! it: [`Undecoded`]. An archive that stores its files whole gives them in place.

pub mod ar;
pub mod archiver;
pub mod brotli;
pub mod bzip2;
pub mod compressor;
pub mod cpio;
pub mod gzip;
pub mod lz4;
pub mod lzw;
pub mod seven_z;
pub mod snappy;
pub mod tar;
pub mod tar_header;
pub mod xz;
pub mod zip;
pub mod zlib;
pub mod zstd;

use std::borrow::Cow;
use std::fmt;
use std::io::{self, Read, Write};

use crate::formats::secure::MAXIMUM_RATIO;
use crate::input::{Input, Pages, Spool};
use crate::java::time::civil;

/// `CompressorParser`'s `memoryLimitInKb`, which the decoders that size their memory by the
/// stream keep within.
pub(crate) const MEMORY_LIMIT_KIB: u32 = 100_000;

/// The most bytes a stream is decoded into memory, and the fewest a stream decoded to a spool
/// may decode into whatever its ratio ([`decode_limit`]).
pub const MAX_DECODED: usize = 256 * 1024 * 1024;

/// The most bytes a stream of `compressed` bytes is decoded into a spool: [`MAX_DECODED`], or a
/// hundred bytes per compressed byte where that is more, the ratio of output to input past
/// which Tika's `SecureContentHandler` takes a document for a zip bomb. Tika's path streams what
/// it decodes into the parser, whose output that guard stops; the native path decodes a stream
/// to a spool before its parser reads it, so it stops a bomb as it decodes, before the bomb
/// fills the disk.
pub fn decode_limit(compressed: usize) -> u64 {
    (MAX_DECODED as u64).max((compressed as u64).saturating_mul(MAXIMUM_RATIO))
}

/// Why a compressed stream or an archive is not read natively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undecoded {
    /// Reading it fails, for this reason, as reading it fails on Tika's path.
    Fails(String),
    /// It decodes into more than [`decode_limit`] bytes, or, decoded into memory, than
    /// [`MAX_DECODED`] bytes.
    TooLarge,
    /// It holds what the native readers do not read the way Tika's path does.
    Unported(&'static str),
    /// It is encrypted, and no password was given for it.
    Encrypted,
    /// It is encrypted, and the password given does not decrypt it, where Tika's decoders fail
    /// on the garbled data with errors of their own.
    BadPassword,
    /// Decoding it needs more memory than [`MEMORY_LIMIT_KIB`], as this says, for which Tika's
    /// `CompressorParser` fails it with a `TikaMemoryLimitException`.
    MemoryLimit(String),
    /// Spooling what it decodes to a temporary file fails, for this reason.
    Unspooled(String),
}

impl fmt::Display for Undecoded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Undecoded::Fails(reason) => write!(f, "reading it fails: {reason}"),
            Undecoded::TooLarge => write!(
                f,
                "it decodes into more than {MAX_DECODED} bytes and {MAXIMUM_RATIO} bytes per \
                 compressed byte"
            ),
            Undecoded::Unported(what) => write!(f, "the native readers do not read {what}"),
            Undecoded::Encrypted => write!(f, "it is encrypted, and no password was given"),
            Undecoded::BadPassword => {
                write!(
                    f,
                    "it is encrypted, and the password given does not decrypt it"
                )
            }
            Undecoded::MemoryLimit(reason) => {
                write!(f, "decoding it needs more memory than allowed: {reason}")
            }
            Undecoded::Unspooled(reason) => {
                write!(f, "spooling what it decodes fails: {reason}")
            }
        }
    }
}

fn throws(reason: impl Into<String>) -> Undecoded {
    Undecoded::Fails(reason.into())
}

/// A file of an archive, as the archive readers give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveFile<'a> {
    pub name: String,
    pub size: u64,
    /// The modification time in milliseconds since the epoch, if the archive records one.
    pub modified_millis: Option<i64>,
    /// The creation time in milliseconds since the epoch, if the archive records one.
    pub created_millis: Option<i64>,
    pub directory: bool,
    /// The data, none for a directory: the archive's own bytes where it stores them whole, else
    /// decoded to a spool.
    pub content: Input<'a>,
}

/// Bytes decoded as US-ASCII, as `ArchiveUtils.toAsciiString` decodes them: each byte beyond
/// ASCII replaced by U+FFFD.
fn ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii() {
                char::from(b)
            } else {
                '\u{FFFD}'
            }
        })
        .collect()
}

/// The CRC-32 of `bytes`.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(bytes);
    crc.sum()
}

/// The DOS date and time `dos` as a wall-clock time in UTC, in milliseconds since the epoch, if
/// its fields are in range. The formats that record DOS times name no zone, and Java's readers
/// take them in the JVM's default zone with a lenient `Calendar`, so the instant depends on the
/// machine and out-of-range fields roll over (#99, #122).
pub fn dos_millis(dos: i64) -> Option<i64> {
    let year = (dos >> 25 & 0x7F) + 1980;
    let month = dos >> 21 & 0x0F;
    let day = dos >> 16 & 0x1F;
    let (hour, minute, second) = (dos >> 11 & 0x1F, dos >> 5 & 0x3F, dos << 1 & 0x3E);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if civil::is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=days_in_month).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = civil::days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000)
}

/// What a stream or an archive decodes into, within [`MAX_DECODED`] bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Limited<T> {
    /// All of it.
    Whole(T),
    /// What it decodes into up to [`MAX_DECODED`] bytes, where it decodes into more: an
    /// archive's files up to the one that passes the limit, which is cut there.
    Cut(T),
}

impl<T> Limited<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Limited<U> {
        match self {
            Limited::Whole(decoded) => Limited::Whole(f(decoded)),
            Limited::Cut(decoded) => Limited::Cut(f(decoded)),
        }
    }

    /// All of it, or [`Undecoded::TooLarge`] where it is cut.
    pub fn whole(self) -> Result<T, Undecoded> {
        match self {
            Limited::Whole(decoded) => Ok(decoded),
            Limited::Cut(_) => Err(Undecoded::TooLarge),
        }
    }
}

impl<'a> Limited<ArchiveFile<'a>> {
    /// Adds the file to the `files` of an archive read before it: all of them, cut, where it is
    /// cut, which ends the reading of the archive.
    fn add_to(self, files: &mut Vec<ArchiveFile<'a>>) -> Option<Limited<Vec<ArchiveFile<'a>>>> {
        match self {
            Limited::Whole(file) => {
                files.push(file);
                None
            }
            Limited::Cut(file) => {
                files.push(file);
                Some(Limited::Cut(std::mem::take(files)))
            }
        }
    }
}

/// The bytes a decoder hands on at a time.
const CHUNK: usize = 64 * 1024;

/// Where a stream is decoded to, and what it then holds.
pub trait Held: Write + Sized {
    /// What it holds once the stream is decoded, or the bytes of data stored whole in place.
    type Bytes<'a>;

    /// Data stored whole, `bytes`, whose pages are `pages`, as what it holds.
    fn stored<'a>(bytes: &'a [u8], pages: Pages<'a>) -> Self::Bytes<'a>;

    /// What it holds once the stream is decoded.
    fn held<'a>(self) -> Result<Self::Bytes<'a>, Undecoded>;
}

/// Memory, for a reader that needs a stream whole there.
impl Held for Vec<u8> {
    type Bytes<'a> = Cow<'a, [u8]>;

    fn stored<'a>(bytes: &'a [u8], _: Pages<'a>) -> Cow<'a, [u8]> {
        Cow::Borrowed(bytes)
    }

    fn held<'a>(self) -> Result<Cow<'a, [u8]>, Undecoded> {
        Ok(Cow::Owned(self))
    }
}

/// A spool, for a stream handed on as a document.
impl Held for Spool {
    type Bytes<'a> = Input<'a>;

    fn stored<'a>(bytes: &'a [u8], pages: Pages<'a>) -> Input<'a> {
        Input::Within(bytes, pages)
    }

    fn held<'a>(self) -> Result<Input<'a>, Undecoded> {
        self.finish().map_err(unspooled)
    }
}

/// Nothing, for a stream decoded only to tell whether it decodes.
impl Held for io::Sink {
    type Bytes<'a> = ();

    fn stored<'a>(_: &'a [u8], _: Pages<'a>) -> Self::Bytes<'a> {}

    fn held<'a>(self) -> Result<Self::Bytes<'a>, Undecoded> {
        Ok(())
    }
}

/// What a stream decodes into, as it is decoded, up to a limit: into `W`, a [`Spool`], or, for a
/// reader that needs it whole in memory, a `Vec`.
pub struct Decoding<W> {
    out: W,
    limit: u64,
    written: u64,
    cut: bool,
}

impl Decoding<Spool> {
    /// The decoding of a stream of `compressed` bytes to a spool, up to [`decode_limit`] bytes.
    /// `declared` is what its container says it decodes into, if it says: a hint, not a limit,
    /// which sends a stream declared too large to hold in memory to a file from the first.
    pub fn spooled(compressed: usize, declared: Option<u64>) -> Self {
        Self::new(Spool::new(declared), decode_limit(compressed))
    }
}

impl Decoding<io::Sink> {
    /// The decoding of a stream of `compressed` bytes that keeps nothing, up to the limit of
    /// [`Decoding::spooled`].
    pub fn discarded(compressed: usize) -> Self {
        Self::new(io::sink(), decode_limit(compressed))
    }
}

impl Decoding<Vec<u8>> {
    /// The decoding of a stream into memory, up to [`MAX_DECODED`] bytes.
    pub fn in_memory() -> Self {
        Self::new(Vec::new(), MAX_DECODED as u64)
    }
}

impl<W: Held> Decoding<W> {
    fn new(out: W, limit: u64) -> Self {
        Self {
            out,
            limit,
            written: 0,
            cut: false,
        }
    }

    /// Adds `decoded`, up to the limit, past which the stream is cut.
    pub fn write(&mut self, decoded: &[u8]) -> Result<(), Undecoded> {
        let room = self.limit - self.written;
        let kept = usize::try_from(room).map_or(decoded.len(), |room| room.min(decoded.len()));
        self.out.write_all(&decoded[..kept]).map_err(unspooled)?;
        self.written += kept as u64;
        self.cut |= kept < decoded.len();
        Ok(())
    }

    /// Whether the stream decoded past the limit, which ends its decoding.
    pub fn is_cut(&self) -> bool {
        self.cut
    }

    /// How many bytes it holds.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// What the stream decoded into: all of it, or cut at the limit.
    pub fn finish<'a>(self) -> Result<Limited<W::Bytes<'a>>, Undecoded> {
        let cut = self.cut;
        let held = self.out.held()?;
        Ok(if cut {
            Limited::Cut(held)
        } else {
            Limited::Whole(held)
        })
    }

    /// Adds what `reader`, a crate's decoder, gives up to its end or the limit, failing where it
    /// fails.
    pub fn read_from(&mut self, mut reader: impl Read) -> Result<(), Undecoded> {
        let mut chunk = vec![0; CHUNK];
        while !self.cut {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => self.write(&chunk[..read])?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(decoder_failure(error)),
            }
        }
        Ok(())
    }
}

fn unspooled(error: io::Error) -> Undecoded {
    Undecoded::Unspooled(error.to_string())
}

/// What `reader`, a crate's decoder of a stream of `compressed` bytes that declares it decodes
/// into `declared`, if it does, gives up to its end or [`decode_limit`] bytes, spooled, failing
/// where it fails.
fn spool_limited(
    reader: impl Read,
    compressed: usize,
    declared: Option<u64>,
) -> Result<Limited<Input<'static>>, Undecoded> {
    let mut decoding = Decoding::spooled(compressed, declared);
    decoding.read_from(reader)?;
    decoding.finish()
}

/// Why a crate's decoder failed: its memory limit, which the decoders given one report as
/// [`std::io::ErrorKind::OutOfMemory`], or else the error itself.
fn decoder_failure(error: std::io::Error) -> Undecoded {
    if error.kind() == std::io::ErrorKind::OutOfMemory {
        Undecoded::MemoryLimit(format!("{error}, the limit being {MEMORY_LIMIT_KIB} KiB"))
    } else {
        throws(error.to_string())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn reads_dos_times_as_utc_wall_clock_times() {
        let dos = |year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64| {
            (year - 1980) << 25 | month << 21 | day << 16 | hour << 11 | minute << 5 | second >> 1
        };
        assert_eq!(
            dos_millis(dos(2024, 2, 29, 23, 59, 58)),
            Some(1_709_251_198_000)
        );
        assert_eq!(dos_millis(0), None);
        assert_eq!(dos_millis(dos(2023, 2, 29, 0, 0, 0)), None);
        assert_eq!(dos_millis(dos(2023, 13, 1, 0, 0, 0)), None);
        assert_eq!(dos_millis(dos(2023, 1, 1, 24, 0, 0)), None);
        assert_eq!(dos_millis(dos(2023, 1, 1, 0, 60, 0)), None);
        assert_eq!(dos_millis(dos(2023, 1, 1, 0, 0, 60)), None);
    }

    /// Bits written least significant first, as deflate packs them.
    #[derive(Default)]
    struct Bits {
        out: Vec<u8>,
        cache: u64,
        count: u32,
    }

    impl Bits {
        fn put(&mut self, value: u64, length: u32) {
            self.cache |= value << self.count;
            self.count += length;
            while self.count >= 8 {
                self.out.push(self.cache as u8);
                self.cache >>= 8;
                self.count -= 8;
            }
        }

        /// A Huffman code, which deflate packs most significant bit first.
        fn code(&mut self, code: u64, length: u32) {
            self.put(code.reverse_bits() >> (64 - length), length);
        }

        fn finish(mut self) -> Vec<u8> {
            if self.count > 0 {
                self.out.push(self.cache as u8);
            }
            self.out
        }
    }

    /// Raw deflate data of one block with fixed codes: a zero byte, then `copies` copies of the
    /// 258 bytes before, so `1 + 258 * copies` zero bytes in all.
    pub(crate) fn deflated_zeros(copies: usize) -> Vec<u8> {
        let mut bits = Bits::default();
        bits.put(1, 1);
        bits.put(1, 2);
        bits.code(0x30, 8);
        for _ in 0..copies {
            bits.code(0xC5, 8);
            bits.code(0, 5);
        }
        bits.code(0, 7);
        bits.finish()
    }

    #[test]
    fn decodes_up_to_a_hundred_bytes_per_compressed_byte_beyond_the_floor() {
        assert_eq!(decode_limit(0), MAX_DECODED as u64);
        assert_eq!(decode_limit(MAX_DECODED / 100), MAX_DECODED as u64);
        assert_eq!(decode_limit(10 << 20), 1000 << 20);
    }

    #[test]
    fn stops_decoding_past_the_limit_into_a_mapped_spool() {
        let small = deflated_zeros(3);
        let inflate = |data: &[u8]| {
            spool_limited(flate2::bufread::DeflateDecoder::new(data), data.len(), None)
        };
        assert_eq!(
            inflate(&small),
            Ok(Limited::Whole(Input::Borrowed(&[0; 775])))
        );
        let bomb = deflated_zeros(MAX_DECODED / 258 + 1);
        let Ok(Limited::Cut(cut)) = inflate(&bomb) else {
            panic!("a bomb is decoded whole");
        };
        assert!(cut.is_mapped());
        assert_eq!(cut.len(), MAX_DECODED);
        assert!(cut.iter().all(|&byte| byte == 0));
        assert_eq!(
            zip::inflate_into(&bomb, Decoding::in_memory()),
            Ok(zip::Content::Oversized(vec![0; MAX_DECODED].into()))
        );
    }

    #[test]
    fn reads_the_files_of_archives_that_store_them_in_place() {
        let entries: [(&str, &[u8]); 2] = [("a.txt", b"first"), ("b.txt", b"second")];
        let tar_entries = entries.map(|(name, content)| (name.as_bytes(), b'0', content));
        let tar = tar_header::tests::tar(&tar_entries);
        let ar = ar::tests::ar(&entries);
        let cpio = cpio::tests::cpio(&entries);
        let pages = Pages::IN_MEMORY;
        for (format, archive, files) in [
            (
                "tar",
                &tar,
                tar::files(&tar, pages).and_then(Limited::whole),
            ),
            ("ar", &ar, ar::files(&ar, pages)),
            ("cpio", &cpio, cpio::files(&cpio, pages)),
        ] {
            let files = files.unwrap_or_else(|undecoded| panic!("{format}: {undecoded}"));
            assert_eq!(files.len(), 2, "{format}");
            for (file, (name, content)) in files.iter().zip(entries) {
                assert_eq!(file.name, name, "{format}");
                let Input::Within(stored, _) = file.content else {
                    panic!("{format} copies {name}");
                };
                assert_eq!(stored, content, "{format}");
                assert!(
                    archive.as_ptr_range().contains(&stored.as_ptr()),
                    "{format}"
                );
            }
        }
    }
}
