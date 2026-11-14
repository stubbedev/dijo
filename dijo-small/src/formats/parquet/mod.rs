//! Apache Parquet files, which Tika does not read, read for their data as the data set parsers
//! write theirs: a table of a row of the leaf columns' paths, then a row for each row of the
//! file, row group by row group. Every column is written, as Tika's tabular parsers write every
//! cell ([`cell`] writes each value): text as it is, numbers, decimals, dates, times and
//! timestamps by their logical types, a repeated column's values of a row apart by spaces, a
//! null as an empty cell, and binary data that is not text not at all. The writer
//! (`created_by`) is the `xmp:CreatorTool`, the row count, the column count and each column's
//! path are the `database:` keys the SAS parser sets, and each key-value pair of the footer is
//! a `parquet:` key, a key of no value an empty one. The pages' and row groups' own metadata
//! (statistics, page indexes, bloom filters) is not read.
//!
//! The footer is Thrift in its compact protocol ([`thrift`]), read whole, its nesting and sizes
//! bounded, and every row group's column chunks checked before anything is written ([`footer`],
//! [`schema`]). The rows are then read a row group at a time, all its column chunks side by
//! side, a page of each at a time ([`column`]), in memory bounded by the pages, at most
//! [`MAX_DECODED`](crate::compress::MAX_DECODED) bytes of them at once: data pages of
//! version 1 and 2, levels in the RLE/bit-packing hybrid or `BIT_PACKED` ([`levels`]), values
//! in every encoding Parquet defines ([`values`]), compressed with Snappy, gzip, Brotli,
//! Zstandard or LZ4 ([`codec`]). A file whose footer is encrypted fails as encrypted, as does one
//! of encrypted columns; a column chunk compressed with LZO, or kept in another file, is not
//! read.

mod bits;
mod cell;
mod codec;
mod column;
mod footer;
mod levels;
mod schema;
mod thrift;
mod values;

use std::fmt;

use codec::Compression;
use column::{Chunk, ColumnReader, Held, Interrupted};
use footer::{FileMetaData, RowGroup};
use schema::{Column, Physical};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::crypt::encrypted_document;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};

const NAME: &str = "dijo::ParquetParser";
const MAGIC: &[u8; 4] = b"PAR1";
const ENCRYPTED_MAGIC: &[u8; 4] = b"PARE";
/// The magic at the start, and the footer's length and the magic at the end.
const FRAME: usize = 12;
const KEY_PREFIX: &str = "parquet:";

/// Why a Parquet file, or a part of it, is not read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Problem {
    /// It is not well-formed, as this says.
    Malformed(String),
    /// It holds what the parser does not read, as this says.
    Unsupported(String),
    /// It is encrypted, as this says.
    Encrypted(&'static str),
}

impl Problem {
    /// The problem, found in `what`.
    fn within(self, what: &str) -> Self {
        match self {
            Self::Malformed(reason) => Self::Malformed(format!("{what}: {reason}")),
            Self::Unsupported(reason) => Self::Unsupported(format!("{what}: {reason}")),
            Self::Encrypted(reason) => Self::Encrypted(reason),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(reason) | Self::Unsupported(reason) => f.write_str(reason),
            Self::Encrypted(reason) => f.write_str(reason),
        }
    }
}

impl From<Problem> for Decline<'_> {
    fn from(problem: Problem) -> Self {
        match problem {
            Problem::Malformed(reason) => {
                Decline::Fails(Error::ParseError(format!("Parquet file: {reason}")))
            }
            Problem::Unsupported(reason) => Decline::Unread(format!("a Parquet file of {reason}")),
            Problem::Encrypted(reason) => Decline::Fails(encrypted_document(reason)),
        }
    }
}

/// The text of Apache Parquet files.
pub struct ParquetParser;

impl Parser for ParquetParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-parquet"]
    }

    /// The footer and a page of each column: the rows are read a page at a time.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    /// Reads the footer and checks every row group's column chunks, failing a file that is not
    /// Parquet, is malformed or is encrypted, before anything is written.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let file = File::open(input)?;
        Ok(Box::new(move |xhtml| file.write(pages, xhtml)))
    }
}

/// A Parquet file whose footer and row groups are checked.
#[derive(Debug)]
struct File<'a> {
    input: &'a [u8],
    footer: &'a [u8],
    metadata: FileMetaData,
    columns: Vec<Column>,
}

impl<'a> File<'a> {
    fn open(input: &'a [u8]) -> Result<Self, Problem> {
        if input.len() < FRAME || !input.starts_with(MAGIC) {
            return Err(Problem::Malformed(
                "it does not start with the magic PAR1".to_owned(),
            ));
        }
        let tail = &input[input.len() - 8..];
        if tail[4..] == ENCRYPTED_MAGIC[..] {
            return Err(Problem::Encrypted(
                "a Parquet file whose footer is encrypted",
            ));
        }
        if tail[4..] != MAGIC[..] {
            return Err(Problem::Malformed(
                "it does not end with the magic PAR1".to_owned(),
            ));
        }
        let length = u32::from_le_bytes(tail[..4].try_into().expect("four bytes")) as usize;
        let footer_end = input.len() - 8;
        let footer_start = footer_end
            .checked_sub(length)
            .filter(|&start| start >= MAGIC.len())
            .ok_or_else(|| {
                Problem::Malformed(format!(
                    "a footer of {length} bytes in a file of {} bytes",
                    input.len()
                ))
            })?;
        let footer = &input[footer_start..footer_end];
        let metadata =
            FileMetaData::read(footer).map_err(|problem| problem.within("the footer"))?;
        if metadata.encrypted_columns {
            return Err(Problem::Encrypted("a Parquet file of encrypted columns"));
        }
        let columns =
            schema::columns(&metadata.schema).map_err(|problem| problem.within("the schema"))?;
        if metadata.num_rows < 0 {
            return Err(Problem::Malformed(format!(
                "the file's row count is {}",
                metadata.num_rows
            )));
        }
        let file = Self {
            input,
            footer,
            metadata,
            columns,
        };
        for index in 0..file.metadata.row_groups.len() {
            file.row_group(index)?;
        }
        Ok(file)
    }

    /// The row group at `index`, its row count and the chunk of each column, checked.
    fn row_group(&self, index: usize) -> Result<(u64, Vec<Chunk>), Problem> {
        let within = |problem: Problem| problem.within(&format!("row group {index}"));
        let group = RowGroup::read(self.footer, self.metadata.row_groups[index]).map_err(within)?;
        let rows = u64::try_from(group.num_rows).map_err(|_| {
            within(Problem::Malformed(format!(
                "its row count is {}",
                group.num_rows
            )))
        })?;
        if group.columns.len() != self.columns.len() {
            return Err(within(Problem::Malformed(format!(
                "it has {} column chunks, for {} columns",
                group.columns.len(),
                self.columns.len()
            ))));
        }
        let data_end = self.input.len() - (self.footer.len() + 8);
        let chunks = group
            .columns
            .iter()
            .zip(&self.columns)
            .map(|(chunk, column)| {
                Self::chunk(chunk, column.physical, data_end)
                    .map_err(|problem| within(problem.within(&format!("column {}", column.path))))
            })
            .collect::<Result<_, _>>()?;
        Ok((rows, chunks))
    }

    /// Where the pages of `chunk`, of a column of `physical`, are, before `data_end`.
    fn chunk(
        chunk: &footer::ColumnChunk,
        physical: Physical,
        data_end: usize,
    ) -> Result<Chunk, Problem> {
        if chunk.encrypted {
            return Err(Problem::Encrypted("a Parquet file of encrypted columns"));
        }
        if chunk.elsewhere {
            return Err(Problem::Unsupported(
                "a column chunk kept in another file".to_owned(),
            ));
        }
        let meta = chunk
            .meta
            .as_ref()
            .ok_or_else(|| Problem::Malformed("the column chunk has no metadata".to_owned()))?;
        if meta.physical != physical.code() {
            return Err(Problem::Malformed(format!(
                "the column chunk is of the type {}, the column of {physical:?}",
                meta.physical
            )));
        }
        let codec =
            match meta.compression {
                Compression::Decoded(codec) => codec,
                Compression::Lzo => return Err(Problem::Unsupported(
                    "a column chunk compressed with LZO, which the native parser does not decode"
                        .to_owned(),
                )),
            };
        let start = match meta.dictionary_page_offset {
            Some(offset) if offset > 0 && offset < meta.data_page_offset => offset,
            _ => meta.data_page_offset,
        };
        let range = usize::try_from(start)
            .ok()
            .zip(usize::try_from(meta.total_compressed_size).ok())
            .and_then(|(start, length)| Some(start..start.checked_add(length)?))
            .filter(|range| range.start >= MAGIC.len() && range.end <= data_end)
            .ok_or_else(|| {
                Problem::Malformed(format!(
                    "the column chunk of {} bytes at byte {start} is not between the magic and \
                     the footer",
                    meta.total_compressed_size
                ))
            })?;
        Ok(Chunk { range, codec })
    }

    fn write(self, pages: Pages<'_>, xhtml: &mut Xhtml<'_>) -> Result<(), Failure> {
        self.write_metadata(xhtml.metadata_mut());
        xhtml.start_element("table")?;
        xhtml.start_element("tr")?;
        for column in &self.columns {
            xhtml.start_element("th")?;
            xhtml.characters(&column.path)?;
            xhtml.end_element("th")?;
        }
        xhtml.end_element("tr")?;
        let paging = Paging::new(pages);
        let mut text = String::new();
        for index in 0..self.metadata.row_groups.len() {
            let failed = |problem: Problem| {
                Failure::Malformed(format!("Parquet file: row group {index}: {problem}"))
            };
            let (rows, chunks) = self.row_group(index).map_err(failed)?;
            let mut readers: Vec<ColumnReader<'_, '_>> = self
                .columns
                .iter()
                .zip(chunks)
                .map(|(column, chunk)| ColumnReader::new(column, self.input, chunk))
                .collect();
            let mut held = Held::default();
            let in_column = |column: &Column, problem: Problem| {
                failed(problem.within(&format!("column {}", column.path)))
            };
            for _ in 0..rows {
                xhtml.start_element("tr")?;
                for (reader, column) in readers.iter_mut().zip(&self.columns) {
                    xhtml.start_element("td")?;
                    match reader.write_row(&mut held, &paging, &mut text, xhtml) {
                        Ok(()) => {}
                        Err(Interrupted::Output(stop)) => return Err(stop.into()),
                        Err(Interrupted::Read(problem)) => return Err(in_column(column, problem)),
                    }
                    xhtml.end_element("td")?;
                }
                xhtml.end_element("tr")?;
            }
            for (reader, column) in readers.iter_mut().zip(&self.columns) {
                reader
                    .finish(&mut held, &paging)
                    .map_err(|problem| in_column(column, problem))?;
            }
        }
        xhtml.end_element("table")?;
        Ok(())
    }

    /// The writer, the row count, the columns' paths and the footer's key-value pairs.
    fn write_metadata(&self, metadata: &mut TikaMetadata) {
        if let Some(created_by) = &self.metadata.created_by {
            metadata.set("xmp:CreatorTool", created_by.as_str());
        }
        metadata.set("database:row-count", self.metadata.num_rows.to_string());
        metadata.set("database:column-count", self.columns.len().to_string());
        for column in &self.columns {
            metadata.add("database:column-name", column.path.as_str());
        }
        for (key, value) in &self.metadata.key_value {
            metadata.add_prefixed(KEY_PREFIX, key, value.as_deref().unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod tests;
