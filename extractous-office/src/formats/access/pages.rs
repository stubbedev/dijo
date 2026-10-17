//! Jackcess's `PageChannel` over a database read whole: pages by number, the first unmasked,
//! the others decoded by the Jet codec of `jackcess-encrypt`'s `CryptCodecProvider`.

use std::borrow::Cow;

use super::buffer::Thrown;
use super::format::{CodecType, Format};

pub(super) const PAGE_TYPE_DATA: u8 = 0x01;
pub(super) const PAGE_TYPE_TABLE_DEF: u8 = 0x02;
pub(super) const PAGE_TYPE_USAGE_MAP: u8 = 0x05;
const ENCODING_KEY_LENGTH: usize = 4;

/// The pages of a database.
pub(super) struct Pages<'a> {
    file: &'a [u8],
    pub(super) format: &'static Format,
    /// The RC4 key of the Jet codec, where the database's encoding key is not blank.
    encoding_key: Option<[u8; ENCODING_KEY_LENGTH]>,
}

impl<'a> Pages<'a> {
    /// `PageChannel.initialize`'s codec: `CryptCodecProvider.createHandler`, which the port has
    /// for the Jet codec; a database under Office encryption is refused as encrypted.
    pub(super) fn open(file: &'a [u8], format: &'static Format) -> Result<Self, Thrown> {
        let mut pages = Self {
            file,
            format,
            encoding_key: None,
        };
        let root = pages.root()?;
        let start = format.offset_encoding_key;
        let key: [u8; ENCODING_KEY_LENGTH] = root[start..start + ENCODING_KEY_LENGTH]
            .try_into()
            .expect("four bytes");
        if key == [0; ENCODING_KEY_LENGTH] {
            return Ok(pages);
        }
        match format.codec {
            CodecType::Jet => pages.encoding_key = Some(key),
            CodecType::Office => return Err(Thrown::Encrypted),
        }
        Ok(pages)
    }

    /// The length of the database file.
    pub(super) fn file_len(&self) -> usize {
        self.file.len()
    }

    /// `readRootPage`: the first page with its header unmasked.
    pub(super) fn root(&self) -> Result<Vec<u8>, Thrown> {
        let size = self.format.page_size;
        let Some(page) = self.file.get(..size) else {
            return Err(Thrown::other(format!(
                "java.io.IOException: Failed attempting to read {size} bytes from page 0, only \
                 read {}",
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
        let Some(key) = self.encoding_key else {
            return Ok(Cow::Borrowed(page));
        };
        let mut page_key = (number as u32).to_le_bytes();
        for (byte, key) in page_key.iter_mut().zip(key) {
            *byte ^= key;
        }
        let mut decoded = page.to_vec();
        rc4(&page_key, &mut decoded);
        Ok(Cow::Owned(decoded))
    }
}

/// RC4 applied to `data` in place.
pub(super) fn rc4(key: &[u8], data: &mut [u8]) {
    let mut state: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
        state.swap(i, usize::from(j));
    }
    let (mut i, mut j) = (0u8, 0u8);
    for byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(state[usize::from(i)]);
        state.swap(usize::from(i), usize::from(j));
        let k = state[usize::from(state[usize::from(i)].wrapping_add(state[usize::from(j)]))];
        *byte ^= k;
    }
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
