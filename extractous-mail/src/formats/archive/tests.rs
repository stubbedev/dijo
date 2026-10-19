//! The archive parsers compared with Tika's path: on the archives of the repository's corpora,
//! on synthetic ones written for what the corpora lack, and on Tika's own test documents.

use super::*;
use crate::compress::gzip::tests::gzip;
use crate::compress::tar_header::tests::tar;
use crate::compress::zip::file::tests::zip;
use crate::compress::zstd::tests::zstd;
use crate::formats::testing::Scratch;

#[test]
fn reads_the_first_segment_of_a_split_zip_up_to_the_entry_it_cuts_short() {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string("../test_files/documents/spanned.z01")
        .unwrap();
    let entries: Vec<&str> = xml
        .split(r#"<div class="embedded" id=""#)
        .skip(1)
        .map(|rest| &rest[..rest.find('"').unwrap()])
        .collect();
    assert_eq!(
        entries,
        [
            "testEXCEL.xls",
            "testHTML.html",
            "testOpenOffice2.odt",
            "testPDF.pdf",
            "testPPT.ppt"
        ]
    );
    assert!(xml.contains("Sample Excel Worksheet"), "{xml}");
    assert_eq!(metadata["zip:integrity-check-result"], ["PARTIAL"]);
    let failures = &metadata["tk:exception:embedded-stream-exception"];
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("(testRTF.rtf): Truncated ZIP file"),
        "{failures:?}"
    );
}

#[test]
fn records_encrypted_zip_entries_as_failures_only() {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string("../test_files/documents/encrypted/encrypted-entry.zip")
        .unwrap();
    assert!(xml.contains("unencrypted.txt"), "{xml}");
    assert!(!xml.contains("<p>encrypted.txt</p>"), "{xml}");
    let integrity = xml.find("zip:integrity-check-result").unwrap();
    assert!(integrity < xml.find("<body>").unwrap(), "{xml}");
    let failures = &metadata["tk:exception:embedded-stream-exception"];
    assert!(
        failures[0].contains("stream (encrypted.txt) is encrypted"),
        "{failures:?}"
    );
}

#[test]
fn reads_zip_entries_as_flagged_and_dated_without_a_zone() {
    use std::io::Write;
    // Tika takes a JAR only by its first entry, reads DOS times in the JVM's zone and takes
    // the UTF-8 flag only as a hint (#99).
    let mut writer = ::zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let time = ::zip::DateTime::from_date_and_time(2020, 6, 15, 12, 30, 10).unwrap();
    let options = ::zip::write::SimpleFileOptions::default().last_modified_time(time);
    for (name, content) in [
        ("caf\u{e9} \u{65e5}\u{672c}.txt", "text"),
        ("META-INF/MANIFEST.MF", "Manifest-Version: 1.0\n"),
    ] {
        writer.start_file(name, options).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    let archive = writer.finish().unwrap().into_inner();
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(&archive)
        .unwrap();
    assert_eq!(metadata["Content-Type"], ["application/java-archive"]);
    assert!(
        xml.contains("<h1>caf\u{e9} \u{65e5}\u{672c}.txt</h1>"),
        "{xml}"
    );
    let entry = crate::compress::zip::file::ZipFile::open(&archive).unwrap();
    let modified = entry.entries()[0].modified().unwrap();
    assert_eq!(
        crate::java::time::instant_string(modified.seconds, modified.nanos),
        "2020-06-15T12:30:10Z"
    );
}

#[test]
fn decodes_zstandard() {
    let scratch = Scratch::new("archive-zstd");
    let path = scratch.write("notes.txt.zst", &zstd(b"Zstandard text."));
    let (parsed, _) = crate::formats::testing::documents(&CompressorParser, &[scratch.directory()]);
    assert_eq!(
        parsed,
        std::slice::from_ref(&path),
        "the native path decodes Zstandard"
    );
    let text = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap()
        .0;
    assert!(text.contains("Zstandard text."), "{text}");
}

/// A backslash is a character of a TAR name, which Tika turns into a slash (#101).
#[test]
fn keeps_backslashes_in_tar_names() {
    let archive = tar(&[(b"back\\slash.txt", b'0', b"text")]);
    let (xml, _) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(&archive)
        .unwrap();
    assert!(xml.contains("id=\"back\\slash.txt\""), "{xml}");
}

/// What extracting `archive` streams, and how the extraction ends: the error it fails with, or
/// the failures its metadata records.
fn extract(archive: &[u8]) -> (String, Result<Vec<String>, crate::Error>) {
    let (mut reader, _) = crate::Extractor::new()
        .extract_bytes(archive)
        .expect("the extraction starts");
    let mut text = Vec::new();
    let read = std::io::Read::read_to_end(&mut reader, &mut text);
    let text = String::from_utf8(text).expect("the text is UTF-8");
    let ended = match read {
        Ok(_) => {
            let metadata = reader
                .metadata()
                .expect("the metadata of a finished extraction");
            Ok(metadata
                .into_iter()
                .filter(|(key, _)| key.starts_with("tk:exception:"))
                .flat_map(|(_, values)| values)
                .collect())
        }
        Err(error) => Err(*error
            .into_inner()
            .expect("the stream fails with the extraction's error")
            .downcast::<crate::Error>()
            .expect("the stream fails with an extraction error")),
    };
    (text, ended)
}

/// Asserts that the extraction `ended` at the zip bomb limit of Tika's `SecureContentHandler`
/// for `cause`. The native path fails with [`Error::LimitExceeded`]. Tika's path, which takes a
/// zip bomb in a build that has it, fails with its exception or records it, as its wrapping of
/// the guard's exception has it.
fn assert_zip_bomb(ended: Result<Vec<String>, crate::Error>, cause: &str) {
    let message = match ended {
        Err(Error::LimitExceeded(message)) => message,
        other => panic!("not stopped as a zip bomb: {other:?}"),
    };
    assert!(message.contains("Suspected zip bomb"), "{message}");
    assert!(message.contains(cause), "{message}");
}

#[test]
fn leaves_a_stream_decoding_past_the_most_the_native_path_decodes_to_a_limit() {
    let declined = undecoded("the Gzip stream", Undecoded::TooLarge, "CompressorParser");
    assert!(
        matches!(&declined, Decline::Fails(Error::LimitExceeded(reason))
            if reason.starts_with("the Gzip stream: it decodes into more than")),
        "{declined:?}"
    );
}

#[test]
fn stops_archives_nested_ten_package_entries_deep_with_what_they_wrote() {
    let mut archive = zip(&[("level1.txt", b"Text at nesting level 1.")]);
    for level in 2..=300 {
        let text = format!("Text at nesting level {level}.");
        archive = zip(&[
            (&format!("level{level}.txt"), text.as_bytes()),
            ("inner.zip", &archive),
        ]);
    }
    let (text, ended) = extract(&archive);
    assert!(text.contains("Text at nesting level 300."), "{text}");
    assert!(text.contains("Text at nesting level 292."), "{text}");
    assert!(!text.contains("Text at nesting level 291."), "{text}");
    assert_zip_bomb(ended, "10 levels of package entry nesting");
}

#[test]
fn stops_an_entry_that_inflates_a_hundredfold_past_a_million_characters() {
    let line = "All work and no play makes Jack a dull boy.\n";
    let bomb = zip(&[("bomb.txt", line.repeat(700_000).as_bytes())]);
    assert!(bomb.len() * 100 < line.len() * 700_000, "{}", bomb.len());
    let (text, ended) = extract(&bomb);
    assert!(text.contains(line), "{}", &text[..100.min(text.len())]);
    assert!(text.len() > 1_000_000, "{}", text.len());
    assert!(text.len() < line.len() * 700_000);
    assert_zip_bomb(ended, "output characters");
}

/// `length` bytes no compressor shrinks, the same each time.
fn incompressible(length: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[test]
fn records_a_bomb_in_an_entry_and_goes_on_with_the_entries_after_it() {
    let line = "All work and no play makes Jack a dull boy.\n";
    let bomb = gzip(line.repeat(70_000).as_bytes(), Some("bomb.txt"));
    let archive = zip(&[
        ("before.txt", b"Before the bomb."),
        ("padding.bin", &incompressible(50_000)),
        ("bomb.txt.gz", &bomb),
        ("after.txt", b"After the bomb."),
    ]);
    assert!(bomb.len() * 100 < line.len() * 70_000, "{}", bomb.len());
    assert!(
        archive.len() * 100 > line.len() * 70_000,
        "{}",
        archive.len()
    );
    let (text, ended) = extract(&archive);
    assert!(text.contains("Before the bomb."), "{}", &text[..100]);
    assert!(text.contains(line), "{}", &text[..100]);
    assert!(
        text.len().abs_diff(bomb.len() * 100) < 10_000,
        "{} characters of {} bytes",
        text.len(),
        bomb.len()
    );
    assert!(
        text.contains("After the bomb."),
        "{}",
        &text[text.len() - 100..]
    );
    let recorded = ended.expect("the archive goes on past the bomb").concat();
    assert!(
        recorded.contains("could not parse the embedded document \"bomb.txt.gz\""),
        "{recorded}"
    );
    assert!(recorded.contains("Zip bomb detected!"), "{recorded}");
    assert!(recorded.contains("output characters"), "{recorded}");
}

#[test]
fn writes_an_entry_up_to_the_most_the_native_path_decodes_and_stops_there() {
    let bomb = vec![0; crate::compress::MAX_DECODED + 1000];
    let archive = zip(&[
        ("before.txt", b"Before the large entry."),
        ("large.bin", &bomb),
        ("after.txt", b"After the large entry."),
    ]);
    let (text, ended) = extract(&archive);
    assert!(text.contains("Before the large entry."), "{text}");
    assert!(!text.contains("After the large entry."), "{text}");
    match ended {
        Err(Error::LimitExceeded(message)) => {
            assert!(message.starts_with("the entry large.bin: "), "{message}");
            assert!(
                message.contains("the most the native parser decodes"),
                "{message}"
            );
        }
        other => panic!("not stopped at the limit: {other:?}"),
    }
}

#[test]
fn writes_a_7z_archive_up_to_the_entry_past_the_most_the_native_path_decodes() {
    let bomb = vec![0; crate::compress::MAX_DECODED + 1000];
    let archive = crate::compress::seven_z::tests::seven_z(&[
        ("before.txt", b"Before the large entry."),
        ("large.bin", &bomb),
        ("after.txt", b"After the large entry."),
    ]);
    let (text, ended) = extract(&archive);
    assert!(text.contains("Before the large entry."), "{text}");
    assert!(!text.contains("After the large entry."), "{text}");
    match ended {
        Err(Error::LimitExceeded(message)) => {
            assert!(message.starts_with("the 7z archive: "), "{message}");
            assert!(
                message.contains("the most the native parser decodes"),
                "{message}"
            );
        }
        other => panic!("not stopped at the limit: {other:?}"),
    }
}

#[test]
fn writes_an_entry_cut_short_as_far_as_it_decodes_and_goes_on() {
    let mut archive = zip(&[
        ("broken.txt", &[7u8; 5000]),
        ("after.txt", b"After the broken entry."),
    ]);
    let start =
        30 + "broken.txt".len() + usize::from(u16::from_le_bytes([archive[28], archive[29]]));
    archive[start] = 0xFF;
    let (text, ended) = extract(&archive);
    assert!(text.contains("After the broken entry."), "{text}");
    let recorded = ended.expect("the archive goes on past the entry");
    let (unreadable, rest): (Vec<&String>, Vec<&String>) = recorded
        .iter()
        .partition(|recorded| recorded.contains("Can't read archive stream"));
    assert_eq!(unreadable.len(), 1, "{recorded:?}");
    assert!(
        unreadable[0].contains("(broken.txt): Truncated ZIP file"),
        "{recorded:?}"
    );
    assert!(
        rest.iter()
            .all(|recorded| recorded.contains("read the document differently from Tika")),
        "{recorded:?}"
    );
}
