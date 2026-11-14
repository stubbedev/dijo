//! The `|TOPIC` file: topic blocks, each a block header and data, LZ77-compressed in most
//! WinHelp 3.1 and later files, and in the data of the blocks one after another, the topic
//! links, records that may run on from one block into the next.
//!
//! A topic position (TOPICPOS) is 12 more than the number of a block times the size of a
//! block's decompressed data, 16 KiB from WinHelp 3.1 on and 2 KiB in WinHelp 3.0, plus an
//! offset into that data. Each topic link gives where the next starts: in WinHelp 3.0 by its
//! distance, in later files by its position; a next position of zero or less ends the links.

use super::lz77;
use super::reader::Reader;
use super::system::Layout;
use super::Malformed;

const BLOCK_HEADER: usize = 12;
const LINK_HEADER: usize = 21;
const WINHELP_30_BLOCK: usize = 0x800;
const DECOMPRESSED_BLOCK: usize = 0x4000;

/// The blocks of `|TOPIC`, and the data of the one read last.
pub(super) struct TopicFile<'a> {
    content: &'a [u8],
    layout: Layout,
    block: Option<usize>,
    data: Vec<u8>,
}

/// A topic link: its record type and data, the second part's as stored, and the length of the
/// text that part expands to.
pub(super) struct Link {
    pub(super) record_type: u8,
    pub(super) data1: Vec<u8>,
    pub(super) data2: Vec<u8>,
    pub(super) text_length: usize,
}

/// The topic links of a topic file, from the position of the next on.
pub(super) struct Links<'a> {
    file: TopicFile<'a>,
    position: Option<usize>,
}

impl Iterator for Links<'_> {
    type Item = Result<Link, Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        let at = self.position.take()?;
        match self.file.link(at) {
            Ok(Some((link, next))) => {
                self.position = Some(next);
                Some(Ok(link))
            }
            Ok(None) => None,
            Err(problem) => Some(Err(problem)),
        }
    }
}

impl<'a> TopicFile<'a> {
    pub(super) fn new(content: &'a [u8], layout: Layout) -> Self {
        Self {
            content,
            layout,
            block: None,
            data: Vec::new(),
        }
    }

    fn block_size(&self) -> usize {
        match self.layout {
            Layout::WinHelp30 => WINHELP_30_BLOCK,
            Layout::WinHelp31 { block_size, .. } => block_size,
        }
    }

    fn decompressed_size(&self) -> usize {
        match self.layout {
            Layout::WinHelp30 => WINHELP_30_BLOCK,
            Layout::WinHelp31 { .. } => DECOMPRESSED_BLOCK,
        }
    }

    /// Reads the data of block `number` unless it was read last; false past the last block.
    fn load(&mut self, number: usize) -> bool {
        if self.block == Some(number) {
            return true;
        }
        let block_size = self.block_size();
        let Some(start) = number
            .checked_mul(block_size)
            .filter(|&start| start < self.content.len())
        else {
            return false;
        };
        let end = start.saturating_add(block_size).min(self.content.len());
        let stored = self.content[start..end].get(BLOCK_HEADER..).unwrap_or(&[]);
        match self.layout {
            Layout::WinHelp31 { lz77: true, .. } => {
                lz77::decompress(stored, DECOMPRESSED_BLOCK, &mut self.data)
            }
            _ => {
                self.data.clear();
                self.data.extend_from_slice(stored);
            }
        }
        self.block = Some(number);
        true
    }

    /// Appends the `length` bytes at `position` to `out`, running on into the blocks after its
    /// own, and moves `position` past them; false where the file ends before them.
    fn read(
        &mut self,
        position: &mut usize,
        mut length: usize,
        out: &mut Vec<u8>,
    ) -> Result<bool, Malformed> {
        let decompressed = self.decompressed_size();
        while length > 0 {
            let relative = position.checked_sub(BLOCK_HEADER).ok_or_else(|| {
                Malformed(format!(
                    "the topic position {position} lies within a block header"
                ))
            })?;
            let (number, offset) = (relative / decompressed, relative % decompressed);
            if !self.load(number) {
                return Ok(false);
            }
            let available = self.data.len().checked_sub(offset).ok_or_else(|| {
                Malformed(format!(
                    "the topic position {position} lies past the {} bytes of its block's data",
                    self.data.len()
                ))
            })?;
            let taken = available.min(length);
            out.extend_from_slice(&self.data[offset..offset + taken]);
            length -= taken;
            *position = if length > 0 {
                (number + 1) * decompressed + BLOCK_HEADER
            } else {
                *position + taken
            };
        }
        Ok(true)
    }

    /// The topic links in order, up to the first that cannot be read.
    pub(super) fn links(self) -> Links<'a> {
        Links {
            file: self,
            position: Some(BLOCK_HEADER),
        }
    }

    /// The topic link at `position` and where the next starts, or none for the last link,
    /// which holds nothing.
    fn link(&mut self, position: usize) -> Result<Option<(Link, usize)>, Malformed> {
        let truncated = || {
            Malformed(format!(
                "the topic file ends within the topic link at {position}"
            ))
        };
        let mut at = position;
        let mut header = Vec::with_capacity(LINK_HEADER);
        if !self.read(&mut at, LINK_HEADER, &mut header)? {
            return Err(truncated());
        }
        let mut fields = Reader::new(&header, "a topic link");
        let block_size = fields.i32()?;
        let text_length = fields.i32()?;
        let _previous = fields.i32()?;
        let next = fields.i32()?;
        let data1_length = fields.i32()?;
        let record_type = fields.u8()?;
        let next = match self.layout {
            Layout::WinHelp30 => {
                let next = usize::try_from(next)
                    .ok()
                    .filter(|&next| next > 0)
                    .ok_or_else(|| {
                        Malformed(format!(
                            "the topic link at {position} puts the next {next} bytes on"
                        ))
                    })?;
                let next = position.saturating_add(next);
                if next >= self.content.len() {
                    return Ok(None);
                }
                next
            }
            Layout::WinHelp31 { .. } => {
                if next <= 0 {
                    return Ok(None);
                }
                let next = next as usize;
                if next <= position {
                    return Err(Malformed(format!(
                        "the topic link at {position} puts the next one back at {next}"
                    )));
                }
                next
            }
        };
        let lengths = usize::try_from(data1_length)
            .ok()
            .and_then(|data1| data1.checked_sub(LINK_HEADER).map(|rest| (data1, rest)))
            .and_then(|(data1, rest)| {
                let data2 = usize::try_from(block_size).ok()?.checked_sub(data1)?;
                Some((rest, data2, usize::try_from(text_length).ok()?))
            });
        let Some((data1_length, data2_length, text_length)) = lengths else {
            return Err(Malformed(format!(
                "the topic link at {position} has the sizes {block_size}, {data1_length} and \
                 {text_length}, which do not fit"
            )));
        };
        let mut data1 = Vec::new();
        let mut data2 = Vec::new();
        if !self.read(&mut at, data1_length, &mut data1)?
            || !self.read(&mut at, data2_length, &mut data2)?
        {
            return Err(truncated());
        }
        Ok(Some((
            Link {
                record_type,
                data1,
                data2,
                text_length,
            },
            next,
        )))
    }
}
