use super::*;
use crate::formats::testing::Scratch;
use crate::text::charset::Charset;

const OST: &str = "../test_files/documents/offline-folder.ost";

#[test]
fn reads_messages_without_tika() {
    let (text, metadata) = crate::Extractor::new().extract_file_to_string(OST).unwrap();
    for expected in [
        "Top of Outlook data file",
        "original email.msg",
        "This is the original email (html)",
        "Forwarded plain text",
    ] {
        assert!(text.contains(expected), "{expected} in {text}");
    }
    assert_eq!(metadata["pst:is-valid"], ["true"]);
    assert!(
        metadata["tk:parsed-by-full-set"]
            .iter()
            .any(|parser| parser == MAIL_ITEM_PARSER),
        "{metadata:?}"
    );
}

#[test]
fn refuses_files_java_libpst_refuses() {
    let scratch = Scratch::new("pst-refused");
    let original = std::fs::read(OST).unwrap();
    let mut unknown = original.clone();
    unknown[10] = 99;
    let mut cyclic = original.clone();
    cyclic[513] = 2;
    let mut ost_2013 = original;
    ost_2013[10] = 36;
    for (bytes, message) in [
        (unknown, "Unrecognised PST File version: 99"),
        (
            cyclic,
            "Only unencrypted and compressable PST files are supported",
        ),
        (ost_2013, "Unable to find node: 97 is desc: true"),
    ] {
        let path = scratch.write("refused.ost", &bytes);
        let error = crate::Extractor::new()
            .extract_file_to_string(path.to_str().unwrap())
            .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
}

#[test]
fn decodes_the_charset_of_every_code_page_java_libpst_names() {
    for (code_page, name) in file::CODE_PAGES {
        if let Some(charset) = Charset::for_name(&name.to_ascii_uppercase()) {
            assert!(charset.decodes(), "code page {code_page}: {name}");
        }
    }
}

#[test]
fn decompresses_rtf_as_java_libpst_does() {
    let mut uncompressed = Vec::new();
    uncompressed.extend_from_slice(&20u32.to_le_bytes());
    uncompressed.extend_from_slice(&8u32.to_le_bytes());
    uncompressed.extend_from_slice(b"MELA");
    uncompressed.extend_from_slice(&[0; 4]);
    uncompressed.extend_from_slice(b"  {\\rtf1} ");
    assert_eq!(lzfu::decode(&uncompressed).unwrap(), "{\\rtf1}");
    let mut compressed = Vec::new();
    compressed.extend_from_slice(&0u32.to_le_bytes());
    compressed.extend_from_slice(&3u32.to_le_bytes());
    compressed.extend_from_slice(b"LZFu");
    compressed.extend_from_slice(&[0; 4]);
    compressed.extend_from_slice(&[0b0000_0010, b'a', 0x00, 0x01, 0, 0]);
    assert_eq!(lzfu::decode(&compressed).unwrap(), "a{\\");
    assert!(lzfu::decode(&compressed[..10]).is_err());
}
