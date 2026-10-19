use dijo_core::{Error, Extractor, Password};
use test_case::test_case;

const PASSWORD: &str = "dijo";

fn encrypted(name: &str) -> String {
    format!("../test_files/documents/encrypted/{name}")
}

#[test_case("user-password.pdf", PASSWORD, "UNITED STATES"; "pdf")]
#[test_case("password.docx", PASSWORD, "Call me Ishmael"; "docx")]
#[test_case("password.7z", PASSWORD, "Text inside an encrypted archive."; "seven zip")]
#[test_case("tika-password.doc", "tika", "This is an encrypted Word 2007 File."; "word 97")]
#[test_case("tika-password.ppt", "tika", "This is an encrypted PowerPoint 2007 slide."; "powerpoint 97")]
#[test_case("tika-password.xls", "tika", "This is an Encrypted Excel spreadsheet."; "excel 97")]
fn test_decrypts_with_the_right_password(name: &str, password: &str, expected: &str) {
    let (content, metadata) = Extractor::new()
        .set_passwords([Password::new("wrong"), Password::new(password)])
        .extract_file_to_string(&encrypted(name))
        .unwrap();
    assert!(content.contains(expected), "{content}");
    assert_eq!(
        metadata.get("dijo:password-used"),
        Some(&vec!["true".to_string()])
    );
}

#[test_case("user-password.pdf"; "pdf")]
#[test_case("password.docx"; "docx")]
#[test_case("password.7z"; "seven zip")]
#[test_case("tika-password.doc"; "word 97")]
#[test_case("tika-password.ppt"; "powerpoint 97")]
#[test_case("tika-password.xls"; "excel 97")]
#[test_case("undecryptable.odt"; "odf, which tika cannot decrypt")]
#[test_case("pgp-encrypted.gpg"; "openpgp")]
#[test_case("pgp-encrypted.asc"; "ascii-armored openpgp")]
fn test_reports_encrypted_without_the_password(name: &str) {
    let error = Extractor::new()
        .extract_file_to_string(&encrypted(name))
        .unwrap_err();
    assert!(matches!(error, Error::Encrypted(_)), "{error:?}");

    let error = Extractor::new()
        .set_passwords([Password::new("wrong")])
        .extract_file(&encrypted(name))
        .map(|_| ())
        .unwrap_err();
    assert!(matches!(error, Error::Encrypted(_)), "{error:?}");
    assert!(!error.to_string().contains("wrong"));
}

#[test]
fn test_owner_password_only_pdf_extracts_without_a_password() {
    let (content, metadata) = Extractor::new()
        .extract_file_to_string(&encrypted("owner-password-only.pdf"))
        .unwrap();
    assert!(content.contains("UNITED STATES"));
    assert_eq!(
        metadata.get("access-permission:can-print"),
        Some(&vec!["false".to_string()])
    );
}

#[test]
fn test_a_signed_message_is_not_an_error() {
    let (content, _) = Extractor::new()
        .extract_file_to_string(&encrypted("smime-signed.eml"))
        .unwrap();
    assert!(
        content.contains("This message is signed with S/MIME."),
        "{content}"
    );
}

#[test]
fn test_armored_openpgp_is_detected() {
    let media_type = Extractor::new()
        .detect_file(&encrypted("pgp-encrypted.asc"))
        .unwrap();
    assert_eq!(media_type.as_str(), "application/pgp-encrypted");
}
