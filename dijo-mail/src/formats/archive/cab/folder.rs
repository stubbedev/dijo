//! The data of a cabinet's folder ([MS-CAB] 2.4): its `CFDATA` blocks decoded in order, each
//! into at most 32 KiB, stored, MSZIP ([MS-MCI] 2.1: a deflate stream after `CK`, whose window
//! is the folder's 32 KiB decoded before it) or LZX (the `lzxd` crate, each block one LZX frame
//! of the folder's stream, with its E8 call translation), and the bytes of a file copied out of
//! them.

use flate2::{Decompress, FlushDecompress, Status};

use super::cabinet::{Folder, Method};
use crate::compress::{Decoding, Held, Undecoded};
use crate::input::Paging;

/// The most bytes a `CFDATA` block decodes into, and the window of MSZIP.
pub(super) const MAX_BLOCK: usize = 32 * 1024;
/// The bytes of a `CFDATA` block before its reserved area.
const BLOCK_HEADER: usize = 8;

/// How a folder's data is decoded.
enum Codec {
    Stored,
    Mszip(Box<Mszip>),
    Lzx(Box<lzxd::Lzxd>),
}

/// The state MSZIP keeps across a folder's blocks: the inflater, reused, and the last 32 KiB the
/// folder decoded into, the dictionary of the next block.
struct Mszip {
    inflater: Decompress,
    window: Vec<u8>,
}

impl Mszip {
    /// Inflates `data`, a block that decodes into `length` bytes, into `out`.
    fn inflate(&mut self, data: &[u8], length: usize, out: &mut Vec<u8>) -> Result<(), String> {
        let deflated = data
            .strip_prefix(b"CK")
            .ok_or("the MSZIP block does not start with CK")?;
        self.inflater.reset(false);
        if !self.window.is_empty() {
            self.inflater
                .set_dictionary(&self.window)
                .map_err(|error| format!("the MSZIP window is refused: {error}"))?;
        }
        let status = self
            .inflater
            .decompress_vec(deflated, out, FlushDecompress::Finish)
            .map_err(|error| format!("the MSZIP block does not inflate: {error}"))?;
        if status != Status::StreamEnd || out.len() != length {
            return Err(format!(
                "the MSZIP block inflates into {}{} bytes, where its header says {length}",
                out.len(),
                if status == Status::StreamEnd {
                    ""
                } else {
                    " or more"
                }
            ));
        }
        let kept = self.window.len().saturating_sub(MAX_BLOCK - out.len());
        self.window.drain(..kept);
        self.window.extend_from_slice(out);
        Ok(())
    }
}

/// Where a folder's data in the cabinet ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum End {
    /// With its last block.
    Folder,
    /// With a block the next cabinet ends, `cbUncomp` 0.
    Split,
}

/// What became of a file's data copied out of its folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Copied {
    /// All of it was copied.
    Whole,
    /// The copy stopped at the limit of what the cabinet is decoded into.
    Cut,
    /// The folder's data ends before the file's does.
    Short(End),
}

/// A folder's data, decoded a block at a time.
pub(super) struct FolderStream<'a, 'p> {
    cabinet: &'a [u8],
    paging: &'p Paging<'a>,
    block_reserve: usize,
    codec: Codec,
    /// Where the next block's header is.
    next_block: usize,
    blocks_left: u16,
    /// The last block decoded.
    block: Vec<u8>,
    /// Where in the folder's decoded data the last block starts.
    start: u64,
    /// Where the folder's data in the cabinet ended, once it has.
    end: Option<End>,
    /// Why the folder's data stopped decoding, which it stops at for good.
    failed: Option<String>,
}

impl<'a, 'p> FolderStream<'a, 'p> {
    /// The stream of `folder` of `cabinet`, whose blocks have reserved areas of `block_reserve`
    /// bytes, if its method is one the parser decodes: else why not. The blocks it reads are
    /// counted read by `paging`.
    pub(super) fn new(
        cabinet: &'a [u8],
        paging: &'p Paging<'a>,
        folder: &Folder,
        block_reserve: usize,
    ) -> Result<Self, String> {
        let codec = match folder.method {
            Method::Stored => Codec::Stored,
            Method::Mszip => Codec::Mszip(Box::new(Mszip {
                inflater: Decompress::new(false),
                window: Vec::with_capacity(MAX_BLOCK),
            })),
            Method::Lzx(window) => Codec::Lzx(Box::new(lzxd::Lzxd::new(window))),
            Method::Quantum => {
                return Err("its folder is compressed with Quantum, which is not decoded".into())
            }
            Method::Undefined(type_compress) => {
                return Err(format!(
                    "its folder is compressed by the method {type_compress:#06x}, which the \
                     cabinet format does not define"
                ))
            }
        };
        Ok(Self {
            cabinet,
            paging,
            block_reserve,
            codec,
            next_block: folder.first_block,
            blocks_left: folder.blocks,
            block: Vec::with_capacity(MAX_BLOCK),
            start: 0,
            end: None,
            failed: None,
        })
    }

    /// Where in the folder's decoded data the stream stands: what it decodes next starts there
    /// or before.
    pub(super) fn start(&self) -> u64 {
        self.start
    }

    /// The bytes of the block at `self.next_block`: its data, and what it decodes into.
    fn block_at(&self) -> Result<(&'a [u8], usize), String> {
        let at = self.next_block;
        let past = || format!("the CFDATA block at offset {at} runs past the end of the cabinet");
        let header_end = at
            .checked_add(BLOCK_HEADER + self.block_reserve)
            .filter(|&end| end <= self.cabinet.len())
            .ok_or_else(past)?;
        let header = &self.cabinet[at..at + BLOCK_HEADER];
        let compressed = usize::from(u16::from_le_bytes([header[4], header[5]]));
        let decoded = usize::from(u16::from_le_bytes([header[6], header[7]]));
        let data = self
            .cabinet
            .get(header_end..header_end + compressed)
            .ok_or_else(past)?;
        if decoded > MAX_BLOCK {
            return Err(format!(
                "the CFDATA block at offset {at} decodes into {decoded} bytes, more than \
                 {MAX_BLOCK}"
            ));
        }
        Ok((data, decoded))
    }

    /// Decodes the next block, if the folder's data in the cabinet goes on: whether it did.
    fn advance(&mut self) -> Result<bool, String> {
        if let Some(reason) = &self.failed {
            return Err(reason.clone());
        }
        let advanced = self.decode_next();
        if let Err(reason) = &advanced {
            self.failed = Some(reason.clone());
        }
        advanced
    }

    /// [`FolderStream::advance`], but for a folder that failed before.
    fn decode_next(&mut self) -> Result<bool, String> {
        if self.end.is_some() {
            return Ok(false);
        }
        if self.blocks_left == 0 {
            self.end = Some(End::Folder);
            return Ok(false);
        }
        let (data, decoded) = self.block_at()?;
        if decoded == 0 {
            self.end = Some(End::Split);
            return Ok(false);
        }
        let at = self.next_block;
        self.start += self.block.len() as u64;
        self.block.clear();
        let decoding = match &mut self.codec {
            Codec::Stored if data.len() == decoded => {
                self.block.extend_from_slice(data);
                Ok(())
            }
            Codec::Stored => Err(format!(
                "the stored block holds {} bytes, where its header says {decoded}",
                data.len()
            )),
            Codec::Mszip(mszip) => mszip.inflate(data, decoded, &mut self.block),
            Codec::Lzx(lzx) => lzx
                .decompress_next(data, decoded)
                .map(|frame| self.block.extend_from_slice(frame))
                .map_err(|error| format!("the LZX frame does not decode: {error}")),
        };
        decoding.map_err(|reason| format!("the CFDATA block at offset {at}: {reason}"))?;
        let read = BLOCK_HEADER + self.block_reserve + data.len();
        self.paging.read(read);
        self.next_block = at + read;
        self.blocks_left -= 1;
        Ok(true)
    }

    /// Copies the `size` bytes at `offset` of the folder's decoded data, which starts no
    /// earlier than [`FolderStream::start`], to `out`, decoding on as far as it needs, each block
    /// counted against the `budget` of bytes left to decode the cabinet into.
    pub(super) fn copy<W: Held>(
        &mut self,
        offset: u64,
        size: u64,
        out: &mut Decoding<W>,
        budget: &mut u64,
    ) -> Result<Copied, Uncopied> {
        debug_assert!(offset >= self.start);
        let end = offset + size;
        loop {
            let block_end = self.start + self.block.len() as u64;
            let (from, to) = (offset.max(self.start), end.min(block_end));
            if from < to {
                out.write(&self.block[(from - self.start) as usize..(to - self.start) as usize])
                    .map_err(Uncopied::Unheld)?;
                if out.is_cut() {
                    return Ok(Copied::Cut);
                }
            }
            if block_end >= end {
                return Ok(Copied::Whole);
            }
            if *budget == 0 {
                return Ok(Copied::Cut);
            }
            if !self.advance().map_err(Uncopied::Corrupt)? {
                return Ok(Copied::Short(self.end.expect("the folder's data ended")));
            }
            *budget = budget.saturating_sub(self.block.len() as u64);
        }
    }
}

/// Why a file's data was not copied.
#[derive(Debug)]
pub(super) enum Uncopied {
    /// The folder's data does not decode, for this reason.
    Corrupt(String),
    /// What it decodes into cannot be held, for this reason.
    Unheld(Undecoded),
}
