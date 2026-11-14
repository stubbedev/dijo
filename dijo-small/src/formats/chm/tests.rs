use super::*;
use crate::compress::lzx::tests::{Stream, Token};

const CHUNK: usize = 0x1000;
const STORAGE: &str = "::DataSpace/Storage/MSCompressed/";

/// `n` as an ENCINT: big-endian groups of seven bits, all but the last with the high bit set.
fn encint(mut n: u64) -> Vec<u8> {
    let mut groups = vec![(n & 0x7f) as u8];
    n >>= 7;
    while n > 0 {
        groups.push((n & 0x7f) as u8 | 0x80);
        n >>= 7;
    }
    groups.reverse();
    groups
}

/// A CHM file of one listing chunk with the LZX control data, reset table and content entries
/// Tika's extractor looks for, `lzxc` as the control data's signature, and then the entries
/// `pages`, stored uncompressed, and `compressed`, in the LZX content: one frame of a verbatim
/// block of literals with a 64 KiB window.
fn chm(pages: &[(&str, &[u8])], compressed: &[(&str, &[u8])], lzxc: &[u8; 4]) -> Vec<u8> {
    let mut control = 0x18u32.to_le_bytes().to_vec();
    control.extend(lzxc);
    for value in [2u32, 2, 2, 1, 0] {
        control.extend(value.to_le_bytes());
    }
    let decoded: Vec<u8> = compressed
        .iter()
        .flat_map(|(_, data)| *data)
        .copied()
        .collect();
    let mut stream = Stream::new(WindowSize::from_bits(16).unwrap(), None);
    stream.verbatim(
        &decoded
            .iter()
            .map(|&byte| Token::Literal(byte))
            .collect::<Vec<_>>(),
    );
    let lzx = stream.finish();
    let mut reset_table = Vec::new();
    for value in [2u32, 2, 8, 0x28] {
        reset_table.extend(value.to_le_bytes());
    }
    let lzx_length = lzx.len() as u64;
    for value in [decoded.len() as u64, lzx_length, 0x8000, 0, lzx_length] {
        reset_table.extend(value.to_le_bytes());
    }
    let mut entries: Vec<(String, Vec<u8>)> = vec![
        (format!("{STORAGE}ControlData"), control),
        (
            format!("{STORAGE}Transform/{{7FC28940-9D31-11D0-9B27-00A0C91E9C7C}}/InstanceData/ResetTable"),
            reset_table,
        ),
        (format!("{STORAGE}Content"), lzx),
    ];
    entries.extend(
        pages
            .iter()
            .map(|(name, data)| (name.to_string(), data.to_vec())),
    );
    let mut content: Vec<u8> = Vec::new();
    let mut chunk = b"PMGL".to_vec();
    chunk.extend([0; 12]);
    chunk.extend((-1i32).to_le_bytes());
    for (name, data) in &entries {
        chunk.extend(encint(name.len() as u64));
        chunk.extend(name.as_bytes());
        chunk.extend(encint(0));
        chunk.extend(encint(content.len() as u64));
        chunk.extend(encint(data.len() as u64));
        content.extend(data);
    }
    let mut offset = 0;
    for (name, data) in compressed {
        chunk.extend(encint(name.len() as u64));
        chunk.extend(name.as_bytes());
        chunk.extend(encint(1));
        chunk.extend(encint(offset));
        chunk.extend(encint(data.len() as u64));
        offset += data.len() as u64;
    }
    let free = CHUNK - chunk.len();
    chunk[4..8].copy_from_slice(&(free as u32).to_le_bytes());
    chunk.resize(CHUNK, 0);

    let directory_offset = 0x60u64;
    let mut itsp = b"ITSP".to_vec();
    for value in [1i32, 0x54, 10, CHUNK as i32, 2, 1, -1, 0, 0, -1, 1, 0x409] {
        itsp.extend(value.to_le_bytes());
    }
    itsp.extend([0; 32]);
    let directory_length = (itsp.len() + chunk.len()) as u64;
    let mut file = b"ITSF".to_vec();
    for value in [3i32, 0x60, 1, 0, 0x409] {
        file.extend(value.to_le_bytes());
    }
    file.extend([0; 32]);
    for value in [
        0u64,
        0,
        directory_offset,
        directory_length,
        directory_offset + directory_length,
    ] {
        file.extend(value.to_le_bytes());
    }
    file.extend(itsp);
    file.extend(chunk);
    file.extend(content);
    file
}

#[test]
fn reads_the_directory_listing_as_tika_does() {
    let file = chm(
        &[("/a.html", b"<p>a</p>"), ("/b.css", b"b {}")],
        &[],
        b"LZXC",
    );
    let extractor = Extractor::new(&file, Pages::IN_MEMORY).unwrap();
    let names: Vec<&str> = extractor
        .listing
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names[3..], ["/a.html", "/b.css"]);
    assert_eq!(extractor.listing.control_data, Some(0));
    assert_eq!(extractor.listing.reset_table, Some(1));
    assert_eq!(
        extractor.extract(&extractor.listing.entries[3]).unwrap(),
        b"<p>a</p>"
    );
    assert_eq!(encint(300), [0x82, 0x2c]);
}

#[test]
fn decompresses_the_entries_of_the_lzx_content() {
    let pages: [(&str, &[u8]); 2] = [("/c.html", b"<p>compressed</p>"), ("/d.htm", b"<p>d</p>")];
    let file = chm(&[], &pages, b"LZXC");
    let extractor = Extractor::new(&file, Pages::IN_MEMORY).unwrap();
    let extracted: Vec<(&str, Vec<u8>)> = extractor.pages().map(Result::unwrap).collect();
    assert_eq!(extracted, pages.map(|(name, data)| (name, data.to_vec())));
}
