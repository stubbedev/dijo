use super::*;

#[test]
fn falls_short_on_what_is_no_presentation() {
    let compound = crate::ole2::tests::compound(&["WordDocument"], b"");
    assert!(matches!(
        plan(&compound, &ParserConfigs::default()),
        Err(Decline::Unread(reason)) if reason.contains("no presentation")
    ));
}

/// A presentation too large to read whole, its size in an embedded object, is read in place:
/// its records built from the document stream where they lie and the object handed on from
/// there, in memory that does not grow with it.
#[test]
fn extracts_presentations_too_large_to_read_whole_in_bounded_memory() {
    use crate::input::MAX_READ_WHOLE;
    use crate::ole2::{write, Node, Storage};
    use dijo_core::large::LargeFile;
    use records::kind;
    use records::tests::record;
    use std::fmt::Write as _;

    let mut log = String::new();
    let mut lines = 0u64;
    while log.len() as u64 <= MAX_READ_WHOLE {
        writeln!(log, "Linje {lines} i logbogen").unwrap();
        lines += 1;
    }
    let mut stream = record(
        0x000F,
        kind::DOCUMENT,
        &record(0x0001, kind::DOCUMENT_ATOM, &[0; 40]),
    );
    let object_at = stream.len() as u32;
    stream.extend(record(0, kind::EX_OLE_OBJ_STG, log.as_bytes()));
    drop(log);
    let pointers_at = stream.len() as u32;
    let mut pointers = ((2u32 << 20) | 1).to_le_bytes().to_vec();
    pointers.extend(0u32.to_le_bytes());
    pointers.extend(object_at.to_le_bytes());
    stream.extend(record(0, kind::PERSIST_PTR_INCREMENTAL_BLOCK, &pointers));
    let edit_at = stream.len() as u32;
    let mut edit = vec![0; 32];
    edit[12..16].copy_from_slice(&pointers_at.to_le_bytes());
    edit[16..20].copy_from_slice(&1u32.to_le_bytes());
    edit[28..32].copy_from_slice(&(-1i32).to_le_bytes());
    stream.extend(record(0, kind::USER_EDIT_ATOM, &edit));
    let mut current_user = vec![0; 28];
    current_user[16..20].copy_from_slice(&edit_at.to_le_bytes());
    let units = |name: &str| name.encode_utf16().collect::<Vec<u16>>();
    let presentation = write(&Storage {
        class_id: [0; 16],
        entries: vec![
            (units("Current User"), Node::Owned(current_user)),
            (units("PowerPoint Document"), Node::Owned(stream)),
        ],
    })
    .unwrap();
    let file = LargeFile::new("large.ppt", &presentation, 0, |_| Vec::new(), b"");
    drop(presentation);
    let last = format!("Linje {} i logbogen", lines - 1);
    let (mut first_seen, mut last_seen) = (false, false);
    let metadata = file.extract_lines("dijo::OfficeParser", |line| match line.trim() {
        "Linje 0 i logbogen" => first_seen = true,
        line if line == last => last_seen = first_seen,
        _ => {}
    });
    assert!(first_seen && last_seen);
    assert_eq!(metadata["Content-Type"], ["application/vnd.ms-powerpoint"]);
}
