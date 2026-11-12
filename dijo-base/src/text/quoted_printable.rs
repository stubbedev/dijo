//! The escapes of quoted-printable text (RFC 2045, section 6.7), which mail bodies, encoded words
//! and vCard 2.1 values share: `=` and two hexadecimal digits for a byte.

const EQ: u8 = b'=';

/// The value of a hexadecimal digit, either case, or `None` for any other byte or none.
pub fn hex_value(byte: Option<u8>) -> Option<u8> {
    match byte? {
        c @ b'0'..=b'9' => Some(c - b'0'),
        c @ b'A'..=b'F' => Some(c - b'A' + 10),
        c @ b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// The escapes of a line of quoted-printable text decoded into `out`, an `=` not followed by two
/// hexadecimal digits kept as it is.
pub fn decode_escapes(line: &[u8], out: &mut Vec<u8>) {
    let mut at = 0;
    while at < line.len() {
        let byte = line[at];
        if byte == EQ {
            if let (Some(upper), Some(lower)) = (
                hex_value(line.get(at + 1).copied()),
                hex_value(line.get(at + 2).copied()),
            ) {
                out.push((upper << 4) | lower);
                at += 3;
                continue;
            }
        }
        out.push(byte);
        at += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_escapes_and_keeps_what_is_not_one() {
        let mut out = Vec::new();
        decode_escapes(b"Caf=C3=a9 =3D =G1 =4", &mut out);
        assert_eq!(out, "Café = =G1 =4".as_bytes());
    }
}
