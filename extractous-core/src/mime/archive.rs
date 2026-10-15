//! The archive and compression formats commons-compress recognizes by their first bytes, as
//! `ArchiveStreamFactory.detect` and `CompressorStreamFactory.detect` recognize them, which
//! Tika's ZIP container detector asks before it looks inside a ZIP file.

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
    let signature = &prefix[..prefix.len().min(12)];
    if [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08", b"PK00"]
        .iter()
        .any(|zip| signature.starts_with(*zip))
    {
        return Some(Archive::Zip);
    }
    if signature.starts_with(b"!<arch>\n") {
        return Some(Archive::Other("application/x-archive"));
    }
    if is_cpio(signature) {
        return Some(Archive::Other("application/x-cpio"));
    }
    if signature.starts_with(&[0x60, 0xEA]) {
        return Some(Archive::Other("application/x-arj"));
    }
    if signature.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(Archive::Other("application/x-7z-compressed"));
    }
    let dump = &prefix[..prefix.len().min(32)];
    if dump.len() >= 32 && u32::from_le_bytes([dump[24], dump[25], dump[26], dump[27]]) == 60012 {
        return Some(Archive::Other("application/x-tika-unix-dump"));
    }
    let header = &prefix[..prefix.len().min(512)];
    if is_tar_magic(header) || (header.len() == 512 && is_tar_by_entries(header)) {
        return Some(Archive::Other("application/x-tar"));
    }
    None
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

fn is_cpio(signature: &[u8]) -> bool {
    if signature.len() < 6 {
        return false;
    }
    (signature[0] == 0x71 && signature[1] == 0xC7)
        || (signature[1] == 0x71 && signature[0] == 0xC7)
        || (signature.starts_with(b"07070") && matches!(signature[5], b'1' | b'2' | b'7'))
}

/// `TarArchiveInputStream.matches`: the magic and version of a POSIX, AIX, GNU or Ant header.
fn is_tar_magic(header: &[u8]) -> bool {
    if header.len() < 265 {
        return false;
    }
    let (magic, version) = (&header[257..263], &header[263..265]);
    (magic == b"ustar\0" && matches!(version, b"00" | b"0 " | b"\0\0"))
        || (magic == b"ustar " && matches!(version, b" \0" | b"0\0"))
}

/// The COMPRESS-117 check of `ArchiveStreamFactory.detect`: the first 512 bytes read as a TAR
/// archive of their own whose first entry, past at most ten directories, is a file with a sound
/// checksum and ASCII names, or is a sound directory followed by the end of the archive.
fn is_tar_by_entries(header: &[u8]) -> bool {
    if header.iter().all(|&b| b == 0) {
        return false;
    }
    let Ok(entry) = TarEntry::parse(header) else {
        return false;
    };
    if matches!(entry.link_flag, b'K' | b'L' | b'g' | b'x' | b'X')
        || (entry.link_flag == b'S' && entry.extended)
    {
        return false;
    }
    if entry.is_directory() && entry.checksum_ok {
        return entry.size == 0;
    }
    entry.checksum_ok
        && !entry.is_directory()
        && is_ascii_name(&entry.group_name)
        && is_ascii_name(&entry.name)
        && is_ascii_name(&entry.user_name)
}

fn is_ascii_name(name: &[u8]) -> bool {
    name.iter().all(|&b| (32..128).contains(&b))
}

/// The fields of a TAR header `TarArchiveEntry` parses, as far as the check needs them.
struct TarEntry {
    name: Vec<u8>,
    user_name: Vec<u8>,
    group_name: Vec<u8>,
    size: i64,
    checksum_ok: bool,
    link_flag: u8,
    extended: bool,
}

/// Why commons-compress rejects a header.
struct Corrupt;

/// A NUL-terminated field.
fn tar_name(header: &[u8], offset: usize, length: usize) -> Vec<u8> {
    header[offset..offset + length]
        .iter()
        .take_while(|&&b| b != 0)
        .copied()
        .collect()
}

/// `TarUtils.parseOctal`.
fn parse_octal(header: &[u8], offset: usize, length: usize) -> Result<i64, Corrupt> {
    let field = &header[offset..offset + length];
    if field[0] == 0 {
        return Ok(0);
    }
    let start = field.iter().take_while(|&&b| b == b' ').count();
    let mut end = length;
    while start < end && matches!(field[end - 1], 0 | b' ') {
        end -= 1;
    }
    let mut result: i64 = 0;
    for &digit in &field[start..end] {
        if !(b'0'..=b'7').contains(&digit) {
            return Err(Corrupt);
        }
        result = result.wrapping_shl(3).wrapping_add(i64::from(digit - b'0'));
    }
    Ok(result)
}

/// `TarUtils.parseOctalOrBinary`: octal, or base-256 when the first byte's high bit is set.
fn parse_octal_or_binary(header: &[u8], offset: usize, length: usize) -> Result<i64, Corrupt> {
    let field = &header[offset..offset + length];
    if field[0] & 0x80 == 0 {
        return parse_octal(header, offset, length);
    }
    let negative = field[0] == 0xFF;
    if length < 9 {
        let mut value: i64 = 0;
        for &b in &field[1..] {
            value = (value << 8) + i64::from(b);
        }
        if negative {
            value -= 1;
            value ^= (1i64 << ((length - 1) * 8)) - 1;
        }
        return Ok(if negative { -value } else { value });
    }
    let mut value: i128 = if field[1] & 0x80 != 0 { -1 } else { 0 };
    for &b in &field[1..] {
        value = (value << 8) | i128::from(b);
    }
    if negative {
        value = !(value - 1);
    }
    let bits = 128
        - if value < 0 {
            (!value).leading_zeros()
        } else {
            value.leading_zeros()
        };
    if bits > 63 {
        return Err(Corrupt);
    }
    let value = value as i64;
    Ok(if negative { -value } else { value })
}

/// `TarUtils.verifyCheckSum`: the stored sum equals the unsigned or the signed byte sum with
/// the checksum field read as spaces.
fn checksum_ok(header: &[u8]) -> Result<bool, Corrupt> {
    let stored = parse_octal(header, 148, 8)?;
    let (mut unsigned, mut signed) = (0i64, 0i64);
    for (i, &b) in header.iter().enumerate() {
        let b = if (148..156).contains(&i) { b' ' } else { b };
        unsigned += i64::from(b);
        signed += i64::from(b as i8);
    }
    Ok(stored == unsigned || stored == signed)
}

impl TarEntry {
    /// `TarArchiveEntry`'s header parse, which fails on a number that is not octal.
    fn parse(header: &[u8]) -> Result<TarEntry, Corrupt> {
        let mut name = tar_name(header, 0, 100);
        for (offset, length) in [(100, 8), (108, 8), (116, 8)] {
            parse_octal_or_binary(header, offset, length)?;
        }
        let size = parse_octal_or_binary(header, 124, 12)?;
        if size < 0 {
            return Err(Corrupt);
        }
        parse_octal_or_binary(header, 136, 12)?;
        let checksum_ok = checksum_ok(header)?;
        let link_flag = header[156];
        let magic = &header[257..263];
        let user_name = tar_name(header, 265, 32);
        let group_name = tar_name(header, 297, 32);
        if matches!(link_flag, b'3' | b'4') {
            parse_octal_or_binary(header, 329, 8)?;
            parse_octal_or_binary(header, 337, 8)?;
        }
        let mut extended = false;
        if magic == b"ustar " {
            parse_octal_or_binary(header, 345, 12)?;
            parse_octal_or_binary(header, 357, 12)?;
            for sparse in 0..4 {
                let offset = 386 + sparse * 24;
                if parse_octal_or_binary(header, offset, 12)? < 0
                    || parse_octal_or_binary(header, offset + 12, 12)? < 0
                {
                    return Err(Corrupt);
                }
            }
            extended = header[482] == 1;
            parse_octal(header, 483, 12)?;
        } else if magic == b"ustar\0" && is_xstar(header) {
            let prefix = tar_name(header, 345, 131);
            parse_octal_or_binary(header, 476, 12)?;
            parse_octal_or_binary(header, 488, 12)?;
            if !prefix.is_empty() {
                name = [prefix, b"/".to_vec(), name].concat();
            }
        } else {
            let prefix = tar_name(header, 345, 155);
            let directory = link_flag == b'5'
                || (!matches!(link_flag, b'x' | b'X' | b'g') && name.ends_with(b"/"));
            if directory && !name.ends_with(b"/") {
                name.push(b'/');
            }
            if !prefix.is_empty() {
                name = [prefix, b"/".to_vec(), name].concat();
            }
        }
        Ok(TarEntry {
            name,
            user_name,
            group_name,
            size,
            checksum_ok,
            link_flag,
            extended,
        })
    }

    fn is_directory(&self) -> bool {
        self.link_flag == b'5'
            || (!matches!(self.link_flag, b'x' | b'X' | b'g') && self.name.ends_with(b"/"))
    }
}

/// `TarArchiveEntry.isXstar` for a header with no global PAX headers before it.
fn is_xstar(header: &[u8]) -> bool {
    if &header[508..512] == b"tar\0" {
        return true;
    }
    let invalid_prefix = header[345 + 130] != 0
        && (header[156] != b'M' || (header[464] & 0x80 == 0 && header[464 + 11] != b' '));
    let invalid_time = |offset: usize| {
        let field = &header[offset..offset + 12];
        field[0] & 0x80 == 0
            && (field[..11].iter().any(|b| !(b'0'..=b'7').contains(b))
                || !matches!(field[11], b' ' | 0))
    };
    !(invalid_prefix || invalid_time(476) || invalid_time(488))
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
