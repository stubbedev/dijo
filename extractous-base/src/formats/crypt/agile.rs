//! Agile encryption of an OOXML package, as POI's `AgileEncryptionInfoBuilder` reads its XML
//! descriptor and `AgileDecryptor` opens it: the key data and data integrity, and the password
//! key encryptor whose verifier a password is checked against and whose encrypted key it opens.
//! AES in CBC mode is ported; the other ciphers and chaining modes POI knows Office never
//! writes, and are left to Tika.

use std::collections::HashMap;

use crate::formats::xml::{attribute_named, parse_sax, Attribute, SaxHandler};
use crate::formats::Failure;

use super::{block_of, filled, generate_key, hash_password, AesKey, Hash, Unopened};

/// The namespaces of the descriptor's elements.
const ENCRYPTION_NAMESPACE: &str = "http://schemas.microsoft.com/office/2006/encryption";
const PASSWORD_NAMESPACE: &str = "http://schemas.microsoft.com/office/2006/keyEncryptor/password";
/// The block keys of the derivations (`AgileDecryptor.k*Block`).
const VERIFIER_INPUT_BLOCK: [u8; 8] = 0xfea7_d276_3b4b_9e79u64.to_be_bytes();
const HASHED_VERIFIER_BLOCK: [u8; 8] = 0xd7aa_0f6d_3061_344eu64.to_be_bytes();
const CRYPTO_KEY_BLOCK: [u8; 8] = 0x146e_0be7_abac_d0d6u64.to_be_bytes();
const INTEGRITY_KEY_BLOCK: [u8; 8] = 0x5fb2_ad01_0cb9_e1f6u64.to_be_bytes();
const INTEGRITY_VALUE_BLOCK: [u8; 8] = 0xa067_7f02_b22c_8433u64.to_be_bytes();
/// The chunks `AgileCipherInputStream` decrypts the package in, each with the IV of its number.
const SEGMENT: usize = 4096;
/// The most hashing rounds this port spends on a password; a descriptor asking for more keeps
/// Tika's parse hashing, which is left to it.
const MAX_SPIN_COUNT: i32 = 10_000_000;

fn unparsed() -> Unopened {
    Unopened::Thrown("Unable to parse encryption descriptor".to_owned())
}

/// The attributes of the first element of each name the descriptor holds, as
/// `getElementsByTagNameNS(...).item(0)` finds it.
#[derive(Default)]
struct Elements(HashMap<(String, String), Vec<Attribute>>);

impl SaxHandler for Elements {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure> {
        self.0
            .entry((namespace.to_owned(), local_name.to_owned()))
            .or_insert_with(|| attributes.to_vec());
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }

    /// POI's `DocumentHelper` disallows a document type declaration.
    fn document_type(&mut self) -> Result<(), Failure> {
        Err(Failure::Malformed(
            "the encryption descriptor declares a document type".to_owned(),
        ))
    }
}

/// The attributes of an element of the descriptor, read as `getAttribute` reads them.
struct Element<'a>(&'a [Attribute]);

impl Element<'_> {
    fn text(&self, name: &str) -> &str {
        attribute_named(self.0, name).unwrap_or_default()
    }

    /// `getIntAttr`: `None` for an empty attribute.
    fn int(&self, name: &str) -> Result<Option<i32>, Unopened> {
        let text = self.text(name);
        if text.is_empty() {
            return Ok(None);
        }
        text.parse()
            .map(Some)
            .map_err(|_| Unopened::Thrown(format!("For input string: \"{text}\"")))
    }

    /// `getBinAttr`: `None` for an empty attribute.
    fn binary(&self, name: &str) -> Result<Option<Vec<u8>>, Unopened> {
        let text = self.text(name);
        if text.is_empty() {
            return Ok(None);
        }
        base64(text).map(Some)
    }
}

/// `Base64.getDecoder().decode`: the strict basic alphabet, its padding optional.
fn base64(text: &str) -> Result<Vec<u8>, Unopened> {
    let illegal = || Unopened::Thrown("Illegal base64 character".to_owned());
    let trimmed = text.trim_end_matches('=');
    if text.len() - trimmed.len() > 2 {
        return Err(illegal());
    }
    let mut out = Vec::with_capacity(trimmed.len() / 4 * 3);
    let mut bits = 0u32;
    let mut count = 0;
    for byte in trimmed.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(illegal()),
        };
        bits = (bits << 6) | u32::from(value);
        count += 1;
        if count == 4 {
            out.extend_from_slice(&bits.to_be_bytes()[1..]);
            bits = 0;
            count = 0;
        }
    }
    match count {
        0 => {}
        2 => out.push((bits >> 4) as u8),
        3 => out.extend_from_slice(&((bits >> 2) as u16).to_be_bytes()),
        _ => {
            return Err(Unopened::Thrown(
                "Last unit does not have enough valid bits".to_owned(),
            ))
        }
    }
    Ok(out)
}

/// The cipher of a key data or key encryptor, which this port opens only as AES in CBC mode.
fn aes_cbc(element: &Element, key_bits: Option<i32>) -> Result<(), Unopened> {
    let cipher = element.text("cipherAlgorithm");
    let bits = key_bits.unwrap_or(-1);
    let known = match cipher {
        "AES" => [128, 192, 256].contains(&bits),
        "RC2" | "RC4" | "DES" | "DESX" | "3DES" | "3DES_112" => true,
        _ => false,
    };
    if !known {
        return Err(Unopened::Thrown(format!(
            "cipher algorithm {cipher}/{bits} not found"
        )));
    }
    let chaining = element.text("cipherChaining");
    if !matches!(chaining, "ChainingModeCBC" | "ChainingModeCFB") {
        return Err(Unopened::Thrown(
            "Cipher algorithm, chaining mode or hash algorithm was null".to_owned(),
        ));
    }
    if cipher != "AES" || chaining != "ChainingModeCBC" {
        return Err(Unopened::Unported(format!(
            "agile encryption with {cipher} in {chaining}, which this port does not decrypt"
        )));
    }
    Ok(())
}

/// `generateIv`: the salt, hashed with a block key where there is one, cut or filled with 0x36
/// to the block size, as the IV of AES.
fn iv(
    hash: Hash,
    salt: &[u8],
    block_key: Option<&[u8]>,
    block_size: usize,
) -> Result<[u8; 16], Unopened> {
    let hashed = match block_key {
        Some(block_key) => hash.digest(&[salt, block_key]),
        None => salt.to_vec(),
    };
    filled(&hashed, block_size, 0x36)
        .try_into()
        .map_err(|_| Unopened::Thrown("Wrong IV length: must be 16 bytes long".to_owned()))
}

/// Agile encryption: the key data's hash, key size, block size and salt; its data integrity;
/// and the password key encryptor.
#[derive(Debug, Clone)]
pub struct Agile {
    hash: Hash,
    key_bytes: usize,
    block_size: usize,
    key_salt: Vec<u8>,
    encrypted_hmac_key: Option<Vec<u8>>,
    encrypted_hmac_value: Option<Vec<u8>>,
    password_hash: Hash,
    password_key_bytes: usize,
    password_block_size: usize,
    salt: Vec<u8>,
    spin_count: u32,
    encrypted_verifier: Option<Vec<u8>>,
    encrypted_verifier_hash: Option<Vec<u8>>,
    encrypted_key: Option<Vec<u8>>,
}

impl Agile {
    /// `parseDescriptor`, and the header and verifier POI makes of the descriptor.
    pub(super) fn read(descriptor: &[u8]) -> Result<Agile, Unopened> {
        let mut elements = Elements::default();
        parse_sax(descriptor, "the encryption descriptor", &mut elements)
            .map_err(|_| unparsed())?;
        let get = |namespace: &str, name: &str| {
            elements
                .0
                .get(&(namespace.to_owned(), name.to_owned()))
                .map(|attributes| Element(attributes))
        };
        get(ENCRYPTION_NAMESPACE, "encryption").ok_or_else(unparsed)?;
        let key_data = get(ENCRYPTION_NAMESPACE, "keyData").ok_or_else(unparsed)?;
        let key_bits = key_data.int("keyBits")?;
        aes_cbc(&key_data, key_bits)?;
        let hash = Hash::from_ecma_name(key_data.text("hashAlgorithm"))?;
        let integrity = get(ENCRYPTION_NAMESPACE, "dataIntegrity");
        get(ENCRYPTION_NAMESPACE, "keyEncryptors").ok_or_else(unparsed)?;
        let hash_size = key_data
            .int("hashSize")?
            .ok_or_else(|| Unopened::Thrown("Invalid hash size: null".to_owned()))?;
        if hash.size() as i32 != hash_size {
            return Err(Unopened::Thrown("Unsupported hash algorithm".to_owned()));
        }
        let salt_size = key_data
            .int("saltSize")?
            .ok_or_else(|| Unopened::Thrown("Invalid salt length: null".to_owned()))?;
        let key_salt = key_data.binary("saltValue")?.unwrap_or_default();
        if key_salt.len() as i32 != salt_size {
            return Err(Unopened::Thrown("Invalid salt length".to_owned()));
        }
        let password = get(PASSWORD_NAMESPACE, "encryptedKey")
            .ok_or_else(|| Unopened::Thrown("encryptedKey not set".to_owned()))?;
        let password_bits = password.int("keyBits")?;
        aes_cbc(&password, password_bits)?;
        let password_hash = Hash::from_ecma_name(password.text("hashAlgorithm"))?;
        let password_hash_size = password
            .int("hashSize")?
            .ok_or_else(|| Unopened::Thrown("encryptedKey not set".to_owned()))?;
        let password_block_size = password
            .int("blockSize")?
            .ok_or_else(|| Unopened::Thrown("blockSize not set".to_owned()))?;
        if password_hash.size() as i32 != password_hash_size {
            return Err(Unopened::Thrown("Unsupported hash algorithm".to_owned()));
        }
        let salt = password.binary("saltValue")?.unwrap_or_default();
        if password.int("saltSize")? != Some(salt.len() as i32) {
            return Err(Unopened::Thrown("Invalid salt size".to_owned()));
        }
        let spin_count = password.int("spinCount")?.unwrap_or_default();
        if spin_count > MAX_SPIN_COUNT {
            return Err(Unopened::Unported(format!(
                "agile encryption of {spin_count} hashing rounds, more than this port spends"
            )));
        }
        let length = |bits: Option<i32>| bits.map_or(0, |bits| bits.max(0) as usize / 8);
        let block_size = key_data.int("blockSize")?.unwrap_or_default().max(0) as usize;
        let integrity_value = |name: &str| {
            integrity
                .as_ref()
                .map(|element| element.binary(name))
                .transpose()
                .map(Option::flatten)
        };
        Ok(Agile {
            hash,
            key_bytes: length(key_bits),
            block_size,
            key_salt,
            encrypted_hmac_key: integrity_value("encryptedHmacKey")?,
            encrypted_hmac_value: integrity_value("encryptedHmacValue")?,
            password_hash,
            password_key_bytes: length(password_bits),
            password_block_size: password_block_size.max(0) as usize,
            salt,
            spin_count: spin_count.max(0) as u32,
            encrypted_verifier: password.binary("encryptedVerifierHashInput")?,
            encrypted_verifier_hash: password.binary("encryptedVerifierHashValue")?,
            encrypted_key: password.binary("encryptedKeyValue")?,
        })
    }

    /// `hashInput`: `input` decrypted with the key the password hash and a block key derive.
    fn hash_input(
        &self,
        password_hash: &[u8],
        block_key: &[u8],
        input: Option<&[u8]>,
    ) -> Result<Vec<u8>, Unopened> {
        let key = generate_key(
            password_hash,
            self.password_hash,
            block_key,
            self.password_key_bytes,
        );
        let key = AesKey::new(&key)?;
        let iv = iv(
            self.password_hash,
            &self.salt,
            None,
            self.password_block_size,
        )?;
        let input = input
            .ok_or_else(|| Unopened::Thrown("Cannot has input without inputKey".to_owned()))?;
        let padded = block_of(
            input,
            input.len().div_ceil(self.password_block_size) * self.password_block_size,
        );
        key.decrypt_cbc_whole(&iv, &padded)
    }

    /// `AgileDecryptor.verifyPassword`: the package's key, where the password opens it.
    pub(super) fn verify(&self, password: &str) -> Result<Option<AgileKey>, Unopened> {
        if self.password_block_size == 0 {
            return Err(Unopened::Thrown("a block size of 0".to_owned()));
        }
        let hashed = hash_password(password, self.password_hash, &self.salt, self.spin_count);
        let verifier = self.hash_input(
            &hashed,
            &VERIFIER_INPUT_BLOCK,
            self.encrypted_verifier.as_deref(),
        )?;
        let verifier_hash = self.password_hash.digest(&[&verifier]);
        let decrypted_hash = self.hash_input(
            &hashed,
            &HASHED_VERIFIER_BLOCK,
            self.encrypted_verifier_hash.as_deref(),
        )?;
        let decrypted_hash = block_of(&decrypted_hash, self.password_hash.size());
        let key = self.hash_input(&hashed, &CRYPTO_KEY_BLOCK, self.encrypted_key.as_deref())?;
        let key = AesKey::new(&block_of(&key, self.key_bytes))?;
        for (block_key, encrypted) in [
            (INTEGRITY_KEY_BLOCK, &self.encrypted_hmac_key),
            (INTEGRITY_VALUE_BLOCK, &self.encrypted_hmac_value),
        ] {
            let iv = iv(self.hash, &self.key_salt, Some(&block_key), self.block_size)?;
            let encrypted = encrypted
                .as_deref()
                .ok_or_else(|| Unopened::Thrown("null input buffer".to_owned()))?;
            key.decrypt_cbc_whole(&iv, encrypted)?;
        }
        Ok((decrypted_hash == verifier_hash).then_some(AgileKey {
            key,
            hash: self.hash,
            key_salt: self.key_salt.clone(),
            block_size: self.block_size,
        }))
    }
}

/// The key of an agile encrypted package.
#[derive(Debug, Clone)]
pub struct AgileKey {
    key: AesKey,
    hash: Hash,
    key_salt: Vec<u8>,
    block_size: usize,
}

impl AgileKey {
    /// `AgileCipherInputStream` read to `length` bytes: each 4096-byte chunk of `data`
    /// decrypted with the IV of its number, failing where POI's stream runs out or leaves part
    /// of a chunk it hands on undecrypted.
    pub(super) fn decrypt(&self, data: &[u8], length: usize) -> Result<Vec<u8>, Unopened> {
        let mut out = Vec::with_capacity(length);
        for (number, chunk) in data.chunks(SEGMENT).enumerate() {
            if out.len() >= length {
                break;
            }
            let wanted = (length - out.len()).min(SEGMENT);
            let whole = chunk.len() - chunk.len() % 16;
            if whole < wanted {
                return Err(Unopened::Unported(
                    "an encrypted package that ends before its length or within a cipher \
                     block, which POI reads short"
                        .to_owned(),
                ));
            }
            let iv = iv(
                self.hash,
                &self.key_salt,
                Some(&(number as u32).to_le_bytes()),
                self.block_size,
            )?;
            out.extend_from_slice(&self.key.decrypt_cbc(&iv, &chunk[..whole])[..wanted]);
        }
        if out.len() < length {
            return Err(Unopened::Unported(
                "an encrypted package that ends before its length, which POI reads short"
                    .to_owned(),
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_as_java_does() {
        assert_eq!(base64("aGk=").unwrap(), b"hi");
        assert_eq!(base64("aGk").unwrap(), b"hi");
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert!(base64("a").is_err());
        assert!(base64("a b").is_err());
    }

    #[test]
    fn fails_on_a_descriptor_poi_cannot_parse() {
        assert!(matches!(
            Agile::read(b"<encryption/>"),
            Err(Unopened::Thrown(reason)) if reason.contains("Unable to parse")
        ));
        assert!(Agile::read(b"<!DOCTYPE x><x/>").is_err());
    }
}
