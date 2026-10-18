//! Jackcess's `PageChannel` over a database read whole: pages by number, the first unmasked,
//! the others decoded by the codec of `jackcess-encrypt`'s `CryptCodecProvider`: the Jet codec,
//! Office encryption, or MSISAM's.

use std::borrow::Cow;

use rc4::{KeyInit, Rc4, StreamCipher};

use super::buffer::Thrown;
use super::crypt::Decryption;
use super::format::{Charset, CodecType, Encoding, Format};
use super::msisam::Msisam;
use crate::formats::Difference;

pub(super) const PAGE_TYPE_DATA: u8 = 0x01;
/// Where Jet 3's header names the database's code page.
const OFFSET_CODE_PAGE: usize = 60;
pub(super) const PAGE_TYPE_TABLE_DEF: u8 = 0x02;
pub(super) const PAGE_TYPE_USAGE_MAP: u8 = 0x05;
const ENCODING_KEY_LENGTH: usize = 4;

/// The pages of a database.
pub(super) struct Pages<'a> {
    file: &'a [u8],
    pub(super) format: &'static Format,
    /// The charset of the database's text: the code page its header names, in Jet 3.
    pub(super) charset: Charset,
    codec: Codec,
}

/// What decodes the pages after the first.
enum Codec {
    /// Nothing: the database's encoding key is blank.
    None,
    /// The Jet codec, RC4 under the encoding key XORed with each page's number.
    Jet([u8; ENCODING_KEY_LENGTH]),
    /// Office encryption, opened with the password.
    Office(Box<Decryption>),
    /// MSISAM's RC4 of the first pages.
    Msisam(Msisam),
}

impl<'a> Pages<'a> {
    /// `PageChannel.initialize`'s codec: `CryptCodecProvider.createHandler`, Office encryption
    /// opened with `password`.
    pub(super) fn open(
        file: &'a [u8],
        format: &'static Format,
        password: &str,
    ) -> Result<Self, Thrown> {
        let mut pages = Self {
            file,
            format,
            charset: Charset::Unicode,
            codec: Codec::None,
        };
        let root = pages.root()?;
        if format.encoding == Encoding::CodePage {
            let code_page = u16::from_le_bytes(
                root[OFFSET_CODE_PAGE..OFFSET_CODE_PAGE + 2]
                    .try_into()
                    .expect("two bytes"),
            );
            pages.charset =
                Charset::of_code_page(code_page).unwrap_or_else(Charset::jackcess_default);
        }
        let start = format.offset_encoding_key;
        let key: [u8; ENCODING_KEY_LENGTH] = root[start..start + ENCODING_KEY_LENGTH]
            .try_into()
            .expect("four bytes");
        let blank = key == [0; ENCODING_KEY_LENGTH];
        pages.codec = match format.codec {
            CodecType::Jet | CodecType::Office if blank => Codec::None,
            CodecType::Jet => Codec::Jet(key),
            CodecType::Office => Codec::Office(Box::new(Decryption::open(&root, key, password)?)),
            CodecType::Msisam => Codec::Msisam(Msisam::open(&root, password)?),
        };
        Ok(pages)
    }

    /// Why Jackcess refuses the password that opens the database's encryption, if it does.
    pub(super) fn refused(&self) -> Option<Difference> {
        match &self.codec {
            Codec::Office(decryption) => decryption.refused.clone(),
            Codec::None | Codec::Jet(_) | Codec::Msisam(_) => None,
        }
    }

    /// The length of the database file.
    pub(super) fn file_len(&self) -> usize {
        self.file.len()
    }

    /// `readRootPage`: the first page with its header unmasked.
    pub(super) fn root(&self) -> Result<Vec<u8>, Thrown> {
        let size = self.format.page_size;
        let Some(page) = self.file.get(..size) else {
            return Err(Thrown::io(format!(
                "Failed attempting to read {size} bytes from page 0, only read {}",
                self.file.len()
            )));
        };
        let mut page = page.to_vec();
        let start = self.format.offset_masked_header;
        for (byte, mask) in page[start..].iter_mut().zip(self.format.header_mask) {
            *byte ^= mask;
        }
        Ok(page)
    }

    /// `readPage`: a page decoded, which throws past the last whole page of the file.
    pub(super) fn page(&self, number: i32) -> Result<Cow<'a, [u8]>, Thrown> {
        if number == 0 {
            return self.root().map(Cow::Owned);
        }
        let size = self.format.page_size;
        let count = self.file.len() / size;
        let index = match usize::try_from(number) {
            Ok(index) if index < count => index,
            _ => {
                return Err(Thrown::illegal_state(format!(
                    "invalid page number {number}"
                )))
            }
        };
        let page = &self.file[index * size..(index + 1) * size];
        match &self.codec {
            Codec::None => Ok(Cow::Borrowed(page)),
            Codec::Jet(key) => {
                let mut page_key = (number as u32).to_le_bytes();
                for (byte, key) in page_key.iter_mut().zip(key) {
                    *byte ^= key;
                }
                let mut decoded = page.to_vec();
                rc4(&page_key, &mut decoded);
                Ok(Cow::Owned(decoded))
            }
            Codec::Office(decryption) => decryption.decrypt(number, page).map(Cow::Owned),
            Codec::Msisam(_) if !Msisam::encodes(number) => Ok(Cow::Borrowed(page)),
            Codec::Msisam(msisam) => {
                let mut decoded = page.to_vec();
                msisam.decode(number, &mut decoded);
                Ok(Cow::Owned(decoded))
            }
        }
    }
}

/// RC4 applied to `data` in place.
pub(super) fn rc4(key: &[u8], data: &mut [u8]) {
    Rc4::new_from_slice(key)
        .expect("an RC4 key of 1 to 256 bytes")
        .apply_keystream(data);
}

#[cfg(test)]
mod tests {
    use super::rc4;

    #[test]
    fn rc4_matches_the_published_test_vector() {
        let mut data = b"Plaintext".to_vec();
        rc4(b"Key", &mut data);
        assert_eq!(data, [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    }
}
