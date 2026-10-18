//! SAS7BDAT data sets, as Tika's `SAS7BDATParser` reads them through parso: the file's
//! properties and column names as metadata, then a table of its columns and rows, each cell
//! written as parso's `DataWriterUtil` writes it.
//!
//! The file is read as parso's `SasFileParser` reads it ([`reader`]), its compressed rows
//! decompressed as parso decompresses them ([`decompress`]), and each cell read and written as
//! parso reads and writes it ([`values`]). A file whose text is in a charset this port does not
//! decode is left to Tika.

mod decompress;
mod reader;
mod values;

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java::time::format_instant;
use reader::{Column, Decoder, Reader};
use values::{Value, Writer};

const MEDIA_TYPE: &str = "application/x-sas-data";

/// Tika's `SAS7BDATParser`.
pub struct Sas7bdatParser;

impl Parser for Sas7bdatParser {
    fn name(&self) -> &'static str {
        "extractous::SAS7BDATParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.sas.SAS7BDATParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Reads the file's metadata as parso does, failing where parso throws, and leaves a file
    /// whose text is in a charset this port does not decode to Tika.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let named = input
            .get(reader::ENCODING_OFFSET)
            .and_then(|&byte| reader::encoding_name(byte));
        let decoder = match named {
            Some(name) => Some(Decoder::named(name).ok_or_else(|| {
                Declined::Deferred(format!(
                    "text in {name}, which the native parser does not decode"
                ))
            })?),
            None => None,
        };
        let tika_name = self.tika_name();
        let mut reader = Reader::open(input, decoder)
            .map_err(|thrown| Declined::Fails(thrown.into_error(tika_name)))?;
        Ok(Box::new(move |xhtml| {
            write_metadata(&reader, xhtml.metadata_mut());
            write_table(&mut reader, xhtml).map_err(|failure| match failure {
                Written::Stopped(failure) => failure,
                Written::Thrown(thrown) => {
                    Failure::Malformed(thrown.into_error(tika_name).to_string())
                }
            })
        }))
    }
}

/// The metadata `SAS7BDATParser` takes from the file's properties and columns, each property
/// parso has not read left unset.
fn write_metadata(reader: &Reader, metadata: &mut TikaMetadata) {
    let properties = &reader.properties;
    let fields = [
        ("Content-Type", Some(MEDIA_TYPE.to_owned())),
        ("dc:title", properties.name.clone()),
        ("dcterms:created", properties.created.map(format_instant)),
        ("dcterms:modified", properties.modified.map(format_instant)),
        (
            "xmpTPg:NPages",
            Some((properties.page_count as i32).to_string()),
        ),
        (
            "database:column-count",
            Some((properties.columns_count as i32).to_string()),
        ),
        (
            "database:row-count",
            Some((properties.row_count as i32).to_string()),
        ),
        ("Content-Encoding", properties.encoding.map(str::to_owned)),
        (
            "extended-properties:Application",
            properties.server_type.clone(),
        ),
        (
            "extended-properties:AppVersion",
            properties.sas_release.clone(),
        ),
        (
            "machine:architectureBits",
            Some(if properties.u64 { "64" } else { "32" }.to_owned()),
        ),
        (
            "machine:endian",
            Some(
                if properties.endianness == 1 {
                    "Little"
                } else {
                    "Big"
                }
                .to_owned(),
            ),
        ),
    ];
    for (name, value) in fields {
        match value {
            Some(value) => metadata.set(name, value),
            None => metadata.remove(name),
        }
    }
    for column in &reader.columns {
        metadata.add("database:column-name", heading(column));
    }
}

/// A column's label, or its name where it has none.
fn heading(column: &Column) -> &str {
    if column.label.is_empty() {
        &column.name
    } else {
        &column.label
    }
}

/// Why the table was not written whole: the output stopped, or parso threw on a row.
enum Written {
    Stopped(Failure),
    Thrown(Thrown),
}

impl From<Stop> for Written {
    fn from(stop: Stop) -> Self {
        Self::Stopped(Failure::from(stop))
    }
}

impl From<Thrown> for Written {
    fn from(thrown: Thrown) -> Self {
        Self::Thrown(thrown)
    }
}

/// The data set's name as a heading, then a table of a row of column headings and a row for
/// each row of data, up to the first row parso reads none of.
fn write_table(reader: &mut Reader, xhtml: &mut Xhtml) -> Result<(), Written> {
    if let Some(name) = &reader.properties.name {
        xhtml.element("h1", name)?;
    }
    xhtml.start_element("table")?;
    xhtml.newline()?;
    xhtml.start_element("tr")?;
    for column in &reader.columns {
        xhtml.start_element_with("th", &[("title", column.name.as_str())])?;
        xhtml.characters(heading(column))?;
        xhtml.end_element("th")?;
    }
    xhtml.end_element("tr")?;
    xhtml.newline()?;
    let mut writer = Writer::default();
    while let Some(row) = reader.read_next()? {
        let texts = row_texts(&reader.columns, &row, &mut writer)?;
        xhtml.start_element("tr")?;
        for text in texts {
            xhtml.start_element("td")?;
            xhtml.characters(&text)?;
            xhtml.end_element("td")?;
        }
        xhtml.end_element("tr")?;
        xhtml.newline()?;
    }
    xhtml.end_element("table")?;
    Ok(())
}

/// `DataWriterUtil.getRowValues`: the text of each column's cell of `row`.
fn row_texts(
    columns: &[Column],
    row: &[Option<Value>],
    writer: &mut Writer,
) -> Result<Vec<String>, Thrown> {
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let cell = row.get(index).ok_or_else(|| {
                Thrown::Runtime(Exception::new(
                    "java.lang.ArrayIndexOutOfBoundsException",
                    format!("Index {index} out of bounds for length {}", row.len()),
                ))
            })?;
            let format = values::Format {
                column: column.id,
                name: &column.format.name,
                precision: column.format.precision,
            };
            writer.text(&format, cell.as_ref())
        })
        .collect()
}

#[cfg(test)]
mod tests;
