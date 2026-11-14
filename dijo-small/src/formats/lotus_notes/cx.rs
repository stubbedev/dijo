//! The CX compression Notes keeps its superblocks and bucket descriptor blocks in, as libnsfdb
//! decompresses it: a stream of bits, read from the lowest bit of each byte up, after three bits
//! of a header that libnsfdb reads only when they are above 3.
//!
//! A 0 bit is followed by a literal byte. A 1 bit starts a back reference into what is already
//! decompressed: then a 1 bit makes it two bytes long and its distance one byte; a 0 bit is
//! followed by a count `n`, as zero bits ended by a one, and `n` bits `v` of its length
//! `2^n - 1 + v + 2`, and, after a 1 bit, a byte of its distance, or, after a 0 bit, a count `m`
//! and `m` bits `w` of the distance's upper bits `2^m - 1 + w` and a byte of its lower ones. A
//! length of 0x102 or more ends the stream.

use super::Problem;

/// The length of a back reference that ends the stream.
const END: usize = 0x102;
/// The count of a length or distance whose eight zero bits leave no room for the one that ends
/// it, whose ninth bit is part of it whatever it is.
const MAX_COUNT: u32 = 9;

/// The bits of a stream, from the lowest bit of each byte up.
struct Bits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u8, Problem> {
        let byte = self.bytes.get(self.at / 8).ok_or_else(|| {
            Problem::Malformed("CX-compressed data ends before its end".to_owned())
        })?;
        let bit = (byte >> (self.at % 8)) & 1;
        self.at += 1;
        Ok(bit)
    }

    fn byte(&mut self) -> Result<u8, Problem> {
        let mut value = 0;
        for shift in 0..8 {
            value |= self.bit()? << shift;
        }
        Ok(value)
    }

    /// A count of zero bits ended by a one bit, both counted.
    fn count(&mut self) -> Result<u32, Problem> {
        let mut count = 1;
        while count < MAX_COUNT && self.bit()? == 0 {
            count += 1;
        }
        if count == MAX_COUNT {
            self.bit()?;
        }
        Ok(count)
    }

    /// `2^count - 1` and the value of the next `count` bits, of a count read first.
    fn counted(&mut self) -> Result<usize, Problem> {
        let count = self.count()?;
        let mut value = 0;
        for shift in 0..count {
            value |= usize::from(self.bit()?) << shift;
        }
        Ok((1 << count) - 1 + value)
    }
}

/// The first `wanted` bytes `stream` decompresses to.
pub(super) fn decompress(stream: &[u8], wanted: usize) -> Result<Vec<u8>, Problem> {
    let header = stream
        .first()
        .ok_or_else(|| Problem::Malformed("CX-compressed data is empty".to_owned()))?
        & 0x07;
    if header <= 3 {
        return Err(Problem::Unread(format!(
            "CX-compressed data whose header bits are {header}, which libnsfdb does not read \
             either"
        )));
    }
    let mut bits = Bits {
        bytes: stream,
        at: 3,
    };
    let mut output = Vec::with_capacity(wanted);
    while output.len() < wanted {
        if bits.bit()? == 0 {
            output.push(bits.byte()?);
            continue;
        }
        let length = match bits.bit()? {
            0 => bits.counted()? + 2,
            _ => 2,
        };
        if length >= END {
            return Err(Problem::Malformed(format!(
                "CX-compressed data ends after {} of the {wanted} bytes it is to hold",
                output.len()
            )));
        }
        let mut distance = 0;
        if length > 2 && bits.bit()? == 0 {
            distance = bits.counted()? << 8;
        }
        distance |= usize::from(bits.byte()?);
        let start = output
            .len()
            .checked_sub(distance)
            .filter(|_| distance > 0)
            .ok_or_else(|| {
                Problem::Malformed(format!(
                    "CX-compressed data refers {distance} bytes back after {} bytes",
                    output.len()
                ))
            })?;
        let end = (output.len() + length).min(wanted);
        for at in start..start + (end - output.len()) {
            let byte = output[at];
            output.push(byte);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes of `bits`, given from the first.
    fn stream(bits: &str) -> Vec<u8> {
        let bits: Vec<u8> = bits
            .bytes()
            .filter(|b| *b != b' ')
            .map(|b| b - b'0')
            .collect();
        bits.chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0u8, |byte, (shift, &bit)| byte | bit << shift)
            })
            .collect()
    }

    /// The bits of `value`, `count` of them from the lowest up.
    fn bits(value: usize, count: usize) -> String {
        (0..count)
            .map(|shift| if value >> shift & 1 == 1 { '1' } else { '0' })
            .collect()
    }

    fn literal(byte: u8) -> String {
        format!("0{}", bits(usize::from(byte), 8))
    }

    const END_MARK: &str = "10 00000001 10000000";

    #[test]
    fn reads_literals_and_back_references_of_every_form() {
        let data = [
            "001".to_owned(),
            literal(b'a'),
            literal(b'b'),
            format!("11{}", bits(2, 8)),
            format!("10 01 00 1{}", bits(2, 8)),
            END_MARK.to_owned(),
        ]
        .concat();
        assert_eq!(decompress(&stream(&data), 9).unwrap(), b"ababababa");
    }

    #[test]
    fn reads_distances_beyond_a_byte() {
        let mut data = "001".to_owned();
        for at in 0..300u32 {
            data += &literal(u8::try_from(at % 251).unwrap());
        }
        data += &format!("10 1 0 0 1 0 {}", bits(300 - 256, 8));
        data += END_MARK;
        let output = decompress(&stream(&data), 303).unwrap();
        assert_eq!(output[300..], [0, 1, 2]);
    }

    #[test]
    fn stops_at_the_bytes_wanted() {
        let data = [
            "001".to_owned(),
            literal(b'x'),
            format!("10 001 111 1{}", bits(1, 8)),
        ]
        .concat();
        assert_eq!(decompress(&stream(&data), 5).unwrap(), b"xxxxx");
    }

    #[test]
    fn declines_header_bits_libnsfdb_does_not_read() {
        let data = ["010".to_owned(), literal(b'x')].concat();
        assert!(matches!(
            decompress(&stream(&data), 1),
            Err(Problem::Unread(_))
        ));
    }

    #[test]
    fn fails_references_before_the_start_and_streams_ending_early() {
        let back = ["001".to_owned(), literal(b'x'), format!("11{}", bits(2, 8))].concat();
        assert!(matches!(
            decompress(&stream(&back), 3),
            Err(Problem::Malformed(_))
        ));
        let ended = ["001".to_owned(), literal(b'x'), END_MARK.to_owned()].concat();
        assert!(matches!(
            decompress(&stream(&ended), 2),
            Err(Problem::Malformed(_))
        ));
        let cut = ["001".to_owned(), literal(b'x')].concat();
        assert!(matches!(
            decompress(&stream(&cut), 2),
            Err(Problem::Malformed(_))
        ));
    }
}
