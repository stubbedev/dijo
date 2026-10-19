//! XOR obfuscation ([MS-OFFCRYPTO] 2.3.6 and 2.3.7): a 16-bit key and verifier a password must
//! match, and the 16-byte array the data is obfuscated with. Excel transforms the data of its
//! records by method 1, as POI's `XORDecryptor` reads it; Word transforms its streams by
//! method 2, as [MS-DOC] 2.2.6.1 and LibreOffice read them.

use super::Unopened;

/// The most password characters the key and verifier derive from.
const MAX_PASSWORD_LENGTH: usize = 15;

/// `InitialCodeArray`, by the password's length.
const INITIAL_CODE: [u16; MAX_PASSWORD_LENGTH] = [
    0xE1F0, 0x1D0F, 0xCC9C, 0x84C0, 0x110C, 0x0E10, 0xF1CE, 0x313E, 0x1872, 0xE139, 0xD40F, 0x84F9,
    0x280C, 0xA96A, 0x4EC3,
];

/// `EncryptionMatrix`: the words each bit of a password character toggles in the key, by the
/// character's place counted from the end of a password of 15.
const ENCRYPTION_MATRIX: [[u16; 7]; MAX_PASSWORD_LENGTH] = [
    [0xAEFC, 0x4DD9, 0x9BB2, 0x2745, 0x4E8A, 0x9D14, 0x2A09],
    [0x7B61, 0xF6C2, 0xFDA5, 0xEB6B, 0xC6F7, 0x9DCF, 0x2BBF],
    [0x4563, 0x8AC6, 0x05AD, 0x0B5A, 0x16B4, 0x2D68, 0x5AD0],
    [0x0375, 0x06EA, 0x0DD4, 0x1BA8, 0x3750, 0x6EA0, 0xDD40],
    [0xD849, 0xA0B3, 0x5147, 0xA28E, 0x553D, 0xAA7A, 0x44D5],
    [0x6F45, 0xDE8A, 0xAD35, 0x4A4B, 0x9496, 0x390D, 0x721A],
    [0xEB23, 0xC667, 0x9CEF, 0x29FF, 0x53FE, 0xA7FC, 0x5FD9],
    [0x47D3, 0x8FA6, 0x0F6D, 0x1EDA, 0x3DB4, 0x7B68, 0xF6D0],
    [0xB861, 0x60E3, 0xC1C6, 0x93AD, 0x377B, 0x6EF6, 0xDDEC],
    [0x45A0, 0x8B40, 0x06A1, 0x0D42, 0x1A84, 0x3508, 0x6A10],
    [0xAA51, 0x4483, 0x8906, 0x022D, 0x045A, 0x08B4, 0x1168],
    [0x76B4, 0xED68, 0xCAF1, 0x85C3, 0x1BA7, 0x374E, 0x6E9C],
    [0x3730, 0x6E60, 0xDCC0, 0xA9A1, 0x4363, 0x86C6, 0x1DAD],
    [0x3331, 0x6662, 0xCCC4, 0x89A9, 0x0373, 0x06E6, 0x0DCC],
    [0x1021, 0x2042, 0x4084, 0x8108, 0x1231, 0x2462, 0x48C4],
];

/// `PadArray`, which fills the obfuscation array past the password.
const PAD: [u8; MAX_PASSWORD_LENGTH] = [
    0xBB, 0xFF, 0xFF, 0xBA, 0xFF, 0xFF, 0xB9, 0x80, 0x00, 0xBE, 0x0F, 0x00, 0xBF, 0x0F, 0x00,
];

/// The password as the single bytes the key, verifier and array derive from: of its first 15
/// UTF-16 units, each one's low byte, or its high byte where the low one is zero.
fn ansi_password(password: &str) -> Vec<u8> {
    password
        .encode_utf16()
        .take(MAX_PASSWORD_LENGTH)
        .map(|unit| match unit.to_le_bytes() {
            [0, high] => high,
            [low, _] => low,
        })
        .collect()
}

/// `CreateXorKey_Method1`.
fn create_key(password: &[u8]) -> u16 {
    let Some(last) = password.len().checked_sub(1) else {
        return 0;
    };
    let mut key = INITIAL_CODE[last];
    let rows = &ENCRYPTION_MATRIX[MAX_PASSWORD_LENGTH - password.len()..];
    for (&character, row) in password.iter().zip(rows) {
        for (bit, word) in row.iter().enumerate() {
            if character >> bit & 1 == 1 {
                key ^= word;
            }
        }
    }
    key
}

/// `CreatePasswordVerifier_Method1`.
fn create_verifier(password: &[u8]) -> u16 {
    if password.is_empty() {
        return 0;
    }
    let rotate = |verifier: u16| ((verifier >> 14) & 1) | ((verifier << 1) & 0x7FFF);
    let mut verifier = password
        .iter()
        .rev()
        .fold(0u16, |verifier, &byte| rotate(verifier) ^ u16::from(byte));
    verifier = rotate(verifier) ^ password.len() as u16;
    verifier ^ 0xCE4B
}

/// `CreateXorArray_Method1`: the password filled with the pad, each byte XORed with the key's
/// byte of its parity and rotated left by `rotation`, which Excel takes as 2 and Word as 7.
fn create_array(password: &[u8], key: u16, rotation: u32) -> [u8; 16] {
    let mut array = [0u8; 16];
    array[..password.len()].copy_from_slice(password);
    let pad = (16 - password.len()).min(PAD.len());
    array[password.len()..password.len() + pad].copy_from_slice(&PAD[..pad]);
    let key = key.to_le_bytes();
    for (at, byte) in array.iter_mut().enumerate() {
        *byte = (*byte ^ key[at & 1]).rotate_left(rotation);
    }
    array
}

/// The key and verifier an obfuscated document stores, which a password opens where its own
/// match them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Xor {
    key: u16,
    verifier: u16,
}

impl Xor {
    /// `XOREncryptionVerifier`: the key, then the verifier, as a workbook's `FILEPASS` record
    /// holds them.
    pub fn read(data: &[u8]) -> Result<Xor, Unopened> {
        match data {
            [key_low, key_high, verifier_low, verifier_high, ..] => Ok(Xor {
                key: u16::from_le_bytes([*key_low, *key_high]),
                verifier: u16::from_le_bytes([*verifier_low, *verifier_high]),
            }),
            _ => Err(Unopened::Thrown(
                "the XOR obfuscation ends before its key and verifier".to_owned(),
            )),
        }
    }

    /// The `lKey` of an obfuscated Word document, the verifier of method 2: the key in its high
    /// word, the verifier in its low one.
    pub fn of_word_verifier(l_key: u32) -> Xor {
        Xor {
            key: (l_key >> 16) as u16,
            verifier: l_key as u16,
        }
    }

    /// The password's bytes, where it opens the obfuscation.
    fn open(&self, password: &str) -> Option<(Vec<u8>, u16)> {
        let ansi = ansi_password(password);
        let key = create_key(&ansi);
        (key == self.key && create_verifier(&ansi) == self.verifier).then_some((ansi, key))
    }

    /// `XORDecryptor.verifyPassword`: the key of a workbook, where the password opens it.
    pub fn verify_excel(&self, password: &str) -> Option<ExcelXorKey> {
        self.open(password)
            .map(|(ansi, key)| ExcelXorKey(create_array(&ansi, key, 2)))
    }

    /// The key of a Word document, where the password opens it.
    pub fn verify_word(&self, password: &str) -> Option<WordXorKey> {
        self.open(password)
            .map(|(ansi, key)| WordXorKey(create_array(&ansi, key, 7)))
    }
}

/// The obfuscation array of a workbook.
#[derive(Debug, Clone)]
pub struct ExcelXorKey([u8; 16]);

impl ExcelXorKey {
    /// XOR data transformation method 1 undone in place on `data`, which starts at `position`
    /// of the stream, of a record of `length` bytes: each byte rotated left by 3 and XORed with
    /// the array at the byte's position plus the length.
    pub fn decrypt_record(&self, data: &mut [u8], position: usize, length: usize) {
        for (at, byte) in (position..).zip(data.iter_mut()) {
            *byte = byte.rotate_left(3) ^ self.0[(at + length) & 0x0F];
        }
    }
}

/// The obfuscation array of a Word document.
#[derive(Debug, Clone)]
pub struct WordXorKey([u8; 16]);

impl WordXorKey {
    /// XOR data transformation method 2 undone in place on `data`, which starts at `position`
    /// of its stream: each byte XORed with the array at its position, unless it or the result
    /// is zero.
    pub fn decrypt(&self, data: &mut [u8], position: usize) {
        for (at, byte) in (position..).zip(data.iter_mut()) {
            let plain = *byte ^ self.0[at & 0x0F];
            if *byte != 0 && plain != 0 {
                *byte = plain;
            }
        }
    }
}

#[cfg(any(test, feature = "testing"))]
impl Xor {
    /// The key and verifier of `password`, as a document stores them.
    pub fn encrypting(password: &str) -> Xor {
        let ansi = ansi_password(password);
        Xor {
            key: create_key(&ansi),
            verifier: create_verifier(&ansi),
        }
    }

    /// The `lKey` of a Word document obfuscated with the password.
    pub fn word_verifier(&self) -> u32 {
        u32::from(self.key) << 16 | u32::from(self.verifier)
    }

    /// The key and verifier as a `FILEPASS` record holds them.
    pub fn written(&self) -> Vec<u8> {
        [self.key, self.verifier]
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [MS-OFFCRYPTO] 2.3.7.4's and POI's example: the verifier of method 2 of "Example".
    #[test]
    fn derives_the_key_and_verifier_of_the_specification() {
        let ansi = ansi_password("Example");
        assert_eq!(create_key(&ansi), 0x64CE);
        assert_eq!(create_verifier(&ansi), 0xED7E);
        let velvet = Xor::encrypting(super::super::DEFAULT_PASSWORD);
        assert_eq!(velvet.verifier, 0x9A0A);
    }

    #[test]
    fn opens_only_with_the_password() {
        let xor = Xor::encrypting("tika");
        assert!(xor.verify_excel("tika").is_some());
        assert!(xor.verify_word("tika").is_some());
        assert!(xor.verify_excel("other").is_none());
        assert_eq!(Xor::read(&xor.written()), Ok(xor));
        assert_eq!(Xor::of_word_verifier(xor.word_verifier()), xor);
        assert!(Xor::read(&[1, 2, 3]).is_err());
    }

    #[test]
    fn takes_the_first_15_characters_and_their_high_bytes_where_the_low_ones_are_zero() {
        assert_eq!(ansi_password("\u{0100}a\u{00E6}"), [0x01, b'a', 0xE6]);
        assert_eq!(ansi_password(&"x".repeat(20)).len(), 15);
        let long = Xor::encrypting(&"x".repeat(20));
        assert!(long.verify_word(&"x".repeat(15)).is_some());
    }

    /// The array of [MS-OFFCRYPTO] 2.3.7.2, whose bytes are XORed with the key and rotated right
    /// by one, is Word's.
    #[test]
    fn rotates_words_array_as_the_specification_does() {
        let ansi = ansi_password("tika");
        let key = create_key(&ansi);
        let [low, high] = key.to_le_bytes();
        let mut filled = ansi.clone();
        filled.extend(&PAD[..16 - ansi.len()]);
        let specified: Vec<u8> = filled
            .iter()
            .enumerate()
            .map(|(at, byte)| (byte ^ if at % 2 == 0 { low } else { high }).rotate_right(1))
            .collect();
        assert_eq!(create_array(&ansi, key, 7).to_vec(), specified);
    }

    #[test]
    fn undoes_words_transformation_but_on_zeros() {
        let key = WordXorKey([0x0F; 16]);
        let mut data = [0x00, 0x0F, 0xF0, 0x0F];
        key.decrypt(&mut data, 5);
        assert_eq!(data, [0x00, 0x0F, 0xFF, 0x0F]);
    }

    #[test]
    fn undoes_excels_transformation_by_position_and_record_length() {
        let mut array = [0u8; 16];
        array[3] = 0xFF;
        let key = ExcelXorKey(array);
        let mut data = [0b0010_0000, 0b0010_0000];
        key.decrypt_record(&mut data, 1, 2);
        assert_eq!(data, [0xFE, 0x01]);
    }
}
