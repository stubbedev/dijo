//! `jackcess-encrypt`'s `MSISAMCryptCodecHandler`: the RC4 that encodes the pages 1 to 14 of an
//! MSISAM (Microsoft Money) database. Its new encryption keys them by the digest of the password
//! and the salt of the header page, checked against a known sequence; the old one by the Jet key
//! the salt makes hashed with the masked password region and the engine name.

use super::buffer::Thrown;
use super::pages::rc4;
use crate::formats::crypt::Hash;
use crate::java::to_upper_case;

const SALT_OFFSET: usize = 0x72;
const SALT_LENGTH: usize = 8;
const BASE_SALT_LENGTH: usize = 4;
const CRYPT_CHECK_START: usize = 0x2e9;
const CRYPT_CHECK_LENGTH: usize = 4;
const ENCRYPTION_FLAGS_OFFSET: usize = 0x298;
const NEW_ENCRYPTION: u8 = 0x6;
const USE_SHA1: u8 = 0x20;
const PASSWORD_LENGTH: usize = 0x28;
const PASSWORD_DIGEST_LENGTH: usize = 0x10;
const MAX_ENCRYPTED_PAGE: i32 = 0xE;
const TRAILING_PASSWORD_LENGTH: usize = 20;
/// `JetCryptCodecHandler.ENCODING_KEY_LENGTH`.
const JET_KEY_LENGTH: usize = 4;
/// Jet 4's `OFFSET_PASSWORD`, `SIZE_PASSWORD` and `OFFSET_HEADER_DATE`, which the MSISAM format
/// keeps.
const OFFSET_PASSWORD: usize = 66;
const SIZE_PASSWORD: usize = 40;
const OFFSET_HEADER_DATE: usize = 114;
/// `JetFormat.OFFSET_ENGINE_NAME` and `LENGTH_ENGINE_NAME`.
const OFFSET_ENGINE_NAME: usize = 4;
const LENGTH_ENGINE_NAME: usize = 15;
const INVALID_CREDENTIALS: &str =
    "com.healthmarketscience.jackcess.crypt.InvalidCredentialsException";

/// The key the pages are decoded with, each page's number applied to its four bytes from
/// `number_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Msisam {
    key: Vec<u8>,
    number_at: usize,
}

impl Msisam {
    /// `MSISAMCryptCodecHandler.create` of the header page `root`, unmasked, under `password`,
    /// which the new encryption checks.
    pub(super) fn open(root: &[u8], password: &str) -> Result<Self, Thrown> {
        if root[ENCRYPTION_FLAGS_OFFSET] & NEW_ENCRYPTION == 0 {
            return Ok(Self {
                key: old_key(root).to_vec(),
                number_at: 0,
            });
        }
        let salt = &root[SALT_OFFSET..SALT_OFFSET + SALT_LENGTH];
        let base_salt = &salt[..BASE_SALT_LENGTH];
        let digest = password_digest(root, password);
        let check_at = CRYPT_CHECK_START + usize::from(root[SALT_OFFSET]);
        let check = &root[check_at..check_at + CRYPT_CHECK_LENGTH];
        if check.iter().any(|&byte| byte != 0) {
            let mut decrypted = check.to_vec();
            rc4(&[digest.as_slice(), salt].concat(), &mut decrypted);
            if decrypted != base_salt {
                return Err(Thrown::illegal_state_of(
                    INVALID_CREDENTIALS,
                    "Incorrect password provided",
                ));
            }
        }
        Ok(Self {
            key: [digest.as_slice(), base_salt].concat(),
            number_at: PASSWORD_DIGEST_LENGTH,
        })
    }

    /// `decodePage`: page `number` decoded in place, where it is one of those encoded.
    pub(super) fn decode(&self, number: i32, page: &mut [u8]) {
        if !Self::encodes(number) {
            return;
        }
        let mut key = self.key.clone();
        let at = self.number_at;
        for (byte, number) in key[at..at + 4].iter_mut().zip(number.to_le_bytes()) {
            *byte ^= number;
        }
        rc4(&key, page);
    }

    /// Whether page `number` is one the codec encodes.
    pub(super) fn encodes(number: i32) -> bool {
        (1..=MAX_ENCRYPTED_PAGE).contains(&number)
    }
}

/// `createPasswordDigest`: the digest, SHA-1 where the header's flags ask for it and MD5
/// otherwise, of the password upper-cased in UTF-16LE, cut or filled with zeros to 40 bytes,
/// cut to 16 bytes.
fn password_digest(root: &[u8], password: &str) -> Vec<u8> {
    let mut bytes = [0u8; PASSWORD_LENGTH];
    let encoded: Vec<u8> = to_upper_case(password)
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let kept = encoded.len().min(PASSWORD_LENGTH);
    bytes[..kept].copy_from_slice(&encoded[..kept]);
    let hash = if root[ENCRYPTION_FLAGS_OFFSET] & USE_SHA1 != 0 {
        Hash::Sha1
    } else {
        Hash::Md5
    };
    let mut digest = hash.digest(&[&bytes]);
    digest.truncate(PASSWORD_DIGEST_LENGTH);
    digest
}

/// `getOldDecryptionKey`: the salt's first four bytes hashed with every other byte of the
/// password region, unmasked by the header's date, its trailing part too, then with the engine
/// name.
fn old_key(root: &[u8]) -> [u8; JET_KEY_LENGTH] {
    let mut key: [u8; JET_KEY_LENGTH] = root[SALT_OFFSET..SALT_OFFSET + JET_KEY_LENGTH]
        .try_into()
        .expect("four bytes");
    let mut region = root[OFFSET_PASSWORD..OFFSET_PASSWORD + SIZE_PASSWORD * 2].to_vec();
    let mask = password_mask(root);
    for (at, byte) in region[..SIZE_PASSWORD].iter_mut().enumerate() {
        *byte ^= mask[at % mask.len()];
    }
    let trailing = region.len() - TRAILING_PASSWORD_LENGTH;
    for (at, byte) in region[trailing..].iter_mut().enumerate() {
        *byte ^= mask[at % mask.len()];
    }
    let hash_data: Vec<u8> = (0..SIZE_PASSWORD).map(|at| region[at * 2]).collect();
    hash_salt(&mut key, &hash_data);
    hash_salt(
        &mut key,
        &root[OFFSET_ENGINE_NAME..OFFSET_ENGINE_NAME + LENGTH_ENGINE_NAME],
    );
    key
}

/// `DatabaseImpl.getPasswordMask`: the integer part of the header's creation date, as Java casts
/// the double, in four little-endian bytes.
fn password_mask(root: &[u8]) -> [u8; 4] {
    let bits = u64::from_le_bytes(
        root[OFFSET_HEADER_DATE..OFFSET_HEADER_DATE + 8]
            .try_into()
            .expect("eight bytes"),
    );
    (f64::from_bits(bits) as i32).to_le_bytes()
}

/// `hashSalt`: the salt as a little-endian int, XORed with each byte of `data` shifted by its
/// position modulo 24.
fn hash_salt(salt: &mut [u8; JET_KEY_LENGTH], data: &[u8]) {
    let mut hash = i32::from_le_bytes(*salt);
    for (at, &byte) in data.iter().enumerate() {
        hash ^= i32::from(byte) << (at % 0x18);
    }
    *salt = hash.to_le_bytes();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(flags: u8) -> Vec<u8> {
        let mut root = vec![0u8; 4096];
        root[OFFSET_ENGINE_NAME..OFFSET_ENGINE_NAME + LENGTH_ENGINE_NAME]
            .copy_from_slice(b"MSISAM Database");
        root[SALT_OFFSET..SALT_OFFSET + SALT_LENGTH].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        root[ENCRYPTION_FLAGS_OFFSET] = flags;
        root
    }

    /// The new encryption checks the password by decrypting a known sequence, the salt's first
    /// four bytes, under the digest of the password and the salt.
    #[test]
    fn checks_the_password_of_the_new_encryption() {
        let mut root = header(NEW_ENCRYPTION | USE_SHA1);
        let digest = password_digest(&root, "money");
        let salt = root[SALT_OFFSET..SALT_OFFSET + SALT_LENGTH].to_vec();
        let mut check = salt[..BASE_SALT_LENGTH].to_vec();
        rc4(&[digest.as_slice(), &salt].concat(), &mut check);
        let at = CRYPT_CHECK_START + usize::from(root[SALT_OFFSET]);
        root[at..at + CRYPT_CHECK_LENGTH].copy_from_slice(&check);
        let opened = Msisam::open(&root, "Money").unwrap();
        assert_eq!(opened.key.len(), PASSWORD_DIGEST_LENGTH + BASE_SALT_LENGTH);
        assert!(matches!(
            Msisam::open(&root, "other"),
            Err(Thrown::Exception(exception)) if exception.message("db").is_some_and(|message| message.contains("Incorrect password"))
        ));
    }

    /// Only the pages 1 to 14 are encoded, each under the key its number is applied to.
    #[test]
    fn decodes_the_first_fourteen_pages_only() {
        let codec = Msisam::open(&header(NEW_ENCRYPTION), "").unwrap();
        let mut first = vec![7u8; 16];
        codec.decode(1, &mut first);
        assert_ne!(first, vec![7u8; 16]);
        let mut fifteenth = vec![7u8; 16];
        codec.decode(15, &mut fifteenth);
        assert_eq!(fifteenth, vec![7u8; 16]);
        assert!(Msisam::encodes(14) && !Msisam::encodes(15) && !Msisam::encodes(0));
    }

    /// The old encryption's key is the Jet key the salt makes, hashed with the password region
    /// and the engine name.
    #[test]
    fn makes_the_old_key_from_the_salt() {
        let root = header(0);
        let codec = Msisam::open(&root, "ignored").unwrap();
        let mut expected = [1, 2, 3, 4];
        hash_salt(&mut expected, &[0; SIZE_PASSWORD]);
        hash_salt(&mut expected, b"MSISAM Database");
        assert_eq!(codec.key, expected);
        assert_eq!(codec.number_at, 0);
    }
}
