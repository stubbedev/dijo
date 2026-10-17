//! Metafiles built for tests, record by record, laid out as Windows writes them.

/// An EMF record of type `id` holding `data`.
#[cfg(feature = "tika")]
pub(crate) fn emf_record(id: u32, data: &[u8]) -> Vec<u8> {
    let mut record = Vec::new();
    record.extend_from_slice(&id.to_le_bytes());
    record.extend_from_slice(&(8 + data.len() as u32).to_le_bytes());
    record.extend_from_slice(data);
    record
}

/// A 32-bit value, little-endian.
#[cfg(feature = "tika")]
pub(crate) fn int(value: i32) -> [u8; 4] {
    value.to_le_bytes()
}

/// An EMF picture of `records` between its header and its end-of-file record.
#[cfg(feature = "tika")]
pub(crate) fn emf(records: &[Vec<u8>]) -> Vec<u8> {
    let mut header = Vec::new();
    for value in [0, 0, 100, 100, 0, 0, 2000, 2000] {
        header.extend_from_slice(&int(value));
    }
    header.extend_from_slice(b" EMF");
    header.extend_from_slice(&int(0x10000));
    header.extend_from_slice(&int(0));
    header.extend_from_slice(&int(records.len() as i32 + 2));
    header.extend_from_slice(&[1, 0, 0, 0]);
    for value in [0, 0, 0, 1024, 768, 320, 240] {
        header.extend_from_slice(&int(value));
    }
    let mut picture = emf_record(0x01, &header);
    for record in records {
        picture.extend_from_slice(record);
    }
    picture.extend(emf_record(0x0E, &[int(0), int(16), int(20)].concat()));
    picture
}

/// An `EMR_EXTTEXTOUTW` (or, not `wide`, `EMR_EXTTEXTOUTA`) record of `text` at `x`, `y`, laid
/// out as Windows writes it, with the rectangle POI reads only for clipped or opaque text.
#[cfg(feature = "tika")]
pub(crate) fn emf_text(text: &str, x: i32, y: i32, wide: bool) -> Vec<u8> {
    let (bytes, chars): (Vec<u8>, usize) = if wide {
        (
            text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            text.encode_utf16().count(),
        )
    } else {
        let bytes: Vec<u8> = text.chars().map(|c| c as u8).collect();
        let chars = bytes.len();
        (bytes, chars)
    };
    let mut data = Vec::new();
    for value in [x, y, x + 50, y + 10] {
        data.extend_from_slice(&int(value));
    }
    data.extend_from_slice(&int(1));
    data.extend_from_slice(&[0; 8]);
    data.extend_from_slice(&int(x));
    data.extend_from_slice(&int(y));
    data.extend_from_slice(&int(chars as i32));
    data.extend_from_slice(&int(76));
    data.extend_from_slice(&int(0));
    data.extend_from_slice(&[0; 16]);
    data.extend_from_slice(&int(0));
    data.extend_from_slice(&bytes);
    data.resize(data.len().div_ceil(4) * 4, 0);
    emf_record(if wide { 0x54 } else { 0x53 }, &data)
}

/// An `EMR_COMMENT` of private data.
#[cfg(feature = "tika")]
pub(crate) fn emf_comment(data: &[u8]) -> Vec<u8> {
    let mut comment = int(data.len() as i32).to_vec();
    comment.extend_from_slice(data);
    comment.resize(comment.len().div_ceil(4) * 4, 0);
    emf_record(0x46, &comment)
}

/// `text` in UTF-16LE.
pub(crate) fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A public `EMR_COMMENT` of `kind` holding `body`.
#[cfg(feature = "tika")]
pub(crate) fn emf_public_comment(kind: u32, body: &[u8]) -> Vec<u8> {
    let mut data = b"GDIC".to_vec();
    data.extend_from_slice(&kind.to_le_bytes());
    data.extend_from_slice(body);
    emf_comment(&data)
}

/// A comment embedding a WMF picture.
#[cfg(feature = "tika")]
pub(crate) fn emf_wmf_comment(wmf: &[u8]) -> Vec<u8> {
    let mut body = vec![1, 0, 0, 0];
    body.extend_from_slice(&int(0));
    body.extend_from_slice(&int(0));
    body.extend_from_slice(&int(wmf.len() as i32));
    body.extend_from_slice(wmf);
    emf_public_comment(0x8000_0001, &body)
}

/// A comment embedding each of `formats` as a format of the picture.
#[cfg(feature = "tika")]
pub(crate) fn emf_formats_comment(formats: &[&[u8]]) -> Vec<u8> {
    let mut body = vec![0; 16];
    body.extend_from_slice(&int(formats.len() as i32));
    let mut offset = 8 + body.len() + 16 * formats.len();
    for format in formats {
        body.extend_from_slice(b" EMF");
        body.extend_from_slice(&int(1));
        body.extend_from_slice(&int(format.len() as i32));
        body.extend_from_slice(&int(offset as i32));
        offset += format.len();
    }
    for format in formats {
        body.extend_from_slice(format);
    }
    emf_public_comment(0x4000_0004, &body)
}

/// A WMF record of `function` holding `data`.
#[cfg(feature = "tika")]
pub(crate) fn wmf_record(function: u16, data: &[u8]) -> Vec<u8> {
    let mut data = data.to_vec();
    data.resize(data.len().div_ceil(2) * 2, 0);
    let mut record = ((6 + data.len() as u32) / 2).to_le_bytes().to_vec();
    record.extend_from_slice(&function.to_le_bytes());
    record.extend_from_slice(&data);
    record
}

/// A WMF picture of `records` and its end-of-file record.
#[cfg(feature = "tika")]
pub(crate) fn wmf(records: &[Vec<u8>]) -> Vec<u8> {
    let mut picture = vec![1, 0, 9, 0, 0, 3];
    let size: usize = 9 + records.iter().map(Vec::len).sum::<usize>() / 2 + 3;
    picture.extend_from_slice(&(size as u32).to_le_bytes());
    picture.extend_from_slice(&[1, 0]);
    picture.extend_from_slice(&int(16));
    picture.extend_from_slice(&[0, 0]);
    for record in records {
        picture.extend_from_slice(record);
    }
    picture.extend(wmf_record(0, &[]));
    picture
}

/// A `META_CREATEFONTINDIRECT` record of a font in the charset `charset`.
#[cfg(feature = "tika")]
pub(crate) fn wmf_font(charset: u8) -> Vec<u8> {
    let mut font = vec![0; 13];
    font.push(charset);
    font.extend_from_slice(&[0; 4]);
    font.extend_from_slice(b"Arial\0");
    wmf_record(0x02FB, &font)
}

/// A `META_TEXTOUT` record of `text`.
#[cfg(feature = "tika")]
pub(crate) fn wmf_text_out(text: &[u8]) -> Vec<u8> {
    let mut data = (text.len() as i16).to_le_bytes().to_vec();
    data.extend_from_slice(text);
    data.resize(2 + text.len().div_ceil(2) * 2, 0);
    data.extend_from_slice(&[0; 4]);
    wmf_record(0x0521, &data)
}

/// A `META_EXTTEXTOUT` record of `text` with a spacing for each character.
#[cfg(feature = "tika")]
pub(crate) fn wmf_ext_text_out(text: &[u8]) -> Vec<u8> {
    let mut data = vec![0; 4];
    data.extend_from_slice(&(text.len() as i16).to_le_bytes());
    data.extend_from_slice(&[0, 0]);
    data.extend_from_slice(text);
    data.resize(8 + text.len().div_ceil(2) * 2, 0);
    for _ in text {
        data.extend_from_slice(&[7, 0]);
    }
    wmf_record(0x0A32, &data)
}
