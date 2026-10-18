//! junrar's `Archive.extractFile` and `doExtractFile`, as Tika's parser calls them for each
//! file of an archive in turn: an entry of a solid archive continues the unpacker's state from
//! the one before it.
//!
//! An entry is extracted whole and checked against its CRC, or not at all: where junrar's
//! stream ends early, with whatever it wrote before, the port gives why the entry is unread.
//! junrar extracts each entry on a thread of its own into a pipe of 32 KiB, so where Tika's
//! parse of a larger entry stops reading before its end, the next entry of a solid stream is
//! decoded from the state the failed thread left; the port decodes every entry to its end
//! (#122).

use super::headers::{FileHeader, Format, Headers};
use super::unpack::{Method, Sum, Thrown, Unpack};
use crate::input::Input;

/// `Method` of a stored entry's header.
const STORED: u8 = 0x30;

/// What extracting an entry gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Extracted {
    /// The entry's data, empty where junrar refuses it before it writes anything, as it
    /// refuses a broken header or an encrypted entry, which it has no password for.
    Data(Input<'static>),
    /// Why the entry's data cannot be given.
    Unread(String),
}

impl From<Thrown> for Extracted {
    fn from(Thrown(exception): Thrown) -> Self {
        Extracted::Unread(format!("unpacking it fails with {exception}"))
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

    /// `doExtractFile`: the file's data unpacked and checked against its checksum.
    fn extract_file(&mut self, unpack: &mut Unpack, header: &FileHeader) -> Extracted {
        if header.full_unpack_size <= 0 || header.broken || header.is_encrypted() {
            return Extracted::Data(Input::Borrowed(&[]));
        }
        let Ok(start) = u64::try_from(header.data_start()) else {
            return Extracted::Data(Input::Borrowed(&[]));
        };
        let solid = self.solid(header);
        if header.is_split() {
            return Extracted::Unread(
                "its data continues from or in another volume of the archive, which the \
                 document does not hold"
                    .to_owned(),
            );
        }
        unpack.start_entry(start, header.full_pack_size, self.sum());
        let unpacked = if header.unp_method == STORED {
            unpack.unstore(header.full_unpack_size)
        } else {
            let version = self.unpack_version(header);
            let Some(method) = Method::of(version) else {
                return Extracted::Unread(format!(
                    "it is compressed for RAR version {version}, which no RAR unpacker reads"
                ));
            };
            if !solid {
                unpack.init();
            }
            unpack.set_dest_size(header.full_unpack_size);
            unpack.do_unpack(method, solid)
        };
        if let Err(thrown) = unpacked {
            return thrown.into();
        }
        if unpack.crc() != header.file_crc {
            return Extracted::Unread("its unpacked data does not match its CRC".to_owned());
        }
        match unpack.take_data() {
            Ok(data) => Extracted::Data(data),
            Err(thrown) => thrown.into(),
        }
    }
}
