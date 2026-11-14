//! The LZ77 compression of help files: a flag byte whose bits, from the least significant on,
//! say whether each of the next eight items is a literal byte or a 16-bit back reference, whose
//! low 12 bits are the distance back less one and high four bits the length less three, into a
//! window of the 4096 bytes written last, zeros before the first.

const WINDOW: usize = 0x1000;
const MIN_MATCH: usize = 3;

/// Decompresses `input` into `out`, which it clears first, up to `limit` bytes: what is left of
/// the input once the limit is reached is not read, as WinHelp does not, and a back reference
/// whose second byte the input lacks ends it.
pub(super) fn decompress(input: &[u8], limit: usize, out: &mut Vec<u8>) {
    out.clear();
    let mut bytes = input.iter().copied();
    while out.len() < limit {
        let Some(flags) = bytes.next() else {
            return;
        };
        for bit in 0..8 {
            if out.len() >= limit {
                return;
            }
            if flags >> bit & 1 == 0 {
                let Some(literal) = bytes.next() else {
                    return;
                };
                out.push(literal);
                continue;
            }
            let (Some(low), Some(high)) = (bytes.next(), bytes.next()) else {
                return;
            };
            let reference = u16::from_le_bytes([low, high]);
            let distance = (usize::from(reference) & (WINDOW - 1)) + 1;
            let length = usize::from(reference >> 12) + MIN_MATCH;
            for _ in 0..length.min(limit - out.len()) {
                let byte = out.len().checked_sub(distance).map_or(0, |from| out[from]);
                out.push(byte);
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// `data` compressed greedily, each reference the longest match in the window, as the help
    /// compilers compress.
    pub(crate) fn compress(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < data.len() {
            let flags_at = out.len();
            out.push(0);
            for bit in 0..8 {
                if at >= data.len() {
                    break;
                }
                let (mut best, mut best_from) = (0, 0);
                for from in at.saturating_sub(WINDOW)..at {
                    let length = (0..(MIN_MATCH + 15).min(data.len() - at))
                        .take_while(|&i| data[from + i] == data[at + i])
                        .count();
                    if length > best {
                        (best, best_from) = (length, from);
                    }
                }
                if best >= MIN_MATCH {
                    out[flags_at] |= 1 << bit;
                    let reference = ((best - MIN_MATCH) << 12 | (at - best_from - 1)) as u16;
                    out.extend(reference.to_le_bytes());
                    at += best;
                } else {
                    out.push(data[at]);
                    at += 1;
                }
            }
        }
        out
    }

    fn decompressed(input: &[u8], limit: usize) -> Vec<u8> {
        let mut out = Vec::new();
        decompress(input, limit, &mut out);
        out
    }

    #[test]
    fn copies_literals_and_back_references_overlapping_what_they_write() {
        assert_eq!(
            decompressed(&[0b0000_0010, b'a', 0x00, 0x30], 100),
            b"aaaaaaa"
        );
        assert_eq!(
            decompressed(&[0b0000_0100, b'a', b'b', 0x01, 0x10], 100),
            b"ababab"
        );
    }

    #[test]
    fn reads_zeros_before_the_start_and_stops_at_the_limit() {
        assert_eq!(decompressed(&[0b0000_0001, 0x09, 0x00], 100), [0; 3]);
        assert_eq!(decompressed(&[0b0000_0010, b'a', 0x00, 0xF0], 5), b"aaaaa");
        assert_eq!(decompressed(&[0x00, b'a', b'b', b'c'], 2), b"ab");
    }

    #[test]
    fn ends_at_a_reference_cut_short() {
        assert_eq!(decompressed(&[0b0000_0010, b'a', 0x01], 100), b"a");
        assert_eq!(decompressed(&[], 100), b"");
    }

    #[test]
    fn decompresses_what_the_compressor_writes() {
        let text: Vec<u8> = b"The keeper lit the lamp; the keeper wound the clock. "
            .iter()
            .copied()
            .cycle()
            .take(9000)
            .chain(0..=255)
            .collect();
        let compressed = compress(&text);
        assert!(compressed.len() < text.len() / 4);
        assert_eq!(decompressed(&compressed, usize::MAX), text);
    }
}
