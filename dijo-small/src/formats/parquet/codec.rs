//! The codecs Parquet compresses pages with (`CompressionCodec`), each page decoded into exactly
//! the bytes its header gives: Snappy blocks, gzip members, Brotli streams, Zstandard frames and
//! LZ4 blocks, raw or in Hadoop's framing. LZO is not decoded.

use std::io::Read;

use super::Problem;

/// A codec a column chunk's pages are compressed with, which is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Uncompressed,
    Snappy,
    Gzip,
    Brotli,
    /// `LZ4`, deprecated: LZ4 blocks in Hadoop's framing, or, as the first writers wrote it, one
    /// raw block.
    Lz4,
    Zstd,
    Lz4Raw,
}

/// The `CompressionCodec` of a column chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Decoded(Codec),
    Lzo,
}

impl Compression {
    pub fn of(code: i32) -> Result<Self, Problem> {
        Ok(Self::Decoded(match code {
            0 => Codec::Uncompressed,
            1 => Codec::Snappy,
            2 => Codec::Gzip,
            3 => return Ok(Self::Lzo),
            4 => Codec::Brotli,
            5 => Codec::Lz4,
            6 => Codec::Zstd,
            7 => Codec::Lz4Raw,
            _ => {
                return Err(Problem::Malformed(format!(
                    "the compression codec {code}, which Parquet does not define"
                )))
            }
        }))
    }
}

/// Decodes `input`, compressed with `codec`, into `output`, which it must fill exactly.
pub fn decompress(codec: Codec, input: &[u8], output: &mut [u8]) -> Result<(), Problem> {
    let length = output.len();
    let failed = |reason: String| {
        Problem::Malformed(format!(
            "a page of {} bytes compressed with {codec:?} does not decode into its {length} \
             bytes: {reason}",
            input.len()
        ))
    };
    match codec {
        Codec::Uncompressed => {
            if input.len() != output.len() {
                return Err(failed("its sizes differ".to_owned()));
            }
            output.copy_from_slice(input);
        }
        Codec::Snappy => {
            let length = snap::raw::decompress_len(input).map_err(|e| failed(e.to_string()))?;
            if length != output.len() {
                return Err(failed(format!("it decodes into {length} bytes")));
            }
            snap::raw::Decoder::new()
                .decompress(input, output)
                .map_err(|e| failed(e.to_string()))?;
        }
        Codec::Gzip => {
            read_exactly(flate2::bufread::GzDecoder::new(input), output).map_err(failed)?
        }
        Codec::Brotli => read_exactly(
            brotli_decompressor::Decompressor::new(input, BROTLI_BUFFER),
            output,
        )
        .map_err(failed)?,
        Codec::Zstd => {
            let decoder = ruzstd::decoding::StreamingDecoder::new(input)
                .map_err(|e| failed(e.to_string()))?;
            read_exactly(decoder, output).map_err(failed)?;
        }
        Codec::Lz4Raw => lz4_block(input, output).map_err(failed)?,
        Codec::Lz4 => {
            if !lz4_hadoop(input, output).map_err(failed)? {
                lz4_block(input, output).map_err(failed)?;
            }
        }
    }
    Ok(())
}

/// The buffer Brotli's decoder reads its input through.
const BROTLI_BUFFER: usize = 4096;

/// Reads `output` full from `decoder`, which must end there.
fn read_exactly(mut decoder: impl Read, output: &mut [u8]) -> Result<(), String> {
    decoder.read_exact(output).map_err(|e| e.to_string())?;
    let mut beyond = [0u8; 1];
    match decoder.read(&mut beyond) {
        Ok(0) => Ok(()),
        Ok(_) => Err("it decodes into more".to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

/// Decodes the raw LZ4 block `input` into all of `output`.
fn lz4_block(input: &[u8], output: &mut [u8]) -> Result<(), String> {
    let length = lz4_flex::block::decompress_into(input, output).map_err(|e| e.to_string())?;
    if length != output.len() {
        return Err(format!("it decodes into {length} bytes"));
    }
    Ok(())
}

/// Decodes `input` as LZ4 blocks in Hadoop's framing into all of `output`, if it is framed so:
/// whether it is.
fn lz4_hadoop(input: &[u8], output: &mut [u8]) -> Result<bool, String> {
    if !is_hadoop_framed(input, output.len()) {
        return Ok(false);
    }
    let (mut at, mut decoded) = (0, 0);
    while at < input.len() {
        let (decodes, takes) = frame_lengths(&input[at..at + 8]);
        lz4_block(
            &input[at + 8..at + 8 + takes],
            &mut output[decoded..decoded + decodes],
        )?;
        (at, decoded) = (at + 8 + takes, decoded + decodes);
    }
    Ok(true)
}

/// What a frame of Hadoop's framing decodes into and takes, by its header: two big-endian
/// lengths.
fn frame_lengths(header: &[u8]) -> (usize, usize) {
    let length = |bytes: &[u8]| u32::from_be_bytes(bytes.try_into().expect("four bytes")) as usize;
    (length(&header[..4]), length(&header[4..8]))
}

/// Whether `input` is frames of Hadoop's framing whose lengths add up to all of it and to
/// `output` bytes.
fn is_hadoop_framed(input: &[u8], output: usize) -> bool {
    let (mut at, mut decoded) = (0usize, 0usize);
    while at < input.len() {
        let Some(header) = input.get(at..at + 8) else {
            return false;
        };
        let (decodes, takes) = frame_lengths(header);
        let (Some(end), Some(until)) = ((at + 8).checked_add(takes), decoded.checked_add(decodes))
        else {
            return false;
        };
        if end > input.len() || until > output {
            return false;
        }
        (at, decoded) = (end, until);
    }
    decoded == output
}
