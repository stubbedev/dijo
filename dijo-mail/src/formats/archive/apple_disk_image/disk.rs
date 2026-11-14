//! The disk a disk image holds, read at any byte: its sectors found in their chunks, a stored
//! chunk read in place, a coded one decoded whole when first read and kept among the last few
//! decoded, so that reading the disk holds at most [`CACHED`] chunks of at most [`MAX_CHUNK`]
//! bytes at once, whatever the image's size.
//!
//! What the disk is read for is counted against a budget of [`decode_limit`] bytes of the
//! image's length: each chunk decoded, and each byte read through [`Disk::read`], so that a
//! hostile image cannot loop the parser over its file system or decode a few bytes into a
//! flood. Once spent, every read fails with [`Fault::Spent`].

use bzip2::{Decompress as Bunzip, Status as BunzipStatus};
use flate2::{Decompress, FlushDecompress, Status};

use super::udif::{Chunk, Coding, SECTOR};
use super::{adc, lzfse};
use crate::compress::decode_limit;
use crate::input::{Pages, Paging};

/// The most bytes a coded chunk is decoded into.
pub(super) const MAX_CHUNK: u64 = 16 << 20;
/// How many decoded chunks are kept.
const CACHED: usize = 4;
static ZEROS: [u8; 64 * 1024] = [0; 64 * 1024];

/// Why the disk is not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Fault {
    /// What is read is missing or corrupt, as this says.
    Corrupt(String),
    /// The budget of what the disk is read for is spent.
    Spent,
}

impl Fault {
    /// The fault, a corruption said to be of `what`.
    pub(super) fn of(self, what: impl std::fmt::Display) -> Fault {
        match self {
            Fault::Corrupt(reason) => Fault::Corrupt(format!("{what}: {reason}")),
            Fault::Spent => Fault::Spent,
        }
    }
}

/// A chunk decoded, by its index.
struct Decoded {
    chunk: usize,
    bytes: Vec<u8>,
}

/// The disk of an image.
pub(super) struct Disk<'a> {
    input: &'a [u8],
    pages: Pages<'a>,
    paging: Paging<'a>,
    chunks: Vec<Chunk>,
    /// The chunks decoded, the most recently read last.
    cache: Vec<Decoded>,
    inflater: Decompress,
    budget: u64,
}

impl<'a> Disk<'a> {
    /// The disk of the image `input`, whose pages are `pages`, of `chunks` in the order of
    /// their sectors.
    pub(super) fn new(input: &'a [u8], pages: Pages<'a>, chunks: Vec<Chunk>) -> Self {
        Disk {
            input,
            pages,
            paging: Paging::new(pages),
            chunks,
            cache: Vec::with_capacity(CACHED),
            inflater: Decompress::new(true),
            budget: decode_limit(input.len()),
        }
    }

    /// Counts `bytes` read against the budget.
    fn spend(&mut self, bytes: u64) -> Result<(), Fault> {
        self.budget = self.budget.checked_sub(bytes).ok_or(Fault::Spent)?;
        Ok(())
    }

    /// The index of the chunk that holds `sector`.
    fn chunk_of(&self, sector: u64) -> Result<usize, Fault> {
        let after = self.chunks.partition_point(|chunk| chunk.sector <= sector);
        after
            .checked_sub(1)
            .filter(|&index| self.chunks[index].end() > sector)
            .ok_or_else(|| Fault::Corrupt(format!("no chunk of the image holds sector {sector}")))
    }

    /// The bytes of the disk from `offset` on, as far as the chunk that holds them goes and
    /// at most `wanted` of them: `None` for zeros, with how many.
    fn piece(&mut self, offset: u64, wanted: u64) -> Result<(Option<&[u8]>, usize), Fault> {
        let index = self.chunk_of(offset / SECTOR)?;
        let chunk = &self.chunks[index];
        let (coding, start) = (chunk.coding, chunk.offset);
        let within = offset - chunk.sector * SECTOR;
        let length = wanted.min(chunk.sectors * SECTOR - within);
        if coding == Coding::Zeros {
            return Ok((None, length.min(ZEROS.len() as u64) as usize));
        }
        let (within, length) = (within as usize, length as usize);
        if coding == Coding::Stored {
            let input = self.input;
            self.paging.read(length);
            return Ok((
                Some(&input[start + within..start + within + length]),
                length,
            ));
        }
        let bytes = self.decoded(index)?;
        Ok((Some(&bytes[within..within + length]), length))
    }

    /// The bytes the coded chunk `index` decodes into, decoding it if it is not kept.
    fn decoded(&mut self, index: usize) -> Result<&[u8], Fault> {
        if let Some(kept) = self.cache.iter().position(|decoded| decoded.chunk == index) {
            let decoded = self.cache.remove(kept);
            self.cache.push(decoded);
            return Ok(&self.cache.last().expect("just kept").bytes);
        }
        let chunk = self.chunks[index].clone();
        let length = chunk.sectors * SECTOR;
        let what = || {
            format!(
                "the chunk of sectors {} to {}",
                chunk.sector,
                chunk.end() - 1
            )
        };
        if length > MAX_CHUNK {
            return Err(Fault::Corrupt(format!(
                "{} decodes into {length} bytes, more than the {MAX_CHUNK} dijo decodes at once",
                what()
            )));
        }
        self.spend(length)?;
        let mut bytes = if self.cache.len() == CACHED {
            self.cache.remove(0).bytes
        } else {
            Vec::new()
        };
        let length = length as usize;
        let input = self.input;
        let coded = &input[chunk.offset..chunk.offset + chunk.length];
        self.paging.read(coded.len());
        let decoding = match chunk.coding {
            Coding::Zlib => inflate(&mut self.inflater, coded, &mut bytes, length),
            Coding::Bzip2 => bunzip(coded, &mut bytes, length),
            Coding::Adc => adc::decode(coded, &mut bytes, length),
            Coding::Lzfse => lzfse::decode(coded, &mut bytes, length).and_then(|()| {
                (bytes.len() == length).then_some(()).ok_or_else(|| {
                    format!(
                        "the LZFSE stream decodes into {} bytes, not {length}",
                        bytes.len()
                    )
                })
            }),
            Coding::Undecoded(method) => Err(format!(
                "it is compressed with {method}, which dijo does not decode"
            )),
            Coding::Unknown(method) => Err(format!(
                "it is compressed by the method {method:#010x}, which the format does not define"
            )),
            Coding::Zeros | Coding::Stored => unreachable!("only coded chunks are decoded"),
        };
        decoding.map_err(|reason| Fault::Corrupt(format!("{}: {reason}", what())))?;
        self.cache.push(Decoded {
            chunk: index,
            bytes,
        });
        Ok(&self.cache.last().expect("just decoded").bytes)
    }

    /// Fills `out` with the disk's bytes from `offset` on, counted against the budget.
    pub(super) fn read(&mut self, offset: u64, out: &mut [u8]) -> Result<(), Fault> {
        self.spend(out.len() as u64)?;
        let mut done = 0;
        while done < out.len() {
            let (bytes, length) = self.piece(offset + done as u64, (out.len() - done) as u64)?;
            match bytes {
                Some(bytes) => out[done..done + length].copy_from_slice(bytes),
                None => out[done..done + length].fill(0),
            }
            done += length;
        }
        Ok(())
    }

    /// Hands `each` the `length` bytes of the disk from `offset` on, a piece at a time.
    pub(super) fn visit<E: From<Fault>>(
        &mut self,
        offset: u64,
        length: u64,
        mut each: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut done = 0;
        while done < length {
            let (bytes, length) = self.piece(offset + done, length - done)?;
            match bytes {
                Some(bytes) => each(bytes)?,
                None => each(&ZEROS[..length])?,
            }
            done += length as u64;
        }
        Ok(())
    }

    /// The `length` bytes of the disk from `offset` on, if one stored chunk holds them all,
    /// in place in the image.
    pub(super) fn in_place(&self, offset: u64, length: u64) -> Option<&'a [u8]> {
        let index = self.chunk_of(offset / SECTOR).ok()?;
        let chunk = &self.chunks[index];
        let within = offset - chunk.sector * SECTOR;
        if chunk.coding != Coding::Stored || within + length > chunk.sectors * SECTOR {
            return None;
        }
        let start = chunk.offset + within as usize;
        self.paging.read(length as usize);
        let input = self.input;
        Some(&input[start..start + length as usize])
    }

    /// The pages of the image.
    pub(super) fn pages(&self) -> Pages<'a> {
        self.pages
    }

    /// The bytes of the image.
    pub(super) fn image_length(&self) -> usize {
        self.input.len()
    }

    /// The sectors of the disk, up to the last its chunks hold.
    pub(super) fn sectors(&self) -> u64 {
        self.chunks.last().map_or(0, Chunk::end)
    }
}

/// Inflates the zlib stream `coded` into `out` with `inflater`, which it resets first, failing
/// unless the stream ends having decoded into `length` bytes.
fn inflate(
    inflater: &mut Decompress,
    coded: &[u8],
    out: &mut Vec<u8>,
    length: usize,
) -> Result<(), String> {
    inflater.reset(true);
    out.clear();
    out.reserve_exact(length);
    let status = inflater
        .decompress_vec(coded, out, FlushDecompress::Finish)
        .map_err(|error| format!("the zlib stream does not inflate: {error}"))?;
    ended(status == Status::StreamEnd, out.len(), length, "zlib")
}

/// Decodes the bzip2 stream `coded` into `out`, failing unless the stream ends having decoded
/// into `length` bytes.
fn bunzip(coded: &[u8], out: &mut Vec<u8>, length: usize) -> Result<(), String> {
    out.clear();
    out.reserve_exact(length);
    let status = Bunzip::new(false)
        .decompress_vec(coded, out)
        .map_err(|error| format!("the bzip2 stream does not decode: {error}"))?;
    ended(
        status == BunzipStatus::StreamEnd,
        out.len(),
        length,
        "bzip2",
    )
}

/// Whether a stream of `codec` that `ended`, or not, decoding into `decoded` bytes, decoded
/// into the `length` its chunk holds.
fn ended(ended: bool, decoded: usize, length: usize, codec: &str) -> Result<(), String> {
    match (ended, decoded == length) {
        (true, true) => Ok(()),
        (true, false) => Err(format!(
            "the {codec} stream decodes into {decoded} bytes, not {length}"
        )),
        (false, _) => Err(format!(
            "the {codec} stream does not end within the {length} bytes of its chunk"
        )),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::compress::{bzip2::tests::bzip2, zlib::tests::zlib};

    fn coded(sector: u64, sectors: u64, coding: Coding, offset: usize, length: usize) -> Chunk {
        Chunk {
            sector,
            sectors,
            coding,
            offset,
            length,
        }
    }

    /// A disk of sectors of `a`, then zeros, then `b` coded with zlib, then `c` with bzip2,
    /// then `d` with ADC, then a gap, then a chunk of an undefined method; and its image.
    fn disk_of_each_coding() -> (Vec<u8>, Vec<Chunk>) {
        let block = |byte: u8, sectors: usize| -> Vec<u8> {
            (0..sectors * SECTOR as usize)
                .map(|at| if at % 7 == 0 { byte } else { b'.' })
                .collect()
        };
        let zlibbed = zlib(&block(b'b', 2));
        let bzipped = bzip2(&block(b'c', 3));
        let adced = super::super::adc::tests::adc(&block(b'd', 1));
        let mut image = block(b'a', 2);
        let mut chunks = vec![coded(0, 2, Coding::Stored, 0, image.len())];
        chunks.push(coded(2, 1, Coding::Zeros, 0, 0));
        for (sector, sectors, coding, bytes) in [
            (3, 2, Coding::Zlib, zlibbed),
            (5, 3, Coding::Bzip2, bzipped),
            (8, 1, Coding::Adc, adced),
        ] {
            chunks.push(coded(sector, sectors, coding, image.len(), bytes.len()));
            image.extend_from_slice(&bytes);
        }
        chunks.push(coded(10, 1, Coding::Unknown(9), 0, 1));
        (image, chunks)
    }

    #[test]
    fn reads_across_chunks_of_each_coding() {
        let (image, chunks) = disk_of_each_coding();
        let mut disk = Disk::new(&image, Pages::IN_MEMORY, chunks);
        let mut out = vec![0xFF; 9 * 512];
        disk.read(0, &mut out).unwrap();
        assert_eq!(out[0], b'a');
        assert_eq!(out[1023], if 1023 % 7 == 0 { b'a' } else { b'.' });
        assert!(out[1024..1536].iter().all(|&byte| byte == 0));
        assert_eq!(out[1536], b'b');
        assert_eq!(out[1536 + 1024], b'c');
        assert_eq!(out[1536 + 1024 + 1536], b'd');
        let mut visited = Vec::new();
        disk.visit::<Fault>(1000, 3600, |piece| {
            visited.extend_from_slice(piece);
            Ok(())
        })
        .unwrap();
        assert_eq!(visited, out[1000..4600]);
        assert_eq!(disk.in_place(100, 900), Some(&image[100..1000]));
        assert_eq!(disk.in_place(1000, 100), None);
        assert_eq!(disk.in_place(1536, 10), None);
        assert_eq!(
            disk.read(9 * 512, &mut [0; 1]),
            Err(Fault::Corrupt(
                "no chunk of the image holds sector 9".into()
            ))
        );
        let undefined = disk.read(10 * 512, &mut [0; 1]).unwrap_err();
        assert!(
            matches!(&undefined, Fault::Corrupt(reason) if reason.contains("method 0x00000009")),
            "{undefined:?}"
        );
    }

    #[test]
    fn keeps_the_last_chunks_decoded() {
        let (image, chunks) = disk_of_each_coding();
        let mut disk = Disk::new(&image, Pages::IN_MEMORY, chunks);
        for _ in 0..3 {
            for sector in [3, 5, 8] {
                disk.read(sector * 512, &mut [0; 512]).unwrap();
            }
        }
        let kept: Vec<usize> = disk.cache.iter().map(|decoded| decoded.chunk).collect();
        assert_eq!(kept, [2, 3, 4]);
        let budget = disk.budget;
        disk.read(5 * 512, &mut [0; 512]).unwrap();
        assert_eq!(disk.budget, budget - 512);
    }

    #[test]
    fn fails_chunks_that_decode_wrong_or_too_large() {
        let (image, mut chunks) = disk_of_each_coding();
        chunks[2].sectors = 3;
        chunks[3].sector = 6;
        chunks[3].sectors = 2;
        chunks.push(coded(20, MAX_CHUNK / SECTOR + 1, Coding::Zlib, 0, 1));
        let mut disk = Disk::new(&image, Pages::IN_MEMORY, chunks);
        let reason = |disk: &mut Disk, sector: u64| match disk.read(sector * 512, &mut [0; 1]) {
            Err(Fault::Corrupt(reason)) => reason,
            other => panic!("read: {other:?}"),
        };
        assert_eq!(
            reason(&mut disk, 3),
            "the chunk of sectors 3 to 5: the zlib stream decodes into 1024 bytes, not 1536"
        );
        assert_eq!(
            reason(&mut disk, 6),
            "the chunk of sectors 6 to 7: the bzip2 stream does not end within the 1024 bytes of \
             its chunk"
        );
        assert!(reason(&mut disk, 20).contains("more than the 16777216 dijo decodes at once"));
    }

    #[test]
    fn stops_once_the_budget_is_spent() {
        let (image, chunks) = disk_of_each_coding();
        let mut disk = Disk::new(&image, Pages::IN_MEMORY, chunks);
        disk.budget = 1000;
        assert_eq!(disk.read(3 * 512, &mut [0; 1]), Err(Fault::Spent));
        disk.budget = 10;
        assert_eq!(disk.read(0, &mut [0; 11]), Err(Fault::Spent));
        assert!(disk.read(0, &mut [0; 10]).is_ok());
    }
}
