//! Tika's `GZipSpecializationDetector`: gzip whose first kilobyte inflates to a WARC or ARC
//! web archive.

use flate2::{Decompress, FlushDecompress, Status};

use crate::mime::media_type::MediaType;

/// The specialized gzip type, or `None` for a document that is not gzip.
pub(crate) fn detect(data: &[u8]) -> Option<MediaType> {
    if !data.starts_with(&[0x1F, 0x8B]) {
        return None;
    }
    let inflated = inflate_member(&data[..data.len().min(1024)]);
    let kind = if inflated.starts_with(b"WARC/") {
        "application/warc+gz"
    } else if inflated.starts_with(b"filedesc://") {
        "application/arc+gz"
    } else {
        "application/gzip"
    };
    Some(MediaType::named(kind))
}

/// What `GzipCompressorInputStream` yields from the first member of `gzip` before the input
/// ends or turns out corrupt.
fn inflate_member(gzip: &[u8]) -> Vec<u8> {
    let Some(body) = member_body(gzip) else {
        return Vec::new();
    };
    let mut inflater = Decompress::new(false);
    let mut out = Vec::new();
    loop {
        let (before_in, before_out) = (inflater.total_in(), out.len());
        out.reserve(4096);
        let consumed = usize::try_from(before_in).expect("the input is a kilobyte");
        match inflater.decompress_vec(&body[consumed..], &mut out, FlushDecompress::None) {
            Ok(Status::StreamEnd) | Err(_) => return out,
            Ok(_) if inflater.total_in() == before_in && out.len() == before_out => return out,
            Ok(_) => {}
        }
    }
}

/// The deflate data after a gzip member header, or `None` for a header commons-compress rejects
/// (its extra field must split into whole subfields) or that the bytes cut short.
fn member_body(gzip: &[u8]) -> Option<&[u8]> {
    const FHCRC: u8 = 0x02;
    const FEXTRA: u8 = 0x04;
    const FNAME: u8 = 0x08;
    const FCOMMENT: u8 = 0x10;
    if gzip.len() < 10 || gzip[2] != 8 || gzip[3] & 0xE0 != 0 {
        return None;
    }
    let flags = gzip[3];
    let mut rest = &gzip[10..];
    if flags & FEXTRA != 0 {
        let length = usize::from(u16::from_le_bytes([*rest.first()?, *rest.get(1)?]));
        let extra = rest.get(2..2 + length)?;
        let mut position = 0;
        while position + 4 <= extra.len() {
            let sublength = usize::from(u16::from_le_bytes([
                extra[position + 2],
                extra[position + 3],
            ]));
            position += 4 + sublength;
            if position > extra.len() {
                return None;
            }
        }
        if position < extra.len() {
            return None;
        }
        rest = &rest[2 + length..];
    }
    for flag in [FNAME, FCOMMENT] {
        if flags & flag != 0 {
            let end = rest.iter().position(|&b| b == 0)?;
            rest = &rest[end + 1..];
        }
    }
    if flags & FHCRC != 0 {
        rest = rest.get(2..)?;
    }
    Some(rest)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::write::GzEncoder;
    use flate2::Compression;

    use super::*;

    fn gzip(content: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap()
    }

    fn detected(data: &[u8]) -> Option<String> {
        detect(data).map(|t| t.to_string())
    }

    #[test]
    fn specializes_web_archives() {
        assert_eq!(
            detected(&gzip(b"WARC/1.0\r\n")).as_deref(),
            Some("application/warc+gz")
        );
        assert_eq!(
            detected(&gzip(b"filedesc://x")).as_deref(),
            Some("application/arc+gz")
        );
        assert_eq!(
            detected(&gzip(b"hello")).as_deref(),
            Some("application/gzip")
        );
    }

    #[test]
    fn reads_a_cut_member_as_far_as_it_goes() {
        let mut long = b"WARC/1.0\r\n".to_vec();
        long.extend((0..20_000u32).flat_map(|i| i.to_le_bytes()));
        let data = gzip(&long);
        assert!(data.len() > 1024);
        assert_eq!(detected(&data).as_deref(), Some("application/warc+gz"));
        assert_eq!(
            detected(b"\x1f\x8b\x07\x00").as_deref(),
            Some("application/gzip")
        );
        assert_eq!(detected(b"PK"), None);
    }
}
