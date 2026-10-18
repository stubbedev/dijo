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

use super::headers::{FileHeader, Headers};
use super::unpack::{Method, Thrown, Unpack};
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
    for (index, header) in headers.files.iter().enumerate() {
        if !header.is_directory() {
            each(index, extract_file(&mut unpack, header))?;
        }
    }
    Ok(())
}

/// `doExtractFile`: the file's data unpacked and checked against its CRC.
fn extract_file(unpack: &mut Unpack, header: &FileHeader) -> Extracted {
    if header.full_unpack_size <= 0 || header.broken || header.is_encrypted() {
        return Extracted::Data(Input::Borrowed(&[]));
    }
    let Ok(start) = u64::try_from(header.data_start()) else {
        return Extracted::Data(Input::Borrowed(&[]));
    };
    if header.is_split() {
        return Extracted::Unread(
            "its data continues from or in another volume of the archive, which the document \
             does not hold"
                .to_owned(),
        );
    }
    unpack.start_entry(start, header.full_pack_size);
    let unpacked = if header.unp_method == STORED {
        unpack.unstore(header.full_unpack_size)
    } else {
        let Some(method) = Method::of(header.unp_version) else {
            return Extracted::Unread(format!(
                "it is compressed for RAR version {}, which no RAR unpacker reads",
                header.unp_version
            ));
        };
        if !header.is_solid() {
            unpack.init();
        }
        unpack.set_dest_size(header.full_unpack_size);
        unpack.do_unpack(method, header.is_solid())
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
