use std::path::Path;
#[cfg(feature = "tika")]
use std::path::PathBuf;

use super::format;
#[cfg(feature = "tika")]
use super::format::Detected;
#[cfg(feature = "tika")]
use super::pages::rc4;
use super::*;
#[cfg(feature = "tika")]
use crate::config::Password;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_same_as_tika, assert_same_as_tika_departing, assert_same_as_tika_normalising,
    conformance_corpus, documents, errors, spools_as_tika_rewriting, Departure, Departures,
};
use crate::formats::parity::{deferral, Scratch};

const FIXTURES: &str = "../test_files/documents/access";

/// Where the native parser departs from Tika's path on purpose.
#[cfg(feature = "tika")]
const DEPARTURES: Departures = &[Departure::Unset("mdb:password", "#122")];

#[cfg(feature = "tika")]
fn names(paths: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
}

#[cfg(feature = "tika")]
#[test]
fn extracts_access_databases_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&JackcessParser, &roots);
    assert_eq!(names(&deferred), [""; 0]);
    let (encrypted, parsed): (Vec<PathBuf>, Vec<PathBuf>) = parsed
        .into_iter()
        .partition(|path| path.ends_with("tika-password.accdb"));
    assert_fails_as_encrypted(&encrypted);
    assert_eq!(
        names(&parsed),
        [
            "ace-calculated.accdb",
            "ace-complex.accdb",
            "ace-extended-dates.accdb",
            "ace-ole.accdb",
            "ace-rich-text.accdb",
            "jet3-ole.mdb",
            "jet3-overflow.mdb",
            "jet3-queries.mdb",
            "jet4-turkish.mdb",
            "jet4-unicode-compression.mdb",
        ]
    );
    assert_same_as_tika_formatting_cells(&parsed);
}

/// Asserts that the native parser extracts each of `databases` as Tika's path does, but for the
/// cells and text it writes otherwise on purpose, as [`as_tika_writes`] undoes them (#122).
#[cfg(feature = "tika")]
fn assert_same_as_tika_formatting_cells(databases: &[PathBuf]) {
    let tika_name = JackcessParser.tika_name();
    for database in databases {
        let (rewrite, normalise) = as_tika_writes(database);
        assert_same_as_tika_normalising(&[database], tika_name, rewrite, normalise);
    }
}

/// Asserts that each of `encrypted`, one database, fails without a password as encrypted, with
/// the error Tika's path fails with, and is left to Tika with one.
#[cfg(feature = "tika")]
fn assert_fails_as_encrypted(encrypted: &[PathBuf]) {
    assert_eq!(encrypted.len(), 1);
    let path = &encrypted[0];
    let (native, tika) = errors(path, &ParserConfigs::default());
    assert_eq!(native, tika);
    assert!(native.starts_with("Encrypted("), "{native}");
    let configs = ParserConfigs::default().with_password(Some(&Password::new("tika")));
    let input = std::fs::read(path).unwrap();
    assert_eq!(
        JackcessParser
            .defers(&input, &TikaMetadata::new(), &configs)
            .as_deref(),
        Some("an encrypted database, which the port does not decrypt")
    );
}

#[test]
fn fails_on_encrypted_databases_as_encrypted() {
    let mut header = fixture("ace-complex.accdb");
    let Ok(format::Detected::Format(format)) = format::detect(&header) else {
        panic!("an ACE database");
    };
    let start = format.offset_encoding_key;
    header[start] ^= 1;
    let scratch = Scratch::new("access-encrypted");
    let path = scratch.write("encrypted.accdb", &header);
    for path in [
        path.as_path(),
        Path::new("../test_files/documents/encrypted/tika-password.accdb"),
    ] {
        let error = crate::Extractor::new()
            .extract_file_to_string(path.to_str().unwrap())
            .unwrap_err();
        assert!(matches!(error, crate::Error::Encrypted(_)), "{error:?}");
    }
}

#[cfg(feature = "tika")]
#[test]
fn spools_embedded_objects_as_tikas_path_does() {
    for name in ["ace-ole.accdb", "jet3-ole.mdb"] {
        let path = Path::new(FIXTURES).join(name);
        let (rewrite, normalise) = as_tika_writes(&path);
        for budget in [u64::MAX, 3000] {
            let spooled =
                spools_as_tika_rewriting(&path, budget, Some((rewrite, Some(normalise))), &[]);
            assert!(matches!(spooled, Ok(1..)), "{name}: {spooled:?}");
        }
    }
}

/// A Jet database encoded as Access encodes it once it is given an encoding key: each page
/// after the first RC4 under the key XORed with the page's number.
#[cfg(feature = "tika")]
fn encoded(mut file: Vec<u8>, key: [u8; 4]) -> Vec<u8> {
    let Ok(Detected::Format(format)) = format::detect(&file) else {
        panic!("a Jet database");
    };
    for (number, page) in file.chunks_mut(format.page_size).enumerate().skip(1) {
        let mut page_key = (number as u32).to_le_bytes();
        for (byte, key) in page_key.iter_mut().zip(key) {
            *byte ^= key;
        }
        rc4(&page_key, page);
    }
    let start = format.offset_encoding_key;
    let mask = &format.header_mask[start - format.offset_masked_header..];
    for (i, byte) in key.into_iter().enumerate() {
        file[start + i] = byte ^ mask[i];
    }
    file
}

#[cfg(feature = "tika")]
#[test]
fn reads_encoded_jet_databases_as_tikas_path_does() {
    let scratch = Scratch::new("access-encoded");
    let jet3 = scratch.write(
        "encoded-jet3.mdb",
        &encoded(fixture("jet3-queries.mdb"), [0x12, 0x34, 0x56, 0x78]),
    );
    let jet4 = scratch.write(
        "encoded-jet4.mdb",
        &encoded(fixture("jet4-turkish.mdb"), [0xFE, 0xDC, 0xBA, 0x98]),
    );
    for path in [&jet3, &jet4] {
        assert_eq!(deferral(&JackcessParser, path), None);
    }
    assert_same_as_tika_formatting_cells(&[jet3, jet4]);
}

/// Jet 4's header: where the database password lies, how long it is, and where the creation
/// date that masks it lies.
const JET4_PASSWORD: usize = 66;
const JET4_PASSWORD_LENGTH: usize = 40;
const JET4_HEADER_DATE: usize = 114;

/// A Jet 4 database given the database password `password`, masked as Access masks it: by the
/// header mask and by the integer part of the database's creation date.
fn with_password(mut file: Vec<u8>, password: &str) -> Vec<u8> {
    let Ok(format::Detected::Format(format)) = format::detect(&file) else {
        panic!("a Jet database");
    };
    let mask = |at: usize| format.header_mask[at - format.offset_masked_header];
    let date: [u8; 8] =
        std::array::from_fn(|i| file[JET4_HEADER_DATE + i] ^ mask(JET4_HEADER_DATE + i));
    let date_mask = (f64::from_le_bytes(date) as i32).to_le_bytes();
    let mut field: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
    field.resize(JET4_PASSWORD_LENGTH, 0);
    for (i, byte) in field.into_iter().enumerate() {
        let at = JET4_PASSWORD + i;
        file[at] = byte ^ date_mask[i % date_mask.len()] ^ mask(at);
    }
    file
}

#[test]
fn keeps_the_database_password_out_of_the_metadata() {
    let scratch = Scratch::new("access-password");
    let path = scratch.write(
        "password.mdb",
        &with_password(fixture("jet4-turkish.mdb"), "hemmelig"),
    );
    let (text, metadata) = crate::Extractor::new()
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap();
    assert!(!metadata.contains_key("mdb:password"), "{metadata:?}");
    assert!(!text.contains("hemmelig"), "{text}");
    #[cfg(feature = "tika")]
    {
        let exact =
            std::panic::catch_unwind(|| assert_same_as_tika(&[&path], JackcessParser.tika_name()));
        assert!(exact.is_err(), "Tika's path exposes the password");
        assert_same_as_tika_departing(&[&path], JackcessParser.tika_name(), DEPARTURES);
    }
}

#[test]
fn leaves_databases_jackcess_throws_reading_to_tika() {
    let scratch = Scratch::new("access-unreadable");
    let original = fixture("jet4-turkish.mdb");
    let truncated = scratch.write("truncated.mdb", &original[..3 * 4096]);
    assert_eq!(
        deferral(&JackcessParser, &truncated).as_deref(),
        Some(
            "an Access database Jackcess throws reading: java.lang.IllegalStateException: \
             invalid page number 6"
        )
    );
}

/// `bytes` with each `from` replaced by `to`, of the same length.
fn replaced(mut bytes: Vec<u8>, from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut start = 0;
    while let Some(at) = bytes[start..]
        .windows(from.len())
        .position(|window| window == from)
    {
        bytes[start + at..start + at + to.len()].copy_from_slice(to);
        start += at + to.len();
    }
    bytes
}

/// Where Jet 3's header names the database's code page.
const JET3_CODE_PAGE: usize = 60;

#[test]
fn reads_jet3_text_in_the_code_page_of_the_database() {
    let scratch = Scratch::new("access-jet3-code-page");
    let original = fixture("jet3-queries.mdb");
    let danish = scratch.write(
        "danish.mdb",
        &replaced(original.clone(), b"Table2", b"Tr\xe6er\xf8"),
    );
    let (xml, _) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string(danish.to_str().unwrap())
        .unwrap();
    assert!(xml.contains("<table name=\"Træerø\">"), "{xml}");
    assert!(xml.contains("[Træerø].[col2]"), "{xml}");

    let mut header = original;
    let Ok(format::Detected::Format(format)) = format::detect(&header) else {
        panic!("a Jet database");
    };
    let at = JET3_CODE_PAGE;
    let mask = &format.header_mask[at - format.offset_masked_header..];
    for (i, byte) in 1251u16.to_le_bytes().into_iter().enumerate() {
        header[at + i] = byte ^ mask[i];
    }
    let cyrillic = scratch.write(
        "cyrillic.mdb",
        &replaced(header, b"Table2", b"\xd2\xe0\xe1\xeb\xe8\xf6"),
    );
    let (xml, _) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string(cyrillic.to_str().unwrap())
        .unwrap();
    assert!(xml.contains("<table name=\"Таблиц\">"), "{xml}");
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&JackcessParser, &[&conformance_corpus()]);
    assert_eq!(names(&deferred), [""; 0]);
    let (encrypted, parsed): (Vec<PathBuf>, Vec<PathBuf>) = parsed
        .into_iter()
        .partition(|path| path.ends_with("testAccess2_encrypted.accdb"));
    assert_fails_as_encrypted(&encrypted);
    assert_eq!(
        names(&parsed),
        [
            "testACCESS.mdb",
            "testAccess2.accdb",
            "testAccess2_2000.mdb",
            "testAccess2_2002-2003.mdb",
            "testAccess_V1997.mdb",
        ]
    );
    assert_same_as_tika_formatting_cells(&parsed);
}

/// The XML the extractor writes of `database`, written as `db.mdb` in a directory of its own.
fn xml_of(database: &[u8]) -> String {
    let scratch = Scratch::new("access-xml");
    let path = scratch.write("db.mdb", database);
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string(path.to_str().unwrap())
        .unwrap()
        .0
}

#[test]
fn reads_every_table_whatever_its_name() {
    let renamed = replaced(fixture("jet3-queries.mdb"), b"Table2", b"Abl\xe6e2");
    let xml = xml_of(&renamed);
    let tables: Vec<&str> = xml
        .match_indices("<table name=\"")
        .map(|(at, start)| {
            let name = &xml[at + start.len()..];
            &name[..name.find('"').unwrap()]
        })
        .collect();
    assert_eq!(tables, ["Ablæe2", "Table1", "Table3"]);
}

#[test]
fn gives_each_column_of_a_name_its_own_values() {
    let utf16 =
        |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let original = xml_of(&fixture("ace-calculated.accdb"));
    let renamed = xml_of(&replaced(
        fixture("ace-calculated.accdb"),
        &utf16("LastFirst"),
        &utf16("FirstName"),
    ));
    assert!(
        renamed.contains("<th>FirstName</th>\t<th>LastName</th>\t<th>FirstName</th>"),
        "{renamed:?}"
    );
    assert!(
        renamed.contains("<td>Bruce</td>\t<td>Wayne</td>\t<td>Wayne, Bruce</td>"),
        "{renamed:?}"
    );
    assert_eq!(renamed, original.replace("LastFirst", "FirstName"));
}

/// Where Jet 4's table definition refers to its free space usage map.
const JET4_FREE_SPACE_PAGES: usize = 59;

#[test]
fn reads_tables_whose_unused_usage_maps_are_broken() {
    let original = fixture("jet4-turkish.mdb");
    let database = Database::open(&original).unwrap();
    let page_size = database.pages.format.page_size;
    let pages: Vec<i32> = database
        .tables()
        .unwrap()
        .iter()
        .map(|table| table.page)
        .collect();
    assert!(!pages.is_empty());
    let mut broken = original.clone();
    for page in pages {
        let at = page as usize * page_size + JET4_FREE_SPACE_PAGES;
        broken[at..at + 4].fill(0);
    }
    assert_eq!(xml_of(&broken), xml_of(&original));
}

#[test]
fn writes_currency_amounts_exactly() {
    assert_eq!(money(10_000_000_000), "1000000.00");
    assert_eq!(money(833_333_333), "83333.3333");
    assert_eq!(money(12_000), "1.20");
    assert_eq!(money(12_345), "1.2345");
    assert_eq!(money(-10_000), "-1.00");
    assert_eq!(money(-1), "-0.0001");
    assert_eq!(money(0), "0.00");
    assert_eq!(money(i64::MIN), "-922337203685477.5808");
}

#[test]
fn writes_numerics_in_positional_notation() {
    let decimal = |negative, magnitude, scale| {
        Decimal {
            negative,
            magnitude,
            scale,
        }
        .plain()
    };
    assert_eq!(decimal(false, 0, 8), "0.00000000");
    assert_eq!(
        decimal(false, 2_787_019_289_824_216_980_830, 28),
        "0.0000002787019289824216980830"
    );
    assert_eq!(decimal(true, 3_622, 2), "-36.22");
    assert_eq!(decimal(true, 0, 2), "0.00");
    assert_eq!(decimal(false, 12, 0), "12");
    assert_eq!(decimal(false, 12, -3), "12000");
}

#[test]
fn writes_dates_with_their_time_whatever_the_time_zone() {
    use crate::java::time::civil::days_from_civil;
    let serial = |year, month, day, fraction: f64| {
        let days = days_from_civil(year, month, day) - days_from_civil(1899, 12, 30);
        (days as f64 + fraction).to_bits() as i64
    };
    assert_eq!(date(serial(2006, 2, 3, 0.0)), "2006-02-03");
    assert_eq!(date(serial(2006, 2, 3, 0.5)), "2006-02-03 12:00:00");
    assert_eq!(
        date(serial(
            2006,
            2,
            3,
            (22.0 * 3600.0 + 11.0 * 60.0 + 5.0) / 86_400.0
        )),
        "2006-02-03 22:11:05"
    );
    assert_eq!(date(serial(1265, 6, 14, 0.0)), "1265-06-14");
    assert_eq!(date(serial(100, 6, 14, 0.0)), "0100-06-14");
    assert_eq!(date((-1.25_f64).to_bits() as i64), "1899-12-29 06:00:00");
}

#[test]
fn writes_rich_text_whole() {
    let long = "æ".repeat(150_000);
    let (text, difference) = rich_text(
        &format!("<div><b>{long}</b></div>"),
        &ParserConfigs::default(),
    )
    .unwrap();
    assert!(text.contains(&long), "{}", text.len());
    assert_eq!(difference, None);
}

#[test]
fn reads_ole_objects_with_empty_names() {
    let mut value = Vec::new();
    value.extend(0x1C15_i16.to_le_bytes());
    value.extend(20_i16.to_le_bytes());
    value.extend(0_i32.to_le_bytes());
    value.extend(0_i16.to_le_bytes());
    value.extend(0_i16.to_le_bytes());
    value.extend(20_i16.to_le_bytes());
    value.extend(20_i16.to_le_bytes());
    value.extend(0_i32.to_le_bytes());
    value.extend(0_i32.to_le_bytes());
    value.extend(0_i32.to_le_bytes());
    assert!(matches!(
        ole::read(&value, &ParserConfigs::default()),
        Ok(Ole::Nothing)
    ));
}
