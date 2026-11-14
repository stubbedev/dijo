//! LZVN, the LZ77 coding of the small blocks of an LZFSE stream (`bvxn`), as Apple's reference
//! `lzvn_decode_base.c` (lzfse 1.0) decodes it: opcodes of a literal of up to three bytes and a
//! match, of a literal alone or of a match alone at the distance before, up to the
//! end-of-stream opcode. Matches reach back into all the stream has decoded into, the blocks
//! before included. Where the reference decoder trusts the distance it carries over to a match
//! alone, this one checks it as it checks any other.

/// What an opcode asks for: the bytes it takes before its literal, its literal's and match's
/// lengths, and its match's distance, if it gives one rather than carrying the one before over.
struct Op {
    length: usize,
    literal: usize,
    matched: usize,
    distance: Option<usize>,
}

/// The opcode at `at` of `block`, or why there is none to decode there; `None` for the end of
/// the stream.
fn op(block: &[u8], at: usize) -> Result<Option<Op>, String> {
    let byte = |offset: usize| {
        block
            .get(at + offset)
            .copied()
            .map(usize::from)
            .ok_or_else(|| format!("the LZVN opcode at byte {at} runs past the block's end"))
    };
    let opcode = byte(0)?;
    let literal = opcode >> 6;
    let matched = (opcode >> 3 & 7) + 3;
    let op = match opcode {
        0x06 => {
            if block.len() < at + 8 {
                return Err(format!(
                    "the LZVN end-of-stream opcode at byte {at} runs past the block's end"
                ));
            }
            return Ok(None);
        }
        0x0E | 0x16 => Op {
            length: 1,
            literal: 0,
            matched: 0,
            distance: None,
        },
        0x70..=0x7F | 0xD0..=0xDF => return Err(undefined(opcode, at)),
        _ if opcode < 0x40 && opcode & 7 == 6 => return Err(undefined(opcode, at)),
        0xA0..=0xBF => {
            let packed = byte(1)? | byte(2)? << 8;
            Op {
                length: 3,
                literal: opcode >> 3 & 3,
                matched: ((opcode & 7) << 2 | packed & 3) + 3,
                distance: Some(packed >> 2),
            }
        }
        0xE0 => Op {
            length: 2,
            literal: byte(1)? + 16,
            matched: 0,
            distance: None,
        },
        0xE1..=0xEF => Op {
            length: 1,
            literal: opcode & 0x0F,
            matched: 0,
            distance: None,
        },
        0xF0 => Op {
            length: 2,
            literal: 0,
            matched: byte(1)? + 16,
            distance: None,
        },
        0xF1..=0xFF => Op {
            length: 1,
            literal: 0,
            matched: opcode & 0x0F,
            distance: None,
        },
        _ if opcode & 7 == 6 => Op {
            length: 1,
            literal,
            matched,
            distance: None,
        },
        _ if opcode & 7 == 7 => Op {
            length: 3,
            literal,
            matched,
            distance: Some(byte(1)? | byte(2)? << 8),
        },
        _ => Op {
            length: 2,
            literal,
            matched,
            distance: Some((opcode & 7) << 8 | byte(1)?),
        },
    };
    Ok(Some(op))
}

fn undefined(opcode: usize, at: usize) -> String {
    format!("the LZVN opcode {opcode:#04x} at byte {at} is undefined")
}

/// Appends the `raw` bytes the LZVN `block` decodes into to `out`, failing where the block ends
/// before its end-of-stream opcode, decodes into more or fewer bytes, or takes fewer bytes than
/// it has.
pub(super) fn decode(block: &[u8], raw: usize, out: &mut Vec<u8>) -> Result<(), String> {
    let end = out.len() + raw;
    let mut at = 0;
    let mut distance = 0;
    while let Some(op) = op(block, at)? {
        let literal = block
            .get(at + op.length..at + op.length + op.literal)
            .ok_or_else(|| {
                format!("the literal of the LZVN opcode at byte {at} runs past the block's end")
            })?;
        if out.len() + op.literal + op.matched > end {
            return Err(format!(
                "the LZVN block decodes into more than the {raw} bytes its header gives"
            ));
        }
        out.extend_from_slice(literal);
        if op.matched > 0 {
            distance = op.distance.unwrap_or(distance);
            copy_match(out, distance, op.matched)
                .map_err(|reason| format!("the LZVN opcode at byte {at}: {reason}"))?;
        }
        at += op.length + op.literal;
    }
    if at + 8 != block.len() || out.len() != end {
        return Err(format!(
            "the LZVN block ends after {} of its {} bytes, having decoded into {} of the {raw} \
             bytes its header gives",
            at + 8,
            block.len(),
            raw - (end - out.len())
        ));
    }
    Ok(())
}

/// Appends the `length` bytes `distance` back from the end of `out`, the copy overlapping what
/// it appends where the distance is shorter than the length, as an LZ77 match does.
pub(super) fn copy_match(out: &mut Vec<u8>, distance: usize, length: usize) -> Result<(), String> {
    if distance == 0 || distance > out.len() {
        return Err(format!(
            "its match reaches {distance} bytes back, where {} bytes are decoded",
            out.len()
        ));
    }
    let start = out.len() - distance;
    if distance >= length {
        out.extend_from_within(start..start + length);
        return Ok(());
    }
    out.reserve(length);
    for index in start..start + length {
        out.push(out[index]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const END: [u8; 8] = [0x06, 0, 0, 0, 0, 0, 0, 0];

    fn decoded(block: &[u8], raw: usize) -> Result<Vec<u8>, String> {
        let mut out = b"ab".to_vec();
        decode(block, raw, &mut out).map(|()| out[2..].to_vec())
    }

    #[test]
    fn decodes_each_kind_of_opcode() {
        let block = [
            &[0xE3, b'x', b'y', b'z'][..],
            &[0x0E],
            &[0x40, 0x01, b'n'],
            &[0x8E, b'p', b'q'],
            &[0xF2],
            &[0x07, 0x0A, 0x00],
            &[0xA8, 0x0D, 0x00, b'm'],
            &[0xE0, 0x00],
            &[b'L'; 16],
            &[0xF0, 0x01],
            &END,
        ]
        .concat();
        let expected = [
            &b"xyz"[..],
            b"nnnn",
            b"pqqqqq",
            b"qq",
            b"nnp",
            b"mnpmn",
            &[b'L'; 33],
        ]
        .concat();
        assert_eq!(decoded(&block, expected.len()).unwrap(), expected);
    }

    #[test]
    fn copies_overlapping_matches_byte_by_byte() {
        let mut out = b"ab".to_vec();
        copy_match(&mut out, 2, 5).unwrap();
        assert_eq!(out, b"abababa");
        copy_match(&mut out, 7, 3).unwrap();
        assert_eq!(out, b"abababaaba");
        assert!(copy_match(&mut out, 11, 1)
            .unwrap_err()
            .contains("11 bytes back"));
        assert!(copy_match(&mut out, 0, 1).is_err());
    }

    #[test]
    fn refuses_undefined_opcodes_truncated_blocks_and_wrong_lengths() {
        for opcode in [0x1E, 0x3E, 0x70, 0x7F, 0xD0, 0xDF] {
            let reason = decoded(&[&[opcode][..], &END].concat(), 0).unwrap_err();
            assert!(reason.contains("is undefined"), "{opcode:#x}: {reason}");
        }
        assert!(decoded(&[0xE3, b'x'], 3).unwrap_err().contains("runs past"));
        assert!(decoded(&[0x06, 0, 0], 0)
            .unwrap_err()
            .contains("end-of-stream"));
        assert!(decoded(&[&[0xE1, b'x'][..], &END].concat(), 0)
            .unwrap_err()
            .contains("more than the 0 bytes"));
        assert!(decoded(&[&[0xE1, b'x'][..], &END].concat(), 2)
            .unwrap_err()
            .contains("decoded into 1 of the 2"));
        assert!(decoded(&[&[0xE1, b'x'][..], &END, &[0]].concat(), 1)
            .unwrap_err()
            .contains("ends after 10 of its 11"));
        assert!(decoded(&[&[0xF1][..], &END].concat(), 1)
            .unwrap_err()
            .contains("reaches 0 bytes back"));
    }
}
