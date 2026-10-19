//! A native Pack200 unpacker (JSR 200), a port of commons-compress's
//! `harmony.unpack200`, which Tika's `CompressorParser` reads `.pack` streams with.
//!
//! An archive is a sequence of segments, each read whole, band after band, in the order of
//! section 5 of the specification: the segment header with its band headers, the constant
//! pool bands, the attribute definition bands, the inner class bands, the class bands, the
//! bytecode bands and the file bands with the file bits ([`Segment::read`]). [`unpack`] reads
//! every segment of an archive into the files of the jar it was packed from, in order.
//!
//! An archive wrapped in gzip, or a jar that was stored rather than packed, both of which
//! commons-compress's `Archive` takes too, are left to the caller.

mod attributes;
mod bytecode;
mod classes;
mod coding;
mod files;
mod header;
mod ic;
mod layout;
mod pool;

use super::{throws, Undecoded, MAX_DECODED};
use attributes::Definitions;
use classes::Class;
use coding::{Bands, Cursor};
use files::File;
use header::Header;
use ic::Tuple;
use pool::Pool;

/// What reading an archive answers with: its values, or why it is not read.
type Result<T> = std::result::Result<T, Undecoded>;

/// The failure of an archive malformed as `reason` says, for which commons-compress throws a
/// `Pack200Exception` or a runtime exception of its own.
fn malformed(reason: impl Into<String>) -> Undecoded {
    throws(reason)
}

/// The UTF-16 code units the strings of an archive may hold together, so that a few bytes
/// of bands that build long strings out of shared prefixes cannot fill the memory.
#[derive(Debug)]
struct Strings {
    left: usize,
}

impl Strings {
    /// The budget of a whole archive: as many code units as [`MAX_DECODED`] bytes hold.
    fn new() -> Self {
        Self {
            left: MAX_DECODED / 2,
        }
    }

    /// Spends `units` code units of the budget, failing as too large once it is spent.
    fn spend(&mut self, units: usize) -> Result<()> {
        self.left = self.left.checked_sub(units).ok_or(Undecoded::TooLarge)?;
        Ok(())
    }
}

/// A file of the jar an archive was packed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JarEntry {
    /// Its name, as `file_name` spells it, or for a class file transmitted without one, the
    /// name of its class followed by `.class`.
    pub(crate) name: String,
    /// `archive_modtime` plus its `file_modtime`, in seconds since the epoch. commons-compress
    /// gives the entry `1000` times this, less the raw offset of the default time zone, as
    /// its time in milliseconds.
    pub(crate) modified_seconds: i64,
    /// Its content: the file bits, or for a class file the class file rebuilt.
    pub(crate) bytes: Vec<u8>,
}

/// A segment of an archive, all of its bands read.
#[derive(Debug)]
struct Segment<'a> {
    header: Header,
    pool: Pool,
    definitions: Definitions,
    /// `ic_All`.
    tuples: Vec<Tuple>,
    /// The classes, their codes holding their bytecodes.
    classes: Vec<Class>,
    /// The files, as the file bands give them.
    files: Vec<File<'a>>,
}

impl<'a> Segment<'a> {
    /// The segment at the start of `input`, which is left at its end, its strings counted
    /// against `strings`.
    fn read(input: &mut Cursor<'a>, strings: &mut Strings) -> Result<Segment<'a>> {
        let header = Header::read(input)?;
        let band_headers = input.bytes(header.band_headers_size)?;
        let mut bands = Bands::new(input.clone(), band_headers);
        let pool = Pool::read(&mut bands, &header, strings)?;
        let definitions = Definitions::read(&mut bands, &header, &pool)?;
        let tuples = ic::read_tuples(&mut bands, &header, &pool)?;
        let mut classes = classes::read_classes(&mut bands, &header, &pool, &definitions, &tuples)?;
        bytecode::read_bytecodes(&mut bands, &pool, &mut classes)?;
        let files = files::read_files(&mut bands, &header, &pool)?;
        *input = bands.input;
        Ok(Segment {
            header,
            pool,
            definitions,
            tuples,
            classes,
            files,
        })
    }

    /// Adds the files of the segment to `entries`, in the order of the file bands, each
    /// class file taking the next class, as commons-compress's `Segment.parseSegment` and
    /// `writeJar` do; `total` counts the bytes of all entries so far.
    fn entries(&self, entries: &mut Vec<JarEntry>, total: &mut usize) -> Result<()> {
        let mut next_class = 0;
        for file in &self.files {
            let (name, bytes) = if file.is_class() {
                let class = next_class;
                next_class += 1;
                if class >= self.classes.len() {
                    return Err(malformed(format!(
                        "the class file {} of a segment of {} classes",
                        class + 1,
                        self.classes.len()
                    )));
                }
                let name = if file.name.is_empty() {
                    let this = self.pool.classes[self.classes[class].this];
                    let mut name = String::from_utf16_lossy(&self.pool.utf8[this]);
                    name.push_str(".class");
                    name
                } else {
                    String::from_utf16_lossy(&file.name)
                };
                (name, self.class_file(class)?)
            } else {
                (String::from_utf16_lossy(&file.name), file.bits.to_vec())
            };
            *total = total
                .checked_add(bytes.len())
                .filter(|&total| total <= MAX_DECODED)
                .ok_or(Undecoded::TooLarge)?;
            entries.push(JarEntry {
                name,
                modified_seconds: i64::from(self.header.modtime as i32) + i64::from(file.modtime),
                bytes,
            });
        }
        Ok(())
    }

    /// The class file of the class at `class`, rebuilt from the bands.
    ///
    /// Class file reconstruction is not ported yet: this answers that the native readers do
    /// not read it, and is where the reconstruction belongs, with `self.pool`,
    /// `self.definitions`, `self.tuples` and `self.classes` at hand.
    fn class_file(&self, class: usize) -> Result<Vec<u8>> {
        let _ = (&self.definitions, &self.tuples, class);
        Err(Undecoded::Unported("the class files of Pack200 archives"))
    }
}

/// The files of the jar the Pack200 archive `input` was packed from, those of each of its
/// segments in turn, as commons-compress's `Pack200UnpackerAdapter` writes them. A segment
/// without file headers gives no files, as in commons-compress.
pub(crate) fn unpack(input: &[u8]) -> Result<Vec<JarEntry>> {
    let mut cursor = Cursor::new(input);
    let mut strings = Strings::new();
    let mut entries = Vec::new();
    let mut total = 0;
    while cursor.remaining() > 0 {
        let segment = Segment::read(&mut cursor, &mut strings)?;
        segment.entries(&mut entries, &mut total)?;
    }
    Ok(entries)
}
