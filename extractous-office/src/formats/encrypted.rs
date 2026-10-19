//! The encrypted Word documents and workbooks `tests/fixtures/crypt/encrypt.py` writes from the
//! plain ones LibreOffice writes, with XOR obfuscation and with RC4 CryptoAPI, opened with their
//! password as Tika's path opens them, and failing without it as Tika's path fails; and those
//! encrypted as nothing decrypts, failing whatever the password.

use std::path::PathBuf;

use crate::config::Password;
use crate::Extractor;

/// The password every fixture is encrypted with.
const PASSWORD: &str = "tika";

/// The encrypted fixtures, each with the plain document it is encrypted from.
const ENCRYPTED: [(&str, &str); 4] = [
    ("cryptoapi.doc", "plain.doc"),
    ("xor.doc", "plain.doc"),
    ("cryptoapi.xls", "plain.xls"),
    ("xor.xls", "plain.xls"),
];

fn fixture(name: &str) -> PathBuf {
    PathBuf::from("tests/fixtures/crypt").join(name)
}

fn text(name: &str, passwords: &[&str]) -> crate::errors::ExtractResult<String> {
    Extractor::new()
        .set_passwords(passwords.iter().copied().map(Password::new))
        .extract_file_to_string(fixture(name).to_str().unwrap())
        .map(|(text, _)| text)
}

#[test]
fn reads_what_the_plain_document_holds_with_the_password_only() {
    for (encrypted, plain) in ENCRYPTED {
        let expected = text(plain, &[]).unwrap();
        assert!(expected.contains("rypteret"), "{plain}: {expected}");
        assert_eq!(
            text(encrypted, &[PASSWORD]).unwrap(),
            expected,
            "{encrypted}"
        );
        for passwords in [&[][..], &["wrong"]] {
            assert!(
                matches!(text(encrypted, passwords), Err(crate::Error::Encrypted(_))),
                "{encrypted} with {passwords:?}"
            );
        }
    }
}

/// The fixtures encrypted as [MS-OFFCRYPTO] 2.3.1 does not allow, which nothing decrypts: AES in
/// a Word document and a workbook, RC4 CryptoAPI in an OOXML package.
const DISALLOWED: [&str; 3] = ["aes.doc", "aes.xls", "rc4.docx"];

#[test]
fn fails_on_disallowed_encryptions_as_encrypted_whatever_the_password() {
    for name in DISALLOWED {
        for passwords in [&[][..], &[PASSWORD], &["wrong"]] {
            assert!(
                matches!(text(name, passwords), Err(crate::Error::Encrypted(_))),
                "{name} with {passwords:?}"
            );
        }
    }
}
