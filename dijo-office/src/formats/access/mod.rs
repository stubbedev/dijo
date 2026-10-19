//! Tika's `JackcessParser` over Microsoft Access databases, Jackcess's reading ported: the
//! database's properties as metadata, then each of the user's tables as a table of
//! its columns and rows, binary values and OLE objects handed on as embedded documents, and each
//! query as the SQL Jackcess writes for it.
//!
//! The port reads the whole database while it prepares, so that what Jackcess would throw is
//! known before anything is written, then again as it writes it, a row at a time. Its pages are
//! read in place from the input, one at a time, decoded where they are encoded; what it holds is
//! the system catalog, the definition of the table being read and the queries' definitions, as
//! Jackcess does. A database under Office encryption is decrypted with the
//! password given, and fails as encrypted without the right one. A damaged database fails as
//! Tika fails on it, rethrown as `JackcessParser` rethrows what Jackcess throws; where
//! Jackcess throws reading pages past the first, the failure is a reading that may differ from
//! Tika's. An MSISAM (Microsoft Money) database is read as Jackcess reads it, its pages
//! decoded as `jackcess-encrypt` decodes them.
//!
//! Where Tika is wrong, the port departs from it (#122): it keeps the database password out of
//! the metadata, decodes Jet 3 text in the database's code page, writes every table with each
//! column's own values, currency amounts, numerics and dates as they are kept, and rich text
//! whole, and reads no usage map but those that hold a table's rows.

mod buffer;
mod column;
mod crypt;
mod database;
mod format;
mod msisam;
mod ole;
mod pages;
mod properties;
mod query;
mod table;
mod usage_map;

#[cfg(test)]
mod tests;

use buffer::{Exception, Kind, Thrown, CLASS_CAST};
use column::{DataType, Decimal, Value};
use database::Database;
use ole::Ole;

use properties::{PropertyMap, PropertyMaps};
use query::Sql;

use std::convert::Infallible;

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
    Failure, Footprint, InputFile, Opened, Parser, Prepared, StringOut,
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
        "dijo::JackcessParser"
    }
    fn tika_name(&self) -> &'static str {
        TIKA_NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-msaccess"]
    }

    /// It reads the database's pages in place, one at a time, twice: once to its end as far as
    /// deciding whether it extracts the database, then as it writes it, each row as it is read.
    /// What grows is what Jackcess holds: the system catalog, a table's definition, the rows of
    /// `MSysComplexColumns` and the queries' definitions.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
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

/// What the parse writes, in the order it reads it: metadata in the order it is set, then each
/// table, its rows one at a time, then the queries.
enum Event {
    Entry(Entry),
    Table { name: String, headers: Vec<String> },
    Row(Vec<Cell>),
    TableEnd,
    Query(String),
}

/// A metadata change.
enum Entry {
    Set(&'static str, String),
    /// `Metadata.add(KeyPrefix, name, value)`.
    AddPrefixed(&'static str, String, String),
    AddMulti(&'static str, String),
}

enum Cell {
    Text(String),
    /// A binary column's bytes, or the document an OLE object holds, handed on bare.
    Resource(Embedded<'static>),
    /// What `handleEmbeddedOfficeDoc` hands on of the compound document an OLE object holds,
    /// in a package entry.
    Storage(Handed<'static>),
}

/// Where the parse hands its events on as it reads them.
type Sink<'s, E> = dyn FnMut(Event) -> Result<(), E> + 's;

/// Why reading the database as it hands its events on stops: Jackcess throws, or the port does
/// not read it, or handing an event on failed.
enum Halt<E> {
    Thrown(Thrown),
    Written(E),
}

impl<E> From<Thrown> for Halt<E> {
    fn from(thrown: Thrown) -> Self {
        Halt::Thrown(thrown)
    }
}

/// Hands `event` on to `sink`.
fn emit<E>(sink: &mut Sink<'_, E>, event: Event) -> Result<(), Halt<E>> {
    sink(event).map_err(Halt::Written)
}

/// Writes an event as `JackcessExtractor` writes what it stands for.
fn write(event: Event, xhtml: &mut Xhtml) -> Result<(), Failure> {
    match event {
        Event::Entry(entry) => {
            let metadata = xhtml.metadata_mut();
            match entry {
                Entry::Set(name, value) => metadata.set(name, value),
                Entry::AddPrefixed(prefix, name, value) => {
                    metadata.add_prefixed(prefix, &name, value)
                }
                Entry::AddMulti(name, value) => {
                    crate::formats::ole2::add_multi(metadata, name, &value)
                }
            }
        }
        Event::Table { name, headers } => {
            xhtml.start_element_with("table", &[("name", &name)])?;
            xhtml.start_element("thead")?;
            xhtml.start_element("tr")?;
            for header in &headers {
                xhtml.start_element("th")?;
                xhtml.characters(header)?;
                xhtml.end_element("th")?;
            }
            xhtml.end_element("tr")?;
            xhtml.end_element("thead")?;
            xhtml.start_element("tbody")?;
        }
        Event::Row(cells) => {
            xhtml.start_element("tr")?;
            for cell in cells {
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
        Event::TableEnd => {
            xhtml.end_element("tbody")?;
            xhtml.end_element("table")?;
        }
        Event::Query(sql) => {
            xhtml.start_element_with("div", &[("type", "sqlQuery")])?;
            xhtml.characters(&sql)?;
            xhtml.end_element("div")?;
        }
    }
    Ok(())
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

/// Why the port does not read a database: what of it the port does not read.
fn unread(what: String) -> Decline<'static> {
    Decline::Unread(format!("{what}, which the port does not read"))
}

/// What Tika fails with where Jackcess, or Tika's extractor over it, throws `exception`
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
/// any, twice: once to its end as far as deciding whether it is extracted, its events let go,
/// so that what Jackcess throws is known before anything is written; then as it is written,
/// each row handed on as it is read. What Jackcess throws opening the file, before it reads a
/// page past the first, the parse fails with as Tika does. What it throws reading the
/// database the port reads it to fail with, but Jackcess may throw on a damaged database
/// elsewhere and otherwise: such a reading is recorded as differing from Tika's.
fn prepare<'a>(input: &'a [u8], configs: &'a ParserConfigs) -> Result<Prepared<'a>, Decline<'a>> {
    let password = configs.password.as_ref().map_or("", Password::expose);
    let pages = Database::pages(input, password).map_err(|thrown| match thrown {
        Thrown::Exception(exception) => Decline::Fails(failure(&exception, UNKNOWN_DATABASE)),
        Thrown::Unported(what) => unread(what),
        Thrown::Unreadable(what) => Decline::Fails(Error::ParseError(what)),
    })?;
    let refused = pages.refused();
    let planned = Database::open(pages)
        .map_err(Halt::Thrown)
        .and_then(|database| {
            let difference = extract(&database, configs, &mut |_| Ok::<(), Infallible>(()))?;
            Ok((database, difference))
        });
    match planned {
        Ok((database, difference)) => unless_different(
            Box::new(move |xhtml| {
                mark_password_used(xhtml.metadata_mut(), configs);
                extract(&database, configs, &mut |event| write(event, xhtml))
                    .map(|_| ())
                    .map_err(|halt| match halt {
                        Halt::Written(failure) => failure,
                        Halt::Thrown(thrown) => Failure::Extraction(Error::ParseError(format!(
                            "the database reads otherwise the second time it is read: {thrown:?}"
                        ))),
                    })
            }),
            refused.or(difference),
        ),
        Err(Halt::Written(never)) => match never {},
        Err(Halt::Thrown(Thrown::Unported(what))) => Err(unread(what)),
        Err(Halt::Thrown(Thrown::Unreadable(what))) => Err(Decline::Fails(Error::ParseError(what))),
        Err(Halt::Thrown(Thrown::Exception(exception))) => Err(Decline::Differs {
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

/// Reads `database` as `JackcessExtractor.parse` does, its events handed to `sink` as they are
/// read: and where the native reading differs from Tika's, if it does.
fn extract<E>(
    database: &Database<'_>,
    configs: &ParserConfigs,
    sink: &mut Sink<'_, E>,
) -> Result<Option<Difference>, Halt<E>> {
    let add_all = |maps: PropertyMaps, prefix: &'static str, sink: &mut Sink<'_, E>| {
        for property in maps.default_map().iter() {
            let value = text(property.value.as_ref(), property.data_type)?;
            emit(
                sink,
                Event::Entry(Entry::AddPrefixed(prefix, property.name.clone(), value)),
            )?;
        }
        Ok::<_, Halt<E>>(())
    };
    add_all(database.database_properties()?, PROPERTY_PREFIX, sink)?;
    add_all(
        database.user_defined_properties()?,
        USER_PROPERTY_PREFIX,
        sink,
    )?;
    summary(&database.summary_properties()?.default_map(), sink)?;
    let mut difference = None;
    for info in database.tables()? {
        let table = database.load_table(&info)?;
        let read_otherwise = table_rows(database, &table, info.page, configs, sink)?;
        difference = difference.or(read_otherwise);
    }
    for definition in database.queries()? {
        let sql = match query::sql(&definition)? {
            Sql::Text(text) => text,
            Sql::Unsupported => "unsupported query type".to_owned(),
        };
        emit(sink, Event::Query(sql))?;
    }
    Ok(difference)
}

/// The summary properties: the title, author and company as Tika's properties, the others under
/// their own names.
fn summary<E>(map: &PropertyMap, sink: &mut Sink<'_, E>) -> Result<(), Halt<E>> {
    let mut found = Vec::new();
    if let Some(title) = map.get(TITLE_PROPERTY) {
        let value = text(title.value.as_ref(), title.data_type)?;
        emit(sink, Event::Entry(Entry::Set(TITLE, value)))?;
        found.push(title.name.clone());
    }
    if let Some(author) = map
        .get(AUTHOR_PROPERTY)
        .filter(|author| author.value.is_some())
    {
        let value = text(author.value.as_ref(), author.data_type)?;
        emit(sink, Event::Entry(Entry::AddMulti(CREATOR, value)))?;
        found.push(author.name.clone());
    }
    if let Some(company) = map.get(COMPANY_PROPERTY) {
        let value = text(company.value.as_ref(), company.data_type)?;
        emit(sink, Event::Entry(Entry::Set(COMPANY, value)))?;
        found.push(company.name.clone());
    }
    for property in map.iter() {
        if !found.contains(&property.name) {
            let value = text(property.value.as_ref(), property.data_type)?;
            emit(
                sink,
                Event::Entry(Entry::AddPrefixed(
                    SUMMARY_PREFIX,
                    property.name.clone(),
                    value,
                )),
            )?;
        }
    }
    Ok(())
}

/// A table's headers and rows as `JackcessExtractor` writes them, each column with its own
/// values, where Tika gives two columns of one name both the later one's (#122): handed to
/// `sink` a row at a time as they are read; and where the native reading differs from Tika's,
/// if it does.
fn table_rows<E>(
    database: &Database<'_>,
    table: &table::Table,
    page: i32,
    configs: &ParserConfigs,
    sink: &mut Sink<'_, E>,
) -> Result<Option<Difference>, Halt<E>> {
    let mut difference = None;
    let columns = &table.columns;
    emit(
        sink,
        Event::Table {
            name: table.name.clone(),
            headers: columns.iter().map(|column| column.name.clone()).collect(),
        },
    )?;
    let mut rich: Option<Vec<bool>> = None;
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
                    Some(Value::Binary(bytes)) => match ole::read(bytes, configs)? {
                        Ole::Nothing => Cell::Text(String::new()),
                        Ole::Link(path) => Cell::Text(path),
                        Ole::Resource(document) => Cell::Resource(document),
                        Ole::Storage(document) => {
                            difference = difference.or_else(|| document.difference());
                            Cell::Storage(document)
                        }
                    },
                    Some(other) => {
                        return Err(
                            Thrown::runtime(CLASS_CAST, format!("{other:?} as byte[]")).into()
                        )
                    }
                },
                DataType::Binary => match value {
                    None => Cell::Text(String::new()),
                    Some(Value::Binary(bytes)) => {
                        Cell::Resource(Embedded::owned(bytes.clone(), TikaMetadata::new()))
                    }
                    Some(other) => {
                        return Err(
                            Thrown::runtime(CLASS_CAST, format!("{other:?} as byte[]")).into()
                        )
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
        emit(sink, Event::Row(cells))?;
    }
    emit(sink, Event::TableEnd)?;
    Ok(difference)
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
            Thrown::unported(format!("rich text the HTML parser does not read: {reason}"))
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
