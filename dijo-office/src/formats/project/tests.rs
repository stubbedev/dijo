use std::collections::BTreeMap;

use super::blocks::{fixed_data, FixedMeta, Props, Sizing, VarData};
use super::plan::renumbered;
use super::*;
use crate::ole2::{write, Node, Storage};

const MAGIC: [u8; 4] = 0xFADF_ADBA_u32.to_le_bytes();

fn units(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}

fn storage(entries: Vec<(&str, Node<'static>)>) -> Node<'static> {
    Node::Storage(Storage {
        class_id: [0; 16],
        entries: entries
            .into_iter()
            .map(|(name, node)| (units(name), node))
            .collect(),
    })
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// A `CompObj` stream naming `format`, as Project writes it.
fn comp_obj(application: &str, format: &str) -> Vec<u8> {
    let mut data = vec![0; COMP_OBJ_HEADER];
    for text in [application, format, "MSProject.Project.9"] {
        data.extend((text.len() as u32 + 1).to_le_bytes());
        data.extend(text.as_bytes());
        data.push(0);
    }
    data
}

/// A Props stream of `values` by key.
fn props(values: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut data = vec![0; 16];
    data[12..14].copy_from_slice(&(values.len() as u16).to_le_bytes());
    for (key, value) in values {
        data.extend((value.len() as i32).to_le_bytes());
        data.extend(key.to_le_bytes());
        data.extend([0; 4]);
        data.extend(value);
        if value.len() % 2 == 1 {
            data.push(0);
        }
    }
    data
}

/// `FixedMeta` and `FixedData` of `records`, each meta record `meta_size` long with its
/// `flags`.
fn fixed(records: &[(u32, Vec<u8>)], meta_size: usize) -> (Vec<u8>, Vec<u8>) {
    let mut meta = MAGIC.to_vec();
    meta.extend([0; 4]);
    meta.extend((records.len() as i32).to_le_bytes());
    meta.extend([0; 4]);
    let mut data = Vec::new();
    for (flags, record) in records {
        let mut item = vec![0; meta_size];
        item[..4].copy_from_slice(&flags.to_le_bytes());
        item[4..8].copy_from_slice(&(data.len() as i32).to_le_bytes());
        meta.extend(item);
        data.extend(record);
    }
    (meta, data)
}

/// `VarMeta` and `Var2Data` of MPP12 and later of `values`: unique ID, key and value.
fn var(values: &[(i32, u16, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
    let mut meta = MAGIC.to_vec();
    meta.extend([0; 4]);
    meta.extend((values.len() as i32).to_le_bytes());
    meta.extend([0; 12]);
    let mut data = Vec::new();
    for (id, key, value) in values {
        meta.extend(id.to_le_bytes());
        meta.extend((data.len() as i32).to_le_bytes());
        meta.extend(key.to_le_bytes());
        meta.extend([0; 2]);
        data.extend((value.len() as i32).to_le_bytes());
        data.extend(value);
    }
    (meta, data)
}

/// A task of the default MPP14 field map: its unique ID, ID, outline level, name, notes and
/// the key of its row in the plan.
struct TaskSpec {
    unique_id: i32,
    id: i32,
    level: u16,
    name: &'static str,
    notes: Option<&'static str>,
    order: f64,
}

/// An MPP14 file of `tasks` and of resources of `resources` names and notes, the root's
/// Props `root_props`.
fn mpp14(
    tasks: &[TaskSpec],
    resources: &[(&str, Option<&str>)],
    root_props: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut task_records = vec![(0, vec![0; 8]); FIRST_TASK_RECORDS];
    let mut task_records2 = vec![(0, vec![0; 24]); FIRST_TASK_RECORDS];
    let mut task_values = Vec::new();
    for task in tasks {
        let mut record = vec![0; 48];
        record[..4].copy_from_slice(&task.unique_id.to_le_bytes());
        record[4..8].copy_from_slice(&task.id.to_le_bytes());
        record[40..42].copy_from_slice(&task.level.to_le_bytes());
        task_records.push((0, record));
        let mut record2 = vec![0; 24];
        record2[16..24].copy_from_slice(&task.order.to_bits().to_le_bytes());
        task_records2.push((0, record2));
        task_values.push((task.unique_id, 14, utf16z(task.name)));
        if let Some(notes) = task.notes {
            task_values.push((task.unique_id, 15, [notes.as_bytes(), &[0]].concat()));
        }
    }
    let mut resource_records = Vec::new();
    let mut resource_values = Vec::new();
    for (index, (name, notes)) in resources.iter().enumerate() {
        let unique_id = index as i32 + 1;
        let mut record = vec![0; 16];
        record[..2].copy_from_slice(&(unique_id as u16).to_le_bytes());
        resource_records.push((0, record));
        resource_values.push((unique_id, 1, utf16z(name)));
        if let Some(notes) = notes {
            resource_values.push((unique_id, 20, [notes.as_bytes(), &[0]].concat()));
        }
    }
    let (task_meta, task_data) = fixed(&task_records, 47);
    let (task_meta2, task_data2) = fixed(&task_records2, 92);
    let (task_var_meta, task_var_data) = var(&task_values);
    let (resource_meta, resource_data) = fixed(&resource_records, 37);
    let (resource_var_meta, resource_var_data) = var(&resource_values);
    let owned = |data: Vec<u8>| Node::Owned(data);
    let Node::Storage(root) = storage(vec![
        (
            COMP_OBJ,
            owned(comp_obj("Microsoft.Project 14.0", "MSProject.MPP14")),
        ),
        ("Props14", owned(props(&root_props))),
        (
            "   114",
            storage(vec![
                ("Props", owned(props(&[]))),
                (
                    "TBkndTask",
                    storage(vec![
                        ("FixedMeta", owned(task_meta)),
                        ("FixedData", owned(task_data)),
                        ("Fixed2Meta", owned(task_meta2)),
                        ("Fixed2Data", owned(task_data2)),
                        ("VarMeta", owned(task_var_meta)),
                        ("Var2Data", owned(task_var_data)),
                    ]),
                ),
                (
                    "TBkndRsc",
                    storage(vec![
                        ("FixedMeta", owned(resource_meta)),
                        ("FixedData", owned(resource_data)),
                        ("VarMeta", owned(resource_var_meta)),
                        ("Var2Data", owned(resource_var_data)),
                    ]),
                ),
            ]),
        ),
    ]) else {
        unreachable!("a storage")
    };
    write(&root).unwrap()
}

/// The records before the first task's.
const FIRST_TASK_RECORDS: usize = 3;

fn extract(document: &[u8]) -> (String, crate::Metadata) {
    crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(document)
        .unwrap()
}

fn plan() -> Vec<u8> {
    let task = |unique_id, id, level, name, notes, order| TaskSpec {
        unique_id,
        id,
        level,
        name,
        notes,
        order,
    };
    mpp14(
        &[
            task(0, 0, 0, "Fyrtårnet", None, 1.0),
            task(1, 1, 1, "Byg tårnet", None, 2.0),
            task(
                4,
                9,
                2,
                "Støb fundamentet",
                Some(r"{\rtf1\ansi Beton fra \'c5lborg\par}"),
                3.0,
            ),
            task(2, 2, 2, "Rejs muren", None, 5.0),
            task(3, 3, 1, "Tænd lampen", Some("Ved solnedgang"), 6.0),
        ],
        &[
            ("Fyrpasser", Some(r"{\rtf1\ansi Bor i t\'e5rnet\par}")),
            ("Murer", None),
        ],
        Vec::new(),
    )
}

/// An MPP14 file, which Tika's detection takes for a compound document of no known kind, is
/// read for its plan: the tasks in the order of their rows, nested by outline level, with
/// their notes, then the resources.
#[test]
fn reads_the_tasks_in_plan_order_nested_by_outline_level_then_the_resources() {
    let (xhtml, metadata) = extract(&plan());
    assert_eq!(metadata["Content-Type"], ["application/vnd.ms-project"]);
    assert!(metadata["tk:parsed-by"].contains(&"dijo::ProjectParser".to_owned()));
    let body = &xhtml[xhtml.find("<body>").unwrap()..];
    let expected = concat!(
        r#"<body><ul class="tasks">	<li><p>Fyrtårnet</p>"#,
        "\n",
        r#"<ul class="tasks">	<li><p>Byg tårnet</p>"#,
        "\n",
        r#"<ul class="tasks">	<li><p>Støb fundamentet</p>"#,
        "\n<p>Beton fra Ålborg</p>\n<p />\n</li>\n\t<li><p>Rejs muren</p>\n</li>\n</ul>\n</li>\n",
        "\t<li><p>Tænd lampen</p>\n<p>Ved solnedgang</p>\n</li>\n</ul>\n</li>\n</ul>\n",
        r#"<ul class="resources">	<li><p>Fyrpasser</p>"#,
        "\n<p>Bor i tårnet</p>\n<p />\n</li>\n\t<li><p>Murer</p>\n</li>\n</ul>\n</body></html>",
    );
    assert_eq!(body, expected);
}

/// The project's properties its summary information lacks are taken from its Props, written
/// as the summary information's are; blank ones are passed over.
#[test]
fn takes_the_projects_properties_its_summary_information_lacks_from_its_props() {
    let values = Props::read(
        &props(&[
            (key::TITLE, utf16z("Fyrtårnet")),
            (key::MANAGER, utf16z("Hans;Grete")),
            (key::SUBJECT, utf16z("   ")),
        ]),
        "Props",
    )
    .unwrap();
    let project = Project {
        props: values,
        plan: Plan {
            tasks: Vec::new(),
            resources: Vec::new(),
        },
    };
    let mut metadata = TikaMetadata::new();
    metadata.set("dc:title", "Fra resuméet");
    project.write_properties(&mut metadata);
    assert_eq!(metadata.values("dc:title"), ["Fra resuméet"]);
    assert_eq!(
        metadata.values("extended-properties:Manager"),
        ["Hans", "Grete"]
    );
    assert!(metadata.values("dc:subject").is_empty());
}

/// A file a password protects from being opened fails as Tika fails on a protected compound
/// document; one only protected from being written is read.
#[test]
fn fails_files_protected_from_being_opened_as_encrypted() {
    let protected = mpp14(
        &[],
        &[],
        vec![
            (key::PASSWORD_FLAG, vec![READ_PASSWORD, 0]),
            (key::PROTECTION_PASSWORD_HASH, vec![1; 64]),
        ],
    );
    let error = crate::Extractor::new()
        .extract_bytes_to_string(&protected)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("EncryptedDocumentException: Unable to process: document is encrypted"),
        "{error}"
    );
    let write_protected = mpp14(
        &[TaskSpec {
            unique_id: 1,
            id: 1,
            level: 1,
            name: "Skrivebeskyttet",
            notes: None,
            order: 2.0,
        }],
        &[],
        vec![(key::PASSWORD_FLAG, vec![0x02, 0])],
    );
    assert!(extract(&write_protected)
        .0
        .contains("<p>Skrivebeskyttet</p>"));
}

/// MPP9 hides a password-flagged file's Props and fixed data behind a mask its encryption
/// code gives; a flagged file with no password in its hash opens, as Project opens it.
#[test]
fn reveals_obfuscated_streams_and_reads_mpp9_passwords() {
    let mut root = Props::read(
        &props(&[
            (key::PASSWORD_FLAG, vec![READ_PASSWORD, 0]),
            (key::ENCRYPTION_CODE, vec![0x55, 0]),
            (key::PROTECTION_PASSWORD_HASH, vec![0xAA; 64]),
        ]),
        "Props9",
    )
    .unwrap();
    let obfuscation = Obfuscation::of(&root);
    assert_eq!(obfuscation, Obfuscation(Some(0xAA)));
    assert_eq!(
        obfuscation
            .reveal(Cow::Borrowed(&[0xAA, 0xAB][..]))
            .as_ref(),
        [0x00, 0x01]
    );
    assert!(!read_protected(Version::Mpp9, &root, obfuscation));
    let mut hash = vec![0xAA; 64];
    hash[PASSWORD_MASK[0]] = b'p' ^ 0xAA;
    root = Props::read(
        &props(&[
            (key::PASSWORD_FLAG, vec![READ_PASSWORD, 0]),
            (key::ENCRYPTION_CODE, vec![0x55, 0]),
            (key::PROTECTION_PASSWORD_HASH, hash),
        ]),
        "Props9",
    )
    .unwrap();
    assert!(read_protected(Version::Mpp9, &root, obfuscation));
    assert!(read_protected(Version::Mpp12, &root, obfuscation));
}

/// The plan of a file of a format of Project's other than MPP9, MPP12 and MPP14 is not read: it
/// is read as the compound document Tika reads it as, with a warning naming its format.
#[test]
fn reads_older_formats_as_compound_documents_only() {
    let Node::Storage(root) = storage(vec![
        (
            COMP_OBJ,
            Node::Owned(comp_obj("Microsoft Project 8.0", "MSProject.MPP8")),
        ),
        ("Props", Node::Owned(props(&[]))),
        ("   1", storage(Vec::new())),
    ]) else {
        unreachable!("a storage")
    };
    let document = write(&root).unwrap();
    let (_, metadata) = extract(&document);
    let warnings = &metadata[crate::formats::embedded::EXCEPTION_WARNING];
    assert!(
        warnings.iter().any(|warning| warning.contains("MSProject.MPP8")),
        "{warnings:?}"
    );
    assert!(!is_project(&Ole2::open(&document).unwrap()));
    let Node::Storage(root) = storage(vec![(COMP_OBJ, Node::Owned(comp_obj(PROJECT_4, "")))])
    else {
        unreachable!("a storage")
    };
    let file_data = write(&root).unwrap();
    let file = Ole2::open(&file_data).unwrap();
    assert_eq!(
        project_format(&file),
        Ok(Some(Format::Unread("MSProject.MPP4".to_owned())))
    );
}

/// A compound document whose `CompObj` names no format of Project's is no Project file.
#[test]
fn knows_project_files_by_their_comp_obj() {
    let Node::Storage(root) = storage(vec![(
        COMP_OBJ,
        Node::Owned(comp_obj("Microsoft Word", "MSWordDoc")),
    )]) else {
        unreachable!("a storage")
    };
    let data = write(&root).unwrap();
    assert!(!is_project(&Ole2::open(&data).unwrap()));
    assert!(is_project(&Ole2::open(&plan()).unwrap()));
    let Node::Storage(root) = storage(vec![(COMP_OBJ, Node::Owned(vec![0; 30]))]) else {
        unreachable!("a storage")
    };
    let data = write(&root).unwrap();
    assert!(project_format(&Ole2::open(&data).unwrap()).is_err());
}

/// The rows of tasks without a key of their own, null tasks among them, are slotted in where
/// their IDs place them among those numbered by key.
#[test]
fn slots_tasks_without_an_order_key_in_by_their_ids() {
    let ordered = BTreeMap::from([(10, 0), (20, 1), (30, 2)]);
    let unordered = BTreeMap::from([(2, 7)]);
    let ids = renumbered(&ordered, &unordered, true).unwrap();
    let mut by_id: Vec<(i32, i32)> = ids.into_iter().map(|(unique, id)| (id, unique)).collect();
    by_id.sort_unstable();
    assert_eq!(by_id, [(0, 0), (1, 1), (2, 7), (3, 2)]);
}

/// Lengths and offsets past the streams leave the records and values they describe out,
/// rather than read past the data.
#[test]
fn keeps_records_and_values_within_their_streams() {
    let (mut meta, data) = fixed(&[(0, vec![1; 4]), (0, vec![2; 4])], 10);
    meta[16 + 10 + 4..16 + 10 + 8].copy_from_slice(&1000_i32.to_le_bytes());
    let meta = FixedMeta::read(&meta, 10, "FixedMeta").unwrap();
    assert_eq!(meta.declared, 2);
    let records = fixed_data(&meta, &data, Sizing::UNLIMITED);
    assert_eq!(records, [Some(&data[..]), None]);
    assert!(FixedMeta::read(&[0; 8], 10, "FixedMeta").is_err());

    let (meta, mut data) = var(&[(1, 14, utf16z("Navn")), (2, 14, utf16z("Langt"))]);
    let second = data.len() - 12 - 4;
    data[second..second + 4].copy_from_slice(&i32::MAX.to_le_bytes());
    let values = VarData::read(&meta, &data, Version::Mpp14, "VarMeta").unwrap();
    assert_eq!(
        values.get(1, 14).map(blocks::unicode),
        Some("Navn".to_owned())
    );
    assert_eq!(values.get(2, 14), None);
    assert!(values.has(2));
    assert!(VarData::read(&[1; 24], &data, Version::Mpp9, "VarMeta").is_err());

    let mut truncated = props(&[(1, vec![1, 2, 3]), (2, vec![4; 8])]);
    truncated.truncate(truncated.len() - 2);
    let read = Props::read(&truncated, "Props").unwrap();
    assert_eq!(read.bytes(1), Some(&[1, 2, 3][..]));
    assert_eq!(read.bytes(2), None);
    assert!(Props::read(&[0; 10], "Props").is_err());
}
