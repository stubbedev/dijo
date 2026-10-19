//! The JDK's `x-ISCII91` decoder (`sun.nio.cs.ext.ISCII91`), as `new String(bytes, charset)`
//! runs it: Devanagari from the bytes 0xA1 to 0xFA, ASCII below 0x80. A vowel or sign that a
//! following nukta can combine with is held back until the next byte, and a held character goes
//! out before the next one. The JDK looks a byte from 0x80 up in its table one place early, so
//! 0x80 decodes as U+007F and each later byte as the table's entry for the byte before it.

const INVALID: u16 = 0xFFFF;
const REPLACEMENT: u16 = 0xFFFD;
const NUKTA: u16 = 0x093C;
const HALANT: u16 = 0x094D;
const ZWNJ: u16 = 0x200C;
const ZWJ: u16 = 0x200D;

/// The table's entries for the bytes 0x81 to 0xFF, at the places the JDK looks them up.
const HIGH: [u16; 127] = [
    0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF,
    0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF,
    0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0x0901, 0x0902, 0x0903, 0x0905,
    0x0906, 0x0907, 0x0908, 0x0909, 0x090A, 0x090B, 0x090E, 0x090F, 0x0910, 0x090D, 0x0912, 0x0913,
    0x0914, 0x0911, 0x0915, 0x0916, 0x0917, 0x0918, 0x0919, 0x091A, 0x091B, 0x091C, 0x091D, 0x091E,
    0x091F, 0x0920, 0x0921, 0x0922, 0x0923, 0x0924, 0x0925, 0x0926, 0x0927, 0x0928, 0x0929, 0x092A,
    0x092B, 0x092C, 0x092D, 0x092E, 0x092F, 0x095F, 0x0930, 0x0931, 0x0932, 0x0933, 0x0934, 0x0935,
    0x0936, 0x0937, 0x0938, 0x0939, 0x200D, 0x093E, 0x093F, 0x0940, 0x0941, 0x0942, 0x0943, 0x0946,
    0x0947, 0x0948, 0x0945, 0x094A, 0x094B, 0x094C, 0x0949, 0x094D, 0x093C, 0x0964, 0xFFFF, 0xFFFF,
    0xFFFF, 0xFFFF, 0xFFFD, 0xFFFD, 0x0966, 0x0967, 0x0968, 0x0969, 0x096A, 0x096B, 0x096C, 0x096D,
    0x096E, 0x096F, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF,
];

/// The character the JDK's table gives `byte`: [`INVALID`] for none, [`REPLACEMENT`] for the
/// attribute and extension codes, which replace the byte after them too.
fn table(byte: u8) -> u16 {
    match byte {
        0x00..=0x7F => u16::from(byte),
        0x80 => 0x7F,
        _ => HIGH[usize::from(byte - 0x81)],
    }
}

/// The nukta form of the held character `context`, if it has one.
fn with_nukta(context: u16) -> Option<u16> {
    Some(match context {
        0x0901 => 0x0950,
        0x0907 => 0x090C,
        0x0908 => 0x0961,
        0x090B => 0x0960,
        0x093F => 0x0962,
        0x0940 => 0x0963,
        0x0943 => 0x0944,
        0x0964 => 0x093D,
        _ => return None,
    })
}

/// `new String(bytes, "x-ISCII91")`: a byte the table has no character for is replaced by
/// U+FFFD, as the decoder's replacement action does.
pub(super) fn decode(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len() + 1);
    let mut context = INVALID;
    let mut held = false;
    for &byte in bytes {
        let current = table(byte);
        if context == REPLACEMENT {
            units.push(REPLACEMENT);
            context = INVALID;
            held = false;
            continue;
        }
        if held && !(current == NUKTA && with_nukta(context).is_some()) {
            units.push(context);
            context = current;
            continue;
        }
        match current {
            _ if with_nukta(current).is_some() => {
                context = current;
                held = true;
                continue;
            }
            NUKTA => units.push(match (with_nukta(context), context) {
                (Some(combined), _) => combined,
                (None, HALANT) => ZWJ,
                (None, _) => NUKTA,
            }),
            HALANT if context == HALANT => units.push(ZWNJ),
            INVALID => {
                units.push(REPLACEMENT);
                continue;
            }
            _ => units.push(current),
        }
        context = current;
        held = false;
    }
    if held {
        units.push(context);
    }
    String::from_utf16(&units).expect("the ISCII-91 table holds no surrogates")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Inputs and what `new String(bytes, "x-ISCII91")` gives on the JDK.
    fn decodes_as_the_jdk_does() {
        let cases: [(&[u8], &str); 16] = [
            (b"\x61\x62\x80", "\u{61}\u{62}\u{7f}"),
            (b"\xb4\xdb\xc3", "\u{916}\u{93f}\u{925}"),
            (b"\xa2\xe9", "\u{902}\u{93c}"),
            (b"\xa7\xe9\x78", "\u{961}\u{78}"),
            (b"\xa7\xa8\xe9", "\u{908}\u{909}\u{93c}"),
            (
                b"\xe9\xe9\xe8\xe8\xe8\xe9",
                "\u{93c}\u{93c}\u{94d}\u{200c}\u{200c}\u{200d}",
            ),
            (
                b"\x61\xf0\x62\x63\x90",
                "\u{61}\u{fffd}\u{fffd}\u{63}\u{fffd}",
            ),
            (b"\xa2\x90\xa7", "\u{902}\u{fffd}\u{908}"),
            (b"\xa1\x90\x61", "\u{901}\u{ffff}\u{61}"),
            (b"\xa1\xe9", "\u{950}"),
            (b"\xa6\xa6\xb3", "\u{907}\u{907}\u{915}"),
            (b"\xa6\xe8\xe8", "\u{907}\u{94d}\u{94d}"),
            (b"\xf0\xe9\xe9", "\u{fffd}\u{fffd}\u{93c}"),
            (b"\xa1", "\u{901}"),
            (b"\xe8\xe9\x61", "\u{94d}\u{200d}\u{61}"),
            (b"\xfe", "\u{fffd}"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(decode(bytes), expected, "{bytes:x?}");
        }
    }
}
