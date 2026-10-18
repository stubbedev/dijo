//! The encryption of Office documents as POI's `poifs.crypt` reads and opens it
//! ([MS-OFFCRYPTO]): an `EncryptionInfo`, its verifier checked against a password, and the keys
//! that decrypt the document: the RC4 keys of Office 97's binary RC4 and of CryptoAPI for the
//! binary formats, and the AES keys of the standard and agile encryption of OOXML packages. The
//! ciphers and hashes are RustCrypto's. The XOR obfuscation of Word and Excel documents is
//! [`xor`]'s.

mod agile;
pub mod xor;

use aes::cipher::BlockCipherDecrypt;
use md5::Md5;
use rc4::{KeyInit, Rc4, StreamCipher};
use sha1::{Digest, Sha1};
use sha2::{Sha256, Sha384, Sha512};

use crate::config::{ParserConfigs, Password};
use crate::errors::Error;
use crate::formats::Unported;

use agile::{Agile, AgileKey};

/// `Decryptor.DEFAULT_PASSWORD`, which Office encrypts a document with when it is only to be
/// write protected.
pub const DEFAULT_PASSWORD: &str = "VelvetSweatshop";
/// The most UTF-16 units of a password binary RC4 and CryptoAPI hash.
const MAX_PASSWORD_UNITS: usize = 255;
/// `StandardEncryptionVerifier.SPIN_COUNT`.
const STANDARD_SPIN_COUNT: u32 = 50_000;
/// The name of the stream an OOXML package is encrypted in.
pub const ENCRYPTED_PACKAGE: &str = "EncryptedPackage";
/// The name of the stream its encryption is described in.
pub const ENCRYPTION_INFO: &str = "EncryptionInfo";

/// The password POI opens an encrypted document with: the one given, else the one Office
/// encrypts a document with that is only write protected (`Biff8EncryptionKey`'s current user
/// password, else `Decryptor.DEFAULT_PASSWORD`, as Tika's `OfficeParser` asks its
/// `PasswordProvider` for one).
pub fn password(configs: &ParserConfigs) -> &str {
    configs
        .password
        .as_ref()
        .map_or(DEFAULT_PASSWORD, Password::expose)
}

/// The error Tika's path fails with where a parser throws a Tika `EncryptedDocumentException`,
/// wrapping the exception `cause` POI throws where there is one, as our Java reports it.
pub fn encrypted(cause: Option<&str>) -> Error {
    let thrown = "Parse error occurred : org.apache.tika.exception.EncryptedDocumentException: \
                  Unable to process: document is encrypted";
    Error::Encrypted(match cause {
        Some(cause) => format!("{thrown} <- caused by {cause}"),
        None => thrown.to_owned(),
    })
}

/// The hash algorithms the encryptions use (`HashAlgorithm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    Md5,
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    /// The hash of the parts, one after the other.
    pub fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut digest = D::new();
            for part in parts {
                digest.update(part);
            }
            digest.finalize().to_vec()
        }
        match self {
            Hash::Md5 => run::<Md5>(parts),
            Hash::Sha1 => run::<Sha1>(parts),
            Hash::Sha256 => run::<Sha256>(parts),
            Hash::Sha384 => run::<Sha384>(parts),
            Hash::Sha512 => run::<Sha512>(parts),
        }
    }

    /// `hashSize`.
    pub fn size(self) -> usize {
        match self {
            Hash::Md5 => 16,
            Hash::Sha1 => 20,
            Hash::Sha256 => 32,
            Hash::Sha384 => 48,
            Hash::Sha512 => 64,
        }
    }

    /// `HashAlgorithm.fromEcmaId(String)`, as the agile descriptor names it.
    fn from_ecma_name(name: &str) -> Result<Hash, Unopened> {
        match name {
            "SHA1" => Ok(Hash::Sha1),
            "SHA256" => Ok(Hash::Sha256),
            "SHA384" => Ok(Hash::Sha384),
            "SHA512" => Ok(Hash::Sha512),
            "MD5" => Ok(Hash::Md5),
            _ => Err(Unopened::Thrown("hash algorithm not found".to_owned())),
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
pub enum Unopened {
    /// POI throws reading the encryption, with what it throws.
    Thrown(String),
    /// An encryption this port does not open, which Tika's path opens.
    Unported(String),
    /// An encryption [MS-OFFCRYPTO] 2.3.1 does not allow in the document, which no Office
    /// application writes and no decryptor of POI's opens, whatever the password: AES in a
    /// binary document, RC4 in an OOXML package.
    Disallowed(&'static str),
}

impl Unopened {
    pub fn unported(self) -> Unported {
        match self {
            Unopened::Thrown(reason) => Unported(format!(
                "POI throws reading the document's encryption, which fails Tika's parse: {reason}"
            )),
            Unopened::Unported(reason) => Unported(reason),
            Unopened::Disallowed(encryption) => Unported(format!(
                "{encryption}, which [MS-OFFCRYPTO] does not allow and POI does not decrypt"
            )),
        }
    }
}

/// `StringUtil.getToUnicodeLE` of a password as Java holds it, cut to `limit` UTF-16 units
/// where given.
fn password_bytes(password: &str, limit: Option<usize>) -> Vec<u8> {
    password
        .encode_utf16()
        .take(limit.unwrap_or(usize::MAX))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// `CryptoFunctions.hashPassword`: the salted hash of the password, hashed again with each
/// iteration's number before it.
fn hash_password(password: &str, hash: Hash, salt: &[u8], spin_count: u32) -> Vec<u8> {
    let mut hashed = hash.digest(&[salt, &password_bytes(password, None)]);
    for iteration in 0..spin_count {
        hashed = hash.digest(&[&iteration.to_le_bytes(), &hashed]);
    }
    hashed
}

/// `CryptoFunctions.generateKey`: the hash of a password hash and a block key, filled with
/// 0x36 to `size`.
fn generate_key(password_hash: &[u8], hash: Hash, block_key: &[u8], size: usize) -> Vec<u8> {
    filled(&hash.digest(&[password_hash, block_key]), size, 0x36)
}

/// `getBlockX`: `bytes` cut or filled with `fill` to `size`.
fn filled(bytes: &[u8], size: usize, fill: u8) -> Vec<u8> {
    let mut out = vec![fill; size];
    let length = size.min(bytes.len());
    out[..length].copy_from_slice(&bytes[..length]);
    out
}

/// `getBlock0`.
fn block_of(bytes: &[u8], size: usize) -> Vec<u8> {
    filled(bytes, size, 0)
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

/// The encryption a format asks `EncryptionInfo` to read (`preferredEncryptionMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// CryptoAPI wherever the version is CryptoAPI's or standard encryption's.
    CryptoApi,
    /// What the version and flags say.
    Unset,
}

/// Any encryption `new EncryptionInfo(input, mode)` reads.
enum AnyEncryption {
    BinaryRc4(BinaryRc4),
    CryptoApi(CryptoApi),
    Standard(Standard),
    Agile(Agile),
}

/// `new EncryptionInfo(input, mode)`.
fn read_encryption(data: &[u8], mode: Mode) -> Result<AnyEncryption, Unopened> {
    let mut input = Input { data, at: 0 };
    let major = input.u16()?;
    let minor = input.u16()?;
    match (major, minor) {
        (1, 1) => Ok(AnyEncryption::BinaryRc4(BinaryRc4::read(&mut input)?)),
        (2..=4, 2) => {
            let aes = input.i32()? & 0x20 != 0;
            input.i32()?;
            let header = Header::read(&mut input)?;
            let verifier = Verifier::read(&mut input, &header)?;
            if mode == Mode::CryptoApi || !aes {
                Ok(AnyEncryption::CryptoApi(CryptoApi::new(header, verifier)?))
            } else if major == 2 {
                Err(Unopened::Unported(
                    "standard encryption of version 2, for which POI has no decryptor".to_owned(),
                ))
            } else {
                Ok(AnyEncryption::Standard(Standard::new(header, verifier)?))
            }
        }
        (4, 4) => {
            input.i32()?;
            Ok(AnyEncryption::Agile(Agile::read(&data[input.at..])?))
        }
        _ => Err(Unopened::Thrown(format!(
            "Unknown encryption: version major: {major} / version minor: {minor}"
        ))),
    }
}

/// The RC4 encryption of a binary document, as `EncryptionInfo` reads it.
#[derive(Debug, Clone)]
pub enum EncryptionInfo {
    BinaryRc4(BinaryRc4),
    CryptoApi(CryptoApi),
}

impl EncryptionInfo {
    /// `new EncryptionInfo(input, mode)` of a binary document.
    pub fn read(data: &[u8], mode: Mode) -> Result<EncryptionInfo, Unopened> {
        match read_encryption(data, mode)? {
            AnyEncryption::BinaryRc4(rc4) => Ok(EncryptionInfo::BinaryRc4(rc4)),
            AnyEncryption::CryptoApi(api) => Ok(EncryptionInfo::CryptoApi(api)),
            AnyEncryption::Standard(_) => {
                Err(Unopened::Disallowed("a binary document encrypted with AES"))
            }
            AnyEncryption::Agile(_) => Err(Unopened::Unported(
                "a binary document with agile encryption, which this port does not decrypt"
                    .to_owned(),
            )),
        }
    }
}

/// The AES encryption of an OOXML package, as `EncryptionInfo` reads its stream.
#[derive(Debug, Clone)]
pub enum PackageEncryption {
    Standard(Standard),
    Agile(Agile),
}

impl PackageEncryption {
    /// `new EncryptionInfo(directory)`: the encryption its `EncryptionInfo` stream describes.
    pub fn read(data: &[u8]) -> Result<PackageEncryption, Unopened> {
        match read_encryption(data, Mode::Unset)? {
            AnyEncryption::Standard(standard) => Ok(PackageEncryption::Standard(standard)),
            AnyEncryption::Agile(agile) => Ok(PackageEncryption::Agile(agile)),
            AnyEncryption::BinaryRc4(_) | AnyEncryption::CryptoApi(_) => {
                Err(Unopened::Disallowed("an OOXML package encrypted with RC4"))
            }
        }
    }

    /// `Decryptor.verifyPassword`: the key of the package, where the password opens it.
    pub fn verify(&self, password: &str) -> Result<Option<PackageKey>, Unopened> {
        Ok(match self {
            PackageEncryption::Standard(standard) => {
                standard.verify(password)?.map(PackageKey::Standard)
            }
            PackageEncryption::Agile(agile) => agile.verify(password)?.map(PackageKey::Agile),
        })
    }
}

/// The key of an OOXML package.
#[derive(Debug, Clone)]
pub enum PackageKey {
    Standard(AesKey),
    Agile(AgileKey),
}

impl PackageKey {
    /// `Decryptor.getDataStream(directory)` read whole: the package the `EncryptedPackage`
    /// stream holds, past the length it starts with.
    pub fn decrypt(&self, stream: &[u8]) -> Result<Vec<u8>, Unopened> {
        let length = stream
            .get(..8)
            .map(|bytes| i64::from_le_bytes(bytes.try_into().expect("eight bytes")))
            .ok_or_else(|| Unopened::Thrown("the encrypted package has no length".to_owned()))?;
        let length = usize::try_from(length).map_err(|_| {
            Unopened::Unported("an encrypted package of a negative length".to_owned())
        })?;
        let encrypted = &stream[8..];
        match self {
            PackageKey::Standard(key) => {
                let needed = (length / 16 + 1) * 16;
                let read = &encrypted[..needed.min(encrypted.len())];
                if !read.len().is_multiple_of(16) || read.len() < length {
                    return Err(Unopened::Unported(
                        "an encrypted package shorter than its length, which Java's cipher \
                         stream fails on"
                            .to_owned(),
                    ));
                }
                let mut plain = key.decrypt_ecb(read);
                plain.truncate(length);
                Ok(plain)
            }
            PackageKey::Agile(key) => key.decrypt(encrypted, length),
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
pub struct Rc4Key(Vec<u8>);

impl Rc4Key {
    /// The cipher of the key, at the start of its stream.
    pub fn cipher(&self) -> Rc4 {
        Rc4::new_from_slice(&self.0).expect("an RC4 key of 5 to 16 bytes")
    }

    /// `data` decrypted in place with the key's stream from its start.
    pub fn decrypt(&self, data: &mut [u8]) {
        self.cipher().apply_keystream(data);
    }
}

/// What decrypts the blocks of a binary document: a block's RC4 key.
pub trait BlockKeys {
    fn block(&self, block: u32) -> Rc4Key;

    /// What `ChunkedCipherInputStream` reads of `data` in chunks of `chunk` bytes, the bytes
    /// before `from` read plain (`readPlain`).
    fn decrypt_chunks(&self, data: &mut [u8], chunk: usize, from: usize)
    where
        Self: Sized,
    {
        if let Some(encrypted) = data.get_mut(from..) {
            ChunkedKeyStream::new(self, chunk).decrypt(encrypted, from);
        }
    }
}

/// The key stream `ChunkedCipherInputStream` decrypts a stream with, by position: each byte
/// with the key of its chunk of `chunk` bytes, from the chunk's start.
pub struct ChunkedKeyStream<'k, K: BlockKeys> {
    keys: &'k K,
    chunk: usize,
    /// The number of the chunk last decrypted, and its key stream.
    current: Option<(usize, Vec<u8>)>,
}

impl<'k, K: BlockKeys> ChunkedKeyStream<'k, K> {
    pub fn new(keys: &'k K, chunk: usize) -> Self {
        ChunkedKeyStream {
            keys,
            chunk,
            current: None,
        }
    }

    /// `data`, which starts at `position` of the stream, decrypted in place.
    pub fn decrypt(&mut self, data: &mut [u8], position: usize) {
        for (at, byte) in (position..).zip(data.iter_mut()) {
            let number = at / self.chunk;
            let stream = match &mut self.current {
                Some((current, stream)) if *current == number => stream,
                current => {
                    let mut stream = vec![0; self.chunk];
                    self.keys.block(number as u32).decrypt(&mut stream);
                    &mut current.insert((number, stream)).1
                }
            };
            *byte ^= stream[at % self.chunk];
        }
    }
}

/// The key of a binary document's RC4 encryption.
#[derive(Debug, Clone)]
pub enum Rc4Keys {
    BinaryRc4(BinaryRc4Key),
    CryptoApi(CryptoApiKey),
}

impl BlockKeys for Rc4Keys {
    fn block(&self, block: u32) -> Rc4Key {
        match self {
            Rc4Keys::BinaryRc4(key) => key.block(block),
            Rc4Keys::CryptoApi(key) => key.block(block),
        }
    }
}

impl EncryptionInfo {
    /// `Decryptor.verifyPassword`: the key, where the password opens the encryption.
    pub fn verify(&self, password: &str) -> Option<Rc4Keys> {
        match self {
            EncryptionInfo::BinaryRc4(rc4) => rc4.verify(password).map(Rc4Keys::BinaryRc4),
            EncryptionInfo::CryptoApi(api) => api.verify(password).map(Rc4Keys::CryptoApi),
        }
    }
}

/// Office 97's binary RC4 (`BinaryRC4EncryptionVerifier`): MD5 for its keys.
#[derive(Debug, Clone)]
pub struct BinaryRc4 {
    salt: [u8; 16],
    verifier: [u8; 16],
    verifier_hash: [u8; 16],
}

impl BinaryRc4 {
    fn read(input: &mut Input) -> Result<BinaryRc4, Unopened> {
        Ok(BinaryRc4 {
            salt: input.array()?,
            verifier: input.array()?,
            verifier_hash: input.array()?,
        })
    }

    /// `BinaryRC4Decryptor.verifyPassword`: the secret key, where the password opens it.
    fn verify(&self, password: &str) -> Option<BinaryRc4Key> {
        let hashed = Hash::Md5.digest(&[&password_bytes(password, Some(MAX_PASSWORD_UNITS))]);
        let mut repeated = Vec::with_capacity(16 * (5 + self.salt.len()));
        for _ in 0..16 {
            repeated.extend_from_slice(&hashed[..5]);
            repeated.extend_from_slice(&self.salt);
        }
        let key = BinaryRc4Key(Hash::Md5.digest(&[&repeated])[..5].to_vec());
        let mut cipher = key.block(0).cipher();
        let mut verifier = self.verifier;
        cipher.apply_keystream(&mut verifier);
        let mut verifier_hash = self.verifier_hash;
        cipher.apply_keystream(&mut verifier_hash);
        (Hash::Md5.digest(&[&verifier]) == verifier_hash).then_some(key)
    }
}

#[cfg(any(test, feature = "testing"))]
impl BinaryRc4 {
    /// Binary RC4's encryption of a verifier with `password`, as Office writes it.
    pub fn encrypting(password: &str) -> BinaryRc4 {
        let salt = [9u8; 16];
        let hashed = Hash::Md5.digest(&[&password_bytes(password, Some(MAX_PASSWORD_UNITS))]);
        let mut repeated = Vec::new();
        for _ in 0..16 {
            repeated.extend_from_slice(&hashed[..5]);
            repeated.extend_from_slice(&salt);
        }
        let key = BinaryRc4Key(Hash::Md5.digest(&[&repeated])[..5].to_vec());
        let mut encrypted = [4u8; 16].to_vec();
        encrypted.extend(Hash::Md5.digest(&[&[4u8; 16]]));
        key.block(0).decrypt(&mut encrypted);
        BinaryRc4 {
            salt,
            verifier: encrypted[..16].try_into().unwrap(),
            verifier_hash: encrypted[16..].try_into().unwrap(),
        }
    }

    /// The encryption as a document writes it: its version, 1.1, then its salt and verifier.
    pub fn written(&self) -> Vec<u8> {
        let mut written = vec![1, 0, 1, 0];
        written.extend(self.salt);
        written.extend(self.verifier);
        written.extend(self.verifier_hash);
        written
    }
}

/// The secret key of binary RC4.
#[derive(Debug, Clone)]
pub struct BinaryRc4Key(Vec<u8>);

impl BlockKeys for BinaryRc4Key {
    /// `initCipherForBlock`: the MD5 of the secret key and the block's number.
    fn block(&self, block: u32) -> Rc4Key {
        Rc4Key(generate_key(&self.0, Hash::Md5, &block.to_le_bytes(), 16))
    }
}

/// CryptoAPI's RC4 encryption (`CryptoAPIEncryptionHeader` and its verifier).
#[derive(Debug, Clone)]
pub struct CryptoApi {
    hash: Hash,
    key_bits: i32,
    verifier: Verifier,
}

impl CryptoApi {
    fn new(header: Header, verifier: Verifier) -> Result<CryptoApi, Unopened> {
        if header.cipher != Cipher::Rc4 {
            return Err(Unopened::Disallowed("a binary document encrypted with AES"));
        }
        Ok(CryptoApi {
            hash: header.hash,
            key_bits: header.key_bits,
            verifier,
        })
    }

    /// `CryptoAPIDecryptor.verifyPassword`: the secret key, where the password opens it.
    fn verify(&self, password: &str) -> Option<CryptoApiKey> {
        let secret = self.hash.digest(&[
            &self.verifier.salt,
            &password_bytes(password, Some(MAX_PASSWORD_UNITS)),
        ]);
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
pub struct CryptoApiKey {
    secret: Vec<u8>,
    hash: Hash,
    key_bits: i32,
}

#[cfg(any(test, feature = "testing"))]
impl CryptoApiKey {
    /// A 128-bit key of SHA-1 from `secret`, for the tests of what it decrypts.
    pub fn of_secret(secret: &[u8]) -> CryptoApiKey {
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

/// Standard encryption of an OOXML package: AES, its key derived with SHA-1.
#[derive(Debug, Clone)]
pub struct Standard {
    key_bits: i32,
    verifier: Verifier,
}

impl Standard {
    fn new(header: Header, verifier: Verifier) -> Result<Standard, Unopened> {
        if header.cipher == Cipher::Rc4 || header.hash != Hash::Sha1 {
            return Err(Unopened::Unported(
                "standard encryption with a cipher or hash other than AES and SHA-1, which this \
                 port does not decrypt"
                    .to_owned(),
            ));
        }
        Ok(Standard {
            key_bits: header.key_bits,
            verifier,
        })
    }

    /// `StandardDecryptor.verifyPassword`: the AES key, where the password opens it.
    fn verify(&self, password: &str) -> Result<Option<AesKey>, Unopened> {
        let hashed = hash_password(
            password,
            Hash::Sha1,
            &self.verifier.salt,
            STANDARD_SPIN_COUNT,
        );
        let final_hash = generate_key(&hashed, Hash::Sha1, &0u32.to_le_bytes(), Hash::Sha1.size());
        let fill_and_xor = |fill: u8| {
            let mut buffer = [fill; 64];
            for (byte, hashed) in buffer.iter_mut().zip(&final_hash) {
                *byte ^= hashed;
            }
            Hash::Sha1.digest(&[&buffer])
        };
        let mut derived = fill_and_xor(0x36);
        derived.extend(fill_and_xor(0x5C));
        let key = AesKey::new(&block_of(&derived, self.key_bits as usize / 8))?;
        let verifier = key.decrypt_ecb_whole(&self.verifier.verifier)?;
        let verifier_hash = key.decrypt_ecb_whole(&self.verifier.verifier_hash)?;
        let opened = Hash::Sha1.digest(&[&verifier]) == verifier_hash[..Hash::Sha1.size()];
        Ok(opened.then_some(key))
    }
}

/// An AES key of one of the three sizes.
#[derive(Clone)]
pub enum AesKey {
    Aes128(aes::Aes128),
    Aes192(aes::Aes192),
    Aes256(aes::Aes256),
}

impl std::fmt::Debug for AesKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AesKey(<secret>)")
    }
}

impl AesKey {
    /// The key of `key`'s bytes, failing as Java's cipher does on a length AES does not take.
    pub fn new(key: &[u8]) -> Result<AesKey, Unopened> {
        use aes::cipher::KeyInit as _;
        let invalid = |_| Unopened::Thrown("Invalid AES key length".to_owned());
        Ok(match key.len() {
            16 => AesKey::Aes128(aes::Aes128::new_from_slice(key).map_err(invalid)?),
            24 => AesKey::Aes192(aes::Aes192::new_from_slice(key).map_err(invalid)?),
            32 => AesKey::Aes256(aes::Aes256::new_from_slice(key).map_err(invalid)?),
            length => {
                return Err(Unopened::Thrown(format!(
                    "Invalid AES key length: {length} bytes"
                )))
            }
        })
    }

    fn decrypt_block(&self, block: &mut [u8; 16]) {
        let block: &mut aes::Block = block.into();
        match self {
            AesKey::Aes128(cipher) => cipher.decrypt_block(block),
            AesKey::Aes192(cipher) => cipher.decrypt_block(block),
            AesKey::Aes256(cipher) => cipher.decrypt_block(block),
        }
    }

    /// AES/ECB/NoPadding of `data`'s whole blocks.
    pub fn decrypt_ecb(&self, data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        for chunk in out.as_chunks_mut::<16>().0 {
            self.decrypt_block(chunk);
        }
        out
    }

    /// AES/ECB/NoPadding's `doFinal`, failing as Java's does on data of no whole number of
    /// blocks.
    fn decrypt_ecb_whole(&self, data: &[u8]) -> Result<Vec<u8>, Unopened> {
        whole_blocks(data)?;
        Ok(self.decrypt_ecb(data))
    }

    /// AES/CBC/NoPadding of `data`'s whole blocks with the IV.
    pub fn decrypt_cbc(&self, iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        let mut previous = *iv;
        for chunk in out.as_chunks_mut::<16>().0 {
            let cipher_text = *chunk;
            self.decrypt_block(chunk);
            for (byte, mask) in chunk.iter_mut().zip(&previous) {
                *byte ^= mask;
            }
            previous = cipher_text;
        }
        out
    }

    /// AES/CBC/NoPadding's `doFinal`, failing as Java's does on data of no whole number of
    /// blocks.
    fn decrypt_cbc_whole(&self, iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, Unopened> {
        whole_blocks(data)?;
        Ok(self.decrypt_cbc(iv, data))
    }
}

/// Fails as a Java cipher without padding fails on data of no whole number of blocks.
fn whole_blocks(data: &[u8]) -> Result<(), Unopened> {
    if !data.len().is_multiple_of(16) {
        return Err(Unopened::Thrown(
            "Input length not multiple of 16 bytes".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CryptoAPI's encryption of a verifier with `password`, as Office writes it.
    fn crypto_api(password: &str, key_bits: i32) -> CryptoApi {
        let salt = [9; 16];
        let secret =
            Hash::Sha1.digest(&[&salt, &password_bytes(password, Some(MAX_PASSWORD_UNITS))]);
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
    fn hashes_passwords_as_poi_does() {
        let hashed = hash_password("a", Hash::Sha1, &[1; 16], 2);
        let first = Hash::Sha1.digest(&[&[1; 16], &[b'a', 0]]);
        let second = Hash::Sha1.digest(&[&0u32.to_le_bytes(), &first]);
        let third = Hash::Sha1.digest(&[&1u32.to_le_bytes(), &second]);
        assert_eq!(hashed, third);
        assert_eq!(generate_key(&[1], Hash::Md5, &[2], 20)[16..], [0x36; 4]);
    }

    #[test]
    fn decrypts_aes_in_the_modes_the_encryptions_use() {
        use aes::cipher::{BlockCipherEncrypt, KeyInit};
        let key = [7u8; 16];
        let cipher = aes::Aes128::new_from_slice(&key).unwrap();
        let mut block = aes::Block::from([3u8; 16]);
        cipher.encrypt_block(&mut block);
        let aes = AesKey::new(&key).unwrap();
        assert_eq!(aes.decrypt_ecb_whole(&block).unwrap(), [3u8; 16]);
        let iv = [5u8; 16];
        let mut chained = [3u8; 16];
        for (byte, mask) in chained.iter_mut().zip(&iv) {
            *byte ^= mask;
        }
        let mut chained = aes::Block::from(chained);
        cipher.encrypt_block(&mut chained);
        assert_eq!(aes.decrypt_cbc_whole(&iv, &chained).unwrap(), [3u8; 16]);
        assert!(aes.decrypt_ecb_whole(&[0; 15]).is_err());
        assert!(AesKey::new(&[0; 20]).is_err());
    }

    #[test]
    fn hashes_at_most_255_units_of_a_password() {
        assert_eq!(
            password_bytes(&"a".repeat(300), Some(MAX_PASSWORD_UNITS)).len(),
            510
        );
    }

    #[test]
    fn reads_what_poi_reads_of_an_encryption_info() {
        assert!(matches!(
            EncryptionInfo::read(&[1, 0, 1, 0], Mode::Unset),
            Err(Unopened::Thrown(_))
        ));
        assert!(matches!(
            EncryptionInfo::read(&[9, 0, 9, 0], Mode::CryptoApi),
            Err(Unopened::Thrown(reason)) if reason.contains("Unknown encryption")
        ));
        assert!(matches!(
            EncryptionInfo::read(&[2, 0, 2, 0, 0], Mode::CryptoApi),
            Err(Unopened::Thrown(_))
        ));
        assert!(matches!(
            PackageEncryption::read(&[3, 0, 2, 0, 0x24, 0, 0, 0]),
            Err(Unopened::Thrown(_))
        ));
    }

    #[test]
    fn opens_binary_rc4_with_the_right_password_only() {
        let rc4 = BinaryRc4::encrypting("pw");
        assert!(rc4.verify("pw").is_some());
        assert!(rc4.verify("other").is_none());
    }

    #[test]
    fn decrypts_each_chunk_with_its_blocks_key_past_the_plain_bytes() {
        let key = CryptoApiKey::of_secret(&[3; 20]);
        let mut data = vec![0u8; 1100];
        key.decrypt_chunks(&mut data, 512, 68);
        let stream = |block: u32, length: usize| {
            let mut bytes = vec![0; length];
            key.block(block).decrypt(&mut bytes);
            bytes
        };
        assert_eq!(data[..68], [0; 68]);
        assert_eq!(data[68..512], stream(0, 512)[68..]);
        assert_eq!(data[512..1024], stream(1, 512)[..]);
        assert_eq!(data[1024..], stream(2, 76)[..]);
        let mut pieces = vec![0u8; 1100];
        let mut key_stream = ChunkedKeyStream::new(&key, 512);
        key_stream.decrypt(&mut pieces[68..600], 68);
        key_stream.decrypt(&mut pieces[600..], 600);
        assert_eq!(pieces, data);
    }
}
