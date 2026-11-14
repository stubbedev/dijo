//! Apple Data Compression, the LZ77 coding of the UDCO chunks of a disk image: a byte with its
//! top bit set starts a literal of its low seven bits plus one bytes; else a match whose byte
//! has bit 6 set takes two bytes more, a 16-bit offset, for a length of its low six bits plus
//! four; else one more, for a 10-bit offset and a length of its bits 2 to 5 plus three. A match
//! copies from its offset plus one bytes back. The coding has no end of its own: a chunk ends
//! once it has decoded into its sectors, and what follows is not read.

use super::lzvn::copy_match;

/// Decodes the ADC chunk `chunk` into `out`, which it empties first, up to the `length` bytes
/// the chunk's sectors hold, failing where it ends before them or a match reaches back past
/// its start.
pub(super) fn decode(chunk: &[u8], out: &mut Vec<u8>, length: usize) -> Result<(), String> {
    out.clear();
    let mut at = 0;
    while out.len() < length {
        let &code = chunk.get(at).ok_or_else(|| {
            format!(
                "the ADC chunk ends after decoding into {} of its {length} bytes",
                out.len()
            )
        })?;
        let truncated = || format!("the ADC code at byte {at} runs past the chunk's end");
        let byte = |offset: usize| chunk.get(at + offset).copied().ok_or_else(truncated);
        let (taken, copied) = if code & 0x80 != 0 {
            let count = usize::from(code & 0x7F) + 1;
            let literal = chunk.get(at + 1..at + 1 + count).ok_or_else(truncated)?;
            if out.len() + count > length {
                return Err(past(length));
            }
            out.extend_from_slice(literal);
            (1 + count, None)
        } else if code & 0x40 != 0 {
            let offset = usize::from(u16::from_be_bytes([byte(1)?, byte(2)?]));
            (3, Some((offset + 1, usize::from(code & 0x3F) + 4)))
        } else {
            let offset = usize::from(code & 0x03) << 8 | usize::from(byte(1)?);
            (2, Some((offset + 1, usize::from(code >> 2 & 0x0F) + 3)))
        };
        if let Some((distance, count)) = copied {
            if out.len() + count > length {
                return Err(past(length));
            }
            copy_match(out, distance, count)
                .map_err(|reason| format!("the ADC code at byte {at}: {reason}"))?;
        }
        at += taken;
    }
    Ok(())
}

fn past(length: usize) -> String {
    format!("the ADC chunk decodes into more than its {length} bytes")
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// `data` coded with ADC: literals of up to 128 bytes, and matches of four bytes or more at
    /// the last place within 64 KiB its next three bytes stood before.
    pub(crate) fn adc(data: &[u8]) -> Vec<u8> {
        let mut coded = Vec::new();
        let mut literal: Vec<u8> = Vec::new();
        let flush = |coded: &mut Vec<u8>, literal: &mut Vec<u8>| {
            for run in literal.chunks(128) {
                coded.push(0x80 | (run.len() - 1) as u8);
                coded.extend_from_slice(run);
            }
            literal.clear();
        };
        let mut last = std::collections::HashMap::new();
        let mut at = 0;
        while at < data.len() {
            let key = data.get(at..at + 3);
            let best = key
                .and_then(|key| last.insert(key, at))
                .filter(|&start| at - start <= 65_536)
                .map(|start| {
                    let length = data[start..]
                        .iter()
                        .zip(&data[at..])
                        .take(67)
                        .take_while(|(a, b)| a == b)
                        .count();
                    (length, at - start)
                })
                .filter(|&(length, _)| length >= 4);
            match best {
                Some((length, distance)) => {
                    flush(&mut coded, &mut literal);
                    let offset = distance - 1;
                    if length <= 18 && offset < 1024 {
                        coded.push(((length - 3) << 2) as u8 | (offset >> 8) as u8);
                        coded.push(offset as u8);
                    } else {
                        coded.push(0x40 | (length - 4) as u8);
                        coded.extend_from_slice(&(offset as u16).to_be_bytes());
                    }
                    at += length;
                }
                None => {
                    literal.push(data[at]);
                    at += 1;
                }
            }
        }
        flush(&mut coded, &mut literal);
        coded
    }

    #[test]
    fn decodes_literals_and_matches_of_both_lengths() {
        let data: Vec<u8> = (0..3000)
            .flat_map(|line| format!("sounding {} fathoms\n", line % 37).into_bytes())
            .chain(std::iter::repeat_n(b'z', 300))
            .collect();
        let coded = adc(&data);
        assert!(coded.len() < data.len() / 4);
        let mut out = Vec::new();
        decode(&coded, &mut out, data.len()).unwrap();
        assert_eq!(out, data);
        let mut trailing = coded.clone();
        trailing.extend_from_slice(b"ignored");
        decode(&trailing, &mut out, data.len()).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn decodes_the_three_code_forms() {
        let coded = [0x82, b'a', b'b', b'c', 0b0000_0100, 0x02, 0x40, 0x00, 0x00];
        let mut out = Vec::new();
        decode(&coded, &mut out, 11).unwrap();
        assert_eq!(out, b"abcabcaaaaa");
    }

    #[test]
    fn fails_chunks_cut_short_reaching_back_too_far_or_too_long() {
        let mut out = Vec::new();
        assert!(decode(&[0x82, b'a'], &mut out, 3)
            .unwrap_err()
            .contains("runs past"));
        assert!(decode(&[0x80, b'a'], &mut out, 2)
            .unwrap_err()
            .contains("decoding into 1 of its 2"));
        assert!(decode(&[0x80, b'a', 0x00, 0x01], &mut out, 4)
            .unwrap_err()
            .contains("reaches 2 bytes back"));
        assert!(decode(&[0x80, b'a', 0x40, 0x00, 0x00], &mut out, 3)
            .unwrap_err()
            .contains("more than its 3 bytes"));
        assert!(decode(&[0x40, 0x00], &mut out, 4)
            .unwrap_err()
            .contains("runs past"));
    }
}
