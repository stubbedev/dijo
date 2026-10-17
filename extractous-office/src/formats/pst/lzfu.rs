//! java-libpst's `LZFu`: compressed RTF, decompressed.

use super::file::{le_checked, Read, Thrown};
use crate::java;

const COMPRESSED: i64 = 0x7546_5A4C;
const UNCOMPRESSED: i64 = 0x414C_454D;
const HEADER: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\n\r\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";
const WINDOW: usize = 4096;

/// `LZFu.decode`: the RTF, decoded as UTF-8 and trimmed.
pub(super) fn decode(data: &[u8]) -> Read<String> {
    let what = "compressed RTF";
    le_checked(data, 0, 4, what)?;
    let size = le_checked(data, 4, 8, what)? as i32;
    let signature = le_checked(data, 8, 12, what)?;
    le_checked(data, 12, 16, what)?;
    if signature == UNCOMPRESSED {
        return Ok(java::trim(&java::utf8(&data[16..])).to_owned());
    }
    if signature != COMPRESSED {
        return Ok(String::new());
    }
    let size = usize::try_from(size)
        .map_err(|_| Thrown::runtime("java.lang.NegativeArraySizeException", size.to_string()))?;
    let byte = |at: usize| {
        data.get(at)
            .copied()
            .ok_or_else(|| Thrown::out_of_bounds(what))
    };
    let mut output = Vec::new();
    let mut window = [0u8; WINDOW];
    window[..HEADER.len()].copy_from_slice(HEADER);
    let mut cursor = HEADER.len();
    let mut at = 16;
    while (at as i64) < data.len() as i64 - 2 && output.len() < size {
        let mut flags = byte(at)?;
        at += 1;
        for _ in 0..8 {
            if output.len() >= size {
                break;
            }
            let reference = flags & 1 == 1;
            flags >>= 1;
            if reference {
                let high = usize::from(byte(at)?);
                let low = usize::from(byte(at + 1)?);
                at += 2;
                let mut index = (high << 4) | (low >> 4);
                for _ in 0..(low & 0xF) + 2 {
                    if output.len() >= size {
                        break;
                    }
                    let value = window[index];
                    output.push(value);
                    window[cursor] = value;
                    cursor = (cursor + 1) % WINDOW;
                    index = (index + 1) % WINDOW;
                }
            } else {
                let value = byte(at)?;
                window[cursor] = value;
                cursor = (cursor + 1) % WINDOW;
                output.push(value);
                at += 1;
            }
        }
    }
    if output.len() != size {
        return Err(Thrown::pst(format!(
            "Error decompressing RTF! Expected {size} bytes, got {} bytes\n",
            output.len()
        )));
    }
    Ok(java::trim(&java::utf8(&output)).to_owned())
}
