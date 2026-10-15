//! gzip files, which flate2 decodes, read as `GzipCompressorInputStream` reads them without
//! `decompressConcatenated`: the first member only; and `GzipUtils`'s names of what they hold.

use super::{read_whole, Undecoded};
use crate::formats::handlers::utf16_length;

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
    /// The original file name of the header, decoded as ISO-8859-1 as commons-compress decodes it.
    pub(crate) file_name: Option<String>,
    pub(crate) content: Vec<u8>,
}

/// The first member of the gzip file `data`, as flate2 reads it: the header checked, the
/// content checked against the trailer's CRC and size, whatever follows ignored.
pub(crate) fn decode(data: &[u8]) -> Result<Member, Undecoded> {
    let mut decoder = flate2::bufread::GzDecoder::new(data);
    let content = read_whole(&mut decoder)?;
    let file_name = decoder
        .header()
        .and_then(flate2::GzHeader::filename)
        .map(|name| name.iter().map(|&byte| char::from(byte)).collect());
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
    fn fails_on_what_is_not_a_whole_member() {
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
    }

    /// commons-compress rejects an extra field its subfields do not fill exactly; flate2,
    /// like gzip itself, does not look inside it.
    #[test]
    fn reads_an_extra_field_commons_compress_rejects() {
        let data = gzip(b"some content", None);
        let mut extra = data[..10].to_vec();
        extra[3] |= 0x04;
        extra.extend_from_slice(&[5, 0, b'a', b'b', 9, 0, 0]);
        extra.extend_from_slice(&data[10..]);
        assert_eq!(decode(&extra).unwrap().content, b"some content");
    }
}
