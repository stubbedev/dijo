use std::path::{Path, PathBuf};

use super::*;
use crate::formats::testing::Scratch;

/// Writes a database at `path` from the SQL `script`.
fn database(path: &Path, script: &str) {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(script).unwrap();
}

/// A database of every declared type the JDBC types depend on, each holding a value of every
/// storage class, with the pragmas set.
const TYPES: &str = r#"
PRAGMA application_id = -1515870811;
PRAGMA user_version = 255;
CREATE TABLE types(
    i INTEGER, b BOOLEAN, t TINYINT, s SMALLINT, big BIGINT, d DATE, ts TIMESTAMP,
    dt DATETIME, dec DECIMAL(10, 2), dbl DOUBLE, dp "DOUBLE PRECISION", num NUMERIC, r REAL,
    f FLOAT, c CHAR(5), cl CLOB, v VARCHAR(20), nv nvarchar, bin BINARY, bl BLOB, untyped,
    spaced "VARCHAR (3)", custom "NOT A TYPE");
INSERT INTO types VALUES (
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23);
INSERT INTO types VALUES (
    4294967296, 4294967296, -9223372036854775808, 9223372036854775807, -2147483649,
    2147483648, -2147483648, 2147483647, 0, -1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1);
INSERT INTO types VALUES (
    0.1, 1.5, 2.25, 1e300, 3.141592653589793, 1e-7, 123456789.125, 0.000123, 2.5, 0.1,
    1e21, 1.0, 100.0, 0.3, 1e-5, 9.99, 7.0, 1e7, 2.0, 3.0, 0.5, 4.5, 6.75);
INSERT INTO types VALUES (
    'one', 'true', 'x', '', '2015-01-02', '2015-01-03', '2015-01-03 15:17:03', 'later', '1.50',
    'pi', 'tau', '12', 'twelve', 'text', 'chars', 'a clob', 'a <varchar> & more', 'ñ',
    'binary text', 'blob text', 'untyped', 'spaced', 'custom');
INSERT INTO types VALUES (
    x'68656c6c6f', x'00', x'01', x'02', x'03', x'04', x'05', x'06', x'07', x'08', x'09',
    x'0a', x'0b', x'0c', x'0d', x'0e', x'0f', x'10', x'c3a9', x'48656c6c6f2c20626c6f62',
    x'68690a', x'ff', x'2e');
INSERT INTO types VALUES (
    NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
    NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL);
CREATE TABLE empty(a TEXT, b INTEGER);
CREATE TABLE names("~home" BLOB, ":colon" BLOB, "a:b" BLOB, "C:drive" BLOB, "dir/name" BLOB);
INSERT INTO names VALUES (x'6869', x'6869', x'6869', x'6869', x'6869');
CREATE VIEW a_view AS SELECT i FROM types;
CREATE TABLE without_rowid(k TEXT PRIMARY KEY, v) WITHOUT ROWID;
INSERT INTO without_rowid VALUES ('b', 2), ('a', 1);
CREATE TABLE counter(id INTEGER PRIMARY KEY AUTOINCREMENT, v);
INSERT INTO counter(v) VALUES ('first');
"#;

/// The databases written for the tests, in `scratch`.
fn written(scratch: &Scratch) -> Vec<PathBuf> {
    let types = scratch.directory().join("types.sqlite");
    database(&types, TYPES);
    let utf16 = scratch.directory().join("utf16.sqlite");
    database(
        &utf16,
        "PRAGMA encoding = 'UTF-16le';
         CREATE TABLE words(w TEXT, r REAL, bin BINARY);
         INSERT INTO words VALUES ('Grüße 普林斯顿', 0.25, x'4100'), ('two', 1e100, x'4142');",
    );
    let wal = scratch.directory().join("wal.sqlite");
    database(
        &wal,
        "PRAGMA journal_mode = WAL;
         CREATE TABLE logged(v TEXT);
         INSERT INTO logged VALUES ('checkpointed');
         PRAGMA wal_checkpoint(TRUNCATE);",
    );
    vec![types, utf16, wal]
}

/// A database in write-ahead-log mode, spooled with its header changed, is read from the spool
/// while it is written, which the reading holds as long as its connection.
#[test]
fn reads_a_database_in_write_ahead_log_mode() {
    let scratch = Scratch::new("sqlite-wal");
    let wal = &written(&scratch)[2];
    let (text, metadata) = crate::Extractor::new()
        .extract_file_to_string(wal.to_str().unwrap())
        .unwrap();
    assert!(text.contains("checkpointed"), "{text}: {metadata:?}");
}

#[test]
fn reads_tables_natively() {
    let (text, metadata) = crate::Extractor::new()
        .extract_file_to_string("../test_files/documents/notes.sqlite")
        .unwrap();
    assert!(
        text.contains("Keeper log\tThe lamp was lit at seven minutes past five."),
        "{text}"
    );
    assert_eq!(metadata["database:table-name"], ["notes"]);
    assert_eq!(metadata["sqlite3:application_id"], ["0"]);
    assert_eq!(
        metadata["tk:parsed-by"].last().unwrap(),
        Sqlite3Parser.name()
    );
}

#[test]
fn writes_values_as_jdbc_types_them() {
    let scratch = Scratch::new("sqlite-types");
    let paths = written(&scratch);
    let (xhtml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_file_to_string(paths[0].to_str().unwrap())
        .unwrap();
    assert_eq!(metadata["sqlite3:application_id"], ["-5a5a5a5b"]);
    assert_eq!(metadata["sqlite3:user_version"], ["ff"]);
    assert_eq!(
        metadata["database:table-name"],
        [
            "types",
            "empty",
            "names",
            "without_rowid",
            "counter",
            "sqlite_sequence"
        ]
    );
    for cell in [
        "<td>4294967296</td>\t<td>false</td>",
        "<td>0.1</td>\t<td>1.5</td>",
        "<td>Infinity</td>\t<td>3.1415927</td>\t<td>1.0E-7</td>\t<td>1.2345679E8</td>",
        "<td>2.5</td>\t<td>0.1</td>\t<td>1.0E21</td>\t<td>1</td>\t<td>100.0</td>",
        "<td>1.0e-05</td>",
        "<td>é</td>",
        "<td>a &lt;varchar&gt; &amp; more</td>",
        "<td>binary text</td>\t<td>blob text</td>",
        "<td /></tr>",
        "<span type=\"blob\" column_name=\"i\" row_number=\"4\">",
        "<h1>~home_0.txt/</h1>",
        "<h1>C:drive_0.txt</h1>",
        "<h1>name_0.txt</h1>",
    ] {
        assert!(xhtml.contains(cell), "{cell} in {xhtml}");
    }
    assert!(!xhtml.contains("a clob"), "{xhtml}");
    assert!(!xhtml.contains("<h1>:colon"), "{xhtml}");
    assert!(xhtml.contains("<h1>a:b_0.txt</h1>"), "{xhtml}");
}

#[test]
fn names_values_as_commons_io_does() {
    let named = |name| normalized_name(name).ok().unwrap();
    assert_eq!(
        named("dir/sub\\BYTES_0.doc").as_deref(),
        Some("BYTES_0.doc")
    );
    assert_eq!(named("~user_1.bin").as_deref(), Some("~user_1.bin/"));
    assert_eq!(named(":x_1.bin"), None);
    assert_eq!(named("1:x_1.bin"), None);
    assert_eq!(named("c:x_1.bin").as_deref(), Some("c:x_1.bin"));
    assert!(normalized_name("a\0b_1.bin").is_err());
}

#[test]
fn writes_integers_in_hex_as_java_does() {
    assert_eq!(java_hex(0), "0");
    assert_eq!(java_hex(255), "ff");
    assert_eq!(java_hex(-1), "-1");
    assert_eq!(java_hex(i32::MIN), "-80000000");
}

#[test]
fn types_values_as_sqlite_jdbc_does() {
    let integer = ValueRef::Integer(1);
    assert_eq!(
        jdbc_type("BOOLEAN", Storage::Integer, integer),
        JdbcType::Boolean
    );
    assert_eq!(
        jdbc_type("REAL", Storage::Integer, integer),
        JdbcType::Integer
    );
    assert_eq!(
        jdbc_type("TEXT", Storage::Integer, ValueRef::Integer(1 << 40)),
        JdbcType::BigInt
    );
    assert_eq!(
        jdbc_type("INTEGER", Storage::Float, ValueRef::Real(1.0)),
        JdbcType::Float
    );
    assert_eq!(
        jdbc_type("REAL", Storage::Float, ValueRef::Real(1.0)),
        JdbcType::Real
    );
    assert_eq!(
        jdbc_type("INTEGER", Storage::Text, ValueRef::Text(b"x")),
        JdbcType::Varchar
    );
    assert_eq!(
        jdbc_type("CLOB", Storage::Text, ValueRef::Text(b"x")),
        JdbcType::Clob
    );
    assert_eq!(
        jdbc_type("BINARY", Storage::Blob, ValueRef::Blob(b"x")),
        JdbcType::Binary
    );
    assert_eq!(
        jdbc_type("FOO", Storage::Null, ValueRef::Null),
        JdbcType::Numeric
    );
    assert_eq!(declared_type_name("varchar (20)"), "VARCHAR ");
    assert_eq!(
        declared_type(None, "cast(x as  int )").as_deref(),
        Some("int")
    );
    assert_eq!(declared_type(None, "x"), None);
}

#[test]
fn reads_a_database_too_large_to_read_whole_in_place() {
    use extractous_core::large::{LargeFile, BUDGET, END};

    use crate::input::MAX_READ_WHOLE;

    let scratch = Scratch::new("sqlite-large");
    let path = scratch.directory().join("large.db");
    let rows = MAX_READ_WHOLE / (1 << 20) + 16;
    database(
        &path,
        &format!(
            "CREATE TABLE large(b BLOB);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {rows})
             INSERT INTO large SELECT zeroblob(1048576) FROM n;
             CREATE TABLE ending(t TEXT);
             INSERT INTO ending VALUES ('{END}');"
        ),
    );
    let file = LargeFile::written("large.db", |out| {
        std::io::copy(&mut std::fs::File::open(&path)?, out).map(|_| ())
    });
    let (text, _) = file.extract_with(
        &extractous_core::Extractor::new(),
        BUDGET,
        "extractous::SQLite3Parser",
    );
    assert!(text.contains(END), "{text}");
}
