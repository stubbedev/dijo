//! PDFBox's `StandardSecurityHandler.prepareForDecryption` as Tika's `PDFParser` opens a document
//! with the password its `PasswordProvider` gives, or the empty one: as its owner when the
//! password is the owner password, else as a user when it is the user password, which grants
//! what the permission bits `P` grant; any other password fails the document.
//!
//! The RC4 revisions 2 to 4 are checked here as PDFBox checks them, but for the key of an owner
//! password shorter than 16 bytes: the specification hashes the whole digest again and again,
//! PDFBox and qpdf only its first key-length bytes, and writers follow either, so both are tried
//! (#129). `lopdf` then decrypts the document with the user password, given the entries as
//! PDFBox read them. The AES-256 revisions 5 and 6 are checked by `lopdf`, with the password
//! encoded as PDFBox encodes it.
//!
//! The entries are read as PDFBox's getters read them, a reference resolved and a mistyped
//! number taken for the default, and where PDFBox throws on them, on a key length its digest
//! does not hold, a missing key or a document ID that is no string, the document fails with what
//! Tika's `PDFParser` fails with on the JVM. The public-key security handler fails for want of
//! the key store Tika never gives it.

use lopdf::encryption::PasswordAlgorithm;
use lopdf::{Dictionary, Document, Object};
use md5::{Digest, Md5};
use rc4::{KeyInit, Rc4, StreamCipher};

use super::cos::{class_cast, name_string, text_string};
use super::Unported;
use crate::formats::thrown::{Exception, Thrown};

/// What a document lets the one who opened it do, as PDFBox's `AccessPermission` holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Access {
    Unencrypted,
    /// Opened with the owner password: everything.
    Owner,
    /// Opened with the user password: what the permission bits `P` grant.
    User(u64),
}

impl Access {
    /// `AccessPermission.isPermissionBitOn`.
    pub(super) fn grants(self, bit: u32) -> bool {
        match self {
            Self::Unencrypted | Self::Owner => true,
            Self::User(permissions) => permissions & (1 << (bit - 1)) != 0,
        }
    }
}

/// Why a document did not open.
#[derive(Debug)]
pub(super) enum Refusal {
    /// The password is neither the owner's nor the user's: PDFBox's `InvalidPasswordException`.
    WrongPassword,
    /// PDFBox has no security handler of this filter name, `null` for none: its `IOException`,
    /// which Tika's `PDFParser` throws as an `EncryptedDocumentException`.
    NoSecurityHandler(String),
    /// PDFBox throws this, which Tika's `PDFParser` throws on.
    Thrown(Thrown),
    Unported(Unported),
}

/// The filter of the standard security handler.
const STANDARD_FILTER: &str = "Standard";
/// The filter of the public-key security handler.
const PUBLIC_KEY_FILTER: &str = "Adobe.PubSec";
/// What `PublicKeySecurityHandler.prepareForDecryption` throws on the password Tika's
/// `PDFParser` opens every document with, where it wants a key store.
const NO_KEY_STORE: &str =
    "Provided decryption material is not compatible with the document - did you pass a null \
     keyStore?";

/// The first entry of a document's `ID`, as `getDocumentIDBytes` reads it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum DocumentId {
    /// A string's bytes, or none where the trailer has no `ID` array, or an empty one.
    Bytes(Vec<u8>),
    /// Another object, which PDFBox fails to cast to a string.
    NotAString(Object),
    /// `null`, whose bytes PDFBox fails to take.
    Null,
}

impl DocumentId {
    /// The bytes PDFBox hashes into the key, or what it throws reading them.
    fn bytes(self) -> Result<Vec<u8>, Refusal> {
        match self {
            Self::Bytes(bytes) => Ok(bytes),
            Self::NotAString(object) => Err(runtime(class_cast(&object, "COSString"))),
            Self::Null => Err(runtime(Exception::new(
                NULL_POINTER,
                "Cannot invoke \"org.apache.pdfbox.cos.COSString.getBytes()\" because \"id\" is \
                 null",
            ))),
        }
    }
}

const NULL_POINTER: &str = "java.lang.NullPointerException";
const ILLEGAL_ARGUMENT: &str = "java.lang.IllegalArgumentException";
const OUT_OF_BOUNDS: &str = "java.lang.ArrayIndexOutOfBoundsException";

fn runtime(exception: Exception) -> Refusal {
    Refusal::Thrown(Thrown::Runtime(exception))
}

fn io(exception: Exception) -> Refusal {
    Refusal::Thrown(Thrown::Io(exception))
}

impl From<Unported> for Refusal {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

/// `InvalidPasswordException`'s message.
pub(super) const WRONG_PASSWORD: &str = "Cannot decrypt PDF, the password is incorrect";

/// How a document opens with a password, before `lopdf` loads it.
pub(super) enum Opening {
    /// Unencrypted: the password is not asked for.
    Unencrypted,
    /// Encrypted by the standard security handler: the first document ID PDFBox hashes into the
    /// key, and how the password opens the document.
    Standard {
        document_id: Vec<u8>,
        encryption: Encryption,
    },
}

/// The encryption of a document of the standard security handler.
pub(super) enum Encryption {
    /// RC4, or AES-128, of revisions 2 to 4, which PDFBox opens with the password: what it
    /// grants, the user password `lopdf` decrypts the document with, and the entries of the
    /// encryption dictionary as PDFBox reads them, which `lopdf` is given in their place.
    Rc4 {
        access: Access,
        user_password: Vec<u8>,
        entries: Dictionary,
    },
    /// AES-256, which `lopdf` checks the password against.
    Aes256(Aes256Revision),
}

/// The revisions of the standard security handler that encrypt with AES-256.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Aes256Revision {
    R5,
    R6,
}

/// `ENCRYPT_PADDING`, which pads a password to 32 bytes.
const PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

impl Opening {
    /// How the document of the encryption dictionary and first document ID `encryption`, if it
    /// has one, opens with `password`, as `COSParser.prepareDecryption` opens it: by the
    /// security handler its filter names, the public-key one failing for want of a key store,
    /// then by the standard one's `prepareForDecryption`, which reads the document ID and the
    /// owner and user keys before it checks the password by the revision.
    pub(super) fn of(
        encryption: Option<(Dictionary, DocumentId)>,
        password: &str,
    ) -> Result<Self, Refusal> {
        let Some((dictionary, id)) = encryption else {
            return Ok(Self::Unencrypted);
        };
        match filter(&dictionary).as_deref() {
            Some(STANDARD_FILTER) => {}
            Some(PUBLIC_KEY_FILTER) => {
                return Err(io(Exception::new("java.io.IOException", NO_KEY_STORE)))
            }
            filter => {
                return Err(Refusal::NoSecurityHandler(
                    filter.unwrap_or("null").to_owned(),
                ))
            }
        }
        let document_id = id.bytes()?;
        let user = key(&dictionary, b"U")?;
        let owner = key(&dictionary, b"O")?;
        let encryption = match int(&dictionary, b"R", 0) {
            2..=4 => Rc4Handler::read(&dictionary, document_id.clone(), owner, user)?
                .opening(password)?,
            5 => Encryption::Aes256(Aes256Revision::R5),
            6 => Encryption::Aes256(Aes256Revision::R6),
            revision => {
                return Err(io(Exception::new(
                    "java.io.IOException",
                    format!("Unknown Encryption Revision {revision}"),
                )))
            }
        };
        Ok(Self::Standard {
            document_id,
            encryption,
        })
    }
}

/// `getOwnerKey` and `getUserKey`: the bytes of the string `name`, if the dictionary has it,
/// cut or padded to 32 bytes for the RC4 revisions, or what PDFBox throws casting another
/// object to a string.
fn key(dictionary: &Dictionary, name: &[u8]) -> Result<Option<Vec<u8>>, Refusal> {
    match dictionary.get(name) {
        Err(_) => Ok(None),
        Ok(Object::String(bytes, _)) => {
            let mut bytes = bytes.clone();
            if int(dictionary, b"R", 0) <= 4 {
                bytes.resize(32, 0);
            }
            Ok(Some(bytes))
        }
        Ok(other) => Err(runtime(class_cast(other, "COSString"))),
    }
}

/// The standard security handler of revisions 2 to 4, as PDFBox checks passwords against it.
struct Rc4Handler {
    revision: i32,
    /// The key's length in bytes.
    length: usize,
    /// `O`, cut or padded to 32 bytes.
    owner: Vec<u8>,
    /// `U`, cut or padded to 32 bytes; a handler of revision 2 without it matches no password.
    user: Option<Vec<u8>>,
    permissions: i32,
    id: Vec<u8>,
    encrypt_metadata: bool,
}

impl Rc4Handler {
    /// The handler of the encryption dictionary `dictionary` of revision 2 to 4, with the
    /// document ID `id` and the keys `owner` and `user`, if it has them, or what PDFBox throws
    /// on its key length or its missing keys whatever the password.
    fn read(
        dictionary: &Dictionary,
        id: Vec<u8>,
        owner: Option<Vec<u8>>,
        user: Option<Vec<u8>>,
    ) -> Result<Self, Refusal> {
        let revision = int(dictionary, b"R", 0);
        let version = int(dictionary, b"V", 0);
        let mut length = if version == 1 {
            5
        } else {
            int(dictionary, b"Length", 40) / 8
        };
        if version == 4 || version == 5 {
            let method = dictionary
                .get(b"CF")
                .and_then(Object::as_dict)
                .and_then(|filters| filters.get(b"StdCF"))
                .and_then(Object::as_dict)
                .and_then(|filter| filter.get(b"CFM"))
                .and_then(Object::as_name);
            let bytes = match method {
                Ok(b"AESV2") => Some(16),
                Ok(b"AESV3") => Some(32),
                _ => None,
            };
            if let Some(bytes) = bytes {
                length = match dictionary.has(b"Length") {
                    true => bytes.min(int(dictionary, b"Length", 40) / 8),
                    false => bytes,
                };
            }
        }
        let length = rc4_key_length(revision, length)?;
        let illegal_key = || {
            runtime(Exception::new(
                ILLEGAL_ARGUMENT,
                "number of bytes must be between 1 and 32",
            ))
        };
        let owner = match owner {
            Some(owner) => owner,
            None if revision == 2 && length == 0 => return Err(illegal_key()),
            None => {
                let array = if revision == 2 { "data" } else { "owner" };
                return Err(runtime(Exception::new(
                    NULL_POINTER,
                    format!("Cannot read the array length because \"{array}\" is null"),
                )));
            }
        };
        if length == 0 {
            return Err(illegal_key());
        }
        if user.is_none() && revision != 2 {
            return Err(runtime(Exception::new(
                NULL_POINTER,
                "Cannot read the array length because \"original\" is null",
            )));
        }
        Ok(Self {
            revision,
            length,
            owner,
            user,
            permissions: int(dictionary, b"P", 0),
            id,
            encrypt_metadata: !matches!(
                dictionary.get(b"EncryptMetadata"),
                Ok(Object::Boolean(false))
            ),
        })
    }

    /// `prepareForDecryption`'s check of `password`.
    fn opening(&self, password: &str) -> Result<Encryption, Refusal> {
        let bytes: Vec<u8> = password
            .chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect();
        let recovered = OwnerKeyHash::ALL
            .into_iter()
            .map(|hash| self.user_password_of_owner(&bytes, hash))
            .find(|recovered| self.is_user_password(recovered));
        let (access, user_password) = if let Some(recovered) = recovered {
            (Access::Owner, unpadded(&recovered))
        } else if self.is_user_password(&bytes) {
            (Access::User(u64::from(self.permissions as u32)), bytes)
        } else {
            return Err(Refusal::WrongPassword);
        };
        Ok(Encryption::Rc4 {
            access,
            user_password,
            entries: self.entries(),
        })
    }

    /// The entries of the encryption dictionary the key is made of, as PDFBox reads them.
    fn entries(&self) -> Dictionary {
        let mut entries = standard_entries();
        let length = i64::try_from(self.length * 8).expect("a key of at most 16 bytes");
        entries.set("R", Object::Integer(i64::from(self.revision)));
        entries.set("P", Object::Integer(i64::from(self.permissions)));
        entries.set("Length", Object::Integer(length));
        entries.set("O", Object::string_literal(self.owner.clone()));
        if let Some(user) = &self.user {
            entries.set("U", Object::string_literal(user.clone()));
        }
        entries
    }

    /// `computeEncryptedKeyRev234`: Algorithm 2.
    fn key(&self, password: &[u8]) -> Vec<u8> {
        let mut md5 = Md5::new();
        md5.update(padded(password));
        md5.update(&self.owner);
        md5.update(self.permissions.to_le_bytes());
        md5.update(&self.id);
        if self.revision == 4 && !self.encrypt_metadata {
            md5.update([0xFF; 4]);
        }
        let mut digest = md5.finalize().to_vec();
        if self.revision >= 3 {
            for _ in 0..50 {
                digest = Md5::digest(&digest[..self.length]).to_vec();
            }
        }
        digest.truncate(self.length);
        digest
    }

    /// `isUserPassword234`: whether `computeUserPassword` makes `U` of the password, all of it
    /// for revision 2 and its first 16 bytes after.
    fn is_user_password(&self, password: &[u8]) -> bool {
        let Some(user) = &self.user else {
            return false;
        };
        let key = self.key(password);
        if self.revision == 2 {
            return rc4(&key, &PADDING) == *user;
        }
        let mut hashed = Md5::new();
        hashed.update(PADDING);
        hashed.update(&self.id);
        let mut data = hashed.finalize().to_vec();
        for round in 0..20u8 {
            let iteration: Vec<u8> = key.iter().map(|byte| byte ^ round).collect();
            data = rc4(&iteration, &data);
        }
        data[..16] == user[..16]
    }

    /// `getUserPassword234`: the padded user password `O` holds for the owner password, its key
    /// hashed as `hash` says.
    fn user_password_of_owner(&self, owner_password: &[u8], hash: OwnerKeyHash) -> Vec<u8> {
        let mut digest = Md5::digest(padded(owner_password)).to_vec();
        if self.revision >= 3 {
            let hashed = hash.hashed(self.length);
            for _ in 0..50 {
                digest = Md5::digest(&digest[..hashed]).to_vec();
            }
        }
        let key = &digest[..self.length];
        if self.revision == 2 {
            return rc4(key, &self.owner);
        }
        let mut data = self.owner.clone();
        for round in (0..20u8).rev() {
            let iteration: Vec<u8> = key.iter().map(|byte| byte ^ round).collect();
            data = rc4(&iteration, &data);
        }
        data
    }
}

/// How much of each digest the 50 rounds of revision 3 and later hash again when the key of an
/// owner password is derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerKeyHash {
    /// The first key-length bytes, as PDFBox's `computeRC4key` and qpdf hash them.
    KeyLength,
    /// All 16 bytes, as the specification's Algorithm 3 says.
    Whole,
}

impl OwnerKeyHash {
    const ALL: [Self; 2] = [Self::KeyLength, Self::Whole];

    /// How many bytes of each digest are hashed again, for a key of `length` bytes.
    fn hashed(self, length: usize) -> usize {
        match self {
            Self::KeyLength => length,
            Self::Whole => 16,
        }
    }
}

/// `truncateOrPad`.
fn padded(password: &[u8]) -> [u8; 32] {
    let mut padded = PADDING;
    let length = password.len().min(32);
    padded[..length].copy_from_slice(&password[..length]);
    padded[length..].copy_from_slice(&PADDING[..32 - length]);
    padded
}

/// A padded password without its padding: the shortest password that pads to it.
fn unpadded(padded: &[u8]) -> Vec<u8> {
    let length = (0..=padded.len())
        .find(|&length| padded[length..] == PADDING[..padded.len() - length])
        .unwrap_or(padded.len());
    padded[..length].to_vec()
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    Rc4::new_from_slice(key)
        .expect("an RC4 key of 1 to 16 bytes")
        .apply_keystream(&mut out);
    out
}

/// `computeRC4key`'s key length, in bytes, as far as PDFBox takes it: the digest of 16 bytes
/// holds no longer key, and a negative length fails the hashing of revision 3 and later and the
/// key of revision 2.
fn rc4_key_length(revision: i32, length: i32) -> Result<usize, Refusal> {
    const DIGEST: i32 = 16;
    let hashed = revision != 2;
    match usize::try_from(length) {
        Ok(bytes) if length <= DIGEST => Ok(bytes),
        Ok(_) if hashed => Err(io(Exception::wrapping(
            "java.io.IOException",
            Exception::new(ILLEGAL_ARGUMENT, "Input buffer too short"),
        ))),
        Ok(_) => Err(runtime(Exception::new(
            OUT_OF_BOUNDS,
            format!("arraycopy: last source index {length} out of bounds for byte[{DIGEST}]"),
        ))),
        Err(_) if hashed => Err(runtime(Exception::new(
            OUT_OF_BOUNDS,
            format!("Range [0, 0 + {length}) out of bounds for length {DIGEST}"),
        ))),
        Err(_) => Err(runtime(Exception::new(
            "java.lang.NegativeArraySizeException",
            length.to_string(),
        ))),
    }
}

/// The entries of an encryption dictionary `lopdf` is given for a document PDFBox opens with
/// the standard security handler, whatever the type its filter is named by.
pub(super) fn standard_entries() -> Dictionary {
    let mut entries = Dictionary::new();
    entries.set("Filter", Object::Name(STANDARD_FILTER.as_bytes().to_vec()));
    entries
}

/// `PDEncryption.getFilter`: the security handler's name, a name or a text string.
fn filter(dictionary: &Dictionary) -> Option<String> {
    match dictionary.get(b"Filter") {
        Ok(Object::Name(name)) => Some(name_string(name)),
        Ok(Object::String(bytes, _)) => Some(text_string(bytes)),
        _ => None,
    }
}

/// `COSDictionary.getInt`: a number entry as Java's `int` holds it, or `default`.
fn int(dictionary: &Dictionary, key: &[u8], default: i32) -> i32 {
    match dictionary.get(key) {
        Ok(Object::Integer(value)) => *value as i32,
        Ok(Object::Real(value)) => *value as i32,
        _ => default,
    }
}

/// The bytes PDFBox makes of `password` for the AES-256 encryption of revision `revision` of
/// `doc`: its UTF-8 for revision 5, after SASLprep for revision 6.
pub(super) fn aes256_password(
    doc: &Document,
    revision: Aes256Revision,
    password: &str,
) -> Result<Vec<u8>, Refusal> {
    match revision {
        Aes256Revision::R5 => Ok(password.as_bytes().to_vec()),
        Aes256Revision::R6 => PasswordAlgorithm::try_from(doc)
            .map_err(|error| Unported(format!("encryption lopdf cannot read: {error}")))?
            .sanitize_password(password)
            .map_err(|error| Unported(format!("a password SASLprep rejects: {error}")).into()),
    }
}

/// Whom a password opens an AES-256 document as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Opener {
    Owner,
    User,
}

impl Opener {
    /// What the opener may do in a document that grants its user the permission bits
    /// `permissions`.
    pub(super) fn access(self, permissions: u64) -> Access {
        match self {
            Self::Owner => Access::Owner,
            Self::User => Access::User(permissions),
        }
    }
}

/// Whom the password of the bytes `password` opens the AES-256 document `doc` as.
pub(super) fn aes256_opener(doc: &Document, password: &[u8]) -> Result<Opener, Refusal> {
    if doc.authenticate_raw_owner_password(password).is_ok() {
        return Ok(Opener::Owner);
    }
    if doc.authenticate_raw_user_password(password).is_ok() {
        return Ok(Opener::User);
    }
    Err(Refusal::WrongPassword)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{document, page_objects, replace, TEXT};
    use super::super::PdfParser;
    use super::*;
    use crate::config::ParserConfigs;
    use crate::formats::metadata::TikaMetadata;
    use crate::formats::{Decline, Parser};

    #[test]
    fn grants_what_the_permission_bits_grant_to_a_user() {
        let user = Access::User(0xFFFF_FFEC);
        assert!(user.grants(3) && user.grants(4) && !user.grants(2));
        let no_extraction = Access::User(-532_i64 as u64);
        assert!(!no_extraction.grants(5) && !no_extraction.grants(10) && no_extraction.grants(3));
        assert!(Access::Owner.grants(5) && Access::Unencrypted.grants(10));
    }

    /// The handler PDFBox's `computeOwnerPassword` and `computeUserPassword` would write for
    /// the passwords, of revision 3 and a 40-bit key, the owner key hashed as `hash` says.
    fn handler(owner_password: &[u8], user_password: &[u8], hash: OwnerKeyHash) -> Rc4Handler {
        let mut handler = Rc4Handler {
            revision: 3,
            length: 5,
            owner: Vec::new(),
            user: Some(vec![0; 32]),
            permissions: -3904,
            id: b"0123456789abcdef".to_vec(),
            encrypt_metadata: true,
        };
        let mut digest = Md5::digest(padded(owner_password)).to_vec();
        for _ in 0..50 {
            digest = Md5::digest(&digest[..hash.hashed(handler.length)]).to_vec();
        }
        let key = &digest[..handler.length];
        let mut owner = rc4(key, &padded(user_password));
        for round in 1..20u8 {
            let iteration: Vec<u8> = key.iter().map(|byte| byte ^ round).collect();
            owner = rc4(&iteration, &owner);
        }
        handler.owner = owner;
        let encryption_key = handler.key(user_password);
        let mut hashed = Md5::new();
        hashed.update(PADDING);
        hashed.update(&handler.id);
        let mut user = hashed.finalize().to_vec();
        for round in 0..20u8 {
            let iteration: Vec<u8> = encryption_key.iter().map(|byte| byte ^ round).collect();
            user = rc4(&iteration, &user);
        }
        user.extend(&PADDING[..16]);
        handler.user = Some(user);
        handler
    }

    /// What `handler` opens with `password`, and the user password it then decrypts with.
    fn opened(handler: &Rc4Handler, password: &str) -> Option<(Access, String)> {
        match handler.opening(password) {
            Ok(Encryption::Rc4 {
                access,
                user_password,
                ..
            }) => Some((access, String::from_utf8(user_password).unwrap())),
            Ok(_) => unreachable!("RC4 encryption"),
            Err(Refusal::WrongPassword) => None,
            Err(refusal) => panic!("{refusal:?}"),
        }
    }

    #[test]
    fn opens_with_the_owner_or_user_password_as_pdfbox_does() {
        let handler = handler(b"ejer", b"bruger", OwnerKeyHash::KeyLength);
        let opened = |password: &str| opened(&handler, password);
        assert_eq!(opened("ejer"), Some((Access::Owner, "bruger".to_owned())));
        assert_eq!(
            opened("bruger"),
            Some((
                Access::User(u64::from(-3904_i32 as u32)),
                "bruger".to_owned()
            ))
        );
        assert_eq!(opened("forkert"), None);
        for password in [&b""[..], b"(", b"(\xbfN", b"bruger"] {
            assert_eq!(padded(&unpadded(&padded(password))), padded(password));
        }
        assert_eq!(unpadded(&padded(b"bruger")), b"bruger");
    }

    /// The document ID of the documents the tests encrypt.
    const ID: &[u8] = b"0123456789abcdef";

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02X}")).collect()
    }

    /// The page of text of `page_objects`, with the trailer's `ID` entry `id` and its
    /// `Encrypt` entry `encryption`, and the objects `extra` numbered from 6.
    fn with_encryption(encryption: &str, id: &str, extra: &[Vec<u8>]) -> Vec<u8> {
        let mut objects: Vec<Vec<u8>> = page_objects(TEXT)
            .into_iter()
            .map(String::into_bytes)
            .collect();
        objects.extend(extra.iter().cloned());
        replace(
            &document(&objects, b"", &[]),
            b"/Root 1 0 R >>",
            format!("/Root 1 0 R /Encrypt {encryption} {id} >>").as_bytes(),
        )
    }

    /// The page of text of `page_objects`, its content encrypted with RC4 of revision 2 for the
    /// user password `user` and the owner password `ejer`, by the encryption dictionary of the
    /// entries `entries`, the permissions -4 and the keys, the objects `extra` numbered from 6
    /// before it.
    fn encrypted(user: &[u8], entries: &str, extra: &[&str]) -> Vec<u8> {
        const PERMISSIONS: i32 = -4;
        let owner_key = &Md5::digest(padded(b"ejer"))[..5];
        let owner = rc4(owner_key, &padded(user));
        let mut md5 = Md5::new();
        md5.update(padded(user));
        md5.update(&owner);
        md5.update(PERMISSIONS.to_le_bytes());
        md5.update(ID);
        let key = md5.finalize()[..5].to_vec();
        let user = rc4(&key, &PADDING);
        let object_key = Md5::digest([&key[..], &[4, 0, 0, 0, 0]].concat());
        let content = rc4(&object_key[..10], TEXT.as_bytes());
        let mut objects: Vec<Vec<u8>> = page_objects(TEXT)
            .into_iter()
            .map(String::into_bytes)
            .collect();
        objects[3] = [
            format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(),
            &content,
            b"\nendstream",
        ]
        .concat();
        objects.extend(extra.iter().map(|object| object.as_bytes().to_vec()));
        objects.push(
            format!(
                "<< {entries} /P {PERMISSIONS} /O <{}> /U <{}> >>",
                hex(&owner),
                hex(&user)
            )
            .into_bytes(),
        );
        let id = format!("/ID [<{}> <{}>]", hex(ID), hex(ID));
        replace(
            &document(&objects, b"", &[]),
            b"/Root 1 0 R >>",
            format!("/Root 1 0 R /Encrypt {} 0 R {id} >>", objects.len()).as_bytes(),
        )
    }

    /// Documents whose encryption entries PDFBox resolves or converts as it reads them, each
    /// with the password that opens it.
    fn readable_encryptions() -> Vec<(&'static str, Vec<u8>, &'static str)> {
        vec![
            (
                "filter-reference.pdf",
                encrypted(b"", "/Filter 6 0 R /V 1 /R 2", &["/Standard"]),
                "",
            ),
            (
                "filter-string.pdf",
                encrypted(b"", "/Filter (Standard) /V 1 /R 2", &[]),
                "",
            ),
            (
                "revision-reference.pdf",
                encrypted(b"", "/Filter /Standard /V 1 /R 6 0 R", &["2"]),
                "",
            ),
            (
                "latin-user-password.pdf",
                encrypted(b"\xE6\xF8\xE5", "/Filter /Standard /V 1 /R 2", &[]),
                "æøå",
            ),
        ]
    }

    #[test]
    fn opens_encryption_whose_entries_pdfbox_resolves_or_converts() {
        for (name, input, password) in readable_encryptions() {
            let configs = ParserConfigs::default()
                .with_password(Some(&crate::config::Password::new(password)));
            let prepared = PdfParser.prepare(&input, &TikaMetadata::new(), &configs);
            assert!(prepared.is_ok(), "{name}: {:?}", prepared.err());
        }
    }

    #[cfg(feature = "tika")]
    #[test]
    fn opens_encryption_whose_entries_pdfbox_resolves_or_converts_as_tikas_path_does() {
        let scratch = crate::formats::parity::Scratch::new("pdf-readable-encryption");
        for (name, input, password) in readable_encryptions() {
            let path = scratch.write(name, &input);
            let configs = ParserConfigs::default()
                .with_password(Some(&crate::config::Password::new(password)));
            crate::formats::parity::assert_same_as_tika_delegating_with(
                &[&path],
                super::super::tests::tika_name(),
                &super::super::DELEGATES,
                &configs,
            );
        }
    }

    /// The failure of `PDFParser` as `CompositeParser` wraps what it throws.
    fn wrapped(wrapping: &str, cause: &str) -> String {
        format!(
            "Parse error occurred : org.apache.tika.exception.TikaException: {wrapping} from \
             org.apache.tika.parser.pdf.PDFParser <- caused by {cause}"
        )
    }

    /// `Unexpected RuntimeException from PDFParser`, caused by the JVM's exception `cause`.
    fn runtime_failure(cause: &str) -> String {
        wrapped("Unexpected RuntimeException", cause)
    }

    /// `TIKA-198`, caused by the `IOException` `cause`.
    fn io_failure(cause: &str) -> String {
        wrapped("TIKA-198: Illegal IOException", cause)
    }

    /// Documents PDFBox fails to open, with the error Tika's path on the JVM fails each with,
    /// and whether Tika's native image fails with the same.
    fn unopenable_encryptions() -> Vec<(&'static str, Vec<u8>, String, bool)> {
        let id = format!("/ID [<{}> <{}>]", hex(ID), hex(ID));
        let standard = |entries: &str| format!("<< /Filter /Standard {entries} >>");
        let class_cast = |class: &str| {
            runtime_failure(&format!(
                "java.lang.ClassCastException: class org.apache.pdfbox.cos.{class} cannot be cast \
                 to class org.apache.pdfbox.cos.COSString (org.apache.pdfbox.cos.{class} and \
                 org.apache.pdfbox.cos.COSString are in unnamed module of loader 'app')"
            ))
        };
        let null = |array: &str| {
            runtime_failure(&format!(
                "java.lang.NullPointerException: Cannot read the array length because \"{array}\" \
                 is null"
            ))
        };
        let keys = "/P -4 /O (owner) /U (user)";
        vec![
            (
                "public-key.pdf",
                with_encryption(
                    "<< /Filter /Adobe.PubSec /V 2 /R 3 /Length 128 >>",
                    &id,
                    &[],
                ),
                io_failure(&format!("java.io.IOException: {NO_KEY_STORE}")),
                false,
            ),
            (
                "filter-reference-to-reference.pdf",
                with_encryption(
                    &standard("/Filter 6 0 R /V 1 /R 2").replacen("/Filter /Standard ", "", 1),
                    &id,
                    &[b"7 0 R".to_vec(), b"/Standard".to_vec()],
                ),
                "Parse error occurred : org.apache.tika.exception.EncryptedDocumentException: \
                 Unable to process: document is encrypted <- caused by java.io.IOException: No \
                 security handler for filter null"
                    .to_owned(),
                true,
            ),
            (
                "revision-7.pdf",
                with_encryption(&standard(&format!("/V 1 /R 7 {keys}")), &id, &[]),
                io_failure("java.io.IOException: Unknown Encryption Revision 7"),
                true,
            ),
            (
                "revision-missing.pdf",
                with_encryption(&standard(&format!("/V 1 {keys}")), &id, &[]),
                io_failure("java.io.IOException: Unknown Encryption Revision 0"),
                true,
            ),
            (
                "revision-string.pdf",
                with_encryption(&standard(&format!("/V 1 /R (2) {keys}")), &id, &[]),
                io_failure("java.io.IOException: Unknown Encryption Revision 0"),
                true,
            ),
            (
                "id-number.pdf",
                with_encryption(&standard(&format!("/V 1 /R 2 {keys}")), "/ID [1 2]", &[]),
                class_cast("COSInteger"),
                false,
            ),
            (
                "id-null.pdf",
                with_encryption(&standard(&format!("/V 1 /R 2 {keys}")), "/ID [null 2]", &[]),
                runtime_failure(
                    "java.lang.NullPointerException: Cannot invoke \
                     \"org.apache.pdfbox.cos.COSString.getBytes()\" because \"id\" is null",
                ),
                false,
            ),
            (
                "owner-name.pdf",
                with_encryption(&standard("/V 1 /R 2 /P -4 /O /Owner /U (user)"), &id, &[]),
                class_cast("COSName"),
                false,
            ),
            (
                "owner-missing-r2.pdf",
                with_encryption(&standard("/V 1 /R 2 /P -4 /U (user)"), &id, &[]),
                null("data"),
                false,
            ),
            (
                "owner-missing-r3.pdf",
                with_encryption(&standard("/V 2 /R 3 /P -4 /U (user)"), &id, &[]),
                null("owner"),
                false,
            ),
            (
                "user-missing-r3.pdf",
                with_encryption(&standard("/V 2 /R 3 /P -4 /O (owner)"), &id, &[]),
                null("original"),
                false,
            ),
            (
                "length-0.pdf",
                with_encryption(&standard(&format!("/V 2 /R 3 /Length 0 {keys}")), &id, &[]),
                runtime_failure(
                    "java.lang.IllegalArgumentException: number of bytes must be between 1 and 32",
                ),
                true,
            ),
            (
                "length-256.pdf",
                with_encryption(
                    &standard(&format!("/V 2 /R 3 /Length 256 {keys}")),
                    &id,
                    &[],
                ),
                io_failure(
                    "java.io.IOException: java.lang.IllegalArgumentException: Input buffer too \
                     short <- caused by java.lang.IllegalArgumentException: Input buffer too short",
                ),
                true,
            ),
            (
                "length-136-r2.pdf",
                with_encryption(
                    &standard(&format!("/V 2 /R 2 /Length 136 {keys}")),
                    &id,
                    &[],
                ),
                runtime_failure(
                    "java.lang.ArrayIndexOutOfBoundsException: arraycopy: last source index 17 \
                     out of bounds for byte[16]",
                ),
                false,
            ),
            (
                "length-negative-r2.pdf",
                with_encryption(
                    &standard(&format!("/V 2 /R 2 /Length -40 {keys}")),
                    &id,
                    &[],
                ),
                runtime_failure("java.lang.NegativeArraySizeException: -5"),
                true,
            ),
            (
                "length-negative-r3.pdf",
                with_encryption(
                    &standard(&format!("/V 2 /R 3 /Length -40 {keys}")),
                    &id,
                    &[],
                ),
                runtime_failure(
                    "java.lang.ArrayIndexOutOfBoundsException: Range [0, 0 + -5) out of bounds \
                     for length 16",
                ),
                true,
            ),
        ]
    }

    #[test]
    fn fails_encryption_pdfbox_throws_on_as_tika_on_the_jvm_does() {
        for (name, input, expected, _) in unopenable_encryptions() {
            match PdfParser.prepare(&input, &TikaMetadata::new(), &ParserConfigs::default()) {
                Err(Decline::Fails(error)) => assert_eq!(error.to_string(), expected, "{name}"),
                Ok(_) => panic!("{name} opens"),
                Err(declined) => panic!("{name}: {declined:?}"),
            }
        }
    }

    /// Tika's native image fails some of these otherwise than the JVM: it names the parser by
    /// its identity hash, which the comparison takes out, and it lacks the public-key security
    /// handler's constructor and the JVM's detailed messages of the exceptions it throws
    /// itself.
    #[cfg(feature = "tika")]
    #[test]
    fn fails_encryption_pdfbox_throws_on_as_tikas_path_does() {
        let scratch = crate::formats::parity::Scratch::new("pdf-unopenable-encryption");
        let unhashed = |error: String| {
            let mut out = String::with_capacity(error.len());
            let mut rest = error.as_str();
            while let Some(at) = rest.find("PDFParser@") {
                out.push_str(&rest[..at + "PDFParser".len()]);
                rest = rest[at + "PDFParser@".len()..]
                    .trim_start_matches(|c: char| c.is_ascii_hexdigit());
            }
            out.push_str(rest);
            out
        };
        for (name, input, _, same) in unopenable_encryptions() {
            if !same {
                continue;
            }
            let path = scratch.write(name, &input);
            let (native, tika) = crate::formats::parity::errors(&path, &ParserConfigs::default());
            assert_eq!(native, unhashed(tika), "{name}");
        }
    }

    /// PDFBox opens a document whose owner key was hashed whole, as the specification says,
    /// only with its user password (#129).
    #[test]
    fn opens_with_an_owner_password_hashed_as_the_specification_says() {
        let handler = handler(b"ejer", b"bruger", OwnerKeyHash::Whole);
        assert_eq!(
            opened(&handler, "ejer"),
            Some((Access::Owner, "bruger".to_owned()))
        );
        assert_eq!(opened(&handler, "forkert"), None);
    }
}
