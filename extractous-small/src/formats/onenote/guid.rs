//! Tika's `GUID` and `ExtendedGUID` of the OneNote parser: sixteen bytes, in the order the GUID
//! is written.

use std::fmt;

use super::file::{tika, Thrown};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Guid(pub(super) [u8; 16]);

impl Guid {
    pub(super) const NIL: Guid = Guid([0; 16]);

    /// `fromCurlyBraceUTF16Bytes`: the GUID a `{...}` string in UTF-16LE names.
    pub(super) fn from_curly_brace_utf16(bytes: &[u8]) -> Result<Guid, Thrown> {
        let invalid = || tika("Invalid GUID string");
        let text = crate::java::utf16le(bytes);
        let units: Vec<u16> = text.encode_utf16().collect();
        let at = |i: usize| units.get(i).copied();
        if units.len() != 38
            || at(0) != Some(u16::from(b'{'))
            || [9, 14, 19, 24]
                .iter()
                .any(|&i| at(i) != Some(u16::from(b'-')))
            || at(37) != Some(u16::from(b'}'))
        {
            return Err(invalid());
        }
        let hex: Vec<u16> = units[1..37]
            .iter()
            .copied()
            .filter(|&unit| unit != u16::from(b'-'))
            .collect();
        if hex.len() != 32 {
            return Err(invalid());
        }
        let mut guid = [0; 16];
        for (i, pair) in hex.chunks(2).enumerate() {
            let digit = |unit: u16| {
                char::from_u32(u32::from(unit))
                    .filter(char::is_ascii_hexdigit)
                    .and_then(|c| c.to_digit(16))
                    .ok_or_else(invalid)
            };
            guid[i] = u8::try_from(digit(pair[0])? << 4 | digit(pair[1])?)
                .expect("two hexadecimal digits are a byte");
        }
        Ok(Guid(guid))
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let part = |range: std::ops::Range<usize>| -> String {
            self.0[range]
                .iter()
                .map(|&value| format!("{value:02X}"))
                .collect()
        };
        write!(
            f,
            "{{{}-{}-{}-{}-{}}}",
            part(0..4),
            part(4..6),
            part(6..8),
            part(8..10),
            part(10..16)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ExtendedGuid {
    pub(super) guid: Guid,
    pub(super) n: i64,
}

impl ExtendedGuid {
    pub(super) const NIL: ExtendedGuid = ExtendedGuid {
        guid: Guid::NIL,
        n: 0,
    };
}

impl Default for ExtendedGuid {
    fn default() -> Self {
        ExtendedGuid::NIL
    }
}

impl fmt::Display for ExtendedGuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}]", self.guid, self.n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_guids_as_tika_does() {
        let mut bytes = [0x0a; 16];
        bytes[0] = 0xab;
        bytes[15] = 0xff;
        let guid = Guid(bytes);
        assert_eq!(guid.to_string(), "{AB0A0A0A-0A0A-0A0A-0A0A-0A0A0A0A0AFF}");
        let named = "{0A0B0C0D-0E0F-1011-1213-14151617181A}";
        let utf16: Vec<u8> = named.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let parsed = Guid::from_curly_brace_utf16(&utf16).unwrap();
        assert_eq!(parsed.to_string(), named);
        assert!(Guid::from_curly_brace_utf16(&utf16[2..]).is_err());
    }
}
