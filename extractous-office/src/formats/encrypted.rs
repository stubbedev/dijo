//! The encrypted Word documents and workbooks `tests/fixtures/crypt/encrypt.py` writes from the
//! plain ones LibreOffice writes, with XOR obfuscation, opened with their
//! password as Tika's path opens them, and failing without it as Tika's path fails.

use std::path::PathBuf;

use crate::config::Password;
use crate::Extractor;

/// The password every fixture is encrypted with.
const PASSWORD: &str = "tika";

/// The encrypted fixtures, each with the plain document it is encrypted from.
const ENCRYPTED: [(&str, &str); 2] = [("xor.doc", "plain.doc"), ("xor.xls", "plain.xls")];

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

#[cfg(feature = "tika")]
mod tika {
    use super::*;
    use crate::config::ParserConfigs;
    use crate::formats::parity;

    const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.OfficeParser";

    fn with_password(password: &str) -> ParserConfigs {
        ParserConfigs::default().with_password(Some(&Password::new(password)))
    }

    /// Asserts that the document fails without the password and with a wrong one as Tika's
    /// path fails on it.
    fn assert_fails_as_tika_without_its_password(name: &str) {
        for configs in [ParserConfigs::default(), with_password("wrong")] {
            let (native, tika) = parity::errors(&fixture(name), &configs);
            assert_eq!(native, tika, "{name}");
        }
    }

    /// Tika's XHTML of the workbooks with the header and footer LibreOffice writes without
    /// their codes, as the native parser writes them (#107).
    fn without_header_and_footer_codes(xhtml: &str) -> String {
        xhtml
            .replace(
                "&amp;C&amp;\"Times New Roman\"&amp;12&amp;A",
                "&amp;\"Times New Roman\"",
            )
            .replace(
                "&amp;C&amp;\"Times New Roman\"&amp;12Page &amp;P",
                "&amp;\"Times New Roman\"Page",
            )
    }

    #[test]
    fn opens_workbooks_encrypted_with_xor_as_tikas_path_does() {
        let names = ["xor.xls"];
        let documents: Vec<PathBuf> = names.into_iter().map(fixture).collect();
        let documents: Vec<&std::path::Path> = documents.iter().map(PathBuf::as_path).collect();
        parity::assert_same_xhtml_as_tika_with(
            &documents,
            TIKA_PARSER,
            &with_password(PASSWORD),
            without_header_and_footer_codes,
        );
        for name in names {
            assert_fails_as_tika_without_its_password(name);
        }
    }

    /// POI reads an obfuscated Word document's key and verifier from its table stream rather
    /// than its FIB, and so opens it with no password; the native parser opens it with its own
    /// (#164), and fails without it as Tika's path does.
    #[test]
    fn opens_an_obfuscated_word_document_where_tika_fails() {
        parity::assert_read_where_tika_fails_with(
            &[fixture("xor.doc").as_path()],
            &with_password(PASSWORD),
            "#164",
        );
        assert_fails_as_tika_without_its_password("xor.doc");
    }
}
