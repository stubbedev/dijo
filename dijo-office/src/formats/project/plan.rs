//! The tasks and resources of a project, as MPXJ's MPP readers find them in the backing
//! stores: which records are items, which of them hold the same item, and in which order the
//! plan lists the tasks.

use std::collections::{BTreeMap, HashMap};

use super::blocks::{
    ansi, fixed_data, padded, unicode, FieldMap, FixedMeta, Props, Sizing, VarData,
};
use super::{key, storage, stream, Malformed, Obfuscation, Version};
use super::{OUTLINE_LEVEL, RESOURCE_CLASS, RESOURCE_NAME, RESOURCE_NOTES, RESOURCE_UNIQUE_ID};
use super::{TASK_CLASS, TASK_ID, TASK_NAME, TASK_NOTES, TASK_UNIQUE_ID};
use crate::ole2::{EntryId, Ole2};

/// The length of each task's and each resource's record of meta data.
const TASK_META_ITEM: usize = 47;
const RESOURCE_META_ITEM: usize = 37;
/// The lengths MPP12's and MPP14's tasks' second records of meta data may have.
const TASK_META2_ITEM_12: usize = 86;
const TASK_META2_ITEMS_14: [usize; 5] = [92, 93, 94, 95, 96];
/// The longest task record MPP9 and MPP12 read.
const TASK_RECORD_MAX: usize = 768;
/// The records before the first task's, which hold no task.
const FIRST_TASK: usize = 3;
/// The flag of a task's meta data that marks it deleted.
const DELETED: u32 = 0x02;
/// The flag of a task's meta data that keeps a later record of the same task from replacing
/// it.
const SUPERSEDED: u32 = 0x04;
/// Where the unique ID and the ID of a null task, a blank row of the plan, lie.
const NULL_TASK_UNIQUE_ID: usize = 0;
const NULL_TASK_ID: usize = 4;
/// Where a task's second fixed record holds the key that orders the plan, and how long the
/// record is at least where it does.
const ORDER_KEY: usize = 16;
const ORDER_KEY_RECORD: usize = 24;
/// The share of the first fixed block a task record must cover, in percent, to be a task.
const TASK_RECORD_COVERAGE: usize = 75;

/// A task's or a resource's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Item {
    pub(super) name: Option<String>,
    /// The notes' bytes: RTF, or text in a code page.
    pub(super) notes: Option<Vec<u8>>,
}

/// A task: its outline level and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Task {
    pub(super) level: u16,
    pub(super) item: Item,
}

/// The tasks in the order the plan lists them, then the resources in the order of their
/// unique IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    pub(super) tasks: Vec<Task>,
    pub(super) resources: Vec<Item>,
}

/// The plan of the project storage `project`, at `path`, whose properties are `props`.
pub(super) fn read(
    file: &Ole2<'_>,
    project: EntryId,
    path: &str,
    version: Version,
    props: &Props,
    obfuscation: Obfuscation,
) -> Result<Plan, Malformed> {
    Ok(Plan {
        tasks: tasks(file, project, path, version, props, obfuscation)?,
        resources: resources(file, project, path, version, props, obfuscation)?,
    })
}

/// The text of the item `id` of `var`: its name and its notes, each where it is not empty.
fn item(var: &VarData<'_>, id: i32, name: u16, notes: u16) -> Item {
    Item {
        name: var
            .get(id, name)
            .map(unicode)
            .filter(|name| !name.is_empty()),
        notes: var
            .get(id, notes)
            .map(ansi)
            .filter(|notes| !notes.is_empty())
            .map(<[u8]>::to_vec),
    }
}

fn int_at(record: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(padded(record, at))
}

fn short_at(record: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(padded(record, at))
}

/// A task read from its record, before the plan is ordered.
struct Row {
    unique_id: i32,
    id: i32,
    task: Task,
}

/// The tasks of `TBkndTask`, in the plan's order.
fn tasks(
    file: &Ole2<'_>,
    project: EntryId,
    path: &str,
    version: Version,
    props: &Props,
    obfuscation: Obfuscation,
) -> Result<Vec<Task>, Malformed> {
    let directory = storage(file, project, "TBkndTask", path)?;
    let path = format!("{path}/TBkndTask");
    let fields = FieldMap::read(
        props,
        &[key::TASK_FIELD_MAP, key::TASK_FIELD_MAP2],
        TASK_CLASS,
        version,
        version.task_defaults(),
    );
    let unique_id_at = fields.fixed(TASK_UNIQUE_ID, "task")?;
    let id_at = fields.fixed(TASK_ID, "task")?;
    let level_at = fields.fixed(OUTLINE_LEVEL, "task")?;
    let name_key = fields.var(TASK_NAME, "task")?;
    let notes_key = fields.var(TASK_NOTES, "task")?;

    let meta_data = stream(file, directory, &path, "FixedMeta")?;
    let meta = FixedMeta::read(&meta_data, TASK_META_ITEM, &format!("{path}/FixedMeta"))?;
    let data = stream(file, directory, &path, "FixedData")?;
    let (data, sizing) = match version {
        Version::Mpp9 => (
            obfuscation.reveal(data),
            Sizing {
                max: TASK_RECORD_MAX,
                min: fields.fixed_size,
            },
        ),
        Version::Mpp12 => (
            data,
            Sizing {
                max: TASK_RECORD_MAX,
                min: fields.fixed_size,
            },
        ),
        Version::Mpp14 => (
            data,
            Sizing {
                max: fields.fixed_size,
                min: 0,
            },
        ),
    };
    let records = fixed_data(&meta, &data, sizing);
    let meta2_data;
    let data2;
    let records2 = match version {
        Version::Mpp9 => None,
        Version::Mpp12 | Version::Mpp14 => {
            meta2_data = stream(file, directory, &path, "Fixed2Meta")?;
            data2 = stream(file, directory, &path, "Fixed2Data")?;
            let what = format!("{path}/Fixed2Meta");
            let meta2 = match version {
                Version::Mpp12 => FixedMeta::read(&meta2_data, TASK_META2_ITEM_12, &what)?,
                _ => FixedMeta::read_sized_by(
                    &meta2_data,
                    &TASK_META2_ITEMS_14,
                    records.len(),
                    &what,
                )?,
            };
            Some(fixed_data(&meta2, &data2, Sizing::UNLIMITED))
        }
    };
    let second = |index: usize| {
        records2
            .as_ref()
            .and_then(|records2| records2.get(index).copied().flatten())
    };
    let var_meta = stream(file, directory, &path, "VarMeta")?;
    let var_data = stream(file, directory, &path, "Var2Data")?;
    let var = VarData::read(&var_meta, &var_data, version, &path)?;
    let null_size = match version {
        Version::Mpp9 => 8,
        Version::Mpp12 | Version::Mpp14 => 16,
    };

    let mut records_by_id: BTreeMap<i32, Option<usize>> = BTreeMap::new();
    for index in (FIRST_TASK..records.len()).rev() {
        let Some(record) = records[index] else {
            continue;
        };
        if records2.is_some() && second(index).is_none() {
            continue;
        }
        let flags = meta.flags(index);
        if flags & DELETED != 0 {
            let unique_id = i32::from(short_at(record, NULL_TASK_UNIQUE_ID));
            match version {
                Version::Mpp14 => {
                    records_by_id.entry(unique_id).or_insert(None);
                }
                Version::Mpp9 | Version::Mpp12 => {
                    records_by_id.insert(unique_id, None);
                }
            }
        } else if record.len() == null_size {
            records_by_id
                .entry(int_at(record, NULL_TASK_UNIQUE_ID))
                .or_insert(Some(index));
        } else if fields.fixed_size == 0
            || record.len() * 100 / fields.fixed_size > TASK_RECORD_COVERAGE
        {
            let unique_id = int_at(record, unique_id_at);
            let known = records_by_id.contains_key(&unique_id);
            if (!known || var.has(unique_id)) && (!known || flags & SUPERSEDED == 0) {
                records_by_id.insert(unique_id, Some(index));
            }
        }
    }

    let mut rows: Vec<Option<Row>> = Vec::new();
    let mut row_by_id: HashMap<i32, usize> = HashMap::new();
    let mut ordered: BTreeMap<i64, i32> = BTreeMap::new();
    let mut unordered: BTreeMap<i32, i32> = BTreeMap::new();
    for (&unique_id, &index) in &records_by_id {
        let Some(record) = index.and_then(|index| records[index]) else {
            continue;
        };
        if record.len() == null_size {
            unordered
                .entry(int_at(record, NULL_TASK_ID))
                .or_insert(int_at(record, NULL_TASK_UNIQUE_ID));
            continue;
        }
        if version == Version::Mpp9
            && (record.len() < fields.fixed_size || (unique_id != 0 && !var.has(unique_id)))
        {
            continue;
        }
        let id = int_at(record, id_at);
        if let Some(&earlier) = row_by_id.get(&id) {
            if !var.has(unique_id) {
                continue;
            }
            if rows[earlier]
                .as_ref()
                .is_some_and(|row| row.task.item.name.is_none())
            {
                rows[earlier] = None;
            }
        }
        if records2.is_some() {
            match second(index.expect("a record read")) {
                Some(record2) if record2.len() >= ORDER_KEY_RECORD => {
                    ordered.insert(i64::from_le_bytes(padded(record2, ORDER_KEY)), unique_id);
                }
                _ => {
                    unordered.insert(id, unique_id);
                }
            }
        }
        row_by_id.insert(id, rows.len());
        rows.push(Some(Row {
            unique_id,
            id,
            task: Task {
                level: short_at(record, level_at),
                item: item(&var, unique_id, name_key, notes_key),
            },
        }));
    }
    let mut rows: Vec<Row> = rows.into_iter().flatten().collect();
    if records2.is_some() {
        let has_summary = rows.iter().any(|row| row.unique_id == 0);
        let ids = renumbered(&ordered, &unordered, has_summary)?;
        for row in &mut rows {
            if let Some(&id) = ids.get(&row.unique_id) {
                row.id = id;
            }
        }
    }
    rows.sort_by_key(|row| row.id);
    Ok(rows.into_iter().map(|row| row.task).collect())
}

/// `postProcessTasks`: the IDs of the tasks of MPP12 and MPP14 by unique ID, numbered in the
/// order of their keys, with the tasks that have none, null tasks among them, slotted in where
/// their own IDs place them. The project's summary task, unique ID 0, is numbered 0.
pub(super) fn renumbered(
    ordered: &BTreeMap<i64, i32>,
    unordered: &BTreeMap<i32, i32>,
    has_summary: bool,
) -> Result<HashMap<i32, i32>, Malformed> {
    let increment = (unordered.len() as i64 / 1000 + 1) * 2000;
    let mut slots: BTreeMap<i64, i32> = BTreeMap::new();
    let mut next = if has_summary { 0 } else { increment };
    for &unique_id in ordered.values() {
        slots.insert(next, unique_id);
        next += increment;
    }
    let mut offsets: HashMap<i64, i64> = HashMap::new();
    for (inserted, (&id, &unique_id)) in unordered.iter().enumerate() {
        let base = (i64::from(id) - inserted as i64) * increment;
        let mut offset = offsets.get(&base).map_or(0, |offset| offset + 1);
        let mut target = base;
        while slots.contains_key(&target) {
            offset += 1;
            if offset >= increment {
                return Err(Malformed(format!(
                    "the task of ID {id} has no slot left in the plan's order"
                )));
            }
            target = base - (increment - offset);
        }
        offsets.insert(base, offset);
        slots.insert(target, unique_id);
    }
    let first = i32::from(!has_summary);
    Ok(slots.into_values().zip(first..).collect())
}

/// The resources of `TBkndRsc` that have variable data, in the order of their unique IDs.
fn resources(
    file: &Ole2<'_>,
    project: EntryId,
    path: &str,
    version: Version,
    props: &Props,
    obfuscation: Obfuscation,
) -> Result<Vec<Item>, Malformed> {
    let directory = storage(file, project, "TBkndRsc", path)?;
    let path = format!("{path}/TBkndRsc");
    let fields = FieldMap::read(
        props,
        &[key::RESOURCE_FIELD_MAP, key::RESOURCE_FIELD_MAP2],
        RESOURCE_CLASS,
        version,
        version.resource_defaults(),
    );
    let unique_id_at = fields.fixed(RESOURCE_UNIQUE_ID, "resource")?;
    let name_key = fields.var(RESOURCE_NAME, "resource")?;
    let notes_key = fields.var(RESOURCE_NOTES, "resource")?;
    let meta_data = stream(file, directory, &path, "FixedMeta")?;
    let meta = FixedMeta::read(&meta_data, RESOURCE_META_ITEM, &format!("{path}/FixedMeta"))?;
    let data = obfuscation.reveal(stream(file, directory, &path, "FixedData")?);
    let records = fixed_data(&meta, &data, Sizing::UNLIMITED);
    let var_meta = stream(file, directory, &path, "VarMeta")?;
    let var_data = stream(file, directory, &path, "Var2Data")?;
    let var = VarData::read(&var_meta, &var_data, version, &path)?;
    let mut resources: BTreeMap<i32, usize> = BTreeMap::new();
    for (index, record) in records.iter().enumerate() {
        let Some(record) = record.filter(|record| record.len() >= fields.fixed_size) else {
            continue;
        };
        resources
            .entry(i32::from(short_at(record, unique_id_at)))
            .or_insert(index);
    }
    Ok(var
        .ids()
        .filter(|id| resources.contains_key(id))
        .map(|id| item(&var, id, name_key, notes_key))
        .collect())
}
