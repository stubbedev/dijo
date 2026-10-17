//! The parts of junrar's `ComprDataIO` a RAR 1.5 to 4 entry of one volume is extracted
//! through: its packed data read from the archive, its unpacked data written out with their
//! CRC-32.

use flate2::Crc;

use super::bits::{BitInput, MAX_SIZE};
use super::{Thrown, OUT_OF_BOUNDS};
use crate::compress::MAX_DECODED;

/// `ComprDataIO` over the archive's bytes.
pub(super) struct DataIo<'a> {
    archive: &'a [u8],
    position: u64,
    packed_left: i64,
    unpacked: Vec<u8>,
    crc: Crc,
    /// The bytes every extraction of the archive kept, which [`MAX_DECODED`] bounds.
    kept: usize,
}

impl<'a> DataIo<'a> {
    pub(super) fn new(archive: &'a [u8]) -> Self {
        DataIo {
            archive,
            position: 0,
            packed_left: 0,
            unpacked: Vec::new(),
            crc: Crc::new(),
            kept: 0,
        }
    }

    /// `init(OutputStream)` and `init(FileHeader)`: the packed data of `packed` bytes from
    /// `start` on, unpacked afresh, with a fresh CRC.
    pub(super) fn start(&mut self, start: u64, packed: i64) {
        self.unpacked = Vec::new();
        self.position = start;
        self.packed_left = packed;
        self.crc = Crc::new();
    }

    /// What the extraction unpacked, once it is over.
    pub(super) fn take_data(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.unpacked)
    }

    /// The CRC-32 of the data written since [`DataIo::start`].
    pub(super) fn crc(&self) -> u32 {
        self.crc.sum()
    }

    /// `unpRead`: up to `count` packed bytes into `buf` from `offset` on, no more than the
    /// entry has left; an archive that ends before them fails as `RandomAccessFile.readFully`
    /// does.
    pub(super) fn read(&mut self, buf: &mut [u8], offset: i32, count: i32) -> Result<i32, Thrown> {
        if count <= 0 {
            return Ok(0);
        }
        let size = if i64::from(count) > self.packed_left {
            self.packed_left as i32
        } else {
            count
        };
        if size == 0 {
            return Ok(0);
        }
        let from = usize::try_from(self.position).map_err(|_| EOF)?;
        let length = usize::try_from(size).map_err(|_| OUT_OF_BOUNDS)?;
        let packed = from
            .checked_add(length)
            .and_then(|end| self.archive.get(from..end))
            .ok_or(EOF)?;
        let target = usize::try_from(offset)
            .ok()
            .and_then(|offset| buf.get_mut(offset..offset.checked_add(length)?))
            .ok_or(OUT_OF_BOUNDS)?;
        target.copy_from_slice(packed);
        self.position += length as u64;
        self.packed_left -= i64::from(size);
        Ok(size)
    }

    /// `unpWrite`: `count` unpacked bytes of `data` from `offset` on.
    pub(super) fn write(&mut self, data: &[u8], offset: i32, count: i32) -> Result<(), Thrown> {
        let unpacked = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(count).ok())
            .and_then(|(offset, count)| data.get(offset..offset.checked_add(count)?))
            .ok_or(OUT_OF_BOUNDS)?;
        self.kept += unpacked.len();
        if self.kept > MAX_DECODED {
            return Err(TOO_LARGE);
        }
        self.unpacked.extend_from_slice(unpacked);
        self.crc.update(unpacked);
        Ok(())
    }
}

const EOF: Thrown = Thrown("an EOFException, the archive ending before the entry's packed data");
const TOO_LARGE: Thrown = Thrown("unpacked data past the most bytes the port decodes");

/// The packed data an unpacker reads, through its bit input: `Unpack15`'s `readTop` and
/// `readBorder` over junrar's `BitInput`, refilled from the archive.
pub(super) struct Input<'a> {
    pub(super) bits: BitInput,
    pub(super) read_top: i32,
    pub(super) read_border: i32,
    pub(super) io: DataIo<'a>,
}

impl Input<'_> {
    /// `unpReadBuf`: the unread bytes moved to the front once past the middle, and the buffer
    /// filled after them; `false` once the bit position has passed the bytes read, as only a
    /// missing next volume, which a port of one volume never reads, otherwise makes it.
    pub(super) fn read_buf(&mut self) -> Result<bool, Thrown> {
        let mut data_size = self.read_top - self.bits.in_addr;
        if data_size < 0 {
            return Ok(false);
        }
        if self.bits.in_addr > MAX_SIZE / 2 {
            if data_size > 0 {
                let from = self.bits.in_addr as usize;
                self.bits
                    .buf
                    .copy_within(from..from + data_size as usize, 0);
            }
            self.bits.in_addr = 0;
            self.read_top = data_size;
        } else {
            data_size = self.read_top;
        }
        let read = self
            .io
            .read(&mut self.bits.buf, data_size, (MAX_SIZE - data_size) & !0xf)?;
        if read > 0 {
            self.read_top += read;
        }
        self.read_border = self.read_top - 30;
        Ok(true)
    }

    /// `Unpack.getChar`: the next byte, which the PPMd decoder reads.
    pub(super) fn get_char(&mut self) -> Result<i32, Thrown> {
        if self.bits.in_addr > MAX_SIZE - 30 {
            self.read_buf()?;
        }
        let byte = self.bits.byte(self.bits.in_addr)?;
        self.bits.in_addr += 1;
        Ok(byte)
    }
}
