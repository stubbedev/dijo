//! PDFBox's `StandardSecurityHandler.prepareForDecryption` as Tika's `PDFParser` opens a document
//! with the password its `PasswordProvider` gives, or the empty one: as its owner when the
//! password is the owner password, else as a user when it is the user password, which grants
//! what the permission bits `P` grant; any other password fails the document.
//!
//! The RC4 revisions 2 to 4 are checked here as PDFBox checks them, but for the key of an owner
//! password shorter than 16 bytes: the specification hashes the whole digest again and again,
//! PDFBox and qpdf only its first key-length bytes, and writers follow either, so both are tried
//! (#129). `lopdf` then decrypts the document with the user password. The AES-256 revisions 5 and 6 are checked by
//! `lopdf`, with the password encoded as PDFBox encodes it.

use lopdf::encryption::PasswordAlgorithm;
use lopdf::{Dictionary, Document, Object};
use md5::{Digest, Md5};
use rc4::{KeyInit, Rc4, StreamCipher};

use super::Unported;

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
    Unported(Unported),
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
    /// RC4 encryption PDFBox opens with the password: what it grants, and the user password
    /// `lopdf` decrypts the document with.
    Rc4 {
        access: Access,
        user_password: String,
    },
    /// AES-256 encryption, which `lopdf` checks the password against.
    Aes256,
}

/// `ENCRYPT_PADDING`, which pads a password to 32 bytes.
const PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

impl Opening {
    /// How the document of the encryption dictionary and first document ID `encryption`, if it
    /// has one, opens with `password`.
    pub(super) fn of(
        encryption: Option<(Dictionary, Vec<u8>)>,
        password: &str,
    ) -> Result<Self, Refusal> {
        let Some((dictionary, id)) = encryption else {
            return Ok(Self::Unencrypted);
        };
        if !matches!(dictionary.get(b"Filter"), Ok(Object::Name(name)) if name == b"Standard") {
            return Err(Unported(
                "encryption by a security handler other than the standard one".to_owned(),
            )
            .into());
        }
        match int(&dictionary, b"R", 2)? {
            2..=4 => Rc4Handler::read(&dictionary, id)?.opening(password),
            5 | 6 => Ok(Self::Aes256),
            revision => Err(Unported(format!(
                "encryption of revision {revision}, which PDFBox does not know"
            ))
            .into()),
        }
    }
}

/// The standard security handler of revisions 2 to 4, as PDFBox checks passwords against it.
struct Rc4Handler {
    revision: i32,
    /// The key's length in bytes.
    length: usize,
    /// `O`, cut or padded to 32 bytes.
    owner: Vec<u8>,
    /// `U`, cut or padded to 32 bytes.
    user: Vec<u8>,
    permissions: i32,
    id: Vec<u8>,
    encrypt_metadata: bool,
}

impl Rc4Handler {
    fn read(dictionary: &Dictionary, id: Vec<u8>) -> Result<Self, Refusal> {
        let version = int(dictionary, b"V", 0)?;
        let mut length = if version == 1 {
            5
        } else {
            int(dictionary, b"Length", 40)? / 8
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
                    true => bytes.min(int(dictionary, b"Length", 40)? / 8),
                    false => bytes,
                };
            }
        }
        let length = usize::try_from(length)
            .ok()
            .filter(|length| (1..=16).contains(length))
            .ok_or_else(|| Unported(format!("an RC4 key of {length} bytes")))?;
        let key = |name: &[u8]| -> Result<Vec<u8>, Refusal> {
            match dictionary.get(name) {
                Ok(Object::String(bytes, _)) => {
                    let mut bytes = bytes.clone();
                    bytes.resize(32, 0);
                    Ok(bytes)
                }
                _ => Err(Unported(format!(
                    "an encryption dictionary without its {} string",
                    String::from_utf8_lossy(name)
                ))
                .into()),
            }
        };
        Ok(Self {
            revision: int(dictionary, b"R", 2)?,
            length,
            owner: key(b"O")?,
            user: key(b"U")?,
            permissions: int(dictionary, b"P", 0)?,
            id,
            encrypt_metadata: !matches!(
                dictionary.get(b"EncryptMetadata"),
                Ok(Object::Boolean(false))
            ),
        })
    }

    /// `prepareForDecryption`'s check of `password`.
    fn opening(&self, password: &str) -> Result<Opening, Refusal> {
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
            (
                Access::User(u64::from(self.permissions as u32)),
                bytes.clone(),
            )
        } else {
            return Err(Refusal::WrongPassword);
        };
        if !user_password
            .iter()
            .all(|byte| (b' '..=b'~').contains(byte))
        {
            return Err(Unported(
                "a user password outside printable ASCII, which lopdf encodes otherwise".to_owned(),
            )
            .into());
        }
        Ok(Opening::Rc4 {
            access,
            user_password: String::from_utf8(user_password).expect("printable ASCII"),
        })
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
        let key = self.key(password);
        if self.revision == 2 {
            return rc4(&key, &PADDING) == self.user;
        }
        let mut hashed = Md5::new();
        hashed.update(PADDING);
        hashed.update(&self.id);
        let mut data = hashed.finalize().to_vec();
        for round in 0..20u8 {
            let iteration: Vec<u8> = key.iter().map(|byte| byte ^ round).collect();
            data = rc4(&iteration, &data);
        }
        data[..16] == self.user[..16]
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

/// `COSDictionary.getInt`: an integer entry as Java's `int` holds it, or `default`.
fn int(dictionary: &Dictionary, key: &[u8], default: i32) -> Result<i32, Refusal> {
    match dictionary.get(key) {
        Err(_) => Ok(default),
        Ok(Object::Integer(value)) => Ok(*value as i32),
        Ok(Object::Real(value)) => Ok(*value as i32),
        Ok(_) => Err(Unported(format!(
            "an encryption entry {} that is no number",
            String::from_utf8_lossy(key)
        ))
        .into()),
    }
}

/// What `password` opens `doc` with, `lopdf` having decrypted its AES-256 encryption with it or
/// the empty password.
pub(super) fn aes256_access(doc: &mut Document, password: &str) -> Result<Access, Refusal> {
    let state = doc.encryption_state.clone().ok_or_else(|| {
        Unported("an encrypted document lopdf read without decrypting it".to_owned())
    })?;
    let dictionary = state.encode().map_err(|error| {
        Unported(format!(
            "an encryption dictionary lopdf cannot restate: {error}"
        ))
    })?;
    let id = (
        doc.objects
            .keys()
            .next_back()
            .map_or(1, |&(number, _)| number + 1),
        0,
    );
    doc.objects.insert(id, Object::Dictionary(dictionary));
    doc.trailer.set("Encrypt", Object::Reference(id));
    let granted = aes256_granted(doc, state.revision(), password);
    doc.trailer.remove(b"Encrypt");
    doc.objects.remove(&id);
    Ok(match granted? {
        Access::User(_) => Access::User(state.permissions().bits()),
        access => access,
    })
}

/// What `password`, UTF-8 for revision 5 and after SASLprep for revision 6 as PDFBox encodes it,
/// opens `doc` as, its encryption dictionary in its trailer.
fn aes256_granted(doc: &Document, revision: i64, password: &str) -> Result<Access, Refusal> {
    let bytes = match revision {
        5 => password.as_bytes().to_vec(),
        _ => PasswordAlgorithm::try_from(doc)
            .map_err(|error| Unported(format!("encryption lopdf cannot read: {error}")))?
            .sanitize_password(password)
            .map_err(|error| Unported(format!("a password SASLprep rejects: {error}")))?,
    };
    if doc.authenticate_raw_owner_password(&bytes).is_ok() {
        return Ok(Access::Owner);
    }
    if doc.authenticate_raw_user_password(&bytes).is_ok() {
        return Ok(Access::User(0));
    }
    Err(Refusal::WrongPassword)
}

/// Why an AES-256 document does not open with `password`, which `lopdf` rejects: a password of
/// printable ASCII is the same bytes to `lopdf` and PDFBox, so PDFBox rejects it too.
pub(super) fn aes256_rejection(password: &str) -> Refusal {
    if password.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
        return Refusal::WrongPassword;
    }
    Refusal::Unported(Unported(
        "a password outside printable ASCII, which lopdf rejects and PDFBox may encode otherwise"
            .to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            user: vec![0; 32],
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
        handler.user = user;
        handler
    }

    /// What `handler` opens with `password`, and the user password it then decrypts with.
    fn opened(handler: &Rc4Handler, password: &str) -> Option<(Access, String)> {
        match handler.opening(password) {
            Ok(Opening::Rc4 {
                access,
                user_password,
            }) => Some((access, user_password)),
            Ok(_) => unreachable!("RC4 encryption"),
            Err(Refusal::WrongPassword) => None,
            Err(Refusal::Unported(unported)) => panic!("{unported:?}"),
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
