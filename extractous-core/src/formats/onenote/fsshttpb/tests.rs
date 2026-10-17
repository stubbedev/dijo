use super::*;

#[test]
fn names_uuids_by_the_md5_of_their_bytes_as_java_does() {
    let uuid = Uuid::named(b"test");
    assert_eq!(uuid.to_string(), "098f6bcd-4621-3373-8ade-4e832627b4f6");
    assert_eq!(uuid.java_hash(), 0xe357_a2cb);
    assert_eq!(
        Uuid::EMPTY.to_string(),
        "00000000-0000-0000-0000-000000000000"
    );
}

#[test]
fn reads_compact_numbers_low_bit_first() {
    assert_eq!(compact(&[0x00], 0).unwrap(), (0, 1));
    assert_eq!(compact(&[0x05], 0).unwrap(), (2, 1));
    assert_eq!(compact(&[0x06, 0x01], 0).unwrap(), (0x41, 2));
    let mut nine = vec![0x80];
    nine.extend(1234567890123u64.to_le_bytes());
    assert_eq!(compact(&nine, 0).unwrap(), (1234567890123, 9));
    assert!(compact(&[0x02], 0).is_err());
}

#[test]
fn reads_extended_guids_of_each_size() {
    let guid = [7u8; 16];
    let mut five = vec![0x04 | (3 << 3)];
    five.extend(guid);
    let (read, length) = ex_guid(&five, 0).unwrap();
    assert_eq!((read.value, read.kind, length), (3, 4, 17));
    assert_eq!(read.guid, Uuid::named(&guid));
    let (null, length) = ex_guid(&[0], 0).unwrap();
    assert_eq!((null.guid, length), (Uuid::EMPTY, 1));
    assert_eq!(
        ex_guid(&[0x01], 0).unwrap_err(),
        Thrown(
            "org.apache.poi.openxml4j.exceptions.InvalidOperationException: Failed to parse the \
             ExGuid, the type value is unexpected"
                .to_owned()
        )
    );
}

#[test]
fn pads_copies_past_the_end_and_fails_from_beyond_it() {
    assert_eq!(uint32(&[1, 2], 0).unwrap(), 0x0201);
    assert_eq!(uint32(&[0xFF, 0xFF, 0xFF, 0xFF], 0).unwrap(), -1);
    assert!(matches!(uint32(&[1], 3), Err(Failure::Unported(_))));
    assert!(int32(&[1, 2, 3], 0).is_err());
}
