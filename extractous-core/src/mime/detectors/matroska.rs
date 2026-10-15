//! Tika's `MatroskaDetector`: an EBML header whose first 64 bytes name the WebM or Matroska
//! document type.

use crate::mime::media_type::MediaType;

pub(crate) fn detect(data: &[u8]) -> Option<MediaType> {
    let header = &data[..data.len().min(64)];
    if !header.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return None;
    }
    let scanned = header.len().saturating_sub(4);
    (4..scanned).find_map(|i| match &header[i..i + 4] {
        b"webm" => Some(MediaType::named("video/webm")),
        b"matr" => Some(MediaType::named("application/x-matroska")),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_document_type() {
        let mut webm = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x82, 0x84];
        webm.extend(b"webm\x42\x87");
        assert_eq!(detect(&webm).unwrap().as_str(), "video/webm");
        let mut mkv = vec![0x1A, 0x45, 0xDF, 0xA3, 0x42, 0x82, 0x88];
        mkv.extend(b"matroska");
        assert_eq!(detect(&mkv).unwrap().as_str(), "application/x-matroska");
    }

    #[test]
    fn needs_the_name_before_the_last_four_bytes() {
        assert!(detect(&[0x1A, 0x45, 0xDF, 0xA3, b'w', b'e', b'b', b'm']).is_none());
        assert!(detect(b"webm").is_none());
    }
}
