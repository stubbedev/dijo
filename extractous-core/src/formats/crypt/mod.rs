//! The encryption of Office documents as POI's `poifs.crypt` reads and opens it
//! ([MS-OFFCRYPTO]): an `EncryptionInfo`, its verifier checked against a password, and the keys
//! that decrypt the document: the RC4 keys of CryptoAPI for the binary formats. The ciphers and
//! hashes are RustCrypto's.

use rc4::{KeyInit, Rc4, StreamCipher};
use sha1::{Digest, Sha1};
use sha2::{Sha256, Sha384, Sha512};

use crate::config::{ParserConfigs, Password};
use crate::errors::Error;
use crate::formats::Unported;

/// `Decryptor.DEFAULT_PASSWORD`, which Office encrypts a document with when it is only to be
/// write protected.
const DEFAULT_PASSWORD: &str = "VelvetSweatshop";
/// The most UTF-16 units of a password CryptoAPI hashes.
const MAX_PASSWORD_UNITS: usize = 255;

/// The password POI opens an encrypted document with: the one given, else the one Office
/// encrypts a document with that is only write protected (`Biff8EncryptionKey`'s current user
/// password, else `Decryptor.DEFAULT_PASSWORD`, as Tika's `OfficeParser` asks its
/// `PasswordProvider` for one).
pub(crate) fn password(configs: &ParserConfigs) -> &str {
    configs
        .password
        .as_ref()
        .map_or(DEFAULT_PASSWORD, Password::expose)
}

/// The error Tika's path fails with where a parser wraps the exception `cause` POI throws in a
/// Tika `EncryptedDocumentException`, as our Java reports it.
pub(crate) fn encrypted(cause: &str) -> Error {
    Error::Encrypted(format!(
        "Parse error occurred : org.apache.tika.exception.EncryptedDocumentException: Unable to \
         process: document is encrypted <- caused by {cause}"
    ))
}

/// The hash algorithms the encryptions use (`HashAlgorithm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    /// The hash of the parts, one after the other.
    fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut digest = D::new();
            for part in parts {
                digest.update(part);
            }
            digest.finalize().to_vec()
        }
        match self {
            Hash::Sha1 => run::<Sha1>(parts),
            Hash::Sha256 => run::<Sha256>(parts),
            Hash::Sha384 => run::<Sha384>(parts),
            Hash::Sha512 => run::<Sha512>(parts),
        }
    }

    /// `HashAlgorithm.fromEcmaId`, past the hash of no algorithm, which fails where it is used.
    fn from_ecma_id(id: i32) -> Result<Hash, Unopened> {
        match id {
            0x8004 => Ok(Hash::Sha1),
            0x800C => Ok(Hash::Sha256),
            0x800D => Ok(Hash::Sha384),
            0x800E => Ok(Hash::Sha512),
            _ => Err(Unopened::Thrown(format!("hash algorithm {id} not found"))),
        }
    }
}

/// Why a document's encryption is not opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unopened {
    /// POI throws reading the encryption, with what it throws.
    Thrown(String),
    /// An encryption this port does not open, which Tika's path opens.
    Unported(String),
}

impl Unopened {
    pub(crate) fn unported(self) -> Unported {
        match self {
            Unopened::Thrown(reason) => Unported(format!(
                "POI throws reading the document's encryption, which fails Tika's parse: {reason}"
            )),
            Unopened::Unported(reason) => Unported(reason),
        }
    }
}

/// `StringUtil.getToUnicodeLE` of a password as Java holds it, cut to `MAX_PASSWORD_UNITS`
/// UTF-16 units.
fn password_bytes(password: &str) -> Vec<u8> {
    password
        .encode_utf16()
        .take(MAX_PASSWORD_UNITS)
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// `getBlock0`: `bytes` cut or filled with zeros to `size`.
fn block_of(bytes: &[u8], size: usize) -> Vec<u8> {
    let mut out = vec![0; size];
    let length = size.min(bytes.len());
    out[..length].copy_from_slice(&bytes[..length]);
    out
}

/// A cursor over the bytes of an `EncryptionInfo`, failing as `LittleEndianInput` does.
struct Input<'a> {
    data: &'a [u8],
    at: usize,
}

impl Input<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], Unopened> {
        let end = self
            .at
            .checked_add(length)
            .filter(|&end| end <= self.data.len());
        let Some(end) = end else {
            return Err(Unopened::Thrown(
                "the encryption info ends before its fields".to_owned(),
            ));
        };
        let taken = &self.data[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u16(&mut self) -> Result<u16, Unopened> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn i32(&mut self) -> Result<i32, Unopened> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Unopened> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }
}

/// The encryption `EncryptionInfo` reads.
#[derive(Debug, Clone)]
pub(crate) enum EncryptionInfo {
    CryptoApi(CryptoApi),
}

impl EncryptionInfo {
    /// `new EncryptionInfo(input, EncryptionMode.cryptoAPI)`, as the binary formats read it.
    pub(crate) fn read(data: &[u8]) -> Result<EncryptionInfo, Unopened> {
        let mut input = Input { data, at: 0 };
        let major = input.u16()?;
        let minor = input.u16()?;
        match (major, minor) {
            (1, 1) => Err(Unopened::Unported(
                "Office 97's binary RC4 encryption, which this port does not decrypt".to_owned(),
            )),
            (2..=4, 2) => {
                input.i32()?;
                input.i32()?;
                let header = Header::read(&mut input)?;
                let verifier = Verifier::read(&mut input, &header)?;
                Ok(EncryptionInfo::CryptoApi(CryptoApi::new(header, verifier)?))
            }
            (4, 4) => Err(Unopened::Unported(
                "agile encryption, which this port does not decrypt".to_owned(),
            )),
            _ => Err(Unopened::Thrown(format!(
                "Unknown encryption: version major: {major} / version minor: {minor}"
            ))),
        }
    }
}

/// The cipher algorithms `CipherAlgorithm` names by their ECMA ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cipher {
    Rc4,
    Aes128,
    Aes192,
    Aes256,
}

impl Cipher {
    fn from_ecma_id(id: i32) -> Result<Cipher, Unopened> {
        match id {
            0x6801 => Ok(Cipher::Rc4),
            0x660E => Ok(Cipher::Aes128),
            0x660F => Ok(Cipher::Aes192),
            0x6610 => Ok(Cipher::Aes256),
            _ => Err(Unopened::Thrown(format!("cipher algorithm {id} not found"))),
        }
    }

    /// `encryptedVerifierHashLength`.
    fn verifier_hash_length(self) -> usize {
        match self {
            Cipher::Rc4 => 20,
            Cipher::Aes128 | Cipher::Aes192 | Cipher::Aes256 => 32,
        }
    }

    /// `allowedKeySize`.
    fn allows(self, key_bits: i32) -> bool {
        match self {
            Cipher::Rc4 => (0x28..=0x80).contains(&key_bits) && key_bits % 8 == 0,
            Cipher::Aes128 => key_bits == 128,
            Cipher::Aes192 => key_bits == 192,
            Cipher::Aes256 => key_bits == 256,
        }
    }
}

/// `CipherProvider.fromEcmaId`: RC4's or AES's.
fn cipher_provider(id: i32) -> Result<(), Unopened> {
    match id {
        1 | 0x18 => Ok(()),
        _ => Err(Unopened::Thrown("cipher provider not found".to_owned())),
    }
}

/// `StandardEncryptionHeader`: what CryptoAPI and standard encryption read of their header.
#[derive(Debug, Clone)]
struct Header {
    cipher: Cipher,
    hash: Hash,
    key_bits: i32,
}

impl Header {
    fn read(input: &mut Input) -> Result<Header, Unopened> {
        input.i32()?;
        input.i32()?;
        let cipher = Cipher::from_ecma_id(input.i32()?)?;
        let hash = Hash::from_ecma_id(input.i32()?)?;
        let mut key_bits = input.i32()?;
        if key_bits == 0 {
            key_bits = 0x28;
        }
        if !cipher.allows(key_bits) {
            return Err(Unopened::Thrown(format!(
                "KeySize {key_bits} not allowed for cipher {cipher:?}"
            )));
        }
        cipher_provider(input.i32()?)?;
        input.take(8)?;
        let check_for_salt = Input {
            data: input.data,
            at: input.at,
        }
        .i32()?;
        if check_for_salt != 16 {
            while input.u16()? != 0 {}
        }
        Ok(Header {
            cipher,
            hash,
            key_bits,
        })
    }
}

/// `StandardEncryptionVerifier`: the salt and the encrypted verifier and its hash.
#[derive(Debug, Clone)]
struct Verifier {
    salt: [u8; 16],
    verifier: [u8; 16],
    verifier_hash: Vec<u8>,
}

impl Verifier {
    fn read(input: &mut Input, header: &Header) -> Result<Verifier, Unopened> {
        let salt_size = input.i32()?;
        if salt_size != 16 {
            return Err(Unopened::Thrown(format!("Salt size != 16: {salt_size}")));
        }
        let salt = input.array()?;
        let verifier = input.array()?;
        input.i32()?;
        let verifier_hash = input.take(header.cipher.verifier_hash_length())?.to_vec();
        Ok(Verifier {
            salt,
            verifier,
            verifier_hash,
        })
    }
}

/// An RC4 key of a block of a document, which a stream of the block's bytes is decrypted with
/// from its start.
#[derive(Debug, Clone)]
pub(crate) struct Rc4Key(Vec<u8>);

impl Rc4Key {
    /// The cipher of the key, at the start of its stream.
    pub(crate) fn cipher(&self) -> Rc4 {
        Rc4::new_from_slice(&self.0).expect("an RC4 key of 5 to 16 bytes")
    }

    /// `data` decrypted in place with the key's stream from its start.
    pub(crate) fn decrypt(&self, data: &mut [u8]) {
        self.cipher().apply_keystream(data);
    }
}

/// What decrypts the blocks of a binary document: a block's RC4 key.
pub(crate) trait BlockKeys {
    fn block(&self, block: u32) -> Rc4Key;
}

/// CryptoAPI's RC4 encryption (`CryptoAPIEncryptionHeader` and its verifier).
#[derive(Debug, Clone)]
pub(crate) struct CryptoApi {
    hash: Hash,
    key_bits: i32,
    verifier: Verifier,
}

impl CryptoApi {
    fn new(header: Header, verifier: Verifier) -> Result<CryptoApi, Unopened> {
        if header.cipher != Cipher::Rc4 {
            return Err(Unopened::Unported(
                "CryptoAPI encryption with a cipher other than RC4, which this port does not open"
                    .to_owned(),
            ));
        }
        Ok(CryptoApi {
            hash: header.hash,
            key_bits: header.key_bits,
            verifier,
        })
    }

    /// `CryptoAPIDecryptor.verifyPassword`: the secret key, where the password opens it.
    pub(crate) fn verify(&self, password: &str) -> Option<CryptoApiKey> {
        let secret = self
            .hash
            .digest(&[&self.verifier.salt, &password_bytes(password)]);
        let key = CryptoApiKey {
            secret,
            hash: self.hash,
            key_bits: self.key_bits,
        };
        let mut cipher = key.block(0).cipher();
        let mut verifier = self.verifier.verifier;
        cipher.apply_keystream(&mut verifier);
        let mut verifier_hash = self.verifier.verifier_hash.clone();
        cipher.apply_keystream(&mut verifier_hash);
        (self.hash.digest(&[&verifier]) == verifier_hash).then_some(key)
    }
}

/// The secret key of CryptoAPI.
#[derive(Debug, Clone)]
pub(crate) struct CryptoApiKey {
    secret: Vec<u8>,
    hash: Hash,
    key_bits: i32,
}

#[cfg(test)]
impl CryptoApiKey {
    /// A 128-bit key of SHA-1 from `secret`, for the tests of what it decrypts.
    pub(crate) fn of_secret(secret: &[u8]) -> CryptoApiKey {
        CryptoApiKey {
            secret: secret.to_vec(),
            hash: Hash::Sha1,
            key_bits: 128,
        }
    }
}

impl BlockKeys for CryptoApiKey {
    /// `initCipherForBlock`: the hash of the secret key and the block's number, cut to the key
    /// size, a 40-bit key filled with zeros to 128 bits.
    fn block(&self, block: u32) -> Rc4Key {
        let hashed = self.hash.digest(&[&self.secret, &block.to_le_bytes()]);
        let mut key = block_of(&hashed, self.key_bits as usize / 8);
        if self.key_bits == 40 {
            key = block_of(&key, 16);
        }
        Rc4Key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CryptoAPI's encryption of a verifier with `password`, as Office writes it.
    fn crypto_api(password: &str, key_bits: i32) -> CryptoApi {
        let salt = [9; 16];
        let secret = Hash::Sha1.digest(&[&salt, &password_bytes(password)]);
        let key = CryptoApiKey {
            secret,
            hash: Hash::Sha1,
            key_bits,
        };
        let mut encrypted = [4u8; 16].to_vec();
        encrypted.extend(Hash::Sha1.digest(&[&[4u8; 16]]));
        key.block(0).decrypt(&mut encrypted);
        CryptoApi {
            hash: Hash::Sha1,
            key_bits,
            verifier: Verifier {
                salt,
                verifier: encrypted[..16].try_into().unwrap(),
                verifier_hash: encrypted[16..].to_vec(),
            },
        }
    }

    #[test]
    fn opens_crypto_api_with_the_right_password_only() {
        let api = crypto_api("pw", 128);
        assert!(api.verify("pw").is_some());
        assert!(api.verify("other").is_none());
        assert!(crypto_api(DEFAULT_PASSWORD, 40)
            .verify(DEFAULT_PASSWORD)
            .is_some());
    }

    #[test]
    fn pads_a_40_bit_key_to_128_bits() {
        let key = CryptoApiKey {
            secret: vec![1; 20],
            hash: Hash::Sha1,
            key_bits: 40,
        };
        assert_eq!(key.block(3).0.len(), 16);
        assert_eq!(key.block(3).0[5..], [0; 11]);
    }

    #[test]
    fn hashes_at_most_255_units_of_a_password() {
        assert_eq!(password_bytes(&"a".repeat(300)).len(), 510);
    }

    #[test]
    fn reads_what_poi_reads_of_an_encryption_info() {
        assert!(matches!(
            EncryptionInfo::read(&[1, 0, 1, 0]),
            Err(Unopened::Unported(_))
        ));
        assert!(matches!(
            EncryptionInfo::read(&[9, 0, 9, 0]),
            Err(Unopened::Thrown(reason)) if reason.contains("Unknown encryption")
        ));
        assert!(matches!(
            EncryptionInfo::read(&[2, 0, 2, 0, 0]),
            Err(Unopened::Thrown(_))
        ));
    }
}
