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

/// The LZX content of a CHM file: its stream, where the stream of each block starts, and how
/// many bytes it decodes to.
struct Content {
    stream: Vec<u8>,
    addresses: Vec<u64>,
    decoded_length: u64,
}

/// A CHM file of one listing chunk with the LZX control data, reset table and content entries
/// Tika's extractor looks for, `lzxc` as the control data's signature, and then the entries
/// `pages`, stored uncompressed, and `compressed`, in the LZX content: one frame of a verbatim
/// block of literals with a 64 KiB window.
fn chm(pages: &[(&str, &[u8])], compressed: &[(&str, &[u8])], lzxc: &[u8; 4]) -> Vec<u8> {
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
    let stream = stream.finish();
    let content = Content {
        addresses: vec![0, stream.len() as u64],
        stream,
        decoded_length: decoded.len() as u64,
    };
    let mut offset = 0;
    let compressed: Vec<(&str, u64, u64)> = compressed
        .iter()
        .map(|(name, data)| {
            let entry = (*name, offset, data.len() as u64);
            offset += data.len() as u64;
            entry
        })
        .collect();
    chm_of(pages, &compressed, content, lzxc)
}

/// A CHM file as [`chm`] writes it, of the LZX `content` with a reset interval of two blocks,
/// whose entries `compressed` are each a name, an offset and a length in what it decodes to.
fn chm_of(
    pages: &[(&str, &[u8])],
    compressed: &[(&str, u64, u64)],
    content: Content,
    lzxc: &[u8; 4],
) -> Vec<u8> {
    let mut control = 0x18u32.to_le_bytes().to_vec();
    control.extend(lzxc);
    for value in [2u32, 2, 2, 1, 0] {
        control.extend(value.to_le_bytes());
    }
    let mut reset_table = Vec::new();
    for value in [2, content.addresses.len() as u32, 8, 0x28] {
        reset_table.extend(value.to_le_bytes());
    }
    let lzx_length = content.stream.len() as u64;
    for value in [content.decoded_length, lzx_length, 0x8000] {
        reset_table.extend(value.to_le_bytes());
    }
    for address in &content.addresses {
        reset_table.extend(address.to_le_bytes());
    }
    let lzx = content.stream;
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
    for (name, offset, length) in compressed {
        chunk.extend(encint(name.len() as u64));
        chunk.extend(name.as_bytes());
        chunk.extend(encint(1));
        chunk.extend(encint(*offset));
        chunk.extend(encint(*length));
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
    let mut data = b"left over".to_vec();
    extractor
        .extract(&extractor.listing.entries[3], &mut data)
        .unwrap();
    assert_eq!(data, b"<p>a</p>");
    assert_eq!(encint(300), [0x82, 0x2c]);
}

#[test]
fn decompresses_the_entries_of_the_lzx_content() {
    let pages: [(&str, &[u8]); 2] = [("/c.html", b"<p>compressed</p>"), ("/d.htm", b"<p>d</p>")];
    let file = chm(&[], &pages, b"LZXC");
    let extractor = Extractor::new(&file, Pages::IN_MEMORY).unwrap();
    let mut data = Vec::new();
    let extracted: Vec<(&str, Vec<u8>)> = extractor
        .pages()
        .map(|entry| {
            extractor.extract(entry, &mut data).unwrap();
            (entry.name.as_str(), data.clone())
        })
        .collect();
    assert_eq!(extracted, pages.map(|(name, data)| (name, data.to_vec())));
}

/// Pages listed out of the order of the content, across the frames of a reset interval and
/// into the next interval, are each decoded whole: a page of an interval decoded before is
/// decoded again from its reset point.
#[test]
fn decodes_pages_listed_out_of_the_order_of_the_content() {
    let first: Vec<u8> = (0..2 * 0x8000u32).map(|i| b'a' + (i % 26) as u8).collect();
    let second = b"<p>after the reset</p>".to_vec();
    let uncompressed = |data: &[u8]| {
        let mut stream = Stream::new(WindowSize::from_bits(16).unwrap(), None);
        stream.uncompressed([1, 1, 1], data);
        stream.finish()
    };
    let mut stream = uncompressed(&first);
    let header = (stream.len() - first.len()) as u64;
    let reset = stream.len() as u64;
    stream.extend(uncompressed(&second));
    let content = Content {
        stream,
        addresses: vec![0, header + 0x8000, reset],
        decoded_length: (first.len() + second.len()) as u64,
    };
    let reset_offset = first.len() as u64;
    let file = chm_of(
        &[],
        &[
            ("/a.html", reset_offset, second.len() as u64),
            ("/m.html", 10, 20),
            ("/z.html", 0x8000 - 100, 300),
            ("/zz.html", reset_offset + 3, 10),
        ],
        content,
        b"LZXC",
    );
    let extractor = Extractor::new(&file, Pages::IN_MEMORY).unwrap();
    let mut data = Vec::new();
    let extracted: Vec<Vec<u8>> = extractor
        .pages()
        .map(|entry| {
            extractor.extract(entry, &mut data).unwrap();
            data.clone()
        })
        .collect();
    assert_eq!(
        extracted,
        [
            second.clone(),
            first[10..30].to_vec(),
            first[0x8000 - 100..0x8000 + 200].to_vec(),
            second[3..13].to_vec(),
        ]
    );
}
