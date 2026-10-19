use super::*;

#[test]
fn reads_guids_as_they_are_stored() {
    let stored = [
        0xF8, 0x17, 0x37, 0x4A, 0x14, 0x1C, 0xE7, 0x49, 0x95, 0x26, 0x81, 0xD9, 0x42, 0xDE, 0x17,
        0x41,
    ];
    let guid = Uuid::stored(stored);
    assert_eq!(guid.to_string(), "4a3717f8-1c14-49e7-9526-81d942de1741");
    assert_eq!(read_guid(&stored, 0).unwrap(), guid);
    assert_eq!(
        Uuid::EMPTY.to_string(),
        "00000000-0000-0000-0000-000000000000"
    );
}

#[test]
fn recognises_the_encryption_key_root_as_it_is_stored() {
    let mut stored = vec![0x04 | (3 << 3)];
    stored.extend([
        0xF8, 0x17, 0x37, 0x4A, 0x14, 0x1C, 0xE7, 0x49, 0x95, 0x26, 0x81, 0xD9, 0x42, 0xDE, 0x17,
        0x41,
    ]);
    let (root, _) = ex_guid(&stored, 0).unwrap();
    assert_eq!(root.key(), store::ENCRYPTION_KEY_ROOT);
}

#[test]
fn reads_a_serial_number_of_another_type_as_what_it_is() {
    let error = Cursor::new(&[0x05], 0).serial_number().unwrap_err();
    assert!(error.0.ends_with("but the actual value is 5"), "{error:?}");
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
    assert_eq!(read.guid, Uuid::stored(guid));
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
    assert_eq!(uint32(&[0xFF, 0xFF, 0xFF, 0xFF], 0).unwrap(), u32::MAX);
    assert_eq!(uint16(&[0xFF, 0xFF], 0).unwrap(), u16::MAX);
    assert!(matches!(uint32(&[1], 3), Err(Failure::Unported(_))));
    assert!(int32(&[1, 2, 3], 0).is_err());
}
