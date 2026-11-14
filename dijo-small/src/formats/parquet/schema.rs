//! A file's schema, flattened depth first into its elements, read into its leaf columns: each
//! the path of names to it, the levels its nesting gives its values, its physical type and the
//! way its values are written, by its logical or else its converted type.

use super::footer::{Logical, SchemaElement, Unit};
use super::Problem;

/// How deep groups nest.
const MAX_NESTING: usize = 128;

/// `Type`: how a column's values are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Physical {
    Boolean,
    Int32,
    Int64,
    Int96,
    Float,
    Double,
    ByteArray,
    /// `FIXED_LEN_BYTE_ARRAY` of this length.
    Fixed(usize),
}

impl Physical {
    /// The type of `code`, of `length` for a fixed-length byte array.
    pub fn of(code: i32, length: Option<i32>) -> Result<Self, Problem> {
        Ok(match code {
            0 => Self::Boolean,
            1 => Self::Int32,
            2 => Self::Int64,
            3 => Self::Int96,
            4 => Self::Float,
            5 => Self::Double,
            6 => Self::ByteArray,
            7 => {
                let length = length.ok_or_else(|| {
                    Problem::Malformed("a FIXED_LEN_BYTE_ARRAY of no length".to_owned())
                })?;
                Self::Fixed(usize::try_from(length).map_err(|_| {
                    Problem::Malformed(format!("a FIXED_LEN_BYTE_ARRAY of length {length}"))
                })?)
            }
            _ => {
                return Err(Problem::Malformed(format!(
                    "the physical type {code}, which Parquet does not define"
                )))
            }
        })
    }

    /// The code of the type, as a column chunk names it.
    pub fn code(self) -> i32 {
        match self {
            Self::Boolean => 0,
            Self::Int32 => 1,
            Self::Int64 => 2,
            Self::Int96 => 3,
            Self::Float => 4,
            Self::Double => 5,
            Self::ByteArray => 6,
            Self::Fixed(_) => 7,
        }
    }
}

/// How a column's values are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Text: `STRING`, `ENUM` and `JSON`, decoded as UTF-8, each malformed sequence replaced.
    Text,
    /// Bytes of no annotation, which older writers store text as: written where they are UTF-8.
    Bytes,
    /// Binary data that is no text (`BSON`, `VARIANT`, geometries, unannotated fixed-length
    /// bytes): not written.
    Binary,
    Boolean,
    Signed,
    Unsigned,
    Decimal {
        scale: i32,
    },
    Date,
    Time(Unit),
    Timestamp {
        utc: bool,
        unit: Unit,
    },
    /// An `INT96`, the nanoseconds of a day and the Julian day of a timestamp in UTC, as Impala,
    /// Hive and Spark write timestamps.
    Int96Timestamp,
    Float,
    Double,
    Float16,
    Uuid,
    /// `INTERVAL`: months, days and milliseconds.
    Interval,
}

/// A leaf column of the schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// The names from the root's child down to the leaf, joined by `.`.
    pub path: String,
    pub physical: Physical,
    pub kind: Kind,
    /// The definition level of a value that is there.
    pub max_definition: u32,
    /// The repetition level of the leaf, 0 for a column of one value a row.
    pub max_repetition: u32,
}

/// `FieldRepetitionType`.
const REQUIRED: i32 = 0;
const OPTIONAL: i32 = 1;
const REPEATED: i32 = 2;

/// A group being read: the children of it left, and the levels and path length it gives them.
struct Group {
    left: usize,
    definition: u32,
    repetition: u32,
    path: usize,
}

/// The leaf columns of the schema `elements`, depth first.
pub fn columns(elements: &[SchemaElement]) -> Result<Vec<Column>, Problem> {
    let root = elements
        .first()
        .ok_or_else(|| Problem::Malformed("the schema has no elements".to_owned()))?;
    let children = root.num_children.ok_or_else(|| {
        Problem::Malformed("the schema's root is not a group of children".to_owned())
    })?;
    let mut groups = vec![Group {
        left: children_count(children, &root.name)?,
        definition: 0,
        repetition: 0,
        path: 0,
    }];
    let mut columns = Vec::new();
    let mut path = String::new();
    let mut next = 1;
    while let Some(group) = groups.last_mut() {
        if group.left == 0 {
            groups.pop();
            continue;
        }
        group.left -= 1;
        let (mut definition, mut repetition) = (group.definition, group.repetition);
        path.truncate(group.path);
        let element = elements.get(next).ok_or_else(|| {
            Problem::Malformed(format!(
                "the schema has {} elements, fewer than its groups' children",
                elements.len()
            ))
        })?;
        next += 1;
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(&element.name);
        match element.repetition {
            Some(REQUIRED) => {}
            Some(OPTIONAL) => definition += 1,
            Some(REPEATED) => (definition, repetition) = (definition + 1, repetition + 1),
            Some(other) => {
                return Err(Problem::Malformed(format!(
                    "the schema element {path} is of the repetition {other}, which Parquet does \
                     not define"
                )))
            }
            None => {
                return Err(Problem::Malformed(format!(
                    "the schema element {path} has no repetition"
                )))
            }
        }
        match (element.physical, element.num_children) {
            (Some(code), None | Some(0)) => {
                let physical = Physical::of(code, element.type_length)
                    .map_err(|problem| problem.within(&path))?;
                let kind = kind(element, physical).map_err(|problem| problem.within(&path))?;
                columns.push(Column {
                    path: path.clone(),
                    physical,
                    kind,
                    max_definition: definition,
                    max_repetition: repetition,
                });
            }
            (None, Some(children)) => {
                if groups.len() == MAX_NESTING {
                    return Err(Problem::Malformed(format!(
                        "the schema's groups nest deeper than {MAX_NESTING} at {path}"
                    )));
                }
                groups.push(Group {
                    left: children_count(children, &path)?,
                    definition,
                    repetition,
                    path: path.len(),
                });
            }
            (Some(_), Some(_)) => {
                return Err(Problem::Malformed(format!(
                    "the schema element {path} is both a leaf of a type and a group of children"
                )))
            }
            (None, None) => {
                return Err(Problem::Malformed(format!(
                    "the schema element {path} is neither a leaf of a type nor a group of \
                     children"
                )))
            }
        }
    }
    if next != elements.len() {
        return Err(Problem::Malformed(format!(
            "{} schema elements follow the elements of the root's children",
            elements.len() - next
        )));
    }
    Ok(columns)
}

fn children_count(children: i32, group: &str) -> Result<usize, Problem> {
    usize::try_from(children)
        .map_err(|_| Problem::Malformed(format!("the group {group:?} has {children} children")))
}

/// `ConvertedType`s, the annotations Parquet had before its logical types.
mod converted {
    pub const UTF8: i32 = 0;
    pub const MAP: i32 = 1;
    pub const MAP_KEY_VALUE: i32 = 2;
    pub const LIST: i32 = 3;
    pub const ENUM: i32 = 4;
    pub const DECIMAL: i32 = 5;
    pub const DATE: i32 = 6;
    pub const TIME_MILLIS: i32 = 7;
    pub const TIME_MICROS: i32 = 8;
    pub const TIMESTAMP_MILLIS: i32 = 9;
    pub const TIMESTAMP_MICROS: i32 = 10;
    pub const UINT_8: i32 = 11;
    pub const UINT_64: i32 = 14;
    pub const INT_8: i32 = 15;
    pub const INT_64: i32 = 18;
    pub const JSON: i32 = 19;
    pub const BSON: i32 = 20;
    pub const INTERVAL: i32 = 21;
}

/// How the values of the leaf `element`, of the type `physical`, are written: by its logical
/// type, else by its converted type, else by its physical type.
fn kind(element: &SchemaElement, physical: Physical) -> Result<Kind, Problem> {
    let logical = element.logical.map(logical_kind).transpose()?.flatten();
    let kind = match (logical, element.converted) {
        (Some(kind), _) => kind,
        (None, Some(code)) => converted_kind(code, element.scale)?,
        (None, None) => plain_kind(physical),
    };
    if !stores(kind, physical) {
        return Err(Problem::Malformed(format!(
            "a {physical:?} column annotated as {kind:?}"
        )));
    }
    Ok(kind)
}

/// How values of the logical type `logical` are written, unless it is one read by the converted
/// type: `UNKNOWN`, of no values, or one defined after this reader.
fn logical_kind(logical: Logical) -> Result<Option<Kind>, Problem> {
    Ok(Some(match logical {
        Logical::String | Logical::Enum | Logical::Json => Kind::Text,
        Logical::Bson | Logical::Variant | Logical::Geometry | Logical::Geography => Kind::Binary,
        Logical::Decimal { scale } => Kind::Decimal { scale },
        Logical::Date => Kind::Date,
        Logical::Time { unit } => Kind::Time(unit),
        Logical::Timestamp { utc, unit } => Kind::Timestamp { utc, unit },
        Logical::Integer { bits, signed } => {
            if ![8, 16, 32, 64].contains(&bits) {
                return Err(Problem::Malformed(format!("an integer of {bits} bits")));
            }
            if signed {
                Kind::Signed
            } else {
                Kind::Unsigned
            }
        }
        Logical::Uuid => Kind::Uuid,
        Logical::Float16 => Kind::Float16,
        Logical::Map | Logical::List => {
            return Err(Problem::Malformed(format!(
                "a leaf annotated as a {logical:?}, which annotates groups"
            )))
        }
        Logical::Unknown | Logical::Unrecognized => return Ok(None),
    }))
}

fn converted_kind(code: i32, scale: Option<i32>) -> Result<Kind, Problem> {
    Ok(match code {
        converted::UTF8 | converted::ENUM | converted::JSON => Kind::Text,
        converted::BSON => Kind::Binary,
        converted::DECIMAL => Kind::Decimal {
            scale: scale.unwrap_or(0),
        },
        converted::DATE => Kind::Date,
        converted::TIME_MILLIS => Kind::Time(Unit::Millis),
        converted::TIME_MICROS => Kind::Time(Unit::Micros),
        converted::TIMESTAMP_MILLIS => Kind::Timestamp {
            utc: true,
            unit: Unit::Millis,
        },
        converted::TIMESTAMP_MICROS => Kind::Timestamp {
            utc: true,
            unit: Unit::Micros,
        },
        converted::UINT_8..=converted::UINT_64 => Kind::Unsigned,
        converted::INT_8..=converted::INT_64 => Kind::Signed,
        converted::INTERVAL => Kind::Interval,
        converted::MAP | converted::MAP_KEY_VALUE | converted::LIST => {
            return Err(Problem::Malformed(format!(
                "a leaf of the converted type {code}, which annotates groups"
            )))
        }
        _ => {
            return Err(Problem::Malformed(format!(
                "the converted type {code}, which Parquet does not define"
            )))
        }
    })
}

/// How values of `physical` of no annotation are written.
fn plain_kind(physical: Physical) -> Kind {
    match physical {
        Physical::Boolean => Kind::Boolean,
        Physical::Int32 | Physical::Int64 => Kind::Signed,
        Physical::Int96 => Kind::Int96Timestamp,
        Physical::Float => Kind::Float,
        Physical::Double => Kind::Double,
        Physical::ByteArray => Kind::Bytes,
        Physical::Fixed(_) => Kind::Binary,
    }
}

/// Whether values of `kind` are stored in `physical`.
fn stores(kind: Kind, physical: Physical) -> bool {
    use Physical::*;
    match kind {
        Kind::Text | Kind::Bytes => physical == ByteArray,
        Kind::Binary => matches!(physical, ByteArray | Fixed(_)),
        Kind::Boolean => physical == Boolean,
        Kind::Signed | Kind::Unsigned => matches!(physical, Int32 | Int64),
        Kind::Decimal { scale } => {
            scale >= 0 && matches!(physical, Int32 | Int64 | ByteArray | Fixed(_))
        }
        Kind::Date => physical == Int32,
        Kind::Time(unit) => physical == if unit == Unit::Millis { Int32 } else { Int64 },
        Kind::Timestamp { .. } => physical == Int64,
        Kind::Int96Timestamp => physical == Int96,
        Kind::Float => physical == Float,
        Kind::Double => physical == Double,
        Kind::Float16 => physical == Fixed(2),
        Kind::Uuid => physical == Fixed(16),
        Kind::Interval => physical == Fixed(12),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(name: &str, physical: Option<i32>, repetition: i32) -> SchemaElement {
        SchemaElement {
            physical,
            type_length: None,
            repetition: Some(repetition),
            name: name.to_owned(),
            num_children: None,
            converted: None,
            scale: None,
            logical: None,
        }
    }

    fn group(name: &str, repetition: Option<i32>, children: i32) -> SchemaElement {
        SchemaElement {
            physical: None,
            repetition,
            num_children: Some(children),
            ..element(name, None, 0)
        }
    }

    #[test]
    fn reads_leaves_with_their_paths_and_levels() {
        let text = SchemaElement {
            logical: Some(Logical::String),
            ..element("element", Some(6), OPTIONAL)
        };
        let elements = [
            group("schema", None, 3),
            element("id", Some(1), REQUIRED),
            group("crew", Some(OPTIONAL), 1),
            group("list", Some(REPEATED), 1),
            text,
            SchemaElement {
                converted: Some(converted::DECIMAL),
                scale: Some(2),
                type_length: Some(4),
                ..element("fee", Some(7), OPTIONAL)
            },
        ];
        let columns = columns(&elements).unwrap();
        let summary: Vec<_> = columns
            .iter()
            .map(|c| (c.path.as_str(), c.kind, c.max_definition, c.max_repetition))
            .collect();
        assert_eq!(
            summary,
            [
                ("id", Kind::Signed, 0, 0),
                ("crew.list.element", Kind::Text, 3, 1),
                ("fee", Kind::Decimal { scale: 2 }, 1, 0),
            ]
        );
        assert_eq!(columns[2].physical, Physical::Fixed(4));
    }

    #[test]
    fn rejects_schemas_whose_elements_do_not_add_up() {
        let leaf = element("id", Some(1), REQUIRED);
        assert!(columns(&[]).is_err());
        assert!(columns(&[group("schema", None, 2), leaf.clone()]).is_err());
        assert!(columns(&[group("schema", None, 1), leaf.clone(), leaf.clone()]).is_err());
        assert!(columns(&[group("schema", None, -1)]).is_err());
        assert!(columns(&[group("schema", None, 1), element("id", Some(1), 7)]).is_err());
        let date_text = SchemaElement {
            logical: Some(Logical::Date),
            ..element("day", Some(6), OPTIONAL)
        };
        assert!(columns(&[group("schema", None, 1), date_text]).is_err());
        let mut deep = vec![group("schema", None, 1)];
        deep.extend((0..MAX_NESTING).map(|_| group("g", Some(OPTIONAL), 1)));
        deep.push(leaf);
        assert!(columns(&deep).is_err());
    }
}
