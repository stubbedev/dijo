//! The Ogg packets of a stream as vorbis-java 0.8's `OggPacketReader` reads them: the next
//! `OggS` capture pattern found within 64 KiB, each page's lacing values cut into packets, and a
//! packet a page leaves unfinished joined to the first of the next page read, whatever its stream.
//! The checksums are not checked, and data that ends inside a page ends the packets.
//!
//! The pages are read in place, and a packet holds its first fragment, its length and where it
//! goes on: a packet that runs over pages, as an audio packet may over all of a file, is joined
//! only where its bytes are asked for, by reading its pages again.

use std::borrow::Cow;

use crate::input::{Pages, Paging};

/// What vorbis-java throws, which fails the parse.
pub(super) type Thrown = String;

/// The bytes of a packet [`Packet::head`] holds at least, unless the packet is shorter: the
/// fragment of a packet that a page leaves unfinished is a run of lacing values of 255.
pub(super) const HEAD: usize = 255;

/// A packet of a page.
#[derive(Clone)]
pub(super) struct Packet<'a> {
    input: &'a [u8],
    head: &'a [u8],
    length: usize,
    /// Where the pages after that of its first fragment are searched for, where it runs on to
    /// them.
    continued_at: Option<usize>,
    pub(super) sid: i32,
    pub(super) granule_position: i64,
    pub(super) beginning_of_stream: bool,
    /// The data size of the page the packet ends on.
    page_data_size: usize,
}

impl<'a> Packet<'a> {
    /// The packet's first bytes: all of it, or at least its first [`HEAD`].
    pub(super) fn head(&self) -> &'a [u8] {
        self.head
    }

    pub(super) fn len(&self) -> usize {
        self.length
    }

    /// All of the packet's bytes.
    pub(super) fn data(&self) -> Cow<'a, [u8]> {
        self.prefix(self.length)
    }

    /// The packet's first `length` bytes, or all of them where it is shorter: those of the pages
    /// it runs on to joined, read again.
    pub(super) fn prefix(&self, length: usize) -> Cow<'a, [u8]> {
        let length = length.min(self.length);
        let Some(at) = self.continued_at.filter(|_| length > self.head.len()) else {
            return Cow::Borrowed(&self.head[..length]);
        };
        let mut joined = Vec::with_capacity(length);
        joined.extend_from_slice(self.head);
        let mut reader = Reader {
            input: self.input,
            at,
            page: None,
            paging: Paging::new(Pages::IN_MEMORY),
        };
        while joined.len() < length {
            let mut page = reader
                .next_page()
                .ok()
                .flatten()
                .expect("the pages a packet runs on to read as they did");
            let (fragment, _) = page.next_fragment();
            joined.extend_from_slice(&fragment[..fragment.len().min(length - joined.len())]);
        }
        Cow::Owned(joined)
    }

    /// `getOverheadBytes`, for what it throws: an `ArithmeticException` for a packet longer than
    /// the data of the page it ends on, where that page holds none.
    pub(super) fn check_overhead(&self) -> Result<(), Thrown> {
        if self.length > self.page_data_size && self.page_data_size == 0 {
            return Err("ArithmeticException: / by zero".to_owned());
        }
        Ok(())
    }
}

/// The start of a packet a page left unfinished: its first fragment, its length so far, and
/// where the next page is searched for.
struct Unfinished<'a> {
    head: &'a [u8],
    length: usize,
    continued_at: usize,
}

/// An `OggPage` read from the stream, and its packet iterator's position.
struct Page<'a> {
    sid: i32,
    granule_position: i64,
    beginning_of_stream: bool,
    lacing: &'a [u8],
    data: &'a [u8],
    current_lacing: usize,
    current_offset: usize,
}

impl<'a> Page<'a> {
    /// `OggPacketIterator.hasNext`: a lacing value left, or the one empty packet of a page
    /// without lacing values.
    fn has_next(&self) -> bool {
        self.current_lacing < self.lacing.len()
            || (self.current_lacing == 0 && self.lacing.is_empty())
    }

    /// `OggPacketIterator.next` without the join: the next fragment of a packet, and whether
    /// the packet runs on past the page.
    fn next_fragment(&mut self) -> (&'a [u8], bool) {
        let mut continues = false;
        let mut lacing_values = 0;
        let mut size = 0;
        for (index, &value) in self.lacing.iter().enumerate().skip(self.current_lacing) {
            size += usize::from(value);
            lacing_values += 1;
            if value < 255 {
                break;
            }
            if index == self.lacing.len() - 1 {
                continues = true;
            }
        }
        let fragment = &self.data[self.current_offset..self.current_offset + size];
        self.current_lacing += lacing_values;
        self.current_offset += size;
        if self.current_lacing == 0 {
            self.current_lacing = 1;
        }
        (fragment, continues)
    }
}

/// `IOUtils.getInt` of the eight bytes of a granule position, with its `int` shifts: the high
/// four bytes shifted by their distance modulo 32, all `-1` but for the third byte as `-1`.
fn granule_position(bytes: [u8; 8]) -> i64 {
    let [b0, b1, b2, b3, b4, b5, b6, b7] = bytes.map(i32::from);
    if b0 == 255 && b1 == 255 && b3 == 255 && b4 == 255 && b5 == 255 && b6 == 255 && b7 == 255 {
        return -1;
    }
    let low = (b3 << 24)
        .wrapping_add(b2 << 16)
        .wrapping_add(b1 << 8)
        .wrapping_add(b0);
    i64::from(b7 << 24) + i64::from(b6 << 16) + i64::from(b5 << 8) + i64::from(b4) + i64::from(low)
}

/// `OggPacketReader` over a stream's bytes, whose pages are released as they are read.
pub(super) struct Reader<'a> {
    input: &'a [u8],
    at: usize,
    page: Option<Page<'a>>,
    paging: Paging<'a>,
}

/// The longest search for the next capture pattern.
const SEARCH_LIMIT: usize = 65536;

impl<'a> Reader<'a> {
    pub(super) fn new(input: &'a [u8], pages: Pages<'a>) -> Self {
        Self {
            input,
            at: 0,
            page: None,
            paging: Paging::new(pages),
        }
    }

    fn read(&mut self) -> Option<u8> {
        let byte = self.input.get(self.at).copied()?;
        self.at += 1;
        Some(byte)
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let bytes = self.input.get(self.at..self.at + count)?;
        self.at += count;
        Some(bytes)
    }

    /// `getNextPacket`: the next packet, `None` at the end of the data.
    pub(super) fn next(&mut self) -> Result<Option<Packet<'a>>, Thrown> {
        let mut unfinished: Option<Unfinished<'a>> = None;
        loop {
            if let Some(page) = &mut self.page {
                if page.has_next() {
                    let first = page.current_lacing == 0;
                    let (fragment, continues) = page.next_fragment();
                    let (head, length, continued_at) = match unfinished.take() {
                        Some(start) => (
                            start.head,
                            start.length + fragment.len(),
                            Some(start.continued_at),
                        ),
                        None => (fragment, fragment.len(), None),
                    };
                    if continues {
                        unfinished = Some(Unfinished {
                            head,
                            length,
                            continued_at: continued_at.unwrap_or(self.at),
                        });
                    } else {
                        return Ok(Some(Packet {
                            input: self.input,
                            head,
                            length,
                            continued_at,
                            sid: page.sid,
                            granule_position: page.granule_position,
                            beginning_of_stream: page.beginning_of_stream && first,
                            page_data_size: page.data.len(),
                        }));
                    }
                }
            }
            match self.next_page()? {
                Some(page) => self.page = Some(page),
                None => return Ok(None),
            }
        }
    }

    /// The next page, past the capture pattern searched for, its bytes counted read; `None`
    /// where the data ends before or inside it.
    fn next_page(&mut self) -> Result<Option<Page<'a>>, Thrown> {
        let start = self.at;
        let page = self
            .search()
            .and_then(|found| if found { self.page() } else { Ok(None) });
        self.paging.read(self.at - start);
        page
    }

    /// Whether the next capture pattern is found within [`SEARCH_LIMIT`] bytes before the data
    /// ends.
    fn search(&mut self) -> Result<bool, Thrown> {
        let mut searched = 0;
        let mut matched = 0;
        while searched < SEARCH_LIMIT {
            let Some(byte) = self.read() else {
                return Ok(false);
            };
            matched = match (matched, byte) {
                (0, b'O') | (1, b'g') | (2, b'g') => matched + 1,
                (3, b'S') => return Ok(true),
                _ => 0,
            };
            searched += 1;
        }
        Err(format!(
            "IOException: Next ogg packet header not found after searching {searched} bytes"
        ))
    }

    /// `new OggPage(inp)` after the capture pattern: `None` where the data ends inside it.
    fn page(&mut self) -> Result<Option<Page<'a>>, Thrown> {
        let Some(&[version, flags]) = self.take(2) else {
            return Ok(None);
        };
        if version != 0 {
            return Err(format!(
                "IllegalArgumentException: Found Ogg page in format {version} but we only support \
                 version 0"
            ));
        }
        let Some(header) = self.take(21) else {
            return Ok(None);
        };
        let granule = granule_position(header[..8].try_into().expect("eight bytes"));
        let sid = i32::from_le_bytes(header[8..12].try_into().expect("four bytes"));
        let lacing_count = usize::from(header[20]);
        let Some(lacing) = self.take(lacing_count) else {
            return Ok(None);
        };
        let size = lacing.iter().map(|&value| usize::from(value)).sum();
        let Some(data) = self.take(size) else {
            return Ok(None);
        };
        Ok(Some(Page {
            sid,
            granule_position: granule,
            beginning_of_stream: flags & 0x02 != 0,
            lacing,
            data,
            current_lacing: 0,
            current_offset: 0,
        }))
    }

    /// `getNextPacketWithSid`.
    pub(super) fn next_with_sid(&mut self, sid: i32) -> Result<Option<Packet<'a>>, Thrown> {
        while let Some(packet) = self.next()? {
            if packet.sid == sid {
                return Ok(Some(packet));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(flags: u8, granule: u64, lacing: &[u8], data: &[u8]) -> Vec<u8> {
        let mut page = b"OggS\x00".to_vec();
        page.push(flags);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&7u32.to_le_bytes());
        page.extend_from_slice(&[0; 8]);
        page.push(u8::try_from(lacing.len()).unwrap());
        page.extend_from_slice(lacing);
        page.extend_from_slice(data);
        page
    }

    #[test]
    fn reads_packets_across_pages_as_vorbis_java_does() {
        let long = vec![1u8; 255];
        let mut input = page(2, 0, &[3, 255], &[b"abc".as_slice(), &long].concat());
        input.extend(page(0, 0x0102_0304, &[2], b"de"));
        input.extend(b"junk");
        input.extend(page(4, 9, &[], b""));
        let mut reader = Reader::new(&input, Pages::IN_MEMORY);
        let first = reader.next().unwrap().unwrap();
        assert_eq!(*first.data(), *b"abc");
        assert!(first.beginning_of_stream);
        let second = reader.next().unwrap().unwrap();
        assert_eq!(second.len(), 257);
        assert_eq!(second.head(), long.as_slice());
        assert_eq!(*second.data(), *[long.as_slice(), b"de"].concat());
        assert_eq!(*second.prefix(256), *[long.as_slice(), b"d"].concat());
        assert_eq!(second.granule_position, 0x0102_0304);
        let empty = reader.next().unwrap().unwrap();
        assert_eq!(empty.len(), 0);
        assert!(reader.next().unwrap().is_none());
    }

    #[test]
    fn reads_granule_positions_with_javas_int_shifts() {
        assert_eq!(granule_position([1, 0, 0, 0, 0, 0, 0, 0]), 1);
        assert_eq!(granule_position([0, 0, 0, 0, 1, 0, 0, 0]), 1);
        assert_eq!(granule_position([0, 0, 0, 0x80, 0, 0, 0, 0]), -0x8000_0000);
        assert_eq!(
            granule_position([0xFF, 0xFF, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
            -1
        );
    }
}
