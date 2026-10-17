//! Tika's `SQLite3Parser` over SQLite databases, read through SQLite itself as Tika reads them
//! through sqlite-jdbc: the names of the tables and the database's application id and user version
//! as metadata, then each table as a table of its columns and rows, each cell written as
//! `JDBCTableReader` writes the JDBC type sqlite-jdbc reports for it, and each binary value handed
//! on as an embedded document.

#[cfg(test)]
mod tests;

use std::fmt::Display;
use std::io::Read;

use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, Statement};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java;
use crate::java::dtoa::{java_double_to_string, java_float_to_string};
use crate::mime::{self, Origin};

const TABLE_NAME: &str = "database:table-name";
const COLUMN_NAME: &str = "database:column-name";
const ROW_NUM: &str = "database:ROW_NUM";
const IS_BLOB: &str = "database:IS_BLOB";
const APPLICATION_ID: &str = "sqlite3:application_id";
const USER_VERSION: &str = "sqlite3:user_version";
const CONTENT_TYPE: &str = "Content-Type";
const CONTENT_LENGTH: &str = "Content-Length";
const RESOURCE_NAME: &str = "tk:resource-name";
const RESOURCE_NAME_EXTENSION_INFERRED: &str = "tk:resource-name-extension-inferred";
/// `SQLite3DBParser.METADATA_KEYS`: each property with the query that reads it.
const PRAGMAS: [(&str, &str); 2] = [
    (
        APPLICATION_ID,
        "select application_id from pragma_application_id",
    ),
    (USER_VERSION, "select user_version from pragma_user_version"),
];
const TABLE_NAMES: &str = "SELECT name FROM sqlite_master WHERE type='table'";
/// The length of a database file's header.
const HEADER_LENGTH: usize = 100;
/// The offsets of the header's file format write and read versions.
const WRITE_VERSION: usize = 18;
const READ_VERSION: usize = 19;
/// The file format version of a database in write-ahead-log mode, and of one with a rollback
/// journal.
const WAL_VERSION: u8 = 2;
const LEGACY_VERSION: u8 = 1;
/// How sqlite-jdbc's `getString` reads a value it is not stored as: as SQLite converts it.
const AS_TEXT: &str = "SELECT CAST(?1 AS TEXT)";

/// Tika's `SQLite3Parser`.
pub(crate) struct Sqlite3Parser;

impl Parser for Sqlite3Parser {
    fn name(&self) -> &'static str {
        "extractous::SQLite3Parser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.sqlite3.SQLite3Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-sqlite3"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let connection = open(input).map_err(|error| fails("opening the database", error))?;
        let tables = table_names(&connection)
            .map_err(|error| fails("reading the database's table names", error))?;
        let properties: Vec<(&str, String)> = PRAGMAS
            .iter()
            .filter_map(|&(name, query)| Some((name, java_hex(pragma(&connection, query)?))))
            .collect();
        Ok(Box::new(move |xhtml| {
            for table in &tables {
                xhtml.metadata_mut().add(TABLE_NAME, table.clone());
            }
            for (name, value) in properties {
                xhtml.metadata_mut().set(name, value);
            }
            let written = Database::new(&connection).and_then(|mut database| {
                tables
                    .iter()
                    .try_for_each(|table| database.write_table(table, xhtml))
            });
            xhtml.end_document()?;
            written
        }))
    }
}

fn fails(reading: &str, error: impl Display) -> Declined {
    Declined::Fails(Error::ParseError(format!(
        "SQLite3Parser failed {reading}: {error}"
    )))
}

/// The database in `input`, opened read-only, as sqlite-jdbc opens the file Tika spools it to:
/// one in write-ahead-log mode as the file alone, without a log, which an in-memory database
/// reads once its header names the rollback journal instead.
fn open(input: &[u8]) -> rusqlite::Result<Connection> {
    let mut connection = Connection::open_in_memory()?;
    let mut header = input[..input.len().min(HEADER_LENGTH)].to_vec();
    if header.get(WRITE_VERSION..=READ_VERSION) == Some(&[WAL_VERSION, WAL_VERSION]) {
        header[WRITE_VERSION] = LEGACY_VERSION;
        header[READ_VERSION] = LEGACY_VERSION;
    }
    let file = header.as_slice().chain(&input[header.len()..]);
    connection.deserialize_read_exact("main", file, input.len(), true)?;
    Ok(connection)
}

/// `getTableNames`.
fn table_names(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare(TABLE_NAMES)?;
    let names = statement.query_map([], |row| match row.get_ref(0)? {
        ValueRef::Text(name) => Ok(java::utf8(name)),
        other => Err(rusqlite::Error::InvalidColumnType(
            0,
            "name".to_owned(),
            other.data_type(),
        )),
    })?;
    names.collect()
}

/// The value a pragma query reads, as `getInt` reads it, or none where the query fails, which
/// `extractMetadata` swallows.
fn pragma(connection: &Connection, query: &str) -> Option<i32> {
    connection
        .query_row(query, [], |row| row.get::<_, Option<i64>>(0))
        .ok()
        .flatten()
        .map(|value| value as i32)
}

/// `Integer.toString(value, 16)`.
fn java_hex(value: i32) -> String {
    if value < 0 {
        format!("-{:x}", value.unsigned_abs())
    } else {
        format!("{value:x}")
    }
}

/// `java.sql.Types`, as sqlite-jdbc's `getColumnType` reports a value of a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JdbcType {
    Boolean,
    TinyInt,
    SmallInt,
    BigInt,
    Date,
    Timestamp,
    Integer,
    Decimal,
    Double,
    Numeric,
    Real,
    Float,
    Char,
    Clob,
    Varchar,
    Binary,
    Blob,
}

/// The storage class of a value, `sqlite3_column_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Storage {
    Integer,
    Float,
    Text,
    Blob,
    Null,
}

impl Storage {
    fn of(value: ValueRef<'_>) -> Self {
        match value {
            ValueRef::Integer(_) => Self::Integer,
            ValueRef::Real(_) => Self::Float,
            ValueRef::Text(_) => Self::Text,
            ValueRef::Blob(_) => Self::Blob,
            ValueRef::Null => Self::Null,
        }
    }

    /// The name `getColumnTypeName` gives a column without a declared type.
    fn type_name(self) -> &'static str {
        match self {
            Self::Integer => "INTEGER",
            Self::Float => "FLOAT",
            Self::Blob => "BLOB",
            Self::Text => "TEXT",
            Self::Null => "NUMERIC",
        }
    }
}

/// `getColumnType`: the JDBC type of a value stored as `storage` in a column of `type_name`, an
/// integer one in or out of the range of an `int`.
fn jdbc_type(type_name: &str, storage: Storage, value: ValueRef<'_>) -> JdbcType {
    let is = |names: &[&str]| names.contains(&type_name);
    if matches!(storage, Storage::Integer | Storage::Null) {
        if is(&["BOOLEAN"]) {
            return JdbcType::Boolean;
        }
        if is(&["TINYINT"]) {
            return JdbcType::TinyInt;
        }
        if is(&["SMALLINT", "INT2"]) {
            return JdbcType::SmallInt;
        }
        if is(&["BIGINT", "INT8", "UNSIGNED BIG INT"]) {
            return JdbcType::BigInt;
        }
        if is(&["DATE"]) {
            return JdbcType::Date;
        }
        if is(&["DATETIME", "TIMESTAMP"]) {
            return JdbcType::Timestamp;
        }
        if storage == Storage::Integer || is(&["INT", "INTEGER", "MEDIUMINT"]) {
            let long = match value {
                ValueRef::Integer(long) => long,
                _ => 0,
            };
            return if i32::try_from(long).is_ok() {
                JdbcType::Integer
            } else {
                JdbcType::BigInt
            };
        }
    }
    if matches!(storage, Storage::Float | Storage::Null) {
        if is(&["DECIMAL"]) {
            return JdbcType::Decimal;
        }
        if is(&["DOUBLE", "DOUBLE PRECISION"]) {
            return JdbcType::Double;
        }
        if is(&["NUMERIC"]) {
            return JdbcType::Numeric;
        }
        if is(&["REAL"]) {
            return JdbcType::Real;
        }
        if storage == Storage::Float || is(&["FLOAT"]) {
            return JdbcType::Float;
        }
    }
    if matches!(storage, Storage::Text | Storage::Null) {
        if is(&["CHARACTER", "NCHAR", "NATIVE CHARACTER", "CHAR"]) {
            return JdbcType::Char;
        }
        if is(&["CLOB"]) {
            return JdbcType::Clob;
        }
        if is(&["DATE"]) {
            return JdbcType::Date;
        }
        if is(&["DATETIME", "TIMESTAMP"]) {
            return JdbcType::Timestamp;
        }
        if storage == Storage::Text || is(&["VARCHAR", "VARYING CHARACTER", "NVARCHAR", "TEXT"]) {
            return JdbcType::Varchar;
        }
    }
    if matches!(storage, Storage::Blob | Storage::Null) {
        if is(&["BINARY"]) {
            return JdbcType::Binary;
        }
        if storage == Storage::Blob || is(&["BLOB"]) {
            return JdbcType::Blob;
        }
    }
    JdbcType::Numeric
}

/// The type a column declares, as `getColumnDeclType` finds it: its declared type, else the
/// type a `cast` its name holds casts to.
fn declared_type(declared: Option<&str>, name: &str) -> Option<String> {
    if let Some(declared) = declared {
        return Some(declared.to_owned());
    }
    type_cast()
        .captures(name)
        .map(|captures| captures[1].to_owned())
}

/// `COLUMN_TYPECAST`, `cast\(.*?\s+as\s+(.*?)\s*\)`, with Java's `.` and `\s`.
fn type_cast() -> &'static regex::Regex {
    static TYPE_CAST: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    TYPE_CAST.get_or_init(|| {
        regex::Regex::new(
            r"cast\([^\n\r\x{85}\x{2028}\x{2029}]*?[ \t\n\x0B\f\r]+as[ \t\n\x0B\f\r]+([^\n\r\x{85}\x{2028}\x{2029}]*?)[ \t\n\x0B\f\r]*\)",
        )
        .expect("the type cast pattern compiles")
    })
}

/// `getColumnTypeName` of a column that declares `declared`: what precedes its first
/// parenthesis, upper-cased.
fn declared_type_name(declared: &str) -> String {
    let base = declared.split('(').next().unwrap_or_default();
    java::to_upper_case(base)
}

/// The open database, with the statement that reads values as text.
struct Database<'c> {
    connection: &'c Connection,
    as_text: Statement<'c>,
}

impl<'c> Database<'c> {
    fn new(connection: &'c Connection) -> Result<Self, Failure> {
        let as_text = connection
            .prepare(AS_TEXT)
            .map_err(|error| malformed("preparing the conversion of values to text", error))?;
        Ok(Self {
            connection,
            as_text,
        })
    }

    /// One table, as `AbstractDBParser.parse` writes it with a `JDBCTableReader`: a failing query
    /// ends it where it fails.
    fn write_table(&mut self, table: &str, xhtml: &mut Xhtml) -> Result<(), Failure> {
        xhtml.start_element_with("table", &[("name", table)])?;
        let query = format!("SELECT * from {table}");
        let mut statement = self
            .connection
            .prepare(&query)
            .map_err(|error| malformed(&format!("querying table {table}"), error))?;
        let columns: Vec<Column> = statement
            .columns()
            .iter()
            .map(|column| Column {
                name: column.name().to_owned(),
                declared: declared_type(column.decl_type(), column.name())
                    .map(|declared| declared_type_name(&declared)),
            })
            .collect();
        let mut rows = statement
            .query([])
            .map_err(|error| malformed(&query, error))?;
        let mut number = 0usize;
        loop {
            let row = rows.next().map_err(|error| malformed(&query, error))?;
            if number == 0 {
                write_head(&columns, xhtml)?;
            }
            let Some(row) = row else {
                break;
            };
            xhtml.start_element("tr")?;
            for (index, column) in columns.iter().enumerate() {
                xhtml.start_element("td")?;
                let value = row
                    .get_ref(index)
                    .map_err(|error| malformed(&format!("reading a value of {table}"), error))?;
                self.write_cell(table, column, number, value, xhtml)?;
                xhtml.end_element("td")?;
            }
            xhtml.end_element("tr")?;
            number += 1;
        }
        xhtml.end_element("tbody")?;
        xhtml.end_element("table")?;
        Ok(())
    }

    /// `handleCell`.
    fn write_cell(
        &mut self,
        table: &str,
        column: &Column,
        row: usize,
        value: ValueRef<'_>,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let storage = Storage::of(value);
        let type_name = column
            .declared
            .as_deref()
            .unwrap_or_else(|| storage.type_name());
        let text = match (jdbc_type(type_name, storage, value), value) {
            (_, ValueRef::Null) | (JdbcType::Clob, _) => return Ok(()),
            (JdbcType::Blob, ValueRef::Blob(data)) => {
                return write_blob(table, &column.name, row, data, xhtml)
            }
            (JdbcType::Boolean, ValueRef::Integer(long)) => (long as i32 != 0).to_string(),
            (JdbcType::Float, ValueRef::Real(double)) => java_float_to_string(double as f32),
            (JdbcType::Double, ValueRef::Real(double)) => java_double_to_string(double),
            (_, ValueRef::Integer(long)) => long.to_string(),
            (_, ValueRef::Text(text)) => java::utf8(text),
            (_, value @ (ValueRef::Real(_) | ValueRef::Blob(_))) => self.text(value)?,
        };
        xhtml.characters_chunk(&text)?;
        Ok(())
    }

    /// `getString` of a value stored as a float or a blob: SQLite's conversion of it to text.
    fn text(&mut self, value: ValueRef<'_>) -> Result<String, Failure> {
        let text = self
            .as_text
            .query_row([ToSqlOutput::Borrowed(value)], |row| {
                match row.get_ref(0)? {
                    ValueRef::Text(text) => Ok(java::utf8(text)),
                    other => Err(rusqlite::Error::InvalidColumnType(
                        0,
                        "text".to_owned(),
                        other.data_type(),
                    )),
                }
            })
            .map_err(|error| malformed("converting a value to text", error))?;
        Ok(text)
    }
}

/// A column of a table's query: its name, and the type its declaration or name declares.
struct Column {
    name: String,
    declared: Option<String>,
}

/// `getHeaders`, then the start of the body.
fn write_head(columns: &[Column], xhtml: &mut Xhtml) -> Result<(), Failure> {
    xhtml.start_element("thead")?;
    xhtml.start_element("tr")?;
    for column in columns {
        xhtml.start_element("th")?;
        xhtml.characters(&column.name)?;
        xhtml.end_element("th")?;
    }
    xhtml.end_element("tr")?;
    xhtml.end_element("thead")?;
    xhtml.start_element("tbody")?;
    Ok(())
}

fn malformed(doing: &str, error: impl Display) -> Failure {
    Failure::Malformed(format!("SQLite3Parser failed {doing}: {error}"))
}

/// `handleBlob`: the bytes of a binary value in a span naming its column and row, parsed as an
/// embedded document named by them and the extension of its detected type.
fn write_blob(
    table: &str,
    column: &str,
    row: usize,
    data: &[u8],
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let mut metadata = TikaMetadata::new();
    metadata.set(TABLE_NAME, table);
    metadata.set(COLUMN_NAME, column);
    metadata.set(ROW_NUM, row.to_string());
    metadata.set(IS_BLOB, "true");
    metadata.set(CONTENT_LENGTH, data.len().to_string());
    let row_number = row.to_string();
    xhtml.start_element_with(
        "span",
        &[
            ("type", "blob"),
            ("column_name", column),
            ("row_number", &row_number),
        ],
    )?;
    let detected = mime::detect(data, Origin::Bytes)
        .map_err(|error| {
            Failure::Malformed(format!(
                "SQLite3Parser failed detecting the type of {column} in row {row} of {table}: \
                 {error:?}"
            ))
        })?
        .media_type;
    metadata.set(CONTENT_TYPE, detected.to_string());
    let name = format!("{column}_{row}{}", mime::extension(&detected));
    if let Some(name) = normalized_name(&name)? {
        metadata.set(RESOURCE_NAME, name);
    }
    metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
    xhtml.parse_embedded(data, &metadata, Wrapping::PackageEntry)?;
    xhtml.end_element("span")?;
    Ok(())
}

/// `FilenameUtils.normalize(FilenameUtils.getName(name))` on Unix: the name after its last
/// separator, none where its prefix is invalid, and with a separator after it where it starts
/// with a `~` user prefix it does not end.
fn normalized_name(name: &str) -> Result<Option<String>, Failure> {
    if name.contains('\0') {
        return Err(Failure::Malformed(format!(
            "SQLite3Parser failed naming a value: java.lang.IllegalArgumentException: Null \
             character present in file/path name {name}"
        )));
    }
    let name = &name[name.rfind(['/', '\\']).map_or(0, |at| at + 1)..];
    let mut characters = name.chars();
    let (Some(first), second) = (characters.next(), characters.next()) else {
        return Ok(Some(name.to_owned()));
    };
    if first == ':' {
        return Ok(None);
    }
    if first == '~' {
        return Ok(Some(format!("{name}/")));
    }
    if second == Some(':') {
        let upper = java::to_upper_case(&first.to_string());
        if !(upper.len() == 1 && upper.as_bytes()[0].is_ascii_uppercase()) {
            return Ok(None);
        }
    }
    Ok(Some(name.to_owned()))
}
