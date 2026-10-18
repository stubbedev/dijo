//! The TAR header as commons-compress's `TarArchiveEntry` parses it, for
//! `ArchiveStreamFactory.detect`, whose answer decides the media type Tika gives a document:
//! the magic of a POSIX, AIX, GNU or Ant header, or else the COMPRESS-117 check of the first
//! record read as an archive of its own; and for the archive's reader ([`super::tar`]). Numbers
//! must be octal or base-256, names are decoded as UTF-8 with `?` for what is malformed.

use std::ops::Range;

use super::zip::decode_name;

pub(super) const RECORD: usize = 512;
const NAME: Range<usize> = 0..100;
const MODE: Range<usize> = 100..108;
const UID: Range<usize> = 108..116;
const GID: Range<usize> = 116..124;
const SIZE: Range<usize> = 124..136;
/// The sparse structs of an old GNU header, offset and length twelve bytes each.
const GNU_SPARSE_STRUCTS: usize = 4;
const SPARSE_STRUCT: usize = 24;
const MTIME: Range<usize> = 136..148;
const CHECKSUM: Range<usize> = 148..156;
const LINK_FLAG: usize = 156;
const MAGIC: Range<usize> = 257..263;
const VERSION: Range<usize> = 263..265;
const USER_NAME: Range<usize> = 265..297;
const GROUP_NAME: Range<usize> = 297..329;
const DEV_MAJOR: Range<usize> = 329..337;
const DEV_MINOR: Range<usize> = 337..345;
const PREFIX: Range<usize> = 345..500;
const XSTAR_PREFIX: Range<usize> = 345..476;
const XSTAR_ATIME: Range<usize> = 476..488;
const XSTAR_CTIME: Range<usize> = 488..500;
const XSTAR_MAGIC: Range<usize> = 508..512;
const XSTAR_MULTIVOLUME: usize = 464;
const GNU_ATIME: Range<usize> = 345..357;
const GNU_CTIME: Range<usize> = 357..369;
const GNU_SPARSE: usize = 386;
const GNU_IS_EXTENDED: usize = 482;
const GNU_REAL_SIZE: Range<usize> = 483..495;
/// The sparse structs of an old GNU extension record, and where its flag that another follows
/// stands.
pub(super) const SPARSE_STRUCTS_IN_EXTENSION: usize = 21;
pub(super) const EXTENSION_IS_EXTENDED: usize = 504;

const LF_CHR: u8 = b'3';
const LF_BLK: u8 = b'4';
const LF_DIR: u8 = b'5';
const LF_MULTIVOLUME: u8 = b'M';
const LF_OLD_GNU_SPARSE: u8 = b'S';
/// The entries whose data `TarArchiveInputStream` reads before the entry they belong to: GNU
/// long names and links, and PAX headers.
const LF_EXTENSIONS: [u8; 5] = *b"KLgxX";

/// commons-compress rejects the header.
#[derive(Debug)]
pub(super) struct Corrupt;

/// `TarArchiveInputStream.matches`: the magic and version of a POSIX, AIX, GNU or Ant header.
pub(crate) fn matches(header: &[u8]) -> bool {
    let (Some(magic), Some(version)) = (header.get(MAGIC), header.get(VERSION)) else {
        return false;
    };
    (magic == b"ustar\0" && matches!(version, b"00" | b"0 " | b"\0\0"))
        || (magic == b"ustar " && matches!(version, b" \0" | b"0\0"))
}

/// `TarUtils.parseOctal`: leading spaces and all trailing NULs and spaces ignored, a field
/// starting with NUL zero.
fn parse_octal(field: &[u8]) -> Result<i64, Corrupt> {
    if field[0] == 0 {
        return Ok(0);
    }
    let start = field.iter().take_while(|&&b| b == b' ').count();
    let mut end = field.len();
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
fn parse_octal_or_binary(field: &[u8]) -> Result<i64, Corrupt> {
    if field[0] & 0x80 == 0 {
        return parse_octal(field);
    }
    let negative = field[0] == 0xFF;
    if field.len() < 9 {
        let mut value: i64 = 0;
        for &b in &field[1..] {
            value = value.wrapping_shl(8).wrapping_add(i64::from(b));
        }
        if negative {
            value -= 1;
            value ^= (1i64 << ((field.len() - 1) * 8)) - 1;
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
    let stored = parse_octal(&header[CHECKSUM])?;
    let (mut unsigned, mut signed) = (0i64, 0i64);
    for (i, &b) in header.iter().enumerate() {
        let b = if CHECKSUM.contains(&i) { b' ' } else { b };
        unsigned += i64::from(b);
        signed += i64::from(b as i8);
    }
    Ok(stored == unsigned || stored == signed)
}

/// `TarUtils.parseName`: the field up to its first NUL, decoded.
fn parse_name(field: &[u8]) -> String {
    let length = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    decode_name(&field[..length])
}

/// `TarArchiveEntry.isXstar` of a header, after global PAX headers whose `SCHILY.archtype`
/// says whether the archive is xstar's, if they give one.
fn is_xstar(header: &[u8], archtype: Option<bool>) -> bool {
    if &header[XSTAR_MAGIC] == b"tar\0" {
        return true;
    }
    if let Some(xstar) = archtype {
        return xstar;
    }
    let invalid_prefix = header[XSTAR_PREFIX.end - 1] != 0
        && (header[LINK_FLAG] != LF_MULTIVOLUME
            || (header[XSTAR_MULTIVOLUME] & 0x80 == 0 && header[XSTAR_MULTIVOLUME + 11] != b' '));
    let invalid_time = |field: &[u8]| {
        field[0] & 0x80 == 0
            && (field[..11].iter().any(|b| !(b'0'..=b'7').contains(b))
                || !matches!(field[11], b' ' | 0))
    };
    !(invalid_prefix || invalid_time(&header[XSTAR_ATIME]) || invalid_time(&header[XSTAR_CTIME]))
}

/// `TarArchiveEntry.normalizeFileName` without the absolute path kept: on Windows without its
/// drive letter and with its separators turned into slashes, then without its leading slashes.
pub(super) fn normalize(name: &str) -> String {
    let mut name = name;
    let windows;
    if cfg!(windows) {
        let bytes = name.as_bytes();
        if name.chars().count() > 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            name = &name[2..];
        }
        windows = name.replace('\\', "/");
        name = &windows;
    }
    name.trim_start_matches('/').to_owned()
}

/// `TarUtils.readSparseStructs`: `count` sparse structs from the start of `bytes`, each an
/// offset and a length, neither negative.
pub(super) fn sparse_structs(bytes: &[u8], count: usize) -> Result<Vec<(i64, i64)>, Corrupt> {
    (0..count)
        .map(|index| {
            let at = index * SPARSE_STRUCT;
            let offset = parse_octal_or_binary(&bytes[at..at + 12])?;
            let length = parse_octal_or_binary(&bytes[at + 12..at + SPARSE_STRUCT])?;
            if offset < 0 || length < 0 {
                return Err(Corrupt);
            }
            Ok((offset, length))
        })
        .collect()
}

/// What `TarArchiveEntry` reads of a header.
pub(super) struct Header {
    pub(super) name: String,
    user_name: String,
    group_name: String,
    checksum_ok: bool,
    pub(super) link_flag: u8,
    pub(super) size: i64,
    /// The modification time, in seconds since the epoch.
    pub(super) mtime: i64,
    /// An old GNU header's sparse structs, offset and length, whether sparse headers follow
    /// it, and the real size of its file.
    pub(super) sparse: Vec<(i64, i64)>,
    pub(super) extended: bool,
    pub(super) real_size: i64,
}

impl Header {
    /// `TarArchiveEntry(globalPaxHeaders, headerBuf, encoding, false)`, after global PAX headers
    /// whose `SCHILY.archtype` says whether the archive is xstar's, if they give one; it fails
    /// on a number that is not octal or base-256, or on a negative size or sparse struct.
    pub(super) fn parse(header: &[u8], archtype: Option<bool>) -> Result<Header, Corrupt> {
        let mut name = parse_name(&header[NAME]);
        for field in [MODE, UID, GID] {
            parse_octal_or_binary(&header[field])?;
        }
        let size = parse_octal_or_binary(&header[SIZE])?;
        if size < 0 {
            return Err(Corrupt);
        }
        let mtime = parse_octal_or_binary(&header[MTIME])?;
        let checksum_ok = checksum_ok(header)?;
        let link_flag = header[LINK_FLAG];
        let user_name = parse_name(&header[USER_NAME]);
        let group_name = parse_name(&header[GROUP_NAME]);
        if matches!(link_flag, LF_CHR | LF_BLK) {
            parse_octal_or_binary(&header[DEV_MAJOR])?;
            parse_octal_or_binary(&header[DEV_MINOR])?;
        }
        let mut sparse = Vec::new();
        let mut extended = false;
        let mut real_size = 0;
        let magic = &header[MAGIC];
        if magic == b"ustar " {
            parse_octal_or_binary(&header[GNU_ATIME])?;
            parse_octal_or_binary(&header[GNU_CTIME])?;
            sparse = sparse_structs(&header[GNU_SPARSE..], GNU_SPARSE_STRUCTS)?;
            extended = header[GNU_IS_EXTENDED] == 1;
            real_size = parse_octal(&header[GNU_REAL_SIZE])?;
        } else if magic == b"ustar\0" && is_xstar(header, archtype) {
            let prefix = parse_name(&header[XSTAR_PREFIX]);
            parse_octal_or_binary(&header[XSTAR_ATIME])?;
            parse_octal_or_binary(&header[XSTAR_CTIME])?;
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        } else {
            let prefix = parse_name(&header[PREFIX]);
            if link_flag == LF_DIR && !name.ends_with('/') {
                name.push('/');
            }
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        Ok(Header {
            name,
            user_name,
            group_name,
            checksum_ok,
            link_flag,
            size,
            mtime,
            sparse,
            extended,
            real_size,
        })
    }

    fn is_directory(&self) -> bool {
        self.link_flag == LF_DIR || self.name.ends_with('/')
    }
}

/// The COMPRESS-117 check of `ArchiveStreamFactory.detect` over the first record alone. Its
/// first entry is a TAR archive's when it is a sound directory, which leaves the stream at its
/// end, or a sound file with ASCII names. An entry whose data `TarArchiveInputStream` reads
/// before the entry it belongs to, or an old GNU sparse file whose sparse headers follow it,
/// runs past the record, which fails the check.
pub(crate) fn is_tar_by_entries(record: &[u8]) -> bool {
    if record.len() != RECORD || record.iter().all(|&b| b == 0) {
        return false;
    }
    let Ok(header) = Header::parse(record, None) else {
        return false;
    };
    if LF_EXTENSIONS.contains(&header.link_flag)
        || (header.link_flag == LF_OLD_GNU_SPARSE && header.extended)
    {
        return false;
    }
    if header.is_directory() {
        return header.checksum_ok;
    }
    let ascii = |name: &str| name.chars().all(|c| (' '..'\u{80}').contains(&c));
    header.checksum_ok
        && ascii(&header.group_name)
        && ascii(&header.name)
        && ascii(&header.user_name)
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use super::*;

    /// A header of `name`, `flag` and `size` with the POSIX magic, a modification time of
    /// 1700000000 and a sound checksum.
    pub fn header(name: &[u8], flag: u8, size: usize) -> Vec<u8> {
        let mut header = vec![0u8; RECORD];
        header[..name.len()].copy_from_slice(name);
        header[MODE].copy_from_slice(b"0000644\0");
        header[UID].copy_from_slice(b"0000000\0");
        header[GID].copy_from_slice(b"0000000\0");
        header[SIZE].copy_from_slice(format!("{size:011o}\0").as_bytes());
        header[MTIME].copy_from_slice(format!("{:011o}\0", 1_700_000_000).as_bytes());
        header[LINK_FLAG] = flag;
        header[MAGIC].copy_from_slice(b"ustar\0");
        header[VERSION].copy_from_slice(b"00");
        sign(&mut header);
        header
    }

    /// `header` with its checksum written.
    fn sign(header: &mut [u8]) {
        header[CHECKSUM].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[CHECKSUM].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    }

    /// The records of `entries`, each a name, a type flag and the data, its last record padded.
    pub fn records(entries: &[(&[u8], u8, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, flag, data) in entries {
            out.extend_from_slice(&header(name, *flag, data.len()));
            out.extend_from_slice(data);
            out.resize(out.len().next_multiple_of(RECORD), 0);
        }
        out
    }

    /// An archive of `entries`, as [`records`] writes them, ended by two zero records.
    pub fn tar(entries: &[(&[u8], u8, &[u8])]) -> Vec<u8> {
        ended(records(entries))
    }

    /// The records of a PAX 1.0 sparse file of `name` whose data, `data`, is one stretch from
    /// its start, the count of its sparse map written with leading zeros for the map to fill its
    /// record.
    pub fn sparse_1_0_filling_a_record(name: &str, data: &[u8]) -> Vec<u8> {
        let rest = format!("0\n{}\n", data.len());
        let count = format!("{:0>width$}\n", 1, width = RECORD - rest.len() - 1);
        let mut stored = format!("{count}{rest}").into_bytes();
        stored.extend_from_slice(data);
        let stored_name = format!("./GNUSparseFile.1/{name}");
        let real_size = data.len().to_string();
        records(&[
            (
                b"././@PaxHeader",
                b'x',
                &pax(&[
                    ("path", &stored_name),
                    ("GNU.sparse.major", "1"),
                    ("GNU.sparse.minor", "0"),
                    ("GNU.sparse.name", name),
                    ("GNU.sparse.realsize", &real_size),
                ]),
            ),
            (stored_name.as_bytes(), b'0', &stored),
        ])
    }

    /// `records` ended by two zero records.
    pub fn ended(mut records: Vec<u8>) -> Vec<u8> {
        records.resize(records.len() + 2 * RECORD, 0);
        records
    }

    /// The records of an old GNU sparse file of `name` and `real_size`: its header, which holds
    /// the first four of `structs`, an extension record for each further 21, and `stored`, its
    /// data, its last record padded.
    pub fn old_gnu_sparse(
        name: &[u8],
        real_size: u64,
        structs: &[(u64, u64)],
        stored: &[u8],
    ) -> Vec<u8> {
        let octal = |value: u64| format!("{value:011o}\0");
        let write = |record: &mut [u8], at: usize, structs: &[(u64, u64)]| {
            for (index, (offset, numbytes)) in structs.iter().enumerate() {
                let at = at + index * SPARSE_STRUCT;
                record[at..at + 12].copy_from_slice(octal(*offset).as_bytes());
                record[at + 12..at + SPARSE_STRUCT].copy_from_slice(octal(*numbytes).as_bytes());
            }
        };
        let (own, mut rest) = structs.split_at(structs.len().min(GNU_SPARSE_STRUCTS));
        let mut out = header(name, LF_OLD_GNU_SPARSE, stored.len());
        out[MAGIC].copy_from_slice(b"ustar ");
        out[VERSION].copy_from_slice(b" \0");
        write(&mut out, GNU_SPARSE, own);
        out[GNU_IS_EXTENDED] = u8::from(!rest.is_empty());
        out[GNU_REAL_SIZE].copy_from_slice(octal(real_size).as_bytes());
        sign(&mut out);
        while !rest.is_empty() {
            let (these, after) = rest.split_at(rest.len().min(SPARSE_STRUCTS_IN_EXTENSION));
            let mut extension = vec![0u8; RECORD];
            write(&mut extension, 0, these);
            extension[EXTENSION_IS_EXTENDED] = u8::from(!after.is_empty());
            out.extend_from_slice(&extension);
            rest = after;
        }
        out.extend_from_slice(stored);
        out.resize(out.len().next_multiple_of(RECORD), 0);
        out
    }

    /// PAX records of `headers`.
    pub fn pax(headers: &[(&str, &str)]) -> Vec<u8> {
        let mut out = String::new();
        for (key, value) in headers {
            let body = format!(" {key}={value}\n");
            let mut length = body.len() + 1;
            while format!("{length}{body}").len() != length {
                length += 1;
            }
            out.push_str(&format!("{length}{body}"));
        }
        out.into_bytes()
    }

    /// `header` without a magic, as an old TAR archive writes it.
    #[cfg(test)]
    fn old(name: &[u8], flag: u8, size: usize) -> Vec<u8> {
        let mut old = header(name, flag, size);
        old[MAGIC].fill(0);
        old[VERSION].fill(0);
        sign(&mut old);
        old
    }

    #[test]
    fn tells_a_tar_by_its_first_entry_as_commons_compress_does() {
        assert!(is_tar_by_entries(&old(b"file.txt", b'0', 0)));
        assert!(is_tar_by_entries(&old(b"\xE9.txt", b'0', 0)));
        assert!(is_tar_by_entries(&old(b"dir/", b'5', 512)));
        assert!(is_tar_by_entries(&old(b"sparse", b'S', 0)));
        assert!(!is_tar_by_entries(&old(b"pax", b'x', 10)));
        assert!(!is_tar_by_entries(&old(b"name", b'L', 0)));
        assert!(!is_tar_by_entries(&[0u8; RECORD]));
        let mut unsound = old(b"file.txt", b'0', 0);
        unsound[CHECKSUM.start] = b'7';
        assert!(!is_tar_by_entries(&unsound));
        let mut not_octal = old(b"file.txt", b'0', 0);
        not_octal[MODE.start] = b'9';
        sign(&mut not_octal);
        assert!(!is_tar_by_entries(&not_octal));
    }
}
