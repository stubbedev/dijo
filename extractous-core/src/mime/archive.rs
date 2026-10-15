//! The archive and compression formats commons-compress recognizes by their first bytes, as
//! `ArchiveStreamFactory.detect` and `CompressorStreamFactory.detect` recognize them, which
//! Tika's ZIP container detector asks before it looks inside a ZIP file.

use crate::compress::archiver::Archiver;

/// What `ArchiveStreamFactory.detect` and Tika's TIFF exception make of a document's first
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Archive {
    /// A TIFF image, which commons-compress could mistake for a TAR archive.
    Tiff,
    Zip,
    /// Another archive format, by its media type.
    Other(&'static str),
}

/// `DefaultZipContainerDetector.detectArchiveFormat` over the first 1024 bytes.
pub(crate) fn archive(prefix: &[u8]) -> Option<Archive> {
    const TIFF: [&[u8]; 3] = [b"MM\x00\x2a", b"II\x2a\x00", b"MM\x00\x2b"];
    if TIFF.iter().any(|signature| prefix.starts_with(signature)) {
        return Some(Archive::Tiff);
    }
    Archiver::detect(prefix).map(|archiver| match archiver {
        Archiver::Zip => Archive::Zip,
        other => Archive::Other(other.media_type()),
    })
}

/// `CompressorStreamFactory.detect`: the media type Tika gives the compression format the first
/// bytes announce.
pub(crate) fn compressor(prefix: &[u8]) -> Option<&'static str> {
    let length = prefix.len().min(12);
    let mut signature = [0u8; 12];
    signature[..length].copy_from_slice(&prefix[..length]);
    let s = &signature;
    let found = if length >= 3 && s.starts_with(b"BZh") {
        "application/x-bzip2"
    } else if length >= 2 && s.starts_with(&[0x1F, 0x8B]) {
        "application/gzip"
    } else if length >= 4 && s.starts_with(&[0xCA, 0xFE, 0xD0, 0x0D]) {
        "application/x-java-pack200"
    } else if length >= 10 && s.starts_with(b"\xFF\x06\x00\x00sNaPpY") {
        "application/x-snappy"
    } else if length > 3 && s.starts_with(&[0x1F, 0x9D]) {
        "application/x-compress"
    } else if length > 3 && s[0] == 0x78 && matches!(s[1], 0x01 | 0x5E | 0x9C | 0xDA) {
        "application/zlib"
    } else if length >= 6 && s.starts_with(b"\xFD7zXZ\x00") {
        "application/x-xz"
    } else if length >= 3 && s.starts_with(&[0x5D, 0, 0]) {
        "application/x-lzma"
    } else if length >= 4 && s.starts_with(&[0x04, 0x22, 0x4D, 0x18]) {
        "application/x-lz4"
    } else if length >= 4
        && (s.starts_with(&[0x28, 0xB5, 0x2F, 0xFD])
            || (s[0] & 0xF0 == 0x50 && s[1..4] == [0x2A, 0x4D, 0x18]))
    {
        "application/zstd"
    } else {
        return None;
    };
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_header(name: &[u8], flag: u8) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..name.len()].copy_from_slice(name);
        header[100..107].copy_from_slice(b"0000644");
        header[124..135].copy_from_slice(b"00000000000");
        header[156] = flag;
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        header
    }

    #[test]
    fn recognizes_archive_signatures() {
        assert_eq!(archive(b"PK\x03\x04rest"), Some(Archive::Zip));
        assert_eq!(archive(b"II\x2a\x00rest"), Some(Archive::Tiff));
        assert_eq!(
            archive(b"!<arch>\nfoo"),
            Some(Archive::Other("application/x-archive"))
        );
        assert_eq!(
            archive(b"070701xxxx"),
            Some(Archive::Other("application/x-cpio"))
        );
        assert_eq!(archive(b"plain text"), None);
    }

    #[test]
    fn recognizes_old_tar_by_its_first_entry() {
        assert_eq!(
            archive(&tar_header(b"file.txt", b'0')),
            Some(Archive::Other("application/x-tar"))
        );
        assert_eq!(
            archive(&tar_header(b"dir/", b'5')),
            Some(Archive::Other("application/x-tar"))
        );
        let mut bad = tar_header(b"file.txt", b'0');
        bad[148] = b'7';
        assert_eq!(archive(&bad), None);
        assert_eq!(archive(&[0u8; 512]), None);
    }

    #[test]
    fn recognizes_compressors() {
        assert_eq!(compressor(b"BZh91AY"), Some("application/x-bzip2"));
        assert_eq!(compressor(b"\x1f\x8b\x08"), Some("application/gzip"));
        assert_eq!(compressor(b"\x78\x9c\x00\x00"), Some("application/zlib"));
        assert_eq!(compressor(b"\x78\x9c\x00"), None);
        assert_eq!(compressor(b"\x5a\x2a\x4d\x18"), Some("application/zstd"));
        assert_eq!(compressor(b"hello"), None);
    }
}
