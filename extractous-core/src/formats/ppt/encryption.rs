//! A presentation's encryption as POI's `HSLFSlideShowEncrypted` opens it: the
//! `DocumentEncryptionAtom` the most recent encrypting user edit names, its CryptoAPI verifier
//! checked against the password, and the records and pictures its key decrypts in place.

use rc4::StreamCipher;

use crate::config::ParserConfigs;
use crate::formats::crypt::{self, BlockKeys, CryptoApiKey, EncryptionInfo};
use crate::formats::{Declined, Unported};

/// `BLIB_STORE_ENTRY_PARTS`: the fields of a blip store entry, each of which POI decrypts from
/// the entry's start.
const STORE_ENTRY_PARTS: [usize; 11] = [1, 1, 16, 2, 4, 4, 4, 1, 1, 1, 1];
/// The length of a blip store entry's fields.
const STORE_ENTRY: usize = 36;
/// The blip instances whose pictures start with two checksums.
const TWO_CHECKSUM_INSTANCES: [u16; 8] = [0x217, 0x3D5, 0x46B, 0x543, 0x6E1, 0x6E3, 0x6E5, 0x7A9];
/// The metafile blip types, whose checksums a 34-byte header follows.
const METAFILE_BLIPS: [u16; 3] = [0xF01A, 0xF01B, 0xF01C];
const BSE: u16 = 0xF007;
/// The exception POI throws where the password does not open the presentation.
const WRONG_PASSWORD: &str = "org.apache.poi.hslf.exceptions.EncryptedPowerPointFileException: \
                              PowerPoint file is encrypted. The correct password needs to be set \
                              via Biff8EncryptionKey.setCurrentUserPassword()";

fn unported(reason: &str) -> Unported {
    Unported(format!(
        "POI fails decrypting the presentation, which fails Tika's parse: {reason}"
    ))
}

/// The encryption of a presentation, opened.
#[derive(Debug)]
pub(super) struct Encryption {
    /// Where its `DocumentEncryptionAtom` is in the document stream.
    pub(super) atom: i32,
    key: CryptoApiKey,
}

impl Encryption {
    /// `new HSLFSlideShowEncrypted(docstream, records)` past finding the atom at `atom`: its
    /// encryption opened with the password, failing as Tika's path does where it does not open
    /// it.
    pub(super) fn open(
        atom: i32,
        info: &EncryptionInfo,
        configs: &ParserConfigs,
    ) -> Result<Encryption, Declined> {
        let EncryptionInfo::CryptoApi(api) = info;
        let key = api
            .verify(crypt::password(configs))
            .ok_or_else(|| Declined::Fails(crypt::encrypted(WRONG_PASSWORD)))?;
        Ok(Encryption { atom, key })
    }

    /// `decryptRecord`: the record at `offset` decrypted in place with the key of its persist
    /// id, its header first, then the length the header gives.
    pub(super) fn decrypt_record(
        &self,
        document: &mut [u8],
        persist_id: i32,
        offset: i32,
    ) -> Result<(), Unported> {
        let start = usize::try_from(offset)
            .ok()
            .filter(|start| start + 8 <= document.len())
            .ok_or_else(|| unported("a record's header runs past the document stream"))?;
        let mut cipher = self.key.block(persist_id as u32).cipher();
        cipher.apply_keystream(&mut document[start..start + 8]);
        let length = i32::from_le_bytes(
            document[start + 4..start + 8]
                .try_into()
                .expect("four bytes"),
        );
        let end = usize::try_from(length)
            .ok()
            .map(|length| start + 8 + length)
            .filter(|&end| end <= document.len() && (end > start + 8 || end < document.len()))
            .ok_or_else(|| unported("a record runs past the document stream"))?;
        cipher.apply_keystream(&mut document[start + 8..end]);
        Ok(())
    }

    /// `decryptPicture`: the picture record at `offset` of the "Pictures" stream decrypted in
    /// place, each of its parts from the start of block 0's key, as POI does, a blip store
    /// entry's fields each from the entry's start.
    pub(super) fn decrypt_picture(&self, stream: &mut [u8], offset: usize) -> Result<(), Unported> {
        self.decrypt_part(stream, offset, 8)?;
        let mut instance = ushort(stream, offset)? >> 4;
        let mut kind = ushort(stream, offset + 2)?;
        let length = i64::from(int(stream, offset + 4)?);
        let mut at = offset + 8;
        let end = at as i64 + length;
        if kind == BSE {
            for part in STORE_ENTRY_PARTS {
                self.decrypt_part(stream, at, part as i64)?;
            }
            at += STORE_ENTRY;
            let name = usize::from(ushort(stream, at - 3)?);
            if name > 0 {
                self.decrypt_part(stream, at, name as i64)?;
                at += name;
            }
            if at as i64 == end {
                return Ok(());
            }
            self.decrypt_part(stream, at, 8)?;
            instance = ushort(stream, at)? >> 4;
            kind = ushort(stream, at + 2)?;
            at += 8;
        }
        let checksums = if TWO_CHECKSUM_INSTANCES.contains(&instance) {
            2
        } else {
            1
        };
        for _ in 0..checksums {
            self.decrypt_part(stream, at, 16)?;
            at += 16;
        }
        let header = if METAFILE_BLIPS.contains(&kind) {
            34
        } else {
            1
        };
        self.decrypt_part(stream, at, header)?;
        at += header as usize;
        self.decrypt_part(stream, at, end - at as i64)
    }

    /// `decryptPicBytes`: `length` bytes at `at` decrypted with block 0's key from its start,
    /// failing where POI's stream reads none or runs out.
    fn decrypt_part(&self, stream: &mut [u8], at: usize, length: i64) -> Result<(), Unported> {
        let range = usize::try_from(length)
            .ok()
            .filter(|&length| length > 0)
            .map(|length| at..at + length)
            .filter(|range| range.end <= stream.len())
            .ok_or_else(|| unported("a picture's part is empty or runs past the stream"))?;
        self.key.block(0).decrypt(&mut stream[range]);
        Ok(())
    }
}

fn ushort(stream: &[u8], at: usize) -> Result<u16, Unported> {
    stream
        .get(at..at + 2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .ok_or_else(|| unported("a picture's header runs past the stream"))
}

fn int(stream: &[u8], at: usize) -> Result<i32, Unported> {
    stream
        .get(at..at + 4)
        .map(|bytes| i32::from_le_bytes(bytes.try_into().expect("four bytes")))
        .ok_or_else(|| unported("a picture's header runs past the stream"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encryption() -> Encryption {
        Encryption {
            atom: 0,
            key: CryptoApiKey::of_secret(&[7; 20]),
        }
    }

    /// `data` with each of `parts`, an offset and a length, XORed with block `block`'s key
    /// stream from its start.
    fn xored(data: &[u8], block: u32, parts: &[(usize, usize)]) -> Vec<u8> {
        let mut out = data.to_vec();
        for &(at, length) in parts {
            encryption()
                .key
                .block(block)
                .decrypt(&mut out[at..at + length]);
        }
        out
    }

    fn header(options: u16, kind: u16, length: i32) -> Vec<u8> {
        let mut bytes = options.to_le_bytes().to_vec();
        bytes.extend(kind.to_le_bytes());
        bytes.extend(length.to_le_bytes());
        bytes
    }

    #[test]
    fn decrypts_a_record_with_its_persist_ids_key() {
        let mut plain = header(0x000F, 1000, 4);
        plain.extend(b"body");
        let mut document = xored(&plain, 5, &[(0, plain.len())]);
        encryption().decrypt_record(&mut document, 5, 0).unwrap();
        assert_eq!(document, plain);
    }

    #[test]
    fn fails_where_poi_decrypts_a_record_past_the_stream() {
        let plain = header(0x000F, 1000, 8);
        let mut document = xored(&plain, 1, &[(0, 8)]);
        assert!(encryption().decrypt_record(&mut document, 1, 0).is_err());
        let mut empty = xored(&header(0, 1000, 0), 1, &[(0, 8)]);
        assert!(encryption().decrypt_record(&mut empty, 1, 0).is_err());
    }

    #[test]
    fn decrypts_each_part_of_a_picture_from_the_key_streams_start() {
        let mut plain = header(0x6E00, 0xF01E, 21);
        plain.extend([1; 16]);
        plain.push(0xFF);
        plain.extend(b"data");
        let mut stream = xored(&plain, 0, &[(0, 8), (8, 16), (24, 1), (25, 4)]);
        encryption().decrypt_picture(&mut stream, 0).unwrap();
        assert_eq!(stream, plain);
    }

    /// POI decrypts each field of a blip store entry from the entry's start, so that of its
    /// first 16 bytes those the fields cover an odd number of times are decrypted.
    #[test]
    fn decrypts_a_blip_store_entry_as_poi_does() {
        let mut entry = [3; STORE_ENTRY];
        entry[33] = 0;
        entry[34] = 0;
        let mut plain = header(0x0002, BSE, STORE_ENTRY as i32);
        plain.extend(entry);
        let mut stream = xored(&plain, 0, &[(0, 8)]);
        encryption().decrypt_picture(&mut stream, 0).unwrap();
        let key_stream = xored(&[0; 16], 0, &[(0, 16)]);
        let mut expected = plain;
        for at in [0, 1].into_iter().chain(4..16) {
            expected[8 + at] ^= key_stream[at];
        }
        assert_eq!(stream, expected);
    }

    #[test]
    fn fails_where_poi_decrypts_an_empty_part() {
        let plain = header(0x6E00, 0xF01E, 17);
        let mut stream = xored(&plain, 0, &[(0, 8)]);
        stream.extend([0; 17]);
        assert!(encryption().decrypt_picture(&mut stream, 0).is_err());
    }
}
