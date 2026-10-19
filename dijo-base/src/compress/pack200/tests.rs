use std::io::Cursor as IoCursor;

use super::*;

const HELLO_WORLD: &[u8] = include_bytes!("../../../tests/fixtures/pack200/testPACK200.pack");

const CLASS_NAME: &str = "org/apache/harmony/archive/tests/internal/pack200/HelloWorld.class";

#[test]
fn unpacks_the_class_files_of_an_archive() {
    let entries = unpack(HELLO_WORLD).unwrap();
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, [CLASS_NAME]);
    let class = &entries[0].bytes;
    assert_eq!(class[..4], [0xCA, 0xFE, 0xBA, 0xBE]);
    let name = b"HelloWorld";
    assert!(class.windows(name.len()).any(|window| window == name));
}

#[test]
fn decodes_an_archive_into_a_jar_of_its_files() {
    let jar = decode(HELLO_WORLD).unwrap().whole().unwrap();
    let mut archive = zip::ZipArchive::new(IoCursor::new(jar.to_vec())).unwrap();
    assert_eq!(archive.len(), 1);
    let entry = archive.by_index(0).unwrap();
    assert_eq!(entry.name().unwrap(), CLASS_NAME);
    assert_eq!(
        entry.size(),
        unpack(HELLO_WORLD).unwrap()[0].bytes.len() as u64
    );
}

#[test]
fn fails_a_truncated_archive() {
    for length in [4, 16, HELLO_WORLD.len() / 2, HELLO_WORLD.len() - 1] {
        assert!(
            unpack(&HELLO_WORLD[..length]).is_err(),
            "{length} bytes unpacked"
        );
    }
}

#[test]
fn fails_a_stream_that_is_no_archive() {
    assert!(unpack(b"\xCA\xFE\xD0\x0Dnot an archive at all").is_err());
}

#[test]
fn writes_entry_times_in_utc() {
    assert_eq!(
        zip_time(1_000_000_000),
        DateTime::from_date_and_time(2001, 9, 9, 1, 46, 40).unwrap()
    );
    assert_eq!(zip_time(0), DateTime::default());
}
