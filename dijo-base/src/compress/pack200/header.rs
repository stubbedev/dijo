//! The segment header (JSR 200 section 5.2): the magic number and version, the archive options,
//! and the counts that size the bands, read as commons-compress's `SegmentHeader` reads them.

use super::coding::{Cursor, UNSIGNED5};
use super::{malformed, Result};

const MAGIC: [u8; 4] = [0xCA, 0xFE, 0xD0, 0x0D];

pub(super) const HAVE_SPECIAL_FORMATS: u32 = 1 << 0;
pub(super) const HAVE_CP_NUMBERS: u32 = 1 << 1;
pub(super) const HAVE_ALL_CODE_FLAGS: u32 = 1 << 2;
pub(super) const HAVE_FILE_HEADERS: u32 = 1 << 4;
pub(super) const DEFLATE_HINT: u32 = 1 << 5;
pub(super) const HAVE_FILE_MODTIME: u32 = 1 << 6;
pub(super) const HAVE_FILE_OPTIONS: u32 = 1 << 7;
pub(super) const HAVE_FILE_SIZE_HI: u32 = 1 << 8;
pub(super) const HAVE_CLASS_FLAGS_HI: u32 = 1 << 9;
pub(super) const HAVE_FIELD_FLAGS_HI: u32 = 1 << 10;
pub(super) const HAVE_METHOD_FLAGS_HI: u32 = 1 << 11;
pub(super) const HAVE_CODE_FLAGS_HI: u32 = 1 << 12;
/// The options the specification leaves unused, which must be clear.
const UNUSED: u32 = !0x1FF7;

/// The header of a segment.
#[derive(Debug)]
pub(super) struct Header {
    pub(super) options: u32,
    /// `#archive_modtime`, in seconds since the epoch, zero for none.
    pub(super) modtime: u32,
    pub(super) file_count: usize,
    pub(super) band_headers_size: usize,
    pub(super) attr_definition_count: usize,
    /// The counts of the twelve constant pools, in `cp_All` order.
    pub(super) pool_counts: [usize; 12],
    pub(super) ic_count: usize,
    pub(super) default_minor: u16,
    pub(super) default_major: u16,
    pub(super) class_count: usize,
}

impl Header {
    /// The header at the start of `input`.
    pub(super) fn read(input: &mut Cursor) -> Result<Header> {
        if input.bytes(4).ok() != Some(&MAGIC[..]) {
            return Err(malformed("Bad header"));
        }
        let minor = scalar(input)?;
        if minor != 7 {
            return Err(malformed("Invalid segment minor version"));
        }
        let major = scalar(input)?;
        if major != 150 {
            return Err(malformed(format!("Invalid segment major version: {major}")));
        }
        let options = scalar(input)?;
        if options & UNUSED != 0 {
            return Err(malformed("Some unused flags are non-zero"));
        }
        let mut header = Header {
            options,
            modtime: 0,
            file_count: 0,
            band_headers_size: 0,
            attr_definition_count: 0,
            pool_counts: [0; 12],
            ic_count: 0,
            default_minor: 0,
            default_major: 0,
            class_count: 0,
        };
        if header.has(HAVE_FILE_HEADERS) {
            scalar(input)?;
            scalar(input)?;
            scalar(input)?;
            header.modtime = scalar(input)?;
            header.file_count = count(input)?;
        }
        if header.has(HAVE_SPECIAL_FORMATS) {
            header.band_headers_size = count(input)?;
            header.attr_definition_count = count(input)?;
        }
        header.pool_counts[0] = count(input)?;
        if header.has(HAVE_CP_NUMBERS) {
            for at in 1..5 {
                header.pool_counts[at] = count(input)?;
            }
        }
        for at in 5..12 {
            header.pool_counts[at] = count(input)?;
        }
        header.ic_count = count(input)?;
        header.default_minor = scalar(input)? as u16;
        header.default_major = scalar(input)? as u16;
        header.class_count = count(input)?;
        Ok(header)
    }

    /// Whether the archive option `option` is set.
    pub(super) fn has(&self, option: u32) -> bool {
        self.options & option != 0
    }
}

/// The next header value, as the 32 bits it stands for.
fn scalar(input: &mut Cursor) -> Result<u32> {
    Ok(UNSIGNED5.scalar(input)? as u32)
}

/// The next header value, a count, which must be no more than the 32-bit signed integers
/// commons-compress reads it as hold.
fn count(input: &mut Cursor) -> Result<usize> {
    let value = scalar(input)?;
    usize::try_from(value as i32).map_err(|_| malformed(format!("the negative count {value}")))
}
