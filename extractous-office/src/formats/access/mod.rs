//! Tika's `JackcessParser` over Microsoft Access databases, Jackcess's reading ported: the
//! database's properties as metadata, then each of the user's tables as a table of
//! its columns and rows, binary values and OLE objects handed on as embedded documents, and each
//! query as the SQL Jackcess writes for it.
//!
//! The port reads the whole database while it prepares, so that what Jackcess would throw is
//! known before anything is written. A database under Office encryption is decrypted with the
//! password given, and fails as encrypted without the right one. A damaged database fails as
//! Tika's path fails on it, rethrown as `JackcessParser` rethrows what Jackcess throws; where
//! Jackcess throws reading pages past the first, the failure is a reading that may differ from
//! Tika's. The port leaves to Tika an MSISAM database.
//!
//! Where Tika is wrong, the port departs from it (#122): it keeps the database password out of
//! the metadata, decodes Jet 3 text in the database's code page, writes every table with each
//! column's own values, currency amounts, numerics and dates as they are kept, and rich text
//! whole, and reads no usage map but those that hold a table's rows.

#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
mod as_tika;
mod buffer;
mod column;
mod crypt;
mod database;
mod format;
mod ole;
mod pages;
mod properties;
mod query;
mod table;
mod usage_map;

#[cfg(test)]
mod tests;

#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub use as_tika::as_tika_writes;
use buffer::{Exception, Kind, Thrown, CLASS_CAST};
use column::{DataType, Decimal, Value};
use database::Database;
use ole::Ole;
use pages::Pages;
use properties::{PropertyMap, PropertyMaps};
use query::Sql;

use crate::config::{ParserConfigs, Password};
use crate::errors::Error;
use crate::formats::crypt as office_crypt;
use crate::formats::embedded::{ParseContext, Wrapping};
use crate::formats::handlers::{Body, ToText};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{handed_on, Embedded, Handed};
use crate::formats::thrown::{Exception as JavaException, Thrown as Rethrown, TIKA_EXCEPTION};
use crate::formats::xhtml::Xhtml;
use crate::formats::{
    html, java_file_name, mark_password_used, noted, unless_different, Decline, Difference,
    Failure, InputFile, Opened, Parser, Prepared, StringOut,
};
use crate::java::dtoa::{java_double_to_string, java_float_to_string};
use crate::java::time::civil::civil_from_days;

const TIKA_NAME: &str = "org.apache.tika.parser.microsoft.JackcessParser";

const UNSUPPORTED_FORMAT: &str = "org.apache.tika.exception.UnsupportedFormatException";
/// What Jackcess's messages `JackcessParser` rethrows otherwise hold or start with.
const INCORRECT_PASSWORD: &str = "Incorrect password";
const INVALID_PAGE_NUMBER: &str = "invalid page number ";
const OLD_MAP_TYPE: &str = "Unrecognized map type: 75";
/// The name Jackcess gives a database without a file.
const UNKNOWN_DATABASE: &str = "<UNKNOWN.DB>";
const SUMMARY_PREFIX: &str = "mdb-summary-prop:";
const PROPERTY_PREFIX: &str = "mdb-prop:";
const USER_PROPERTY_PREFIX: &str = "mdb-user-prop:";
const TITLE: &str = "dc:title";
const CREATOR: &str = "dc:creator";
const COMPANY: &str = "extended-properties:Company";
const TITLE_PROPERTY: &str = "Title";
const AUTHOR_PROPERTY: &str = "Author";
const COMPANY_PROPERTY: &str = "Company";
const TEXT_FORMAT_PROPERTY: &str = "TextFormat";
const RICH_TEXT_FORMAT: i8 = 1;
/// The decimals a currency amount is kept with, and the fewest it is written with.
const MONEY_SCALE: i64 = 4;
const MONEY_MIN_DECIMALS: usize = 2;
const MILLIS_PER_DAY: i64 = 86_400_000;
const MILLIS_BETWEEN_EPOCH_AND_1900: i64 = 25_569 * MILLIS_PER_DAY;

/// Tika's `JackcessParser`.
pub struct JackcessParser;

impl Parser for JackcessParser {
    fn name(&self) -> &'static str {
        "extractous::JackcessParser"
    }
    fn tika_name(&self) -> &'static str {
        TIKA_NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-msaccess"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        prepare(input, configs)
    }
}

/// What the parse writes: metadata in the order it is set, the tables and the queries.
struct Extraction {
    metadata: Vec<Entry>,
    tables: Vec<TableContent>,
    queries: Vec<String>,
}

/// A metadata change.
enum Entry {
    Set(&'static str, String),
    /// `Metadata.add(KeyPrefix, name, value)`.
    AddPrefixed(&'static str, String, String),
    AddMulti(&'static str, String),
}

struct TableContent {
    name: String,
    headers: Vec<String>,
    rows: Vec<Vec<Cell>>,
}

enum Cell {
    Text(String),
    /// A binary column's bytes, or the document an OLE object holds, handed on bare.
    Resource(Embedded<'static>),
    /// What `handleEmbeddedOfficeDoc` hands on of the compound document an OLE object holds,
    /// in a package entry.
    Storage(Handed<'static>),
}

impl Extraction {
    fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let metadata = xhtml.metadata_mut();
        for entry in &self.metadata {
            match entry {
                Entry::Set(name, value) => metadata.set(name, value.clone()),
                Entry::AddPrefixed(prefix, name, value) => {
                    metadata.add_prefixed(prefix, name, value.clone())
                }
                Entry::AddMulti(name, value) => {
                    crate::formats::ole2::add_multi(metadata, name, value)
                }
            }
        }
        for table in self.tables {
            xhtml.start_element_with("table", &[("name", &table.name)])?;
            xhtml.start_element("thead")?;
            xhtml.start_element("tr")?;
            for header in &table.headers {
                xhtml.start_element("th")?;
                xhtml.characters(header)?;
                xhtml.end_element("th")?;
            }
            xhtml.end_element("tr")?;
            xhtml.end_element("thead")?;
            xhtml.start_element("tbody")?;
            for row in table.rows {
                xhtml.start_element("tr")?;
                for cell in row {
                    xhtml.start_element("td")?;
                    match cell {
                        Cell::Text(text) => xhtml.characters(&text)?,
                        Cell::Resource(document) => xhtml.parse_embedded_input_opened(
                            &handed_on(document.data)?,
                            &document.metadata,
                            Wrapping::Bare,
                            Opened::Alone,
                        )?,
                        Cell::Storage(handed) => handed.write(xhtml, Wrapping::PackageEntry)?,
                    }
                    xhtml.end_element("td")?;
                }
                xhtml.end_element("tr")?;
            }
            xhtml.end_element("tbody")?;
            xhtml.end_element("table")?;
        }
        for sql in self.queries {
            xhtml.start_element_with("div", &[("type", "sqlQuery")])?;
            xhtml.characters(&sql)?;
            xhtml.end_element("div")?;
        }
        Ok(())
    }
}

/// `JackcessExtractor.toString`: a value as its column's or property's type writes it; a value
/// of another class throws.
///
/// A currency amount is written exactly, as a plain number with at least two decimals, where
/// Tika writes it through a double with the root locale's currency sign `¤` and two decimals; a
/// numeric in positional notation, where Tika's `BigDecimal.toString` may use an exponent; and
/// a date as its own date and time, where Tika takes it through the machine's time zone and
/// drops the time (#122).
fn text(value: Option<&Value>, data_type: DataType) -> Result<String, Thrown> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    let mismatch = || {
        Err(Thrown::runtime(
            CLASS_CAST,
            format!("{value:?} as {data_type:?}"),
        ))
    };
    Ok(match (data_type, value) {
        (DataType::Long, Value::Long(v)) => v.to_string(),
        (DataType::Text | DataType::Memo, Value::Text(v)) => v.clone(),
        (DataType::Money, Value::Money(v)) => money(*v),
        (DataType::ShortDateTime, Value::Date(bits)) => date(*bits),
        (DataType::Boolean, Value::Boolean(v)) => v.to_string(),
        (DataType::Int, Value::Int(v)) => v.to_string(),
        (DataType::Double, Value::Double(v)) => java_double_to_string(*v),
        (DataType::Float, Value::Float(v)) => java_float_to_string(*v),
        (DataType::Numeric, Value::Numeric(v)) => v.plain(),
        (DataType::Byte, Value::Byte(v)) => v.to_string(),
        (DataType::Guid, Value::Guid(v)) => v.clone(),
        (
            DataType::Long
            | DataType::Text
            | DataType::Memo
            | DataType::Money
            | DataType::ShortDateTime
            | DataType::Boolean
            | DataType::Int
            | DataType::Double
            | DataType::Float
            | DataType::Byte
            | DataType::Numeric
            | DataType::Guid,
            _,
        ) => return mismatch(),
        _ => String::new(),
    })
}

/// A currency amount, kept in ten-thousandths: exactly, with at least two decimals.
fn money(ten_thousandths: i64) -> String {
    let mut amount = Decimal {
        negative: ten_thousandths < 0,
        magnitude: u128::from(ten_thousandths.unsigned_abs()),
        scale: MONEY_SCALE,
    }
    .plain();
    let decimals = MONEY_SCALE as usize;
    let trailing = amount.bytes().rev().take_while(|&b| b == b'0').count();
    amount.truncate(amount.len() - trailing.min(decimals - MONEY_MIN_DECIMALS));
    amount
}

/// A date as Access keeps it, the days from 1899-12-30 as a double whose fraction, unsigned
/// for the days before, is the time of day: its date as `yyyy-MM-dd`, followed by its time as
/// `HH:mm:ss` where that is not midnight, and by the milliseconds where there are any.
fn date(bits: i64) -> String {
    let value = f64::from_bits(bits as u64);
    let date_part = i128::from(value as i64) * i128::from(MILLIS_PER_DAY);
    let time_part = i128::from(crate::java::round(
        (value.abs() % 1.0) * MILLIS_PER_DAY as f64,
    ));
    let millis = date_part + time_part - i128::from(MILLIS_BETWEEN_EPOCH_AND_1900);
    let day_millis = i128::from(MILLIS_PER_DAY);
    let days = i64::try_from(millis.div_euclid(day_millis)).expect("days of an i64 date");
    let of_day = millis.rem_euclid(day_millis) as i64;
    let (year, month, day) = civil_from_days(days);
    let mut text = format!("{year:04}-{month:02}-{day:02}");
    if of_day != 0 {
        let seconds = of_day / 1000;
        text.push_str(&format!(
            " {:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        ));
        if of_day % 1000 != 0 {
            text.push_str(&format!(".{:03}", of_day % 1000));
        }
    }
    text
}

/// Why the port leaves a database to Tika: what of it the port does not read.
fn deferred(what: String) -> Decline<'static> {
    Decline::Deferred(format!("{what}, which the port does not read"))
}

/// What Tika's path fails with where Jackcess, or Tika's extractor over it, throws `exception`
/// reading the database Jackcess names `database`: what `JackcessParser.parse` rethrows it as,
/// wrapped as `CompositeParser` wraps it.
fn failure(exception: &Exception, database: &str) -> Error {
    let message = exception.message(database);
    let cause = exception.java(database);
    let corrupt = || {
        Rethrown::Tika(
            JavaException::new(
                TIKA_EXCEPTION,
                format!(
                    "Corrupt Access database: {}",
                    message.as_deref().unwrap_or("null")
                ),
            )
            .caused_by(cause.clone()),
        )
    };
    let has = |text: &str| {
        message
            .as_deref()
            .is_some_and(|message| message.contains(text))
    };
    let rethrown = match exception.kind {
        Kind::Io if has(OLD_MAP_TYPE) => Rethrown::Tika(JavaException::new(
            UNSUPPORTED_FORMAT,
            "Jackcess doesn't process mdb versions before v97",
        )),
        Kind::Io => Rethrown::Io(cause),
        Kind::IndexOutOfBounds => corrupt(),
        Kind::IllegalState if has(INCORRECT_PASSWORD) => {
            return office_crypt::encrypted(Some(&cause.to_string()))
        }
        Kind::IllegalState
            if message
                .as_deref()
                .is_some_and(|message| message.starts_with(INVALID_PAGE_NUMBER)) =>
        {
            corrupt()
        }
        Kind::IllegalState | Kind::Runtime => Rethrown::Runtime(cause),
    };
    rethrown.into_error(TIKA_NAME)
}

/// The name Jackcess gives a database: its file's, where Tika holds it in the file it was read
/// from, else that of a database without a file, as the port reads one from memory where Tika
/// spools it to a temporary file of its own.
fn database_name(file: &InputFile) -> &str {
    match file {
        InputFile::Original(path) => java_file_name(path),
        InputFile::Spooled => UNKNOWN_DATABASE,
    }
}

/// Reads the database as `JackcessParser.parse` does, under the password `configs` gives, if
/// any. What Jackcess throws opening the file, before it reads a page past the first, the
/// parse fails with as Tika's path does. What it throws reading the database the port reads it
/// to fail with, but Jackcess may throw on a damaged database elsewhere and otherwise: Tika's
/// path extracts such a database in a build with Tika.
fn prepare<'a>(input: &[u8], configs: &'a ParserConfigs) -> Result<Prepared<'a>, Decline<'a>> {
    let password = configs.password.as_ref().map_or("", Password::expose);
    let pages = Database::pages(input, password).map_err(|thrown| match thrown {
        Thrown::Exception(exception) => Decline::Fails(failure(&exception, UNKNOWN_DATABASE)),
        Thrown::Unported(what) => deferred(what),
    })?;
    let refused = pages.refused();
    match extract(pages, configs) {
        Ok((extraction, difference)) => unless_different(
            Box::new(move |xhtml| {
                mark_password_used(xhtml.metadata_mut(), configs);
                extraction.write(xhtml)
            }),
            refused.or(difference),
        ),
        Err(Thrown::Unported(what)) => Err(deferred(what)),
        Err(Thrown::Exception(exception)) => Err(Decline::Differs {
            difference: refused.unwrap_or_else(|| {
                Difference(format!(
                    "a damaged database, which Jackcess may throw on elsewhere and otherwise than \
                 the port: {}",
                    exception.java(UNKNOWN_DATABASE)
                ))
            }),
            reading: Box::new(move |xhtml| {
                let database = database_name(xhtml.context().input_file());
                Err(Failure::Extraction(failure(&exception, database)))
            }),
        }),
    }
}

/// Reads the database of `pages` as `JackcessExtractor.parse` does.
fn extract(
    pages: Pages<'_>,
    configs: &ParserConfigs,
) -> Result<(Extraction, Option<Difference>), Thrown> {
    let database = Database::open(pages)?;
    let mut metadata = Vec::new();
    let add_all = |maps: PropertyMaps, prefix: &'static str, metadata: &mut Vec<Entry>| {
        for property in maps.default_map().iter() {
            let value = text(property.value.as_ref(), property.data_type)?;
            metadata.push(Entry::AddPrefixed(prefix, property.name.clone(), value));
        }
        Ok::<_, Thrown>(())
    };
    add_all(
        database.database_properties()?,
        PROPERTY_PREFIX,
        &mut metadata,
    )?;
    add_all(
        database.user_defined_properties()?,
        USER_PROPERTY_PREFIX,
        &mut metadata,
    )?;
    summary(&database.summary_properties()?.default_map(), &mut metadata)?;
    let mut tables = Vec::new();
    let mut difference = None;
    for info in database.tables()? {
        let table = database.load_table(&info)?;
        let (content, read_otherwise) = table_content(&database, &table, info.page, configs)?;
        difference = difference.or(read_otherwise);
        tables.push(content);
    }
    let mut queries = Vec::new();
    for definition in database.queries()? {
        queries.push(match query::sql(&definition)? {
            Sql::Text(text) => text,
            Sql::Unsupported => "unsupported query type".to_owned(),
        });
    }
    Ok((
        Extraction {
            metadata,
            tables,
            queries,
        },
        difference,
    ))
}

/// The summary properties: the title, author and company as Tika's properties, the others under
/// their own names.
fn summary(map: &PropertyMap, metadata: &mut Vec<Entry>) -> Result<(), Thrown> {
    let mut found = Vec::new();
    if let Some(title) = map.get(TITLE_PROPERTY) {
        metadata.push(Entry::Set(
            TITLE,
            text(title.value.as_ref(), title.data_type)?,
        ));
        found.push(title.name.clone());
    }
    if let Some(author) = map
        .get(AUTHOR_PROPERTY)
        .filter(|author| author.value.is_some())
    {
        metadata.push(Entry::AddMulti(
            CREATOR,
            text(author.value.as_ref(), author.data_type)?,
        ));
        found.push(author.name.clone());
    }
    if let Some(company) = map.get(COMPANY_PROPERTY) {
        metadata.push(Entry::Set(
            COMPANY,
            text(company.value.as_ref(), company.data_type)?,
        ));
        found.push(company.name.clone());
    }
    for property in map.iter() {
        if !found.contains(&property.name) {
            let value = text(property.value.as_ref(), property.data_type)?;
            metadata.push(Entry::AddPrefixed(
                SUMMARY_PREFIX,
                property.name.clone(),
                value,
            ));
        }
    }
    Ok(())
}

/// A table's headers and rows as `JackcessExtractor` writes them, each column with its own
/// values, where Tika gives two columns of one name both the later one's (#122).
fn table_content(
    database: &Database<'_>,
    table: &table::Table,
    page: i32,
    configs: &ParserConfigs,
) -> Result<(TableContent, Option<Difference>), Thrown> {
    let mut difference = None;
    let columns = &table.columns;
    let mut rich: Option<Vec<bool>> = None;
    let mut rows = Vec::new();
    for row in table.rows(&database.pages, vec![true; columns.len()]) {
        let row = row?;
        let rich = match &rich {
            Some(rich) => rich,
            None => rich.insert(rich_text_columns(database, table, page)?),
        };
        let mut cells = Vec::with_capacity(columns.len());
        for (index, column) in columns.iter().enumerate() {
            let value = row[index].as_ref();
            cells.push(match column.data_type {
                DataType::Ole => match value {
                    None => Cell::Text(String::new()),
                    Some(Value::Binary(bytes)) => match ole::read(bytes, configs) {
                        Ok(Ole::Nothing) => Cell::Text(String::new()),
                        Ok(Ole::Link(path)) => Cell::Text(path),
                        Ok(Ole::Resource(document)) => Cell::Resource(document),
                        Ok(Ole::Storage(document)) => {
                            difference = difference.or_else(|| document.difference());
                            Cell::Storage(document)
                        }
                        Err(thrown) => return Err(thrown),
                    },
                    Some(other) => {
                        return Err(Thrown::runtime(CLASS_CAST, format!("{other:?} as byte[]")))
                    }
                },
                DataType::Binary => match value {
                    None => Cell::Text(String::new()),
                    Some(Value::Binary(bytes)) => {
                        Cell::Resource(Embedded::owned(bytes.clone(), TikaMetadata::new()))
                    }
                    Some(other) => {
                        return Err(Thrown::runtime(CLASS_CAST, format!("{other:?} as byte[]")))
                    }
                },
                data_type => {
                    let text = text(value, data_type)?;
                    if rich[index] {
                        let (text, read_otherwise) = rich_text(&text, configs)?;
                        difference = difference.or(read_otherwise);
                        Cell::Text(text)
                    } else {
                        Cell::Text(text)
                    }
                }
            });
        }
        rows.push(cells);
    }
    Ok((
        TableContent {
            name: table.name.clone(),
            headers: columns.iter().map(|column| column.name.clone()).collect(),
            rows,
        },
        difference,
    ))
}

/// `isRichText` of each column: a memo whose `TextFormat` property is the byte of rich text.
///
/// Only a table with a memo reads its properties for it. Tika reads the properties of every
/// column it writes as text, those of each multi-valued field's flat table too, so that any of
/// them it cannot read fails the whole parse (#122).
fn rich_text_columns(
    database: &Database<'_>,
    table: &table::Table,
    page: i32,
) -> Result<Vec<bool>, Thrown> {
    if table
        .columns
        .iter()
        .all(|column| column.data_type != DataType::Memo)
    {
        return Ok(vec![false; table.columns.len()]);
    }
    let read;
    let properties = match &table.properties {
        Some(properties) => properties,
        None => {
            read = database.object_properties(page)?;
            &read
        }
    };
    Ok(table
        .columns
        .iter()
        .map(|column| {
            column.data_type == DataType::Memo
                && properties.column(&column.name).value(TEXT_FORMAT_PROPERTY)
                    == Some(&Value::Byte(RICH_TEXT_FORMAT))
        })
        .collect())
}

/// What `JSoupParser` writes of rich text, as text, all of it: Tika writes it through a
/// `BodyContentHandler` whose write limit of 100,000 characters fails the parse (#122). Where
/// the HTML parser reads the rich text differently from Tika's, it says how.
fn rich_text(html: &str, configs: &ParserConfigs) -> Result<(String, Option<Difference>), Thrown> {
    let mut metadata = TikaMetadata::new();
    metadata.set("Content-Type", "text/html; charset=UTF-8");
    let (prepared, difference) =
        noted(html::prepare(html.as_bytes(), &metadata)).map_err(|reason| {
            Thrown::unported(format!("rich text the HTML parser defers: {reason}"))
        })?;
    let context = ParseContext::new(configs);
    let mut out = StringOut::default();
    let mut handler = Body::new(ToText::new(&mut out));
    let (_, _, result) = Xhtml::write(&mut handler, metadata, &context, prepared);
    result.map_err(|failure| {
        Thrown::unported(format!("rich text the HTML parser fails on: {failure:?}"))
    })?;
    let difference = difference
        .map(|Difference(reason)| Difference(format!("rich text in an Access table: {reason}")));
    Ok((out.0, difference))
}
