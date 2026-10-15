//! The archive formats commons-compress's `ArchiveStreamFactory.detect` recognizes by their first
//! bytes.

use super::tar_header;

/// An archive format `ArchiveStreamFactory.detect` recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Archiver {
    /// ZIP, and JAR, which it reads as ZIP.
    Zip,
    Ar,
    Cpio,
    Arj,
    SevenZ,
    Dump,
    Tar,
}

impl Archiver {
    /// `ArchiveStreamFactory.detect` over the first bytes: the format they announce.
    pub(crate) fn detect(prefix: &[u8]) -> Option<Archiver> {
        let signature = &prefix[..prefix.len().min(12)];
        if [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08", b"PK00"]
            .iter()
            .any(|zip| signature.starts_with(*zip))
        {
            return Some(Archiver::Zip);
        }
        if signature.starts_with(b"!<arch>\n") {
            return Some(Archiver::Ar);
        }
        if is_cpio(signature) {
            return Some(Archiver::Cpio);
        }
        if signature.starts_with(&[0x60, 0xEA]) {
            return Some(Archiver::Arj);
        }
        if signature.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
            return Some(Archiver::SevenZ);
        }
        let dump = &prefix[..prefix.len().min(32)];
        if dump.len() >= 32 && u32::from_le_bytes([dump[24], dump[25], dump[26], dump[27]]) == 60012
        {
            return Some(Archiver::Dump);
        }
        let header = &prefix[..prefix.len().min(512)];
        if tar_header::matches(header)
            || (header.len() == 512 && tar_header::is_tar_by_entries(header))
        {
            return Some(Archiver::Tar);
        }
        None
    }

    /// The media type Tika gives the format.
    pub(crate) fn media_type(self) -> &'static str {
        match self {
            Archiver::Zip => "application/zip",
            Archiver::Ar => "application/x-archive",
            Archiver::Cpio => "application/x-cpio",
            Archiver::Arj => "application/x-arj",
            Archiver::SevenZ => "application/x-7z-compressed",
            Archiver::Dump => "application/x-tika-unix-dump",
            Archiver::Tar => "application/x-tar",
        }
    }
}

/// `CpioArchiveInputStream.matches`: the old binary magic in either byte order, or the ASCII
/// magic of the new, new-with-checksum or old ASCII format.
fn is_cpio(signature: &[u8]) -> bool {
    if signature.len() < 6 {
        return false;
    }
    (signature[0] == 0x71 && signature[1] == 0xC7)
        || (signature[1] == 0x71 && signature[0] == 0xC7)
        || (signature.starts_with(b"07070") && matches!(signature[5], b'1' | b'2' | b'7'))
}
