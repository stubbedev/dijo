//! Jackcess's `DatabaseImpl` as Tika opens it read-only: the format and codec of the file, the
//! system catalog (`MSysObjects`) read whole, and the lookups its table finder makes in it: the
//! parents of tables and of the database's own objects, the user's tables, system tables by name
//! and the properties of objects.
//!
//! Jackcess looks rows of the catalog up through its index on the parent and the name where the
//! index is one it can read, and by scanning the catalog otherwise; the port always scans,
//! which finds the same rows in a catalog whose index matches its rows.

use std::cell::OnceCell;
use std::collections::HashMap;

use super::buffer::{
    Context as ErrorContext, Thrown, CLASS_CAST, ILLEGAL_ARGUMENT, INDEX_OUT_OF_BOUNDS,
};
use super::column::{Column, Complex, DataType, Value};
use super::format;
use super::pages::Pages;
use super::properties::PropertyMaps;
use super::table::{Context, IndexMatch, Row, Table};

const PAGE_SYSTEM_CATALOG: i32 = 2;
const DB_PARENT_ID: i32 = 0xF00_0000;
const SYSTEM_OBJECT_FLAGS: i32 = 0x8000_0000_u32 as i32 | 0x02;
const TYPE_TABLE: i16 = 1;
const TYPE_LINKED_ODBC_TABLE: i16 = 4;
const TYPE_QUERY: i16 = 5;
const TYPE_LINKED_TABLE: i16 = 6;
const TABLE_SYSTEM_CATALOG: &str = "MSysObjects";
const TABLE_SYSTEM_QUERIES: &str = "MSysQueries";
const TABLE_SYSTEM_COMPLEX_COLUMNS: &str = "MSysComplexColumns";
const COMPLEX_TABLE_ID: &str = "ConceptualTableID";
const COMPLEX_FLAT_TABLE_ID: &str = "FlatTableID";
const COMPLEX_COLUMN_NAME: &str = "ColumnName";
const COMPLEX_TYPE_OBJECT_ID: &str = "ComplexTypeObjectID";
const ATTACHMENT_TYPE_TABLE: &str = "MSysComplexType_Attachment";
const VERSION_HISTORY_TYPE_PREFIX: &str = "MSysComplexTypeVH_";
const COMPLEX_TYPE_PREFIX: &str = "MSysComplexType_";
const SYSTEM_OBJECT_NAME_TABLES: &str = "Tables";
const SYSTEM_OBJECT_NAME_DATABASES: &str = "Databases";
const OBJECT_NAME_DB_PROPS: &str = "MSysDb";
const OBJECT_NAME_SUMMARY_PROPS: &str = "SummaryInfo";
const OBJECT_NAME_USERDEF_PROPS: &str = "UserDefined";
const CAT_COL_ID: &str = "Id";
const CAT_COL_NAME: &str = "Name";
const CAT_COL_TYPE: &str = "Type";
const CAT_COL_FLAGS: &str = "Flags";
const CAT_COL_PARENT_ID: &str = "ParentId";
const CAT_COL_PROPS: &str = "LvProp";
const CAT_COL_DETAILS: [&str; 3] = ["Database", "ForeignName", "Connect"];

/// A row of the system catalog.
#[derive(Debug)]
struct CatalogRow {
    name: Option<String>,
    object_type: Option<i16>,
    id: i32,
    flags: i32,
    parent: i32,
    properties: Option<Vec<u8>>,
}

/// Which table finder Jackcess uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finder {
    /// `DefaultTableFinder`, through the catalog's indexes.
    Indexed,
    /// `FallbackTableFinder`, by scanning, with the parent of the catalog's own row.
    Scanning { catalog_parent: Option<i32> },
}

/// A database opened as Tika opens it.
pub(super) struct Database<'a> {
    pub(super) pages: Pages<'a>,
    catalog: Vec<CatalogRow>,
    finder: Finder,
    table_parent: i32,
    db_parent: i32,
    /// `MSysComplexColumns`, read once its first complex column needs it.
    complex_columns: OnceCell<Result<Vec<ComplexRow>, Thrown>>,
}

/// A row of `MSysComplexColumns`, by its primary key.
#[derive(Debug)]
struct ComplexRow {
    key: Option<i32>,
    table_id: Option<i32>,
    column_name: Option<String>,
    flat_table_id: Option<i32>,
    type_object_id: Option<i32>,
}

/// Reading the system catalog's own definition, before Jackcess has a table finder.
struct Bootstrap;

impl Context for Bootstrap {
    fn properties(&self, _page: i32) -> Result<PropertyMaps, Thrown> {
        Err(no_finder())
    }

    fn complex(
        &self,
        _page: i32,
        _table: &str,
        _column: &Column,
        _id: i32,
    ) -> Result<Complex, Thrown> {
        Err(no_finder())
    }
}

/// What Jackcess throws reading the system catalog's properties or complex columns before its
/// table finder is made: it dereferences the finder, still null.
fn no_finder() -> Thrown {
    Thrown::null_pointer()
}

/// A catalog row's table, as a `TableInfo`: its name and the page its definition starts on.
pub(super) struct TableInfo {
    pub(super) name: String,
    pub(super) page: i32,
}

impl TableInfo {
    /// The table of a row the lookup found by its name.
    fn of_found(row: &CatalogRow) -> Self {
        TableInfo {
            name: row
                .name
                .clone()
                .expect("a row found by its name has a name"),
            page: row.id,
        }
    }
}

impl<'a> Database<'a> {
    /// The pages of `file`, as `DatabaseImpl.open` opens it read-only with a `CryptCodecProvider`
    /// of `password`: its format, and the codec of its pages.
    pub(super) fn pages(file: &'a [u8], password: &str) -> Result<Pages<'a>, Thrown> {
        Pages::open(file, format::detect(file)?, password)
    }

    /// The rest of `DatabaseImpl.open`: the system catalog of the database of `pages`.
    pub(super) fn open(pages: Pages<'a>) -> Result<Self, Thrown> {
        let catalog_table = Table::read(
            &pages,
            TABLE_SYSTEM_CATALOG.to_owned(),
            PAGE_SYSTEM_CATALOG,
            &Bootstrap,
        )?;
        let catalog = read_catalog(&catalog_table, &pages)?;
        let finder = match catalog_table.find_index(&[CAT_COL_PARENT_ID, CAT_COL_NAME]) {
            IndexMatch::Exact { valid: true } => {
                if catalog_table.find_index(&[CAT_COL_ID]) == IndexMatch::None {
                    return Err(Thrown::unported(
                        "a system catalog without the index on its ids Jackcess looks objects \
                         up in",
                    ));
                }
                Finder::Indexed
            }
            IndexMatch::Exact { valid: false } | IndexMatch::None => Finder::Scanning {
                catalog_parent: None,
            },
            IndexMatch::Partial => {
                return Err(Thrown::unported(
                    "a system catalog indexed on its parents and names only with more columns",
                ))
            }
        };
        let mut database = Self {
            pages,
            catalog,
            finder,
            table_parent: 0,
            db_parent: 0,
            complex_columns: OnceCell::new(),
        };
        database.read_parents()?;
        Ok(database)
    }

    /// `readSystemCatalog`'s parent of the tables, and `getDbParentId`.
    fn read_parents(&mut self) -> Result<(), Thrown> {
        let mut table_parent = None;
        let mut db_parent = None;
        if let Finder::Scanning { .. } = self.finder {
            let named = |name: &str| self.catalog.iter().find(|row| is_named(row, name));
            table_parent = named(SYSTEM_OBJECT_NAME_TABLES).map(|row| row.id);
            db_parent = named(SYSTEM_OBJECT_NAME_DATABASES).map(|row| row.id);
            let catalog_parent = named(TABLE_SYSTEM_CATALOG).map(|row| row.parent);
            self.finder = Finder::Scanning { catalog_parent };
        }
        let table_parent = table_parent
            .or_else(|| {
                self.find(DB_PARENT_ID, SYSTEM_OBJECT_NAME_TABLES)
                    .map(|row| row.id)
            })
            .ok_or_else(|| {
                Thrown::io("Did not find required parent table id")
                    .in_context(ErrorContext::Database)
            })?;
        let db_parent = db_parent
            .or_else(|| {
                self.find(DB_PARENT_ID, SYSTEM_OBJECT_NAME_DATABASES)
                    .map(|row| row.id)
            })
            .ok_or_else(|| {
                Thrown::io("Did not find required parent db id").in_context(ErrorContext::Database)
            })?;
        self.table_parent = table_parent;
        self.db_parent = db_parent;
        Ok(())
    }

    /// `findRow(parentId, name)`: the first row under `parent` named so, whatever the case.
    fn find(&self, parent: i32, name: &str) -> Option<&CatalogRow> {
        self.catalog
            .iter()
            .find(|row| row.parent == parent && is_named(row, name))
    }

    /// `getPropertiesForDbObject`: the properties of the database's object named so.
    fn db_object_properties(&self, name: &str) -> Result<PropertyMaps, Thrown> {
        let bytes = self
            .find(self.db_parent, name)
            .and_then(|row| row.properties.as_deref());
        PropertyMaps::read(bytes, &self.pages)
    }

    /// `getDatabaseProperties`.
    pub(super) fn database_properties(&self) -> Result<PropertyMaps, Thrown> {
        self.db_object_properties(OBJECT_NAME_DB_PROPS)
    }

    /// `getSummaryProperties`.
    pub(super) fn summary_properties(&self) -> Result<PropertyMaps, Thrown> {
        self.db_object_properties(OBJECT_NAME_SUMMARY_PROPS)
    }

    /// `getUserDefinedProperties`.
    pub(super) fn user_defined_properties(&self) -> Result<PropertyMaps, Thrown> {
        self.db_object_properties(OBJECT_NAME_USERDEF_PROPS)
    }

    /// `findRow(objectId)`: the first row whose id is `id`.
    fn find_by_id(&self, id: i32) -> Option<&CatalogRow> {
        self.catalog.iter().find(|row| row.id == id)
    }

    /// `getPropertiesForObject`: the properties of the object whose id is `id`.
    pub(super) fn object_properties(&self, id: i32) -> Result<PropertyMaps, Thrown> {
        let bytes = self
            .find_by_id(id)
            .and_then(|row| row.properties.as_deref());
        PropertyMaps::read(bytes, &self.pages)
    }

    /// The user's tables: each row of the catalog under the parent of tables that is a local
    /// table and no system object, ordered by name as `String.CASE_INSENSITIVE_ORDER` orders
    /// them, and in catalog order among names that differ only in case.
    ///
    /// Jackcess lists the names and then looks each up again, which stops Tika's iteration at
    /// the first name the lookup does not find and keeps only one of two names that differ
    /// only in case (#122); the rows themselves name every table.
    pub(super) fn tables(&self) -> Result<Vec<TableInfo>, Thrown> {
        let mut tables = Vec::new();
        for row in self.catalog.iter().filter(|row| {
            row.parent == self.table_parent
                && row.object_type == Some(TYPE_TABLE)
                && row.flags & SYSTEM_OBJECT_FLAGS == 0
        }) {
            let Some(name) = &row.name else {
                return Err(Thrown::null_pointer());
            };
            tables.push(TableInfo {
                name: name.clone(),
                page: row.id,
            });
        }
        tables.sort_by(|a, b| crate::java::compare_ignore_case(&a.name, &b.name));
        Ok(tables)
    }

    /// `lookupTable(name, parent)`: the table row the finder finds.
    fn lookup(&self, name: &str, parent: i32) -> Result<Option<&CatalogRow>, Thrown> {
        let row = match self.finder {
            Finder::Indexed => self
                .find(parent, name)
                .filter(|row| is_table_type(row.object_type)),
            Finder::Scanning { .. } => self.catalog.iter().find(|row| {
                is_table_type(row.object_type) && row.parent == parent && is_named(row, name)
            }),
        };
        match row.and_then(|row| row.object_type) {
            Some(TYPE_LINKED_TABLE) => {
                return Err(Thrown::Unreadable(format!(
                    "the table {name} is linked to another database, which Tika's link resolver \
                     refuses to open with an AssertionError that fails the parse"
                )))
            }
            Some(TYPE_LINKED_ODBC_TABLE) => {
                return Err(Thrown::unported(
                    "a table linked over ODBC, which Jackcess reads as a definition of no rows",
                ))
            }
            _ => {}
        }
        Ok(row)
    }

    /// `getTable(name, true)`'s `TableInfo`: the user's table named so.
    fn table_info(&self, name: &str) -> Result<Option<TableInfo>, Thrown> {
        Ok(self
            .lookup(name, self.table_parent)?
            .map(TableInfo::of_found))
    }

    /// `getSystemTable`: through the parent of the catalog's own row where the scanning finder
    /// knows it, else as a user's table.
    fn system_table_info(&self, name: &str) -> Result<Option<TableInfo>, Thrown> {
        if let Finder::Scanning {
            catalog_parent: Some(parent),
        } = self.finder
        {
            if let Some(row) = self.lookup(name, parent)? {
                return Ok(Some(TableInfo::of_found(row)));
            }
        }
        self.table_info(name)
    }

    /// `loadTable`: the definition of the table `info` names.
    pub(super) fn load_table(&self, info: &TableInfo) -> Result<Table, Thrown> {
        Table::read(&self.pages, info.name.clone(), info.page, self)
    }

    /// `getTable(pageNumber)`: the table whose definition starts on `page`, named by its row of
    /// the catalog, if it has one.
    fn table_by_id(&self, page: i32) -> Result<Option<Table>, Thrown> {
        let Some(row) = self.find_by_id(page) else {
            return Ok(None);
        };
        if row.object_type == Some(TYPE_LINKED_ODBC_TABLE) {
            return Err(Thrown::unported(
                "a complex column supported by a linked ODBC table",
            ));
        }
        let Some(name) = row.name.clone() else {
            return Err(Thrown::unported(
                "a complex column supported by a table without a name",
            ));
        };
        Table::read(&self.pages, name, page, self).map(Some)
    }

    /// `getSystemComplexColumns` and its primary key: the rows of `MSysComplexColumns`.
    fn complex_columns(&self) -> Result<&[ComplexRow], Thrown> {
        self.complex_columns
            .get_or_init(|| self.read_complex_columns())
            .as_deref()
            .map_err(Clone::clone)
    }

    fn read_complex_columns(&self) -> Result<Vec<ComplexRow>, Thrown> {
        let Some(info) = self.system_table_info(TABLE_SYSTEM_COMPLEX_COLUMNS)? else {
            return Err(Thrown::io("Could not find system table MSysComplexColumns")
                .in_context(ErrorContext::Database));
        };
        let table = self.load_table(&info)?;
        let key = match table.primary_key() {
            None => {
                return Err(
                    Thrown::runtime(ILLEGAL_ARGUMENT, "No primary key index found")
                        .in_context(ErrorContext::Table(table.name.clone())),
                )
            }
            Some(&[key]) if table.columns[key].data_type == DataType::Long => key,
            Some(_) => {
                return Err(Thrown::unported(
                    "complex columns keyed other than by one integer column",
                ))
            }
        };
        let wanted = vec![true; table.columns.len()];
        let mut rows = Vec::new();
        for row in table.rows(&self.pages, wanted) {
            let row = row?;
            let get = |name: &str| value_named(&table, &row, name);
            rows.push(ComplexRow {
                key: integer(row[key].as_ref())?,
                table_id: integer(get(COMPLEX_TABLE_ID))?,
                column_name: string(get(COMPLEX_COLUMN_NAME))?,
                flat_table_id: integer(get(COMPLEX_FLAT_TABLE_ID))?,
                type_object_id: integer(get(COMPLEX_TYPE_OBJECT_ID))?,
            });
        }
        Ok(rows)
    }

    /// `getQueries`: each query of the catalog with the rows of `MSysQueries` that are its.
    pub(super) fn queries(&self) -> Result<Vec<QueryDefinition>, Thrown> {
        let Some(info) = self.system_table_info(TABLE_SYSTEM_QUERIES)? else {
            return Err(Thrown::io("Could not find system table MSysQueries")
                .in_context(ErrorContext::Database));
        };
        let table = self.load_table(&info)?;
        let mut definitions = Vec::new();
        let mut rows_by_id: HashMap<i32, Vec<QueryRow>> = HashMap::new();
        for row in &self.catalog {
            if let (Some(name), Some(TYPE_QUERY)) = (&row.name, row.object_type) {
                definitions.push((row.id, row.flags, name.clone()));
                rows_by_id.insert(row.id, Vec::new());
            }
        }
        let wanted = vec![true; table.columns.len()];
        for row in table.rows(&self.pages, wanted) {
            let row = QueryRow::read(&table, &row?)?;
            if let Some(rows) = row.object_id.and_then(|id| rows_by_id.get_mut(&id)) {
                rows.push(row);
            }
        }
        Ok(definitions
            .into_iter()
            .map(|(id, flags, name)| QueryDefinition {
                name,
                flags,
                rows: rows_by_id.get(&id).cloned().unwrap_or_default(),
            })
            .collect())
    }
}

impl Context for Database<'_> {
    fn properties(&self, page: i32) -> Result<PropertyMaps, Thrown> {
        self.object_properties(page)
    }

    fn complex(
        &self,
        page: i32,
        table: &str,
        column: &Column,
        complex_id: i32,
    ) -> Result<Complex, Thrown> {
        let in_column = || ErrorContext::Column {
            table: table.to_owned(),
            column: column.name.clone(),
        };
        let row = self
            .complex_columns()?
            .iter()
            .find(|row| row.key == Some(complex_id))
            .ok_or_else(|| {
                Thrown::io(format!(
                    "Could not find complex column info for complex column with id {complex_id}"
                ))
                .in_context(in_column())
            })?;
        let unboxed = |value: Option<i32>| value.ok_or_else(Thrown::null_pointer);
        let table_id = unboxed(row.table_id)?;
        if table_id != page {
            return Err(Thrown::io(format!(
                "Found complex column for table {table_id} but expected table {page}"
            ))
            .in_context(in_column()));
        }
        if let Some(name) = &row.column_name {
            if !crate::java::equals_ignore_case(name, &column.name) {
                return Err(Thrown::io(format!(
                    "Found complex column info for column {name} but expected column {}",
                    column.name
                ))
                .in_context(in_column()));
            }
        }
        let flat_table_id = unboxed(row.flat_table_id)?;
        let type_object_id = unboxed(row.type_object_id)?;
        let type_table = self.table_by_id(type_object_id)?;
        let flat_table = self.table_by_id(flat_table_id)?;
        let (Some(type_table), Some(flat_table)) = (type_table, flat_table) else {
            return Err(Thrown::io(format!(
                "Could not find supporting tables ({type_object_id}, {flat_table_id}) for complex \
                 column with id {complex_id}"
            ))
            .in_context(in_column()));
        };
        complex_info(&type_table, &flat_table, flat_table_id)
    }
}

/// What `ComplexColumnSupport.create` makes of a complex column's type and flat tables, with
/// the checks of the `ComplexColumnInfoImpl` constructors.
fn complex_info(type_table: &Table, flat_table: &Table, flat_page: i32) -> Result<Complex, Thrown> {
    let type_name = &type_table.name;
    let (type_columns, other_columns): (Vec<&Column>, Vec<&Column>) =
        flat_table.columns.iter().partition(|column| {
            type_table
                .columns
                .iter()
                .any(|own| crate::java::equals_ignore_case(&own.name, &column.name))
        });
    let primary_key = other_columns
        .iter()
        .rfind(|column| !column.complex_foreign_key && column.auto_number);
    let foreign_key = other_columns
        .iter()
        .rfind(|column| column.complex_foreign_key)
        .or_else(|| {
            other_columns
                .iter()
                .rfind(|column| !column.auto_number && column.data_type == DataType::Long)
        });
    if primary_key.is_none() || foreign_key.is_none() {
        return Err(Thrown::io(format!(
            "Could not find expected columns in flat table {} for complex column",
            flat_table.name
        )));
    }
    let multi_value = || -> Result<Complex, Thrown> {
        if type_columns.is_empty() {
            return Err(Thrown::index_out_of_bounds(
                INDEX_OUT_OF_BOUNDS,
                Some("Index 0 out of bounds for length 0".to_owned()),
            ));
        }
        Ok(Complex::MultiValue {
            flat_table_page: flat_page,
        })
    };
    let version_history = || Complex::VersionHistory {
        value_column: type_columns
            .iter()
            .rfind(|column| column.data_type == DataType::Memo)
            .map(|column| column.name.clone()),
    };
    let starts_with = |prefix: &str| {
        let start: String = type_name.chars().take(prefix.chars().count()).collect();
        crate::java::equals_ignore_case(&start, prefix)
    };
    Ok(
        if crate::java::equals_ignore_case(ATTACHMENT_TYPE_TABLE, type_name) {
            Complex::Other
        } else if starts_with(VERSION_HISTORY_TYPE_PREFIX) {
            version_history()
        } else if starts_with(COMPLEX_TYPE_PREFIX) {
            if is_multi_value(type_table) {
                multi_value()?
            } else {
                Complex::Other
            }
        } else if is_multi_value(type_table) {
            multi_value()?
        } else if is_attachment(type_table) {
            Complex::Other
        } else if is_version_history(type_table) {
            version_history()
        } else {
            Complex::Other
        },
    )
}

/// `isMultiValueColumn`: one column, of a type a multi-valued field holds.
fn is_multi_value(type_table: &Table) -> bool {
    matches!(
        type_table.columns.as_slice(),
        [column] if matches!(
            column.data_type,
            DataType::Byte
                | DataType::Int
                | DataType::Long
                | DataType::Float
                | DataType::Double
                | DataType::Guid
                | DataType::Numeric
                | DataType::Text
                | DataType::BigInt
        )
    )
}

fn count(table: &Table, data_type: DataType) -> usize {
    table
        .columns
        .iter()
        .filter(|column| column.data_type == data_type)
        .count()
}

/// `isAttachmentColumn`: at least six columns, with the memo, texts, OLE object, date and
/// integer of an attachment.
fn is_attachment(type_table: &Table) -> bool {
    type_table.columns.len() >= 6
        && count(type_table, DataType::Memo) >= 1
        && count(type_table, DataType::Text) >= 2
        && count(type_table, DataType::Ole) >= 1
        && count(type_table, DataType::ShortDateTime) >= 1
        && count(type_table, DataType::Long) >= 1
}

/// `isVersionHistoryColumn`: at least two columns, with a memo and a date.
fn is_version_history(type_table: &Table) -> bool {
    type_table.columns.len() >= 2
        && count(type_table, DataType::Memo) >= 1
        && count(type_table, DataType::ShortDateTime) >= 1
}

/// A named query of the catalog: its flags and its rows of `MSysQueries`.
pub(super) struct QueryDefinition {
    pub(super) name: String,
    pub(super) flags: i32,
    pub(super) rows: Vec<QueryRow>,
}

/// `QueryImpl.Row`: a row of `MSysQueries`.
#[derive(Debug, Clone, Default)]
pub(super) struct QueryRow {
    pub(super) attribute: Option<i8>,
    pub(super) expression: Option<String>,
    pub(super) flag: Option<i16>,
    pub(super) extra: Option<i32>,
    pub(super) name1: Option<String>,
    pub(super) name2: Option<String>,
    object_id: Option<i32>,
}

impl QueryRow {
    /// `new QueryImpl.Row(row)`: each column's value cast to its class.
    fn read(table: &Table, row: &Row) -> Result<Self, Thrown> {
        let get = |name: &str| value_named(table, row, name);
        let query_row = Self {
            attribute: cast(get("Attribute"), |v| match v {
                Value::Byte(b) => Some(*b),
                _ => None,
            })?,
            expression: string(get("Expression"))?,
            flag: cast(get("Flag"), |v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })?,
            extra: integer(get("LvExtra"))?,
            name1: string(get("Name1"))?,
            name2: string(get("Name2"))?,
            object_id: integer(get("ObjectId"))?,
        };
        cast(get("Order"), |v| match v {
            Value::Binary(_) => Some(()),
            _ => None,
        })?;
        Ok(query_row)
    }
}

/// `RowImpl.get(name)`: the value of the last column named exactly so.
fn value_named<'r>(table: &Table, row: &'r Row, name: &str) -> Option<&'r Value> {
    table
        .columns
        .iter()
        .zip(row)
        .rfind(|(column, _)| column.name == name)
        .and_then(|(_, value)| value.as_ref())
}

/// A value cast to the class `pick` takes, which throws for another.
fn cast<T>(value: Option<&Value>, pick: impl Fn(&Value) -> Option<T>) -> Result<Option<T>, Thrown> {
    match value {
        None => Ok(None),
        Some(value) => pick(value)
            .map(Some)
            .ok_or_else(|| Thrown::runtime(CLASS_CAST, format!("{value:?}"))),
    }
}

fn string(value: Option<&Value>) -> Result<Option<String>, Thrown> {
    cast(value, |v| match v {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    })
}

fn integer(value: Option<&Value>) -> Result<Option<i32>, Thrown> {
    cast(value, |v| match v {
        Value::Long(i) => Some(*i),
        _ => None,
    })
}

/// Whether the row's name is `name`, as `equalsIgnoreCase` compares.
fn is_named(row: &CatalogRow, name: &str) -> bool {
    row.name
        .as_deref()
        .is_some_and(|row_name| crate::java::equals_ignore_case(row_name, name))
}

/// `isTableType`: a table, linked or not.
fn is_table_type(object_type: Option<i16>) -> bool {
    matches!(
        object_type,
        Some(TYPE_TABLE | TYPE_LINKED_ODBC_TABLE | TYPE_LINKED_TABLE)
    )
}

/// The catalog's rows, each with the values the finder reads. Jackcess unboxes each row's id,
/// flags and parent where it reads them, so the port reads only a catalog where none is null;
/// and it reads the other columns where it looks tables up, so the port reads them all.
fn read_catalog(table: &Table, pages: &Pages<'_>) -> Result<Vec<CatalogRow>, Thrown> {
    let expected: [(&str, &[DataType]); 6] = [
        (CAT_COL_ID, &[DataType::Long]),
        (CAT_COL_NAME, &[DataType::Text, DataType::Memo]),
        (CAT_COL_TYPE, &[DataType::Int]),
        (CAT_COL_FLAGS, &[DataType::Long]),
        (CAT_COL_PARENT_ID, &[DataType::Long]),
        (CAT_COL_PROPS, &[DataType::Ole, DataType::Binary]),
    ];
    for (name, types) in expected {
        let found = table.columns.iter().filter(|column| column.name == name);
        let mut found = found.peekable();
        if found.peek().is_none() || !found.all(|column| types.contains(&column.data_type)) {
            return Err(Thrown::unported(
                "a system catalog whose columns differ from those of Access",
            ));
        }
    }
    let details_are_text = table
        .columns
        .iter()
        .filter(|column| CAT_COL_DETAILS.contains(&column.name.as_str()))
        .all(|column| matches!(column.data_type, DataType::Text | DataType::Memo));
    if !details_are_text {
        return Err(Thrown::unported(
            "a system catalog whose columns differ from those of Access",
        ));
    }
    let wanted = table
        .columns
        .iter()
        .map(|column| {
            expected.iter().any(|(name, _)| column.name == *name)
                || CAT_COL_DETAILS.contains(&column.name.as_str())
        })
        .collect();
    let mut rows = Vec::new();
    for row in table.rows(pages, wanted) {
        let row = row?;
        let get = |name: &str| value_named(table, &row, name);
        let required = |name: &str| -> Result<i32, Thrown> {
            integer(get(name))?.ok_or_else(|| {
                Thrown::unported(
                    "a system catalog row without an id, flags or a parent, which Jackcess \
                     unboxes",
                )
            })
        };
        rows.push(CatalogRow {
            name: string(get(CAT_COL_NAME))?,
            object_type: cast(get(CAT_COL_TYPE), |v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })?,
            id: required(CAT_COL_ID)?,
            flags: required(CAT_COL_FLAGS)?,
            parent: required(CAT_COL_PARENT_ID)?,
            properties: cast(get(CAT_COL_PROPS), |v| match v {
                Value::Binary(bytes) => Some(bytes.clone()),
                _ => None,
            })?,
        });
    }
    Ok(rows)
}
