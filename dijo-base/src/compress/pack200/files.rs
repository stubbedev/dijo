//! The file bands (JSR 200 section 5.11) and the file bits after them, read as
//! commons-compress's `FileBands.read` and `processFileBits` read them.

use super::coding::{Bands, DELTA5, UNSIGNED5};
use super::header::{Header, HAVE_FILE_MODTIME, HAVE_FILE_OPTIONS, HAVE_FILE_SIZE_HI};
use super::pool::{references, Pool};
use super::{malformed, Result};

/// The `file_options` bit of a file to be deflated.
pub(super) const DEFLATE: i32 = 1 << 0;

/// The `file_options` bit of a class file.
pub(super) const IS_CLASS: i32 = 1 << 1;

/// A file of a segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct File<'a> {
    /// Its `file_name`, empty for a class file named after its class.
    pub(super) name: Vec<u16>,
    /// Its `file_modtime`, in seconds after `archive_modtime`, zero where not transmitted.
    pub(super) modtime: i32,
    /// Its `file_options`, zero where not transmitted.
    pub(super) options: i32,
    /// Its file bits, of the `file_size` transmitted, empty for a class file rebuilt from the
    /// class bands.
    pub(super) bits: &'a [u8],
}

impl File<'_> {
    /// Whether it is a class file, which takes the next class of the segment: one marked so
    /// in `file_options`, or one without a name, as in commons-compress's `Segment`.
    pub(super) fn is_class(&self) -> bool {
        self.options & IS_CLASS != 0 || self.name.is_empty()
    }

    /// Whether it is to be deflated in the jar, by `file_options`; commons-compress deflates
    /// every file of a segment with the deflate hint set.
    pub(super) fn deflate(&self) -> bool {
        self.options & DEFLATE != 0
    }
}

/// The `header.file_count` files of a segment, read from the `file_bands`, `file_name`,
/// `file_size_hi` where transmitted, `file_size_lo`, `file_modtime` and `file_options` where
/// transmitted, then their bits, read straight from the stream.
pub(super) fn read_files<'a>(
    bands: &mut Bands<'a>,
    header: &Header,
    pool: &Pool,
) -> Result<Vec<File<'a>>> {
    let count = header.file_count;
    let names = references(bands.band(UNSIGNED5, count)?, pool.utf8.len(), "cp_Utf8")?;
    let high = if header.has(HAVE_FILE_SIZE_HI) {
        bands.band(UNSIGNED5, count)?
    } else {
        vec![0; count]
    };
    let low = bands.band(UNSIGNED5, count)?;
    let modtimes = if header.has(HAVE_FILE_MODTIME) {
        bands.band(DELTA5, count)?
    } else {
        vec![0; count]
    };
    let options = if header.has(HAVE_FILE_OPTIONS) {
        bands.band(UNSIGNED5, count)?
    } else {
        vec![0; count]
    };
    let mut files = Vec::with_capacity(count);
    for (((name, (high, low)), modtime), options) in names
        .into_iter()
        .zip(high.into_iter().zip(low))
        .zip(modtimes)
        .zip(options)
    {
        let size = (u64::from(high as u32) << 32) | u64::from(low as u32);
        let size = usize::try_from(size as i32)
            .map_err(|_| malformed(format!("the file size {size} past the 32-bit sizes")))?;
        files.push(File {
            name: pool.utf8[name].clone(),
            modtime,
            options,
            bits: bands.input.bytes(size)?,
        });
    }
    Ok(files)
}
