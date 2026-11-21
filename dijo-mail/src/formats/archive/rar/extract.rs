//! junrar 8.1.2's `Archive.extractFile` and `doExtractFile`, as Tika's parser calls them for each
//! file of an archive in turn through `getInputStream`: an entry of a solid archive continues
//! the unpacker's state from the one before it.
//!
//! An entry is extracted whole and checked against its CRC, or its extraction fails: junrar's
//! stream then throws on the read that reaches the entry's declared size or its end, which Tika's
//! parser takes as the end of the entry and records. What reaches the entry's parse before that
//! depends on how the reads of the parse and the writes of junrar's extraction thread into its
//! pipe of 32 KiB interleave; a parse reading it in one go gets nothing, and the port gives every
//! failed entry empty (#122). junrar extracts each entry on a thread of its own, so where Tika's
//! parse of a larger entry stops reading before its end, the next entry of a solid stream is
//! decoded from the state the stopped thread left; the port decodes every entry to its end.

use super::headers::{FileHeader, Format, Headers};
use super::unpack::{Method, Sum, Thrown, Unpack};
use crate::input::Input;

/// `Method` of a stored entry's header.
const STORED: u8 = 0x30;

/// What extracting an entry gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Extracted {
    /// The entry's data, extracted whole and checked.
    Data(Input<'static>),
    /// junrar's extraction fails, as it does on a broken header, an encrypted entry, which it
    /// has no password for, a checksum that does not match, data that continues in another
    /// volume and data its unpacker throws on.
    Failed,
    /// Why the port does not give the entry's data, which junrar gives.
    Unread(String),
}

impl From<Thrown> for Extracted {
    fn from(thrown: Thrown) -> Self {
        match thrown {
            Thrown::Java(_) => Extracted::Failed,
            Thrown::Unread(reason) => {
                Extracted::Unread(format!("unpacking it fails with {reason}"))
            }
        }
    }
}

/// Hands `each` what extracting each file of the archive gives, its data spooled, with the
/// file's index, in order, but of its directories, which Tika's parser does not open: one
/// file's data at a time, up to the first failure of `each`.
pub(super) fn each<E>(
    data: &[u8],
    headers: &Headers,
    mut each: impl FnMut(usize, Extracted) -> Result<(), E>,
) -> Result<(), E> {
    let mut unpack = Unpack::new(data);
    let mut extraction = Extraction {
        format: headers.format,
        solid: headers.solid,
        extracted: 0,
    };
    for (index, header) in headers.files.iter().enumerate() {
        if !header.is_directory() {
            each(index, extraction.extract_file(&mut unpack, header))?;
        }
    }
    Ok(())
}

/// What decides how the entries of an archive are extracted: its format, whether its main
/// header says it is solid, and, for RAR 1.4, how many entries junrar has unpacked or copied
/// out so far.
struct Extraction {
    format: Format,
    solid: bool,
    extracted: usize,
}

impl Extraction {
    /// Whether the entry of `header` continues the solid stream of the one before it: in RAR
    /// 1.5 and later as its header says; in RAR 1.4, which has no flag for it, every entry but
    /// the first junrar extracts where the main header says the archive is solid.
    fn solid(&mut self, header: &FileHeader) -> bool {
        match self.format {
            Format::Rar15 => header.is_solid(),
            Format::Rar14 => {
                self.extracted += 1;
                self.extracted > 1 && self.solid
            }
        }
    }

    /// The version of the unpacker of the entry of `header`: a compressed RAR 1.4 entry is
    /// unpacked as RAR 1.5's are, whatever version its header gives.
    fn unpack_version(&self, header: &FileHeader) -> i32 {
        match self.format {
            Format::Rar14 => 15,
            Format::Rar15 => header.unp_version,
        }
    }

    fn sum(&self) -> Sum {
        match self.format {
            Format::Rar14 => Sum::Rar14,
            Format::Rar15 => Sum::Crc32,
        }
    }

    /// `doExtractFile`: the file's data unpacked and checked against its checksum, refused for a
    /// broken header or an encrypted entry. An entry of no data is extracted too, which its
    /// checksum may fail.
    fn extract_file(&mut self, unpack: &mut Unpack, header: &FileHeader) -> Extracted {
        if header.broken || header.is_encrypted() {
            return Extracted::Failed;
        }
        let Ok(start) = u64::try_from(header.data_start()) else {
            return Extracted::Failed;
        };
        let solid = self.solid(header);
        if header.is_split() {
            return Extracted::Failed;
        }
        unpack.start_entry(
            start,
            header.full_pack_size,
            u64::try_from(header.full_unpack_size).ok(),
            self.sum(),
        );
        let unpacked = if header.unp_method == STORED {
            unpack.unstore(header.full_unpack_size)
        } else {
            if !solid {
                unpack.init();
            }
            unpack.set_dest_size(header.full_unpack_size);
            match Method::of(self.unpack_version(header)) {
                Some(method) => unpack.do_unpack(method, solid),
                None => Ok(()),
            }
        };
        if let Err(thrown) = unpacked {
            return thrown.into();
        }
        if unpack.crc() != header.file_crc {
            return Extracted::Failed;
        }
        match unpack.take_data() {
            Ok(data) => Extracted::Data(data),
            Err(thrown) => thrown.into(),
        }
    }
}
