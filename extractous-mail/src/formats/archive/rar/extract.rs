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

/// `Method` of a stored entry's header.
const STORED: u8 = 0x30;

/// What extracting an entry gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Extracted {
    /// The entry's data, empty where junrar refuses it before it writes anything, as it
    /// refuses a broken header or an encrypted entry, which it has no password for.
    Data(Vec<u8>),
    /// Why the entry's data cannot be given.
    Unread(String),
}

impl From<Thrown> for Extracted {
    fn from(Thrown(exception): Thrown) -> Self {
        Extracted::Unread(format!("unpacking it fails with {exception}"))
    }
}

/// What extracting each file of the archive gives, in order, but of its directories, which
/// Tika's parser does not open.
pub(super) fn entries(data: &[u8], headers: &Headers) -> Vec<(usize, Extracted)> {
    let mut unpack = Unpack::new(data);
    headers
        .files
        .iter()
        .enumerate()
        .filter(|(_, header)| !header.is_directory())
        .map(|(index, header)| (index, extract_file(&mut unpack, header)))
        .collect()
}

/// `doExtractFile`: the file's data unpacked and checked against its CRC.
fn extract_file(unpack: &mut Unpack, header: &FileHeader) -> Extracted {
    if header.full_unpack_size <= 0 || header.broken || header.is_encrypted() {
        return Extracted::Data(Vec::new());
    }
    let Ok(start) = u64::try_from(header.data_start()) else {
        return Extracted::Data(Vec::new());
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
    Extracted::Data(unpack.take_data())
}
