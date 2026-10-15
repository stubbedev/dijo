//! gzip files as commons-compress's `GzipCompressorInputStream` reads them without
//! `decompressConcatenated`: the first member only, its header checked, its content inflated and
//! checked against the trailer's CRC and size, whatever follows it ignored.

use super::{crc32, inflate_whole, throws, Undecoded};
use crate::formats::handlers::utf16_length;

const ID1: u8 = 31;
const ID2: u8 = 139;
const DEFLATED: u8 = 8;
const FHCRC: u8 = 0x02;
const FEXTRA: u8 = 0x04;
const FNAME: u8 = 0x08;
const FCOMMENT: u8 = 0x10;
const FRESERVED: u8 = 0xE0;

/// `GzipUtils`'s suffixes of gzip file names, with what replaces each in the uncompressed name.
const UNCOMPRESSED_SUFFIXES: [(&str, &str); 11] = [
    (".tgz", ".tar"),
    (".taz", ".tar"),
    (".svgz", ".svg"),
    (".cpgz", ".cpio"),
    (".wmz", ".wmf"),
    (".emz", ".emf"),
    (".gz", ""),
    (".z", ""),
    ("-gz", ""),
    ("-z", ""),
    ("_z", ""),
];

/// `GzipUtils.getUncompressedFileName`: `name` with the shortest of its suffixes, in any case,
/// that names a gzip file replaced, unless the suffix is all of it; `None` when lower-casing
/// `name` changes its length, which commons-compress mixes up.
pub(crate) fn uncompressed_file_name(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let length = utf16_length(&lower);
    if length != utf16_length(name) {
        return None;
    }
    let mut lengths: Vec<usize> = UNCOMPRESSED_SUFFIXES
        .iter()
        .map(|(suffix, _)| suffix.len())
        .collect();
    lengths.sort_unstable();
    lengths.dedup();
    for suffix_length in lengths.into_iter().take_while(|&l| l < length) {
        let found = UNCOMPRESSED_SUFFIXES
            .iter()
            .find(|(suffix, _)| suffix.len() == suffix_length && lower.ends_with(suffix));
        if let Some((suffix, replacement)) = found {
            let stem = name.len().checked_sub(suffix.len())?;
            if !name.is_char_boundary(stem) || !name[stem..].eq_ignore_ascii_case(suffix) {
                return None;
            }
            return Some(format!("{}{replacement}", &name[..stem]));
        }
    }
    Some(name.to_owned())
}

/// The first member of a gzip file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Member {
    /// The original file name of the header, decoded as ISO-8859-1.
    pub(crate) file_name: Option<String>,
    pub(crate) content: Vec<u8>,
}

/// The header fields read front to back, a read past the end failing as `DataInput` fails.
struct Header<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Header<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Undecoded> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| throws("the input ends inside the .gz header"))?;
        let taken = &self.data[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8, Undecoded> {
        Ok(self.take(1)?[0])
    }

    /// `readToNull`: the bytes up to a zero byte, which is taken too.
    fn up_to_null(&mut self) -> Result<&'a [u8], Undecoded> {
        let rest = &self.data[self.at..];
        let length = rest
            .iter()
            .position(|&byte| byte == 0)
            .ok_or_else(|| throws("the input ends inside the .gz header"))?;
        self.at += length + 1;
        Ok(&rest[..length])
    }
}

/// `ExtraField.fromBytes`: subfields that fill the extra field exactly.
fn check_extra(extra: &[u8]) -> Result<(), Undecoded> {
    let mut at = 0;
    while at + 4 <= extra.len() {
        let length = usize::from(u16::from_le_bytes([extra[at + 2], extra[at + 3]]));
        at += 4;
        if length > extra.len() - at {
            return Err(throws("an extra subfield is longer than the extra field"));
        }
        at += length;
    }
    if at < extra.len() {
        return Err(throws("the extra field ends inside a subfield"));
    }
    Ok(())
}

/// The first member of the gzip file `data`, or why commons-compress fails on it.
pub(crate) fn decode(data: &[u8]) -> Result<Member, Undecoded> {
    let mut header = Header { data, at: 0 };
    if data.first() != Some(&ID1) || data.get(1) != Some(&ID2) {
        return Err(throws("Input is not in the .gz format."));
    }
    header.at = 2;
    let method = header.byte()?;
    if method != DEFLATED {
        return Err(throws(format!(
            "Unsupported compression method {method} in the .gz header"
        )));
    }
    let flags = header.byte()?;
    if flags & FRESERVED != 0 {
        return Err(throws("Reserved flags are set in the .gz header."));
    }
    header.take(6)?;
    if flags & FEXTRA != 0 {
        let length = header.take(2)?;
        let length = usize::from(u16::from_le_bytes([length[0], length[1]]));
        check_extra(header.take(length)?)?;
    }
    let file_name = if flags & FNAME != 0 {
        Some(
            header
                .up_to_null()?
                .iter()
                .map(|&byte| char::from(byte))
                .collect(),
        )
    } else {
        None
    };
    if flags & FCOMMENT != 0 {
        header.up_to_null()?;
    }
    if flags & FHCRC != 0 {
        header.take(2)?;
    }
    let (content, taken) = inflate_whole(&data[header.at..], false)?;
    let trailer = data
        .get(header.at + taken..header.at + taken + 8)
        .ok_or_else(|| throws("the input ends inside the .gz trailer"))?;
    let crc = u32::from_le_bytes(trailer[..4].try_into().expect("four bytes"));
    if crc != crc32(&content) {
        return Err(throws("Gzip-compressed data is corrupt (CRC32 error)."));
    }
    let size = u32::from_le_bytes(trailer[4..].try_into().expect("four bytes"));
    if u64::from(size) != content.len() as u64 & 0xFFFF_FFFF {
        return Err(throws(
            "Gzip-compressed data is corrupt (uncompressed size mismatch).",
        ));
    }
    Ok(Member { file_name, content })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    /// A gzip member of `content`, with `name` in its header if given.
    pub(crate) fn gzip(content: &[u8], name: Option<&str>) -> Vec<u8> {
        let mut builder = flate2::GzBuilder::new();
        if let Some(name) = name {
            builder = builder.filename(name.as_bytes());
        }
        let mut encoder = builder.write(Vec::new(), flate2::Compression::default());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn names_the_uncompressed_file_as_gzip_utils_does() {
        assert_eq!(uncompressed_file_name("a.tgz").as_deref(), Some("a.tar"));
        assert_eq!(uncompressed_file_name("x.SVGZ").as_deref(), Some("x.svg"));
        assert_eq!(uncompressed_file_name("x.Z").as_deref(), Some("x"));
        assert_eq!(uncompressed_file_name("x_z").as_deref(), Some("x"));
        assert_eq!(uncompressed_file_name(".gz").as_deref(), Some(".gz"));
        assert_eq!(uncompressed_file_name("file").as_deref(), Some("file"));
        assert_eq!(uncompressed_file_name("\u{130}.gz"), None);
    }

    #[test]
    fn decodes_the_first_member() {
        let mut data = gzip(b"first", Some("caf\u{e9}.txt"));
        let latin1 = data.iter().position(|&b| b == 0xC3).unwrap();
        data.splice(latin1..latin1 + 2, [0xE9]);
        data.extend_from_slice(&gzip(b"second", None));
        data.extend_from_slice(b"trailing garbage");
        let member = decode(&data).unwrap();
        assert_eq!(member.file_name.as_deref(), Some("caf\u{e9}.txt"));
        assert_eq!(member.content, b"first");
    }

    #[test]
    fn fails_where_commons_compress_fails() {
        let data = gzip(b"some content", None);
        assert!(decode(&data[..data.len() - 1]).is_err());
        assert!(decode(&data[..12]).is_err());
        let mut corrupt = data.clone();
        let crc = corrupt.len() - 8;
        corrupt[crc] ^= 1;
        assert!(decode(&corrupt).is_err());
        let mut reserved = data.clone();
        reserved[3] |= 0x20;
        assert!(decode(&reserved).is_err());
        let mut extra = data[..10].to_vec();
        extra[3] |= FEXTRA;
        extra.extend_from_slice(&[5, 0, b'a', b'b', 9, 0, 0]);
        extra.extend_from_slice(&data[10..]);
        assert!(decode(&extra).is_err());
    }
}
