use std::path::Path;

use super::buffer::Buffer;
use super::format;
use super::pages::PAGE_TYPE_DATA;
use super::table::{row_end, row_start};
use super::*;
use crate::config::Password;
use crate::formats::crypt::Hash;
use crate::formats::testing::Scratch;

const FIXTURES: &str = "../test_files/documents/access";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
}

/// A database under agile encryption, and its password.
const ENCRYPTED: &str = "../test_files/documents/encrypted/tika-password.accdb";
const PASSWORD: &str = "tika";

/// An encryption provider of Office's, as Access writes its description and encrypts pages.
#[derive(Debug, Clone, Copy)]
enum Provider {
    BinaryRc4,
    CryptoApi,
    /// Standard encryption, its verifier encrypted with the key of this block: 0, as Office
    /// encrypts it, or the number of the last hashing round, as Jackcess checks it.
    Standard(u32),
}

/// The block of the key Jackcess checks a password against standard encryption with.
const JACKCESS_VERIFIER_BLOCK: u32 = 49_999;

/// What makes the key of a block of a database's encryption.
type PageKey = Box<dyn Fn(&[u8]) -> Vec<u8>>;

/// `file`, an ACE database, encrypted as Access encrypts it with `provider` and `password`:
/// the provider described on the header page, its verifier encrypted, and each later page
/// encrypted under the provider's key of the encoding key XORed with the page's number.
fn encrypted(mut file: Vec<u8>, provider: Provider, password: &str) -> Vec<u8> {
    use aes::cipher::{BlockCipherEncrypt, KeyInit};
    use rc4::{Rc4, StreamCipher};
    let Ok(format) = format::detect(&file) else {
        panic!("an ACE database");
    };
    let encoding_key = [0x5A, 0x17, 0xC3, 0x08];
    let password: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let salt = [0x42; 16];
    let verifier = [0x24; 16];
    let block = |number: u32| -> [u8; 4] {
        std::array::from_fn(|i| number.to_le_bytes()[i] ^ encoding_key[i])
    };
    let rc4 = |key: &[u8], parts: &mut [&mut [u8]]| {
        let mut cipher = Rc4::new_from_slice(key).unwrap();
        for part in parts {
            cipher.apply_keystream(part);
        }
    };
    let aes = |key: &[u8], data: &mut [u8]| {
        let cipher = aes::Aes128::new_from_slice(key).unwrap();
        for chunk in data.chunks_mut(16) {
            let mut block = aes::Block::try_from(&*chunk).unwrap();
            cipher.encrypt_block(&mut block);
            chunk.copy_from_slice(&block);
        }
    };
    let sha1 = |parts: &[&[u8]]| Hash::Sha1.digest(parts);
    let md5 = |parts: &[&[u8]]| Hash::Md5.digest(parts);
    let header = |flags: i32, algorithm: i32, csp: &str| {
        let mut header = Vec::new();
        for field in [flags, 0, algorithm, 0x8004, 128, 0x18, 0, 0] {
            header.extend_from_slice(&i32::to_le_bytes(field));
        }
        header.extend(csp.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
        let mut described = (header.len() as i32).to_le_bytes().to_vec();
        described.extend(header);
        described
    };
    let mut info = Vec::new();
    let page_key: PageKey = match provider {
        Provider::BinaryRc4 => {
            let mut filled = md5(&[&password])[..5].to_vec();
            filled.extend_from_slice(&salt);
            let repeated: Vec<u8> = filled.iter().copied().cycle().take(336).collect();
            let base = md5(&[&repeated])[..5].to_vec();
            let key = move |block: &[u8]| md5(&[&base, block]);
            let mut sealed = verifier;
            let mut sealed_hash = md5(&[&verifier]);
            rc4(
                &key(&0i32.to_le_bytes()),
                &mut [&mut sealed, &mut sealed_hash],
            );
            info.extend([1, 0, 1, 0]);
            info.extend(salt);
            info.extend(sealed);
            info.extend(sealed_hash);
            Box::new(key)
        }
        Provider::CryptoApi => {
            let base = sha1(&[&salt, &password]);
            let key = move |block: &[u8]| sha1(&[&base, block])[..16].to_vec();
            let mut sealed = verifier;
            let mut sealed_hash = sha1(&[&verifier]);
            rc4(
                &key(&0i32.to_le_bytes()),
                &mut [&mut sealed, &mut sealed_hash],
            );
            info.extend([4, 0, 2, 0]);
            info.extend(0x04i32.to_le_bytes());
            info.extend(header(
                0x04,
                0x6801,
                "Microsoft Enhanced Cryptographic Provider v1.0",
            ));
            info.extend(16i32.to_le_bytes());
            info.extend(salt);
            info.extend(sealed);
            info.extend(20i32.to_le_bytes());
            info.extend(sealed_hash);
            Box::new(key)
        }
        Provider::Standard(verifier_block) => {
            let iterated = (0..50_000u32).fold(sha1(&[&salt, &password]), |hashed, i| {
                sha1(&[&i.to_le_bytes(), &hashed])
            });
            let key = move |block: &[u8]| {
                let final_hash = sha1(&[&iterated, block]);
                let x = |fill: u8| {
                    let mut buffer = [fill; 64];
                    for (byte, hashed) in buffer.iter_mut().zip(&final_hash) {
                        *byte ^= hashed;
                    }
                    sha1(&[&buffer])
                };
                let mut derived = x(0x36);
                derived.extend(x(0x5C));
                derived[..16].to_vec()
            };
            let key0 = key(&verifier_block.to_le_bytes());
            let mut sealed = verifier;
            aes(&key0, &mut sealed);
            let mut sealed_hash = sha1(&[&verifier]);
            sealed_hash.resize(32, 0);
            aes(&key0, &mut sealed_hash);
            info.extend([4, 0, 2, 0]);
            info.extend(0x24i32.to_le_bytes());
            info.extend(header(
                0x24,
                0x660E,
                "Microsoft Enhanced RSA and AES Cryptographic Provider",
            ));
            info.extend(16i32.to_le_bytes());
            info.extend(salt);
            info.extend(sealed);
            info.extend(20i32.to_le_bytes());
            info.extend(sealed_hash);
            Box::new(key)
        }
    };
    let at = 0x299;
    file[at..at + 2].copy_from_slice(&(info.len() as i16).to_le_bytes());
    file[at + 2..at + 2 + info.len()].copy_from_slice(&info);
    let start = format.offset_encoding_key;
    let mask = &format.header_mask[start - format.offset_masked_header..];
    for (i, byte) in encoding_key.into_iter().enumerate() {
        file[start + i] = byte ^ mask[i];
    }
    for (number, page) in file.chunks_mut(format.page_size).enumerate().skip(1) {
        let key = page_key(&block(number as u32));
        match provider {
            Provider::BinaryRc4 | Provider::CryptoApi => rc4(&key, &mut [page]),
            Provider::Standard(_) => aes(&key, page),
        }
    }
    file
}

#[test]
fn reads_databases_under_each_office_encryption() {
    let scratch = Scratch::new("access-office-encryption");
    let original = fixture("ace-complex.accdb");
    let (plain, _) = crate::Extractor::new()
        .extract_file_to_string(&format!("{FIXTURES}/ace-complex.accdb"))
        .unwrap();
    for provider in [
        Provider::BinaryRc4,
        Provider::CryptoApi,
        Provider::Standard(0),
        Provider::Standard(JACKCESS_VERIFIER_BLOCK),
    ] {
        let path = scratch.write(
            &format!("{provider:?}.accdb"),
            &encrypted(original.clone(), provider, "hemmelig"),
        );
        let path = path.to_str().unwrap();
        let (text, _) = crate::Extractor::new()
            .set_passwords([Password::new("hemmelig")])
            .extract_file_to_string(path)
            .unwrap();
        assert_eq!(text, plain, "{provider:?}");
        let error = crate::Extractor::new()
            .set_passwords([Password::new("forkert")])
            .extract_file_to_string(path)
            .unwrap_err();
        assert!(matches!(error, crate::Error::Encrypted(_)), "{error:?}");
    }
}

#[test]
fn reads_encrypted_databases_with_their_password() {
    for extractor in [
        crate::Extractor::new(),
        crate::Extractor::new().set_passwords([Password::new("wrong")]),
    ] {
        let error = extractor.extract_file_to_string(ENCRYPTED).unwrap_err();
        assert!(matches!(error, crate::Error::Encrypted(_)), "{error:?}");
    }
    let (text, _) = crate::Extractor::new()
        .set_passwords([Password::new(PASSWORD)])
        .extract_file_to_string(ENCRYPTED)
        .unwrap();
    assert!(!text.trim().is_empty(), "{text}");
}

/// Jet 4's header: where the database password lies, how long it is, and where the creation
/// date that masks it lies.
const JET4_PASSWORD: usize = 66;
const JET4_PASSWORD_LENGTH: usize = 40;
const JET4_HEADER_DATE: usize = 114;

/// A Jet 4 database given the database password `password`, masked as Access masks it: by the
/// header mask and by the integer part of the database's creation date.
fn with_password(mut file: Vec<u8>, password: &str) -> Vec<u8> {
    let Ok(format) = format::detect(&file) else {
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
}

#[test]
fn fails_on_damaged_databases_as_jackcess_throws() {
    let scratch = Scratch::new("access-damaged");
    let original = fixture("jet4-turkish.mdb");
    let short = scratch.write("short.mdb", &original[..2000]);
    let error = crate::Extractor::new()
        .extract_file_to_string(short.to_str().unwrap())
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Parse error occurred : org.apache.tika.exception.TikaException: TIKA-198: Illegal \
         IOException from org.apache.tika.parser.microsoft.JackcessParser <- caused by \
         java.io.IOException: Failed attempting to read 4096 bytes from page 0, only read 2000"
    );
    let truncated = scratch.write("truncated.mdb", &original[..3 * 4096]);
    let (difference, failure) = failing_reading(&truncated);
    assert_eq!(
        difference.0,
        "a damaged database, which Jackcess may throw on elsewhere and otherwise than the port: \
         java.lang.IllegalStateException: invalid page number 6"
    );
    let corrupt = "Parse error occurred : org.apache.tika.exception.TikaException: Corrupt Access \
                   database: invalid page number 6 <- caused by \
                   java.lang.IllegalStateException: invalid page number 6";
    assert_eq!(failure, format!("ParseError({corrupt:?})"));
    {
        let error = crate::Extractor::new()
            .extract_file_to_string(truncated.to_str().unwrap())
            .unwrap_err();
        assert_eq!(error.to_string(), corrupt);
    }
}

/// Where the native parser's reading of the database at `path` may differ from Tika's, a
/// damaged database's, and the error the reading fails with, which the extraction takes.
fn failing_reading(path: &Path) -> (Difference, String) {
    let input = std::fs::read(path).unwrap();
    let configs = ParserConfigs::default();
    let Err(Decline::Differs {
        reading,
        difference,
    }) = JackcessParser.prepare(&input, &TikaMetadata::new(), &configs)
    else {
        panic!("{}: no reading that differs", path.display());
    };
    let context =
        ParseContext::new(&configs).reading(InputFile::Original(path.to_str().unwrap().to_owned()));
    let mut out = StringOut::default();
    let mut handler = Body::new(ToText::new(&mut out));
    let (_, _, result) = Xhtml::write(&mut handler, TikaMetadata::new(), &context, reading);
    let Err(Failure::Extraction(error)) = result else {
        panic!("{}: the reading does not fail: {result:?}", path.display());
    };
    (difference, format!("{error:?}"))
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
    let Ok(format) = format::detect(&header) else {
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

/// The Jet 3 fixture with its header naming the code page `code_page`.
fn jet3_in_code_page(code_page: u16) -> Vec<u8> {
    let mut header = fixture("jet3-queries.mdb");
    let Ok(format) = format::detect(&header) else {
        panic!("a Jet database");
    };
    let at = JET3_CODE_PAGE;
    let mask = &format.header_mask[at - format.offset_masked_header..];
    for (i, byte) in code_page.to_le_bytes().into_iter().enumerate() {
        header[at + i] = byte ^ mask[i];
    }
    header
}

/// A Jet 3 database whose header names no code page is decoded in code page 1252, the ANSI code
/// page of Access 97, where Jackcess decodes every Jet 3 database as UTF-8 (#185).
#[test]
fn reads_jet3_text_of_no_code_page_in_code_page_1252() {
    let scratch = Scratch::new("access-jet3-no-code-page");
    let unnamed = scratch.write(
        "unnamed.mdb",
        &replaced(jet3_in_code_page(0), b"Table2", b"Tr\xe6er\xf8"),
    );
    let (xml, _) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string(unnamed.to_str().unwrap())
        .unwrap();
    assert!(xml.contains("<table name=\"Træerø\">"), "{xml}");
}

/// A Jet 3 database in a code page that has no charset fails, naming the code page, where
/// Jackcess decodes it as UTF-8 (#185).
#[test]
fn fails_on_jet3_text_of_a_code_page_without_a_charset() {
    let scratch = Scratch::new("access-jet3-unknown-code-page");
    let unknown = scratch.write("unknown.mdb", &jet3_in_code_page(9));
    let error = crate::Extractor::new()
        .extract_file_to_string(unknown.to_str().unwrap())
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("code page 9, which has no charset"),
        "{error}"
    );
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
    let database = Database::open(Database::pages(&original, "").unwrap()).unwrap();
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

/// A database too large to read whole is read a page at a time and written a row at a time, in
/// memory that does not grow with it: a table of a data page repeated past `MAX_READ_WHOLE`,
/// which a reference usage map gives it.
#[test]
fn extracts_databases_too_large_to_read_whole_in_bounded_memory() {
    use crate::input::MAX_READ_WHOLE;
    use dijo_core::large::LargeFile;

    let mut file = fixture("jet4-turkish.mdb");
    let format = format::detect(&file).unwrap();
    let size = format.page_size;
    let database = Database::open(Database::pages(&file, "").unwrap()).unwrap();
    let table = database
        .tables()
        .unwrap()
        .into_iter()
        .next()
        .expect("a table");
    drop(database);
    let definition = usize::try_from(table.page).unwrap();
    let data_page = file
        .chunks(size)
        .position(|page| {
            page[0] == PAGE_TYPE_DATA
                && i32::from_le_bytes(page[4..8].try_into().unwrap()) == table.page
        })
        .expect("a data page of the table");
    let data = file[data_page * size..(data_page + 1) * size].to_vec();
    let owned_at = definition * size + format.offset_owned_pages;
    let map_row = i32::from(file[owned_at] as i8);
    let map_page = usize::from(file[owned_at + 1])
        | usize::from(file[owned_at + 2]) << 8
        | usize::from(file[owned_at + 3]) << 16;
    let (start, end) = {
        let page = Buffer::new(&file[map_page * size..(map_page + 1) * size]);
        (
            usize::try_from(row_start(&page, map_row, format).unwrap()).unwrap(),
            usize::try_from(row_end(&page, map_row, format).unwrap()).unwrap(),
        )
    };
    let first_copy = file.len() / size;
    let copies = usize::try_from(MAX_READ_WHOLE).unwrap() / size + 1;
    let usage_page = first_copy + copies;
    let data_start = format.offset_usage_map_page_data;
    assert!(
        usage_page < (size - data_start) * 8,
        "one usage map page maps them all"
    );
    let mut usage = vec![0u8; size];
    usage[0] = 0x05;
    usage[1] = 0x01;
    for page in std::iter::once(data_page).chain(first_copy..usage_page) {
        usage[data_start + page / 8] |= 1 << (page % 8);
    }
    let row = &mut file[map_page * size + start..map_page * size + end];
    row.fill(0);
    row[0] = 0x01;
    let pointer = format.offset_reference_map_page_numbers;
    row[pointer..pointer + 4].copy_from_slice(&i32::try_from(usage_page).unwrap().to_le_bytes());
    let large = LargeFile::new("large.mdb", &file, copies as u64, |_| data.clone(), &usage);
    drop(file);
    let mut lines = 0u64;
    let metadata = large.extract_lines("dijo::JackcessParser", |_| lines += 1);
    assert!(
        lines > copies as u64,
        "{lines} lines of {copies} pages of rows"
    );
    assert_eq!(metadata["Content-Type"], ["application/x-msaccess"]);
}
