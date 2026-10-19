//! The Office encryption of an ACE database as `jackcess-encrypt`'s `OfficeCryptCodecHandler`
//! opens it: the encryption provider the header page describes, its verifier checked against
//! the password, and the key each later page is decrypted with, made of the provider's key and
//! the database's encoding key XORed with the page's number. Agile encryption, standard
//! encryption, CryptoAPI's RC4 and Office 97's binary RC4 are ported, agile encryption with each
//! of its hashes and with AES, RC2, DES and triple DES in CBC, ECB or CFB mode, on RustCrypto's
//! ciphers; agile encryption with RC4, or in CCM or GCM mode, which BouncyCastle adapts
//! otherwise, is left to Tika.
//!
//! Jackcess checks a password against standard encryption with a key derived from the number of
//! the last of its 50,000 hashing rounds, where [MS-OFFCRYPTO] derives it from block 0: its
//! iterations overwrite the buffer that holds the block's number. It refuses the right password
//! of a database Office encrypted, which the port opens, as a reading that differs from Tika's.

use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt};
use aes::{Aes128, Aes192, Aes256};
use des::{Des, TdesEde2, TdesEde3};
use rc2::Rc2;
use rc4::{KeyInit, Rc4, StreamCipher};

use super::buffer::{Buffer, Thrown, ILLEGAL_ARGUMENT};
use crate::formats::crypt::Hash;
use crate::formats::xml::{attribute_named, parse_sax, Attribute, SaxHandler, StartTag};
use crate::formats::{Difference, Failure};

/// Where the header page describes the encryption.
const CRYPT_STRUCTURE_OFFSET: i64 = 0x299;
/// The most UTF-16 units of a password Jackcess hashes.
const MAX_PASSWORD_LENGTH: usize = 255;
const INVALID_CREDENTIALS: &str =
    "com.healthmarketscience.jackcess.crypt.InvalidCredentialsException";
const INVALID_CONFIGURATION: &str =
    "com.healthmarketscience.jackcess.crypt.InvalidCryptoConfigurationException";
const UNSUPPORTED_CODEC: &str = "com.healthmarketscience.jackcess.impl.UnsupportedCodecException";
const DATA_LENGTH: &str = "org.bouncycastle.crypto.DataLengthException";
const ARITHMETIC: &str = "java.lang.ArithmeticException";
const NEGATIVE_ARRAY_SIZE: &str = "java.lang.NegativeArraySizeException";
/// `EncryptionHeader`'s flags.
const CRYPTO_API_FLAG: i32 = 0x04;
const EXTERNAL_FLAG: i32 = 0x10;
const AES_FLAG: i32 = 0x20;
/// `EncryptionHeader`'s algorithm ids.
const ALGORITHM_FLAGS: i32 = 0;
const ALGORITHM_RC4: i32 = 0x6801;
const ALGORITHM_AES_128: i32 = 0x660E;
const ALGORITHM_AES_192: i32 = 0x660F;
const ALGORITHM_AES_256: i32 = 0x6610;
const HASH_FLAGS: i32 = 0;
const HASH_SHA1: i32 = 0x8004;
/// `EncryptionVerifier`'s sizes.
const SALT_SIZE: i32 = 16;
const VERIFIER_SIZE: i64 = 16;
/// `ECMAStandardEncryptionProvider.HASH_ITERATIONS`; `NonStandardEncryptionProvider` hashes
/// none.
const STANDARD_ITERATIONS: u32 = 50_000;
/// The reserved value before an agile descriptor.
const AGILE_RESERVED: i32 = 0x40;
/// The block keys of the agile derivations.
const VERIFIER_INPUT_BLOCK: [u8; 8] = [0xfe, 0xa7, 0xd2, 0x76, 0x3b, 0x4b, 0x9e, 0x79];
const VERIFIER_VALUE_BLOCK: [u8; 8] = [0xd7, 0xaa, 0x0f, 0x6d, 0x30, 0x61, 0x34, 0x4e];
const KEY_VALUE_BLOCK: [u8; 8] = [0x14, 0x6e, 0x0b, 0xe7, 0xab, 0xac, 0xd0, 0xd6];
/// The most hashing rounds the port spends on a password; a descriptor asking for more keeps
/// Jackcess hashing, and is left to Tika.
const MAX_SPIN_COUNT: i64 = 10_000_000;
const ENCRYPTION_NAMESPACE: &str = "http://schemas.microsoft.com/office/2006/encryption";
const PASSWORD_NAMESPACE: &str = "http://schemas.microsoft.com/office/2006/keyEncryptor/password";
const CERTIFICATE_NAMESPACE: &str =
    "http://schemas.microsoft.com/office/2006/keyEncryptor/certificate";

fn invalid_configuration(message: impl Into<String>) -> Thrown {
    Thrown::illegal_state_of(INVALID_CONFIGURATION, message)
}

fn unsupported_codec(message: impl Into<String>) -> Thrown {
    Thrown::runtime(UNSUPPORTED_CODEC, message)
}

/// `getPasswordBytes`: the password's first 255 UTF-16 units in UTF-16LE, a high surrogate cut
/// from its low one encoded as the replacement Java's encoder writes for it.
fn password_bytes(password: &str) -> Vec<u8> {
    password
        .encode_utf16()
        .take(MAX_PASSWORD_LENGTH)
        .collect::<Vec<u16>>()
        .iter()
        .enumerate()
        .flat_map(|(at, &unit)| {
            let cut = (0xD800..0xDC00).contains(&unit) && at + 1 == MAX_PASSWORD_LENGTH;
            if cut { 0xFFFD } else { unit }.to_le_bytes()
        })
        .collect()
}

/// `int2bytes`: an int as four little-endian bytes.
fn int_bytes(value: i32) -> [u8; 4] {
    value.to_le_bytes()
}

/// `fixToLength`: `bytes` cut to `length`, or filled to it with `fill`.
fn fixed(bytes: &[u8], length: i64, fill: u8) -> Result<Vec<u8>, Thrown> {
    let length = usize::try_from(length)
        .map_err(|_| Thrown::runtime(NEGATIVE_ARRAY_SIZE, length.to_string()))?;
    let mut out = vec![fill; length];
    let kept = length.min(bytes.len());
    out[..kept].copy_from_slice(&bytes[..kept]);
    Ok(out)
}

/// `iterateHash`: the hash hashed again `iterations` times, each time after the iteration's
/// number.
fn iterate(hash: Hash, base: Vec<u8>, iterations: u32) -> Vec<u8> {
    (0..iterations).fold(base, |hashed, iteration| {
        hash.digest(&[&iteration.to_le_bytes(), &hashed])
    })
}

/// The block ciphers of agile encryption (`XmlEncryptionDescriptor.CipherAlgorithm`) this port
/// decrypts with: all but RC4, which BouncyCastle wraps as a block cipher of its own. Both
/// `_3DES` and `_3DES112` are BouncyCastle's `DESedeEngine`, which takes a key of two or three
/// DES keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cipher {
    Aes,
    Rc2,
    Des,
    TripleDes,
}

/// A block cipher keyed, as its BouncyCastle engine is initialised with the key.
enum BlockKey {
    Aes128(Aes128),
    Aes192(Aes192),
    Aes256(Aes256),
    Rc2(Rc2),
    Des(Des),
    TripleDes(TdesEde3),
    TwoKeyTripleDes(TdesEde2),
}

impl Cipher {
    /// The cipher keyed with `key`, or what BouncyCastle's engine throws on a key of its length.
    fn key(self, key: &[u8]) -> Result<BlockKey, Thrown> {
        let wrong = |message: &str| Thrown::runtime(ILLEGAL_ARGUMENT, message);
        Ok(match (self, key.len()) {
            (Cipher::Aes, 16) => BlockKey::Aes128(Aes128::new_from_slice(key).expect("16")),
            (Cipher::Aes, 24) => BlockKey::Aes192(Aes192::new_from_slice(key).expect("24")),
            (Cipher::Aes, 32) => BlockKey::Aes256(Aes256::new_from_slice(key).expect("32")),
            (Cipher::Aes, _) => return Err(wrong("Key length not 128/192/256 bits.")),
            (Cipher::Rc2, 1..=128) => BlockKey::Rc2(Rc2::new_with_eff_key_len(key, key.len() * 8)),
            (Cipher::Rc2, _) => return Err(wrong("RC2 key length must be 1 to 128 bytes")),
            (Cipher::Des, 8) => BlockKey::Des(Des::new_from_slice(key).expect("8")),
            (Cipher::Des, _) => return Err(wrong("DES key too long - should be 8 bytes")),
            (Cipher::TripleDes, 24) => {
                BlockKey::TripleDes(TdesEde3::new_from_slice(key).expect("24"))
            }
            (Cipher::TripleDes, 16) => {
                BlockKey::TwoKeyTripleDes(TdesEde2::new_from_slice(key).expect("16"))
            }
            (Cipher::TripleDes, _) => return Err(wrong("key size must be 16 or 24 bytes.")),
        })
    }
}

/// Runs `$apply` with the cipher of a [`BlockKey`], whatever its type.
macro_rules! with_cipher {
    ($key:expr, $cipher:ident => $apply:expr) => {
        match $key {
            BlockKey::Aes128($cipher) => $apply,
            BlockKey::Aes192($cipher) => $apply,
            BlockKey::Aes256($cipher) => $apply,
            BlockKey::Rc2($cipher) => $apply,
            BlockKey::Des($cipher) => $apply,
            BlockKey::TripleDes($cipher) => $apply,
            BlockKey::TwoKeyTripleDes($cipher) => $apply,
        }
    };
}

/// One block of `cipher`, decrypted in place.
fn decrypt_block<C: BlockCipherDecrypt>(cipher: &C, block: &mut [u8]) {
    cipher.decrypt_block(block.try_into().expect("a block of the cipher's size"));
}

/// One block of `cipher`, encrypted in place.
fn encrypt_block<C: BlockCipherEncrypt>(cipher: &C, block: &mut [u8]) {
    cipher.encrypt_block(block.try_into().expect("a block of the cipher's size"));
}

impl BlockKey {
    fn block_size(&self) -> usize {
        match self {
            BlockKey::Aes128(_) | BlockKey::Aes192(_) | BlockKey::Aes256(_) => 16,
            _ => 8,
        }
    }

    fn decrypt_block(&self, block: &mut [u8]) {
        with_cipher!(self, cipher => decrypt_block(cipher, block));
    }

    fn encrypt_block(&self, block: &mut [u8]) {
        with_cipher!(self, cipher => encrypt_block(cipher, block));
    }
}

/// The chaining of an agile cipher (`XmlEncryptionDescriptor.CipherChaining`) this port
/// decrypts with: all but CCM and GCM, which BouncyCastle adapts from AEAD ciphers. CFB is
/// BouncyCastle's `CFBBlockCipher` of 8 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chaining {
    Cbc,
    Ecb,
    Cfb8,
}

/// `decryptBytes` of `data` with `key` in `chaining` mode, its IV `iv`: in CBC mode one of the
/// cipher's block size, as BouncyCastle requires; in CFB mode as much of `iv` as fills a block,
/// after zeros where it is shorter. ECB and CBC decrypt whole blocks only, as `doFinal` throws
/// on a part of one.
fn decrypt(key: &BlockKey, chaining: Chaining, iv: &[u8], data: &[u8]) -> Result<Vec<u8>, Thrown> {
    let size = key.block_size();
    let mut out = data.to_vec();
    if chaining != Chaining::Cfb8 && !data.len().is_multiple_of(size) {
        return Err(Thrown::runtime(DATA_LENGTH, "data not block size aligned"));
    }
    match chaining {
        Chaining::Ecb => {
            for block in out.chunks_exact_mut(size) {
                key.decrypt_block(block);
            }
        }
        Chaining::Cbc => {
            if iv.len() != size {
                return Err(Thrown::runtime(
                    ILLEGAL_ARGUMENT,
                    "initialisation vector must be the same length as block size",
                ));
            }
            let mut previous = iv;
            for (block, encrypted) in out.chunks_exact_mut(size).zip(data.chunks_exact(size)) {
                key.decrypt_block(block);
                for (byte, chained) in block.iter_mut().zip(previous) {
                    *byte ^= chained;
                }
                previous = encrypted;
            }
        }
        Chaining::Cfb8 => {
            let mut register = vec![0; size];
            let kept = iv.len().min(size);
            register[size - kept..].copy_from_slice(&iv[..kept]);
            let mut stream = vec![0; size];
            for byte in &mut out {
                stream.copy_from_slice(&register);
                key.encrypt_block(&mut stream);
                let encrypted = *byte;
                *byte ^= stream[0];
                register.rotate_left(1);
                register[size - 1] = encrypted;
            }
        }
    }
    Ok(out)
}

/// RC4 with `key` applied to `parts`, one stream after the other.
fn rc4(key: &[u8], parts: &mut [&mut [u8]]) {
    let mut cipher = Rc4::new_from_slice(key).expect("an RC4 key of 5 to 256 bytes");
    for part in parts {
        cipher.apply_keystream(part);
    }
}

/// What decrypts the pages after the first of a database under Office encryption.
pub(super) struct Decryption {
    /// The database's encoding key, which each page's number is XORed into.
    encoding_key: [u8; 4],
    provider: Provider,
    /// Why Jackcess refuses the password that opens the encryption, if it does.
    pub(super) refused: Option<Difference>,
}

/// What checking a password against an encryption's verifier finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    Opens,
    Refuses,
    /// It opens it, but Jackcess checks it otherwise, and refuses it.
    OpensWhereJackcessRefuses,
}

impl From<bool> for Check {
    fn from(opens: bool) -> Self {
        if opens {
            Check::Opens
        } else {
            Check::Refuses
        }
    }
}

enum Provider {
    /// Office 97's binary RC4: the MD5 of the base hash and the block, as an RC4 key of 16 bytes.
    BinaryRc4 { base: Vec<u8> },
    /// CryptoAPI's RC4: the SHA-1 of the base hash and the block, cut to the key size.
    CryptoApi { base: Vec<u8>, key_bits: i32 },
    /// Standard encryption: AES in ECB mode, its key derived from the iterated base hash and
    /// the block.
    Standard { iterated: Vec<u8>, key_bytes: i64 },
    /// Agile encryption: the key data's cipher, keyed by the key the password opens, its IV
    /// the key data's hash of its salt and the block.
    Agile {
        key: Box<BlockKey>,
        chaining: Chaining,
        hash: Hash,
        salt: Vec<u8>,
        block_size: i64,
    },
}

impl Decryption {
    /// `OfficeCryptCodecHandler.create`: the encryption the header page `root` describes,
    /// opened with `password`, where the database's encoding key is not blank.
    pub(super) fn open(root: &[u8], encoding_key: [u8; 4], password: &str) -> Result<Self, Thrown> {
        let mut header = Buffer::new(root);
        let info_length = header.i16_at(CRYPT_STRUCTURE_OFFSET)?;
        header.set_position(CRYPT_STRUCTURE_OFFSET + 2)?;
        let info = header.take(i64::from(info_length))?;
        let mut info = Buffer::new(info);
        let major = info.i16()? as u16;
        let minor = info.i16()? as u16;
        let password = password_bytes(password);
        let (provider, checked) = match (major, minor) {
            (4, 4) => Agile::read(&mut info)?.open(&password)?,
            (1, 1) => binary_rc4(&mut info, &password)?,
            (3 | 4, 3) => {
                return Err(unsupported_codec(
                    "Extensible encryption provider is not supported",
                ))
            }
            (2..=4, 2) => {
                let flags = info.i32()?;
                if flags & CRYPTO_API_FLAG == 0 {
                    return Err(unsupported_provider(major, minor));
                }
                if flags & AES_FLAG != 0 {
                    standard(&mut info, &password, STANDARD_ITERATIONS)?
                } else {
                    let start = info.position() as i64;
                    match crypto_api(&mut info, &password) {
                        Err(Thrown::Exception(exception))
                            if exception.class() == INVALID_CONFIGURATION =>
                        {
                            info.set_position(start)?;
                            standard(&mut info, &password, 0)
                                .map_err(|_| Thrown::Exception(exception))?
                        }
                        opened => opened?,
                    }
                }
            }
            _ => return Err(unsupported_provider(major, minor)),
        };
        let refused = match checked {
            Check::Opens => None,
            Check::OpensWhereJackcessRefuses => Some(Difference::new(
                "a database under standard encryption, whose password Jackcess checks with a key \
                 of the number of its last hashing round, not of block 0, which refuses the \
                 right one",
            )),
            Check::Refuses => {
                return Err(Thrown::illegal_state_of(
                    INVALID_CREDENTIALS,
                    "Incorrect password provided",
                ))
            }
        };
        Ok(Decryption {
            encoding_key,
            provider,
            refused,
        })
    }

    /// `decodePage`: page `number`, after the first, decrypted.
    pub(super) fn decrypt(&self, number: i32, page: &[u8]) -> Result<Vec<u8>, Thrown> {
        let mut block = int_bytes(number);
        for (byte, key) in block.iter_mut().zip(self.encoding_key) {
            *byte ^= key;
        }
        match &self.provider {
            Provider::BinaryRc4 { base } => {
                let mut page = page.to_vec();
                rc4(&binary_rc4_key(base, &block)?, &mut [&mut page]);
                Ok(page)
            }
            Provider::CryptoApi { base, key_bits } => {
                let mut page = page.to_vec();
                rc4(&crypto_api_key(base, *key_bits, &block)?, &mut [&mut page]);
                Ok(page)
            }
            Provider::Standard {
                iterated,
                key_bytes,
            } => {
                let key = Cipher::Aes.key(&standard_key(iterated, &block, *key_bytes)?)?;
                decrypt(&key, Chaining::Ecb, &[], page)
            }
            Provider::Agile {
                key,
                chaining,
                hash,
                salt,
                block_size,
            } => {
                let iv = fixed(&hash.digest(&[salt, &block]), *block_size, 0x36)?;
                decrypt(key, *chaining, &iv, page)
            }
        }
    }
}

fn unsupported_provider(major: u16, minor: u16) -> Thrown {
    unsupported_codec(format!(
        "Unsupported office encryption provider: vMajor {major}, vMinor {minor}"
    ))
}

/// `OfficeBinaryDocRC4Provider`: the provider and whether the password opens it.
fn binary_rc4(info: &mut Buffer<'_>, password: &[u8]) -> Result<(Provider, Check), Thrown> {
    let salt = info.take(16)?;
    let mut verifier = info.take(16)?.to_vec();
    let mut verifier_hash = info.take(16)?.to_vec();
    let mut filled = fixed(&Hash::Md5.digest(&[password]), 5, 0)?;
    filled.extend_from_slice(salt);
    let repeated: Vec<u8> = filled.iter().copied().cycle().take(336).collect();
    let base = fixed(&Hash::Md5.digest(&[&repeated]), 5, 0)?;
    rc4(
        &binary_rc4_key(&base, &int_bytes(0))?,
        &mut [&mut verifier, &mut verifier_hash],
    );
    let verified = Hash::Md5.digest(&[&verifier]) == verifier_hash;
    Ok((Provider::BinaryRc4 { base }, verified.into()))
}

fn binary_rc4_key(base: &[u8], block: &[u8]) -> Result<Vec<u8>, Thrown> {
    fixed(&Hash::Md5.digest(&[base, block]), 16, 0)
}

/// What `EncryptionHeader.read` and the `EncryptionVerifier` after it read of CryptoAPI's or
/// standard encryption.
struct Header {
    key_bits: i32,
    salt: Vec<u8>,
    verifier: Vec<u8>,
    verifier_hash_size: i32,
    verifier_hash: Vec<u8>,
}

/// `EncryptionHeader.CryptoAlgorithm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Algorithm {
    External,
    Rc4,
    Aes128,
    Aes192,
    Aes256,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Algorithm::External => "EXTERNAL",
            Algorithm::Rc4 => "RC4",
            Algorithm::Aes128 => "AES_128",
            Algorithm::Aes192 => "AES_192",
            Algorithm::Aes256 => "AES_256",
        }
    }

    /// The smallest and largest key size, and the length of the encrypted verifier hash.
    fn sizes(self) -> (i32, i32, i64) {
        match self {
            Algorithm::External => (0, 0, 0),
            Algorithm::Rc4 => (0x28, 0x200, 20),
            Algorithm::Aes128 => (0x80, 0x80, 32),
            Algorithm::Aes192 => (0xC0, 0xC0, 32),
            Algorithm::Aes256 => (0x100, 0x100, 32),
        }
    }

    fn is_aes(self) -> bool {
        matches!(
            self,
            Algorithm::Aes128 | Algorithm::Aes192 | Algorithm::Aes256
        )
    }
}

impl Header {
    /// `EncryptionHeader.read` of an encryption whose algorithm `valid` accepts, with SHA-1,
    /// and the verifier after it.
    fn read(
        info: &mut Buffer<'_>,
        valid: fn(Algorithm) -> bool,
        valid_names: &str,
    ) -> Result<Self, Thrown> {
        let length = info.i32()?;
        let start = info.position() as i64;
        let limit = info.limit() as i64;
        info.set_limit(start + i64::from(length))?;
        let header = Self::read_fields(info, valid, valid_names);
        info.set_limit(limit)?;
        let (algorithm, key_bits) = header?;
        info.set_position(start + i64::from(length))?;
        let salt_size = info.i32()?;
        if salt_size != SALT_SIZE {
            return Err(invalid_configuration(format!(
                "salt size {salt_size} must be {SALT_SIZE}"
            )));
        }
        let salt = info.take(i64::from(salt_size))?.to_vec();
        let verifier = info.take(VERIFIER_SIZE)?.to_vec();
        let verifier_hash_size = info.i32()?;
        let verifier_hash = info.take(algorithm.sizes().2)?.to_vec();
        Ok(Header {
            key_bits,
            salt,
            verifier,
            verifier_hash_size,
            verifier_hash,
        })
    }

    /// The `EncryptionHeader` constructor and the checks of `read`: the algorithm and the key
    /// size.
    fn read_fields(
        info: &mut Buffer<'_>,
        valid: fn(Algorithm) -> bool,
        valid_names: &str,
    ) -> Result<(Algorithm, i32), Thrown> {
        let flags = info.i32()?;
        info.i32()?;
        let algorithm_id = info.i32()?;
        let hash_id = info.i32()?;
        let key_size = info.i32()?;
        info.i32()?;
        let algorithm = match algorithm_id {
            ALGORITHM_FLAGS if flags & EXTERNAL_FLAG != 0 => Algorithm::External,
            ALGORITHM_FLAGS if flags & CRYPTO_API_FLAG != 0 && flags & AES_FLAG != 0 => {
                Algorithm::Aes128
            }
            ALGORITHM_FLAGS if flags & CRYPTO_API_FLAG != 0 => Algorithm::Rc4,
            ALGORITHM_RC4 => Algorithm::Rc4,
            ALGORITHM_AES_128 => Algorithm::Aes128,
            ALGORITHM_AES_192 => Algorithm::Aes192,
            ALGORITHM_AES_256 => Algorithm::Aes256,
            _ => {
                return Err(unsupported_codec(format!(
                    "Unsupported encryption algorithm {algorithm_id} (flags {flags})"
                )))
            }
        };
        let external_hash = match hash_id {
            HASH_FLAGS => flags & EXTERNAL_FLAG != 0,
            HASH_SHA1 => false,
            _ => {
                return Err(unsupported_codec(format!(
                    "Unsupported hash algorithm {hash_id} (flags {flags})"
                )))
            }
        };
        info.i32()?;
        info.i32()?;
        let csp_name = csp_name(info)?;
        let key_bits = match key_size {
            0 if algorithm == Algorithm::Rc4 => {
                let name = crate::java::trim(&csp_name).to_lowercase();
                if name.is_empty() || name.contains(" base ") {
                    0x28
                } else {
                    0x80
                }
            }
            0 => algorithm.sizes().0,
            key_size => key_size,
        };
        let header = || format!("{}/{key_bits}", algorithm.name());
        if !valid(algorithm) {
            return Err(invalid_configuration(format!(
                "{} crypto algorithm must be one of [{valid_names}]",
                header()
            )));
        }
        if external_hash {
            return Err(invalid_configuration(format!(
                "{} hash algorithm must be one of [SHA1]",
                header()
            )));
        }
        let (smallest, largest, _) = algorithm.sizes();
        if !(smallest..=largest).contains(&key_bits) {
            return Err(invalid_configuration(format!(
                "{} key size is outside allowable range",
                header()
            )));
        }
        if key_bits % 8 != 0 {
            return Err(invalid_configuration(format!(
                "{} key size must be multiple of 8",
                header()
            )));
        }
        Ok((algorithm, key_bits))
    }
}

/// `readCspName`: the rest of the header in UTF-16LE, up to a NUL.
fn csp_name(info: &mut Buffer<'_>) -> Result<String, Thrown> {
    let length = info.remaining() / 2 * 2;
    let bytes = info.take(length as i64)?;
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|&unit| unit != 0)
        .collect();
    Ok(String::from_utf16_lossy(&units))
}

/// `RC4CryptoAPIProvider`: the provider and whether the password opens it.
fn crypto_api(info: &mut Buffer<'_>, password: &[u8]) -> Result<(Provider, Check), Thrown> {
    let header = Header::read(info, |algorithm| algorithm == Algorithm::Rc4, "RC4")?;
    let base = Hash::Sha1.digest(&[&header.salt, password]);
    let mut verifier = header.verifier.clone();
    let mut verifier_hash = header.verifier_hash.clone();
    rc4(
        &crypto_api_key(&base, header.key_bits, &int_bytes(0))?,
        &mut [&mut verifier, &mut verifier_hash],
    );
    let size = i64::from(header.verifier_hash_size);
    let verified =
        fixed(&verifier_hash, size, 0)? == fixed(&Hash::Sha1.digest(&[&verifier]), size, 0)?;
    Ok((
        Provider::CryptoApi {
            base,
            key_bits: header.key_bits,
        },
        verified.into(),
    ))
}

fn crypto_api_key(base: &[u8], key_bits: i32, block: &[u8]) -> Result<Vec<u8>, Thrown> {
    let key = fixed(
        &Hash::Sha1.digest(&[base, block]),
        i64::from(key_bits / 8),
        0,
    )?;
    if key_bits == 40 {
        return fixed(&key, 16, 0);
    }
    Ok(key)
}

/// `ECMAStandardEncryptionProvider`, its key hashed `iterations` times: the provider and
/// whether the password opens it.
fn standard(
    info: &mut Buffer<'_>,
    password: &[u8],
    iterations: u32,
) -> Result<(Provider, Check), Thrown> {
    let header = Header::read(info, Algorithm::is_aes, "AES_128, AES_192, AES_256")?;
    let iterated = iterate(
        Hash::Sha1,
        Hash::Sha1.digest(&[&header.salt, password]),
        iterations,
    );
    let key_bytes = i64::from(header.key_bits / 8);
    let check = |block: [u8; 4]| -> Result<bool, Thrown> {
        let key = Cipher::Aes.key(&standard_key(&iterated, &block, key_bytes)?)?;
        let verifier = decrypt(&key, Chaining::Ecb, &[], &header.verifier)?;
        let verifier_hash = decrypt(&key, Chaining::Ecb, &[], &header.verifier_hash)?;
        let size = i64::from(header.verifier_hash_size);
        Ok(fixed(&verifier_hash, size, 0)? == fixed(&Hash::Sha1.digest(&[&verifier]), size, 0)?)
    };
    let opens = check(int_bytes(0))?;
    let jackcess_opens = match iterations.checked_sub(1) {
        Some(last) => check(int_bytes(last as i32))?,
        None => opens,
    };
    let checked = match (opens, jackcess_opens) {
        (_, true) => Check::Opens,
        (true, false) => Check::OpensWhereJackcessRefuses,
        (false, false) => Check::Refuses,
    };
    Ok((
        Provider::Standard {
            iterated,
            key_bytes,
        },
        checked,
    ))
}

/// `cryptDeriveKey` of standard encryption: the SHA-1 of the iterated hash and the block,
/// XORed into blocks of 0x36 and 0x5C, each hashed, the two cut to the key size.
fn standard_key(iterated: &[u8], block: &[u8], key_bytes: i64) -> Result<Vec<u8>, Thrown> {
    let final_hash = Hash::Sha1.digest(&[iterated, block]);
    let x = |fill: u8| {
        let mut buffer = [fill; 64];
        for (byte, hashed) in buffer.iter_mut().zip(&final_hash) {
            *byte ^= hashed;
        }
        Hash::Sha1.digest(&[&buffer])
    };
    let mut derived = x(0x36);
    derived.extend(x(0x5C));
    fixed(&derived, key_bytes, 0)
}

/// The attributes of the elements of an agile descriptor `XmlEncryptionParser` reads.
#[derive(Default)]
struct Descriptor {
    /// The qualified name of the root element, its namespace and local name.
    root: Option<(String, String, String)>,
    /// The depth of each element open.
    depth: usize,
    key_data: Option<(String, Vec<Attribute>)>,
    data_integrity: Option<(String, Vec<Attribute>)>,
    /// The depth of the first `keyEncryptors` element, while it is open.
    key_encryptors: Option<usize>,
    found_key_encryptors: bool,
    encryptors: Vec<Encryptor>,
    /// The `keyEncryptor` elements open, by their index in `encryptors`.
    open_encryptors: Vec<(usize, usize)>,
}

/// A `keyEncryptor` element: its qualified name and attributes, and the first `encryptedKey`
/// in it of each namespace.
struct Encryptor {
    name: String,
    attributes: Vec<Attribute>,
    password: Option<(String, Vec<Attribute>)>,
    certificate: Option<(String, Vec<Attribute>)>,
}

impl SaxHandler for Descriptor {
    fn start_element(&mut self, _: &str, _: &str, _: &[Attribute]) -> Result<(), Failure> {
        unreachable!("the descriptor's elements are read by start_tag")
    }

    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        let element = || (tag.qualified_name.to_owned(), tag.attributes.to_vec());
        self.depth += 1;
        if self.root.is_none() {
            self.root = Some((
                tag.qualified_name.to_owned(),
                tag.namespace.to_owned(),
                tag.local_name.to_owned(),
            ));
            return Ok(());
        }
        match (tag.namespace, tag.local_name) {
            (ENCRYPTION_NAMESPACE, "keyData") if self.key_data.is_none() => {
                self.key_data = Some(element())
            }
            (ENCRYPTION_NAMESPACE, "dataIntegrity") if self.data_integrity.is_none() => {
                self.data_integrity = Some(element())
            }
            (ENCRYPTION_NAMESPACE, "keyEncryptors") if !self.found_key_encryptors => {
                self.found_key_encryptors = true;
                self.key_encryptors = Some(self.depth);
            }
            (ENCRYPTION_NAMESPACE, "keyEncryptor") if self.key_encryptors.is_some() => {
                self.open_encryptors
                    .push((self.encryptors.len(), self.depth));
                self.encryptors.push(Encryptor {
                    name: tag.qualified_name.to_owned(),
                    attributes: tag.attributes.to_vec(),
                    password: None,
                    certificate: None,
                });
            }
            (PASSWORD_NAMESPACE | CERTIFICATE_NAMESPACE, "encryptedKey") => {
                for &(index, _) in &self.open_encryptors {
                    let encryptor = &mut self.encryptors[index];
                    let slot = if tag.namespace == PASSWORD_NAMESPACE {
                        &mut encryptor.password
                    } else {
                        &mut encryptor.certificate
                    };
                    if slot.is_none() {
                        *slot = Some(element());
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, _: &str, _: &str) -> Result<(), Failure> {
        if self
            .open_encryptors
            .last()
            .is_some_and(|&(_, depth)| depth == self.depth)
        {
            self.open_encryptors.pop();
        }
        if self.key_encryptors == Some(self.depth) {
            self.key_encryptors = None;
        }
        self.depth -= 1;
        Ok(())
    }

    fn characters(&mut self, _: &str) -> Result<(), Failure> {
        Ok(())
    }
}

/// An element of the descriptor, as `getElement` found it.
struct Element<'d>(&'d (String, Vec<Attribute>));

/// Why `XmlEncryptionParser` fails parsing the descriptor, but with an
/// `InvalidCryptoConfigurationException` of its own: the exception it wraps.
fn unparsed() -> Thrown {
    invalid_configuration("Failed parsing encryption descriptor")
}

impl Element<'_> {
    /// `getStringAttribute`: the attribute trimmed, which must not be empty.
    fn text(&self, name: &str) -> Result<&str, Thrown> {
        let (element, attributes) = self.0;
        attribute_named(attributes, name)
            .map(crate::java::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                invalid_configuration(format!("Could not find xml config {name} under {element}"))
            })
    }

    /// `getLongAttribute`.
    fn long(&self, name: &str) -> Result<i64, Thrown> {
        crate::java::parse_long(self.text(name)?).ok_or_else(unparsed)
    }

    /// `getBase64Attribute`.
    fn binary(&self, name: &str) -> Result<Vec<u8>, Thrown> {
        crate::java::base64_decode(self.text(name)?).map_err(|_| unparsed())
    }
}

/// The parameters of a key data or password key encryptor.
struct Parameters {
    block_size: i64,
    key_bits: i64,
    cipher: String,
    chaining: String,
    hash: String,
    salt: Vec<u8>,
}

impl Parameters {
    fn read(element: &Element) -> Result<Self, Thrown> {
        element.long("saltSize")?;
        let block_size = element.long("blockSize")?;
        let key_bits = element.long("keyBits")?;
        element.long("hashSize")?;
        Ok(Parameters {
            block_size,
            key_bits,
            cipher: element.text("cipherAlgorithm")?.to_owned(),
            chaining: element.text("cipherChaining")?.to_owned(),
            hash: element.text("hashAlgorithm")?.to_owned(),
            salt: element.binary("saltValue")?,
        })
    }

    /// `XmlEncryptionDescriptor.initDigest`.
    fn hash(&self) -> Result<Hash, Thrown> {
        match enum_name(&self.hash).as_str() {
            "SHA1" => Ok(Hash::Sha1),
            "SHA256" => Ok(Hash::Sha256),
            "SHA384" => Ok(Hash::Sha384),
            "SHA512" => Ok(Hash::Sha512),
            "MD5" => Ok(Hash::Md5),
            "MD4" => Ok(Hash::Md4),
            "MD2" => Ok(Hash::Md2),
            "RIPEMD128" => Ok(Hash::Ripemd128),
            "RIPEMD160" => Ok(Hash::Ripemd160),
            "WHIRLPOOL" => Ok(Hash::Whirlpool),
            _ => Err(unsupported_parameter(&self.hash)),
        }
    }

    /// `XmlEncryptionDescriptor.initCipher`: the cipher and chaining this port decrypts with.
    fn cipher(&self) -> Result<(Cipher, Chaining), Thrown> {
        let chaining = match enum_name(&self.chaining).as_str() {
            "CHAININGMODECBC" => Some(Chaining::Cbc),
            "CHAININGMODEECB" => Some(Chaining::Ecb),
            "CHAININGMODECFB" => Some(Chaining::Cfb8),
            "CHAININGMODECCM" | "CHAININGMODEGCM" => None,
            _ => return Err(unsupported_parameter(&self.chaining)),
        };
        let algorithm = match enum_name(&self.cipher).as_str() {
            "AES" => Some(Cipher::Aes),
            "RC2" => Some(Cipher::Rc2),
            "DES" => Some(Cipher::Des),
            "_3DES" | "_3DES112" => Some(Cipher::TripleDes),
            "RC4" => None,
            _ => return Err(unsupported_parameter(&self.cipher)),
        };
        match (algorithm, chaining) {
            (Some(algorithm), Some(chaining)) => Ok((algorithm, chaining)),
            _ => Err(Thrown::unported(format!(
                "agile encryption with {} in {}",
                self.cipher, self.chaining
            ))),
        }
    }
}

/// `parseEnum`'s name: trimmed, upper case, without `-` and `_`, and `_` before a leading digit.
fn enum_name(text: &str) -> String {
    let name: String = crate::java::trim(text)
        .to_uppercase()
        .chars()
        .filter(|&character| character != '-' && character != '_')
        .collect();
    if name.starts_with(|character: char| character.is_ascii_digit()) {
        return format!("_{name}");
    }
    name
}

fn unsupported_parameter(text: &str) -> Thrown {
    invalid_configuration(format!("Unsupported encryption parameter: {text}"))
}

/// What `AgileEncryptionProvider` reads of its descriptor.
struct Agile {
    key_data: Parameters,
    password: Parameters,
    spin_count: i64,
    verifier_input: Vec<u8>,
    verifier_value: Vec<u8>,
    key_value: Vec<u8>,
}

impl Agile {
    /// The `AgileEncryptionProvider` constructor's reading: the reserved value, then the
    /// descriptor `XmlEncryptionParser` parses, with one password key encryptor.
    fn read(info: &mut Buffer<'_>) -> Result<Self, Thrown> {
        let reserved = info.i32()?;
        if reserved != AGILE_RESERVED {
            return Err(invalid_configuration(format!(
                "Unexpected reserved value {reserved}"
            )));
        }
        let xml = info.take(info.remaining() as i64)?;
        let mut descriptor = Descriptor::default();
        parse_sax(xml, "the encryption descriptor", &mut descriptor).map_err(|_| unparsed())?;
        let Some((root, namespace, local_name)) = &descriptor.root else {
            return Err(unparsed());
        };
        if local_name != "encryption" || namespace != ENCRYPTION_NAMESPACE {
            return Err(invalid_configuration(format!(
                "Unexpected xml config {root}"
            )));
        }
        let missing = |name: &str| {
            invalid_configuration(format!("Could not find xml config {name} under {root}"))
        };
        let key_data = descriptor
            .key_data
            .as_ref()
            .ok_or_else(|| missing("keyData"))?;
        let key_data = Parameters::read(&Element(key_data))?;
        if let Some(integrity) = &descriptor.data_integrity {
            let integrity = Element(integrity);
            integrity.binary("encryptedHmacKey")?;
            integrity.binary("encryptedHmacValue")?;
        }
        if !descriptor.found_key_encryptors {
            return Err(missing("keyEncryptors"));
        }
        let mut password = None;
        for encryptor in &descriptor.encryptors {
            let uri = Element(&(encryptor.name.clone(), encryptor.attributes.clone()))
                .text("uri")?
                .to_owned();
            let (found, namespace) = if uri == PASSWORD_NAMESPACE {
                (&encryptor.password, PASSWORD_NAMESPACE)
            } else if uri == CERTIFICATE_NAMESPACE {
                (&encryptor.certificate, CERTIFICATE_NAMESPACE)
            } else {
                return Err(invalid_configuration(format!(
                    "Unexpected xml config {uri} under {}",
                    encryptor.name
                )));
            };
            let Some(found) = found else {
                return Err(invalid_configuration(format!(
                    "Could not find xml config encryptedKey under {}",
                    encryptor.name
                )));
            };
            let element = Element(found);
            if namespace == CERTIFICATE_NAMESPACE {
                for name in ["encryptedKeyValue", "x509Certificate", "certVerifier"] {
                    element.binary(name)?;
                }
                continue;
            }
            let parameters = Parameters::read(&element)?;
            let spin_count = element.long("spinCount")?;
            password = Some((
                parameters,
                spin_count,
                element.binary("encryptedVerifierHashInput")?,
                element.binary("encryptedVerifierHashValue")?,
                element.binary("encryptedKeyValue")?,
            ));
        }
        let one_password = descriptor.encryptors.len() == 1
            && descriptor.encryptors[0].attributes.iter().any(|attribute| {
                attribute.qualified_name == "uri" && attribute.value == PASSWORD_NAMESPACE
            });
        let (password, spin_count, verifier_input, verifier_value, key_value) = password
            .filter(|_| one_password)
            .ok_or_else(|| invalid_configuration("Missing or unexpected key encryptor"))?;
        Ok(Agile {
            key_data,
            password,
            spin_count,
            verifier_input,
            verifier_value,
            key_value,
        })
    }

    /// `cryptDeriveKey`: the password's key of a block, cut or filled with 0x36 to the key size.
    fn derive_key(
        &self,
        hash: Hash,
        algorithm: Cipher,
        password: &[u8],
        block: &[u8],
    ) -> Result<BlockKey, Thrown> {
        let base = hash.digest(&[&self.password.salt, password]);
        let iterated = iterate(hash, base, self.spin_count as u32);
        let key = fixed(
            &hash.digest(&[&iterated, block]),
            i64::from(self.password.key_bits as i32 / 8),
            0x36,
        )?;
        algorithm.key(&key)
    }

    /// The rest of the constructor and `verifyPassword`: the provider, and whether `password`
    /// opens it.
    fn open(self, password: &[u8]) -> Result<(Provider, Check), Thrown> {
        if !(0..=MAX_SPIN_COUNT).contains(&self.spin_count) {
            return Err(Thrown::unported(format!(
                "agile encryption of {} hashing rounds",
                self.spin_count
            )));
        }
        let hash = self.password.hash()?;
        let (algorithm, chaining) = self.password.cipher()?;
        let decrypt = |block: &[u8], encrypted: &[u8]| -> Result<Vec<u8>, Thrown> {
            let key = self.derive_key(hash, algorithm, password, block)?;
            decrypt(&key, chaining, &self.password.salt, encrypted)
        };
        let key_value = decrypt(&KEY_VALUE_BLOCK, &self.key_value)?;
        let verifier = decrypt(&VERIFIER_INPUT_BLOCK, &self.verifier_input)?;
        let verifier_hash = decrypt(&VERIFIER_VALUE_BLOCK, &self.verifier_value)?;
        let mut test_hash = hash.digest(&[&verifier]);
        let block_size = self.password.block_size as i32;
        if block_size == 0 {
            return Err(Thrown::runtime(ARITHMETIC, "/ by zero"));
        }
        let length = test_hash.len() as i32;
        if length % block_size != 0 {
            let rounded = (length + block_size - 1) / block_size * block_size;
            test_hash = fixed(&test_hash, i64::from(rounded), 0)?;
        }
        let verified = verifier_hash == test_hash;
        let key_hash = self.key_data.hash()?;
        let (key_algorithm, key_chaining) = self.key_data.cipher()?;
        Ok((
            Provider::Agile {
                key: Box::new(key_algorithm.key(&key_value)?),
                chaining: key_chaining,
                hash: key_hash,
                salt: self.key_data.salt,
                block_size: i64::from(self.key_data.block_size as i32),
            },
            verified.into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parameters_as_parse_enum_does() {
        assert_eq!(enum_name(" ChainingModeCBC "), "CHAININGMODECBC");
        assert_eq!(enum_name("3DES_112"), "_3DES112");
        assert_eq!(enum_name("SHA-512"), "SHA512");
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    /// Each cipher and chaining of agile encryption decrypts the vectors of FIPS 46-3 and
    /// SP 800-38A as BouncyCastle does (#186).
    #[test]
    fn decrypts_each_agile_cipher_and_chaining() {
        let aes = Cipher::Aes
            .key(&hex("2b7e151628aed2a6abf7158809cf4f3c"))
            .unwrap();
        let iv = hex("000102030405060708090a0b0c0d0e0f");
        assert_eq!(
            decrypt(
                &aes,
                Chaining::Cbc,
                &iv,
                &hex("7649abac8119b246cee98e9b12e9197d")
            )
            .unwrap(),
            hex("6bc1bee22e409f96e93d7e117393172a")
        );
        assert_eq!(
            decrypt(
                &aes,
                Chaining::Cfb8,
                &iv,
                &hex("3b79424c9c0dd436bace9e0ed4586a4f32b9")
            )
            .unwrap(),
            hex("6bc1bee22e409f96e93d7e117393172aae2d")
        );
        let des = Cipher::Des.key(&hex("133457799bbcdff1")).unwrap();
        assert_eq!(
            decrypt(&des, Chaining::Ecb, &[], &hex("85e813540f0ab405")).unwrap(),
            hex("0123456789abcdef")
        );
        let triple = Cipher::TripleDes
            .key(&hex("133457799bbcdff1133457799bbcdff1133457799bbcdff1"))
            .unwrap();
        assert_eq!(
            decrypt(&triple, Chaining::Ecb, &[], &hex("85e813540f0ab405")).unwrap(),
            hex("0123456789abcdef")
        );
        let two_key = Cipher::TripleDes
            .key(&hex("133457799bbcdff1133457799bbcdff1"))
            .unwrap();
        assert_eq!(
            decrypt(&two_key, Chaining::Ecb, &[], &hex("85e813540f0ab405")).unwrap(),
            hex("0123456789abcdef")
        );
        let rc2 = Cipher::Rc2
            .key(&hex("88bca90e90875a7f0f79c384627bafb2"))
            .unwrap();
        assert_eq!(rc2.block_size(), 8);
        assert!(matches!(
            decrypt(&rc2, Chaining::Cbc, &iv, &[0; 8]),
            Err(Thrown::Exception(_))
        ));
        assert!(matches!(
            decrypt(&aes, Chaining::Ecb, &[], &[0; 15]),
            Err(Thrown::Exception(_))
        ));
        assert!(Cipher::Des.key(&[0; 16]).is_err());
    }

    /// The hashes of agile encryption that Jackcess takes from BouncyCastle hash as it does
    /// (#186).
    #[test]
    fn names_every_agile_hash() {
        let parameters = |hash: &str| Parameters {
            block_size: 16,
            key_bits: 128,
            cipher: "AES".into(),
            chaining: "ChainingModeCFB".into(),
            hash: hash.into(),
            salt: Vec::new(),
        };
        for (name, hash) in [
            ("MD4", Hash::Md4),
            ("MD2", Hash::Md2),
            ("RIPEMD-128", Hash::Ripemd128),
            ("RIPEMD-160", Hash::Ripemd160),
            ("WHIRLPOOL", Hash::Whirlpool),
        ] {
            assert_eq!(parameters(name).hash().unwrap(), hash);
        }
        assert_eq!(
            parameters("SHA1").cipher().unwrap(),
            (Cipher::Aes, Chaining::Cfb8)
        );
        assert_eq!(
            Hash::Md4.digest(&[b"abc"]),
            hex("a448017aaf21d8525fc10ae87aa6729d")
        );
        assert_eq!(
            Hash::Ripemd160.digest(&[b"abc"]),
            hex("8eb208f7e05d987a9b044a8e98c6b087f15a0bfc")
        );
        assert_eq!(
            Hash::Md2.digest(&[b"abc"]),
            hex("da853b0d3f88d99b30283a69e6ded6bb")
        );
    }

    #[test]
    fn cuts_passwords_as_jackcess_does() {
        assert_eq!(password_bytes("ab"), [b'a', 0, b'b', 0]);
        let long = format!("{}\u{1F600}", "x".repeat(254));
        let bytes = password_bytes(&long);
        assert_eq!(bytes.len(), 510);
        assert_eq!(bytes[508..], [0xFD, 0xFF]);
    }
}
