//! Tika's `JackcessParser` over Microsoft Access databases, Jackcess's reading ported: the
//! database's password and properties as metadata, then each of the user's tables as a table of
//! its columns and rows, binary values and OLE objects handed on as embedded documents, and each
//! query as the SQL Jackcess writes for it.
//!
//! The port reads the whole database while it prepares, so that whatever Jackcess would throw
//! leaves the database to Tika before anything is written. It also leaves to Tika a database
//! encrypted with Office encryption, which needs a password, an MSISAM database, and a Jet 3
//! table name Jackcess may not find through the catalog's index.

mod buffer;
mod column;
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

use buffer::Thrown;
use column::{Complex, DataType, Value};
use database::Database;
use ole::Ole;
use properties::{PropertyMap, PropertyMaps};
use query::Sql;

use crate::config::ParserConfigs;
use crate::formats::embedded::{ParseContext, Wrapping};
use crate::formats::handlers::{Body, ToText};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{Embedded, Handed};
use crate::formats::xhtml::Xhtml;
use crate::formats::{html, Declined, Deferral, Failure, Opened, Parser, Prepared, StringOut};
use crate::java::decimal::{BigDecimal, Rounding};
use crate::java::decimal_format::{DecimalFormat, ROOT_CURRENCY};
use crate::java::dtoa::{java_double_to_string, java_float_to_string};
use crate::java::simple_date_format::SimpleDateFormat;
use crate::java::time::TimeZone;
use crate::java::utf16_length;

const SUMMARY_PREFIX: &str = "mdb-summary-prop:";
const PROPERTY_PREFIX: &str = "mdb-prop:";
const USER_PROPERTY_PREFIX: &str = "mdb-user-prop:";
const PASSWORD: &str = "mdb:password";
const TITLE: &str = "dc:title";
const CREATOR: &str = "dc:creator";
const COMPANY: &str = "extended-properties:Company";
const TITLE_PROPERTY: &str = "Title";
const AUTHOR_PROPERTY: &str = "Author";
const COMPANY_PROPERTY: &str = "Company";
const TEXT_FORMAT_PROPERTY: &str = "TextFormat";
const RICH_TEXT_FORMAT: i8 = 1;
/// `NumberFormat.getCurrencyInstance(Locale.ROOT)`'s pattern.
const CURRENCY_PATTERN: &str = "\u{a4}\u{a0}#,##0.00";
/// `DateFormat.getDateInstance(DateFormat.SHORT, Locale.ROOT)`'s pattern.
const SHORT_DATE_PATTERN: &str = "y-MM-dd";
/// The default limit of the `BodyContentHandler` the text of rich text goes through.
const RICH_TEXT_WRITE_LIMIT: usize = 100_000;
const MILLIS_PER_DAY: i64 = 86_400_000;
const MILLIS_BETWEEN_EPOCH_AND_1900: i64 = 25_569 * MILLIS_PER_DAY;

/// Tika's `JackcessParser`.
pub(crate) struct JackcessParser;

impl Parser for JackcessParser {
    fn name(&self) -> &'static str {
        "extractous::JackcessParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.JackcessParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-msaccess"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let extraction = extract(input, configs).map_err(Declined::Deferred)?;
        Ok(Box::new(move |xhtml| extraction.write(xhtml)))
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
    Resource(Embedded),
    /// What `handleEmbeddedOfficeDoc` hands on of the compound document an OLE object holds,
    /// in a package entry.
    Storage(Handed),
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
                        Cell::Resource(document) => xhtml.parse_embedded_opened(
                            &document.data,
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

/// How `JackcessExtractor.toString` writes dates and currency amounts.
struct Formats {
    currency: DecimalFormat,
    date: SimpleDateFormat,
    zone: &'static TimeZone,
}

impl Formats {
    fn new() -> Result<Self, Deferral> {
        Ok(Self {
            currency: DecimalFormat::with_currency(
                CURRENCY_PATTERN,
                Rounding::HalfEven,
                ROOT_CURRENCY,
            )
            .expect("the currency pattern compiles"),
            date: SimpleDateFormat::new(SHORT_DATE_PATTERN).expect("the date pattern compiles"),
            zone: TimeZone::java_default()?,
        })
    }

    /// `JackcessExtractor.toString`: a value as its column's or property's type writes it; a
    /// value of another class throws.
    fn text(&self, value: Option<&Value>, data_type: DataType) -> Result<String, Thrown> {
        let Some(value) = value else {
            return Ok(String::new());
        };
        let mismatch = || {
            Err(Thrown::other(format!(
                "java.lang.ClassCastException: {value:?} as {data_type:?}"
            )))
        };
        Ok(match (data_type, value) {
            (DataType::Long, Value::Long(v)) => v.to_string(),
            (DataType::Text | DataType::Memo, Value::Text(v)) => v.clone(),
            (DataType::Money, Value::Money(v)) => {
                let amount = BigDecimal::from_digits(*v < 0, &v.unsigned_abs().to_string(), 4);
                self.currency.format_f64(amount.to_f64())
            }
            (DataType::ShortDateTime, Value::Date(bits)) => self.date(*bits),
            (DataType::Boolean, Value::Boolean(v)) => v.to_string(),
            (DataType::Int, Value::Int(v)) => v.to_string(),
            (DataType::Double, Value::Double(v)) => java_double_to_string(*v),
            (DataType::Float, Value::Float(v)) => java_float_to_string(*v),
            (DataType::Numeric, Value::Numeric(v)) => v.to_java_string(),
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

    /// `formatShortDateTime` of the `Date` Jackcess makes of a date's bits: the local date and
    /// time the double counts in days from 1899-12-30, in the default zone.
    fn date(&self, bits: i64) -> String {
        let value = f64::from_bits(bits as u64);
        let date_part = (value as i64).wrapping_mul(MILLIS_PER_DAY);
        let time_part = crate::java::round((value.abs() % 1.0) * MILLIS_PER_DAY as f64);
        let local = date_part
            .wrapping_add(time_part)
            .wrapping_sub(MILLIS_BETWEEN_EPOCH_AND_1900);
        let offset = self
            .zone
            .offset_at_utc(local.wrapping_sub(self.zone.raw_offset()));
        let epoch = local.wrapping_sub(offset);
        self.date
            .format(epoch, self.zone)
            .expect("the short date pattern names no time zone")
    }
}

/// Reads the database as `JackcessExtractor.parse` does.
fn extract(input: &[u8], configs: &ParserConfigs) -> Result<Extraction, Deferral> {
    let formats = Formats::new()?;
    let thrown = Thrown::reason;
    let database = Database::open(input).map_err(thrown)?;
    let mut metadata = Vec::new();
    if let Some(password) = database.password().map_err(thrown)? {
        metadata.push(Entry::Set(PASSWORD, password));
    }
    let add_all = |maps: PropertyMaps, prefix: &'static str, metadata: &mut Vec<Entry>| {
        for property in maps.default_map().iter() {
            let value = formats.text(property.value.as_ref(), property.data_type)?;
            metadata.push(Entry::AddPrefixed(prefix, property.name.clone(), value));
        }
        Ok::<_, Thrown>(())
    };
    add_all(
        database.database_properties().map_err(thrown)?,
        PROPERTY_PREFIX,
        &mut metadata,
    )
    .map_err(thrown)?;
    add_all(
        database.user_defined_properties().map_err(thrown)?,
        USER_PROPERTY_PREFIX,
        &mut metadata,
    )
    .map_err(thrown)?;
    summary(
        &database.summary_properties().map_err(thrown)?.default_map(),
        &formats,
        &mut metadata,
    )
    .map_err(thrown)?;
    let mut tables = Vec::new();
    let mut iterated = Vec::new();
    for name in database.table_names().map_err(thrown)? {
        let Some(info) = database.table_info(&name).map_err(thrown)? else {
            break;
        };
        if iterated.contains(&info.page) {
            return Err(
                "two names for one table definition, which Jackcess may hand out as one table \
                 whose rows were read"
                    .to_owned(),
            );
        }
        iterated.push(info.page);
        let table = database.load_table(&info).map_err(thrown)?;
        tables.push(table_content(
            &database, &table, info.page, &formats, configs,
        )?);
    }
    let mut queries = Vec::new();
    for definition in database.queries().map_err(thrown)? {
        queries.push(match query::sql(&definition).map_err(thrown)? {
            Sql::Text(text) => text,
            Sql::Unsupported => "unsupported query type".to_owned(),
        });
    }
    Ok(Extraction {
        metadata,
        tables,
        queries,
    })
}

/// The summary properties: the title, author and company as Tika's properties, the others under
/// their own names.
fn summary(map: &PropertyMap, formats: &Formats, metadata: &mut Vec<Entry>) -> Result<(), Thrown> {
    let mut found = Vec::new();
    if let Some(title) = map.get(TITLE_PROPERTY) {
        metadata.push(Entry::Set(
            TITLE,
            formats.text(title.value.as_ref(), title.data_type)?,
        ));
        found.push(title.name.clone());
    }
    if let Some(author) = map
        .get(AUTHOR_PROPERTY)
        .filter(|author| author.value.is_some())
    {
        metadata.push(Entry::AddMulti(
            CREATOR,
            formats.text(author.value.as_ref(), author.data_type)?,
        ));
        found.push(author.name.clone());
    }
    if let Some(company) = map.get(COMPANY_PROPERTY) {
        metadata.push(Entry::Set(
            COMPANY,
            formats.text(company.value.as_ref(), company.data_type)?,
        ));
        found.push(company.name.clone());
    }
    for property in map.iter() {
        if !found.contains(&property.name) {
            let value = formats.text(property.value.as_ref(), property.data_type)?;
            metadata.push(Entry::AddPrefixed(
                SUMMARY_PREFIX,
                property.name.clone(),
                value,
            ));
        }
    }
    Ok(())
}

/// A table's headers and rows as `JackcessExtractor` writes them.
fn table_content(
    database: &Database<'_>,
    table: &table::Table,
    page: i32,
    formats: &Formats,
    configs: &ParserConfigs,
) -> Result<TableContent, Deferral> {
    let thrown = Thrown::reason;
    let columns = &table.columns;
    let sources: Vec<usize> = columns
        .iter()
        .map(|column| {
            columns
                .iter()
                .rposition(|other| other.name == column.name)
                .expect("a column is named as itself")
        })
        .collect();
    let mut rich: Option<Vec<bool>> = None;
    let mut rows = Vec::new();
    for row in table.rows(&database.pages, vec![true; columns.len()]) {
        let row = row.map_err(thrown)?;
        let rich = match &rich {
            Some(rich) => rich,
            None => rich.insert(rich_text_columns(database, table, page).map_err(thrown)?),
        };
        let mut cells = Vec::with_capacity(columns.len());
        for (index, column) in columns.iter().enumerate() {
            let value = row[sources[index]].as_ref();
            cells.push(match column.data_type {
                DataType::Ole => match value {
                    None => Cell::Text(String::new()),
                    Some(Value::Binary(bytes)) => match ole::read(bytes, configs) {
                        Ok(Ole::Nothing) => Cell::Text(String::new()),
                        Ok(Ole::Link(path)) => Cell::Text(path),
                        Ok(Ole::Resource(document)) => Cell::Resource(document),
                        Ok(Ole::Storage(document)) => Cell::Storage(document),
                        Err(unread) => return Err(unread.reason()),
                    },
                    Some(other) => {
                        return Err(thrown(Thrown::other(format!(
                            "java.lang.ClassCastException: {other:?} as byte[]"
                        ))))
                    }
                },
                DataType::Binary => match value {
                    None => Cell::Text(String::new()),
                    Some(Value::Binary(bytes)) => Cell::Resource(Embedded {
                        data: bytes.clone(),
                        metadata: TikaMetadata::new(),
                    }),
                    Some(other) => {
                        return Err(thrown(Thrown::other(format!(
                            "java.lang.ClassCastException: {other:?} as byte[]"
                        ))))
                    }
                },
                data_type => {
                    let text = formats.text(value, data_type).map_err(thrown)?;
                    if rich[index] {
                        Cell::Text(rich_text(&text, configs)?)
                    } else {
                        Cell::Text(text)
                    }
                }
            });
        }
        rows.push(cells);
    }
    Ok(TableContent {
        name: table.name.clone(),
        headers: columns.iter().map(|column| column.name.clone()).collect(),
        rows,
    })
}

/// `isRichText` of each column: a memo whose `TextFormat` property is the byte of rich text.
///
/// Tika reads a column's properties for each cell it writes as text, so where a row has such a
/// cell the table's properties are read, and the properties of the value column of each
/// multi-valued field.
fn rich_text_columns(
    database: &Database<'_>,
    table: &table::Table,
    page: i32,
) -> Result<Vec<bool>, Thrown> {
    if table
        .columns
        .iter()
        .all(|column| matches!(column.data_type, DataType::Ole | DataType::Binary))
    {
        return Ok(vec![false; table.columns.len()]);
    }
    for column in &table.columns {
        if let Some(Complex::MultiValue { flat_table_page }) = &column.complex {
            database.object_properties(*flat_table_page)?;
        }
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

/// What `JSoupParser` writes of rich text through a `BodyContentHandler`, as text.
fn rich_text(html: &str, configs: &ParserConfigs) -> Result<String, Deferral> {
    let mut metadata = TikaMetadata::new();
    metadata.set("Content-Type", "text/html; charset=UTF-8");
    let prepared = html::prepare(html.as_bytes(), &metadata)
        .map_err(|reason| format!("rich text in an Access table that defers: {reason}"))?;
    let context = ParseContext::new(configs);
    let mut out = StringOut::default();
    let mut handler = Body::new(ToText::new(&mut out));
    let (_, _, result) = Xhtml::write(&mut handler, metadata, &context, prepared);
    result.map_err(|failure| format!("rich text in an Access table that fails: {failure:?}"))?;
    if utf16_length(&out.0) > RICH_TEXT_WRITE_LIMIT {
        return Err(
            "rich text in an Access table past the write limit of the BodyContentHandler its \
             text goes through, which fails the parse"
                .to_owned(),
        );
    }
    Ok(out.0)
}
