//! A column chunk read a row at a time: its pages read in turn, each decompressed into a buffer
//! the reader keeps, or read in place where it is not compressed, its levels and values decoded
//! as the rows ask for them. The dictionary page, if there is one, comes first.

use std::ops::Range;

use super::cell;
use super::codec::{decompress, Codec};
use super::footer::{Encoding, PageHeader, PageKind};
use super::levels::Levels;
use super::schema::{Column, Physical};
use super::values::{Dictionary, Values};
use super::Problem;
use crate::compress::MAX_DECODED;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::input::Paging;

/// What the readers of a row group's columns hold together, at most [`MAX_DECODED`] bytes.
#[derive(Debug, Default)]
pub struct Held(usize);

impl Held {
    /// Takes `to` bytes in place of `from`.
    fn change(&mut self, from: usize, to: usize) -> Result<(), Problem> {
        let held = self.0 - from + to;
        if held > MAX_DECODED {
            return Err(Problem::Unsupported(format!(
                "pages that decode into more than {MAX_DECODED} bytes at once"
            )));
        }
        self.0 = held;
        Ok(())
    }
}

/// Where a column chunk's pages are in the file, and how they are compressed.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub range: Range<usize>,
    pub codec: Codec,
}

/// Why a row was not written whole.
#[derive(Debug)]
pub enum Interrupted {
    Output(Stop),
    Read(Problem),
}

impl From<Stop> for Interrupted {
    fn from(stop: Stop) -> Self {
        Self::Output(stop)
    }
}

impl From<Problem> for Interrupted {
    fn from(problem: Problem) -> Self {
        Self::Read(problem)
    }
}

/// Where a page's bytes are.
#[derive(Debug, Clone)]
enum Source {
    /// In the file, uncompressed.
    Input(Range<usize>),
    /// In the reader's buffer, decompressed.
    Buffer,
}

/// The bytes of a page from `source`.
fn bytes<'s>(source: &Source, input: &'s [u8], buffer: &'s [u8]) -> &'s [u8] {
    match source {
        Source::Input(range) => &input[range.clone()],
        Source::Buffer => buffer,
    }
}

/// The data page being read.
#[derive(Debug)]
struct Page {
    source: Source,
    /// The levels not read yet.
    left: u32,
    repetition: Option<Levels>,
    definition: Option<Levels>,
    /// Where the values are and how they are encoded, which they are decoded by from the first
    /// one read.
    range: Range<usize>,
    encoding: Encoding,
    values: Option<Values>,
}

/// A column chunk being read.
#[derive(Debug)]
pub struct ColumnReader<'a, 'c> {
    column: &'c Column,
    input: &'a [u8],
    chunk: Chunk,
    /// Where the next page's header is.
    at: usize,
    buffer: Vec<u8>,
    dictionary: Option<Dictionary>,
    page: Option<Page>,
    /// The repetition level of the next value, read ahead to tell where a row ends.
    next_repetition: Option<u32>,
    data_pages: bool,
    held: usize,
}

impl<'a, 'c> ColumnReader<'a, 'c> {
    pub fn new(column: &'c Column, input: &'a [u8], chunk: Chunk) -> Self {
        Self {
            column,
            input,
            at: chunk.range.start,
            chunk,
            buffer: Vec::new(),
            dictionary: None,
            page: None,
            next_repetition: None,
            data_pages: false,
            held: 0,
        }
    }

    /// Writes the cell of the next row: its values, those of a repeated column apart by spaces.
    pub fn write_row(
        &mut self,
        held: &mut Held,
        paging: &Paging<'_>,
        text: &mut String,
        xhtml: &mut Xhtml<'_>,
    ) -> Result<(), Interrupted> {
        let repetition = self.entry(held, paging, text)?.ok_or_else(|| {
            Problem::Malformed("the column ends before its row group's rows do".to_owned())
        })?;
        if repetition != 0 {
            return Err(Problem::Malformed(format!(
                "a row starts at the repetition level {repetition}"
            ))
            .into());
        }
        let mut written = !text.is_empty();
        xhtml.characters(text)?;
        while self.column.max_repetition > 0 && self.continues(held, paging)? {
            self.entry(held, paging, text)?;
            if text.is_empty() {
                continue;
            }
            if written {
                xhtml.characters(" ")?;
            }
            xhtml.characters(text)?;
            written = true;
        }
        Ok(())
    }

    /// Fails if the column holds values past its row group's rows.
    pub fn finish(&mut self, held: &mut Held, paging: &Paging<'_>) -> Result<(), Problem> {
        if self.ready(held, paging)? {
            return Err(Problem::Malformed(
                "the column holds more values than its row group's rows".to_owned(),
            ));
        }
        Ok(())
    }

    /// Whether the next value continues the row of the last one.
    fn continues(&mut self, held: &mut Held, paging: &Paging<'_>) -> Result<bool, Problem> {
        if !self.ready(held, paging)? {
            return Ok(false);
        }
        let repetition = match self.next_repetition {
            Some(repetition) => repetition,
            None => {
                let page = self.page.as_mut().expect("a ready reader has a page");
                let levels = page.repetition.as_mut().ok_or_else(|| {
                    Problem::Malformed(
                        "a repeated column's page of no repetition levels".to_owned(),
                    )
                })?;
                let page_bytes = bytes(&page.source, self.input, &self.buffer);
                let repetition = levels.next(page_bytes, self.column.max_repetition)?;
                self.next_repetition = Some(repetition);
                repetition
            }
        };
        Ok(repetition > 0)
    }

    /// Reads the next level entry, its value's text into `text`, which a null leaves empty: its
    /// repetition level, or none past the column chunk's last page.
    fn entry(
        &mut self,
        held: &mut Held,
        paging: &Paging<'_>,
        text: &mut String,
    ) -> Result<Option<u32>, Problem> {
        text.clear();
        if !self.ready(held, paging)? {
            return Ok(None);
        }
        let column = self.column;
        let page = self.page.as_mut().expect("a ready reader has a page");
        let page_bytes = bytes(&page.source, self.input, &self.buffer);
        let repetition = match (self.next_repetition.take(), &mut page.repetition) {
            (Some(repetition), _) => repetition,
            (None, Some(levels)) => levels.next(page_bytes, column.max_repetition)?,
            (None, None) => 0,
        };
        let definition = match &mut page.definition {
            Some(levels) => levels.next(page_bytes, column.max_definition)?,
            None => column.max_definition,
        };
        page.left -= 1;
        if definition < column.max_definition {
            return Ok(Some(repetition));
        }
        let values = match &mut page.values {
            Some(values) => values,
            slot @ None => slot.insert(Values::new(
                page_bytes,
                page.range.clone(),
                page.encoding,
                column.physical,
                self.dictionary.is_some(),
            )?),
        };
        let raw = values.next(page_bytes, column.physical, self.dictionary.as_ref())?;
        cell::write(column.kind, raw, text)?;
        Ok(Some(repetition))
    }

    /// Whether a page with levels left is read, reading on to the next where the last is done.
    fn ready(&mut self, held: &mut Held, paging: &Paging<'_>) -> Result<bool, Problem> {
        loop {
            if self.page.as_ref().is_some_and(|page| page.left > 0) {
                return Ok(true);
            }
            if !self.read_page(held, paging)? {
                return Ok(false);
            }
        }
    }

    /// Takes the bytes of the buffer and the dictionary, the buffer grown to `buffer` bytes and
    /// the dictionary to `dictionary`.
    fn hold(&mut self, held: &mut Held, buffer: usize, dictionary: usize) -> Result<(), Problem> {
        let to = buffer.max(self.buffer.capacity()) + dictionary;
        held.change(self.held, to)?;
        self.held = to;
        Ok(())
    }

    /// The buffer, `length` bytes long.
    fn fill(&mut self, held: &mut Held, length: usize) -> Result<(), Problem> {
        let dictionary = self.dictionary.as_ref().map_or(0, Dictionary::held);
        self.hold(held, length, dictionary)?;
        self.buffer.clear();
        self.buffer.resize(length, 0);
        Ok(())
    }

    /// Reads the next data page, after the dictionary page and index pages before it: whether
    /// there is one.
    fn read_page(&mut self, held: &mut Held, paging: &Paging<'_>) -> Result<bool, Problem> {
        loop {
            let end = self.chunk.range.end;
            if self.at >= end {
                self.page = None;
                return Ok(false);
            }
            let start = self.at;
            let (header, length) = PageHeader::read(&self.input[start..end])
                .map_err(|problem| problem.within(&format!("the page header at byte {start}")))?;
            let body = start + length;
            let body_end = body
                .checked_add(header.compressed)
                .filter(|&body_end| body_end <= end)
                .ok_or_else(|| {
                    Problem::Malformed(format!(
                        "the page at byte {start} runs past its column chunk's end at byte {end}"
                    ))
                })?;
            self.at = body_end;
            paging.read(body_end - start);
            let body = body..body_end;
            match header.kind {
                PageKind::Other => {}
                PageKind::Dictionary {
                    num_values,
                    encoding,
                } => self.read_dictionary(held, body, header.uncompressed, num_values, encoding)?,
                PageKind::Data {
                    num_values,
                    encoding,
                    definition,
                    repetition,
                } => {
                    self.data_pages = true;
                    let source = if self.chunk.codec == Codec::Uncompressed {
                        self.in_place(&body, header.uncompressed)?
                    } else {
                        self.fill(held, header.uncompressed)?;
                        decompress(self.chunk.codec, &self.input[body], &mut self.buffer)?;
                        Source::Buffer
                    };
                    let page_bytes = bytes(&source, self.input, &self.buffer);
                    let mut at = 0;
                    let column = self.column;
                    let repetition_levels = (column.max_repetition > 0)
                        .then(|| {
                            Levels::v1(
                                page_bytes,
                                &mut at,
                                repetition,
                                column.max_repetition,
                                num_values,
                            )
                        })
                        .transpose()?;
                    let definition_levels = (column.max_definition > 0)
                        .then(|| {
                            Levels::v1(
                                page_bytes,
                                &mut at,
                                definition,
                                column.max_definition,
                                num_values,
                            )
                        })
                        .transpose()?;
                    let values = at..page_bytes.len();
                    self.start_page(
                        source,
                        num_values,
                        repetition_levels,
                        definition_levels,
                        values,
                        encoding,
                    );
                    return Ok(true);
                }
                PageKind::DataV2 {
                    num_values,
                    encoding,
                    definition_length,
                    repetition_length,
                    compressed,
                } => {
                    self.data_pages = true;
                    let levels = repetition_length as usize + definition_length as usize;
                    if levels > body.len() || levels > header.uncompressed {
                        return Err(Problem::Malformed(format!(
                            "the page at byte {start} has levels of {levels} bytes, more than \
                             it holds"
                        )));
                    }
                    let source = if compressed && self.chunk.codec != Codec::Uncompressed {
                        self.fill(held, header.uncompressed)?;
                        let (head, tail) = self.buffer.split_at_mut(levels);
                        head.copy_from_slice(&self.input[body.start..body.start + levels]);
                        decompress(
                            self.chunk.codec,
                            &self.input[body.start + levels..body.end],
                            tail,
                        )?;
                        Source::Buffer
                    } else {
                        self.in_place(&body, header.uncompressed)?
                    };
                    let length = bytes(&source, self.input, &self.buffer).len();
                    let repetition_end = repetition_length as usize;
                    let column = self.column;
                    let repetition_levels = (column.max_repetition > 0)
                        .then(|| Levels::v2(0..repetition_end, column.max_repetition));
                    let definition_levels = (column.max_definition > 0)
                        .then(|| Levels::v2(repetition_end..levels, column.max_definition));
                    self.start_page(
                        source,
                        num_values,
                        repetition_levels,
                        definition_levels,
                        levels..length,
                        encoding,
                    );
                    return Ok(true);
                }
            }
        }
    }

    /// The bytes of an uncompressed page in the file, which are as many as it decodes into.
    fn in_place(&self, body: &Range<usize>, uncompressed: usize) -> Result<Source, Problem> {
        if body.len() != uncompressed {
            return Err(Problem::Malformed(format!(
                "an uncompressed page of {} bytes that decodes into {uncompressed}",
                body.len()
            )));
        }
        Ok(Source::Input(body.clone()))
    }

    fn start_page(
        &mut self,
        source: Source,
        left: u32,
        repetition: Option<Levels>,
        definition: Option<Levels>,
        range: Range<usize>,
        encoding: Encoding,
    ) {
        self.next_repetition = None;
        self.page = Some(Page {
            source,
            left,
            repetition,
            definition,
            range,
            encoding,
            values: None,
        });
    }

    fn read_dictionary(
        &mut self,
        held: &mut Held,
        body: Range<usize>,
        uncompressed: usize,
        num_values: u32,
        encoding: Encoding,
    ) -> Result<(), Problem> {
        if self.dictionary.is_some() || self.data_pages {
            return Err(Problem::Malformed(
                "a dictionary page after the column chunk's first page".to_owned(),
            ));
        }
        if !matches!(encoding, Encoding::Plain | Encoding::PlainDictionary) {
            return Err(Problem::Malformed(format!(
                "a dictionary page encoded in {encoding:?}"
            )));
        }
        let starts = if self.column.physical == Physical::ByteArray {
            uncompressed
        } else {
            0
        };
        self.hold(held, 0, uncompressed + starts)?;
        let mut bytes = vec![0; uncompressed];
        decompress(self.chunk.codec, &self.input[body], &mut bytes)?;
        let dictionary = Dictionary::new(bytes, self.column.physical, num_values)?;
        let exact = dictionary.held();
        self.dictionary = Some(dictionary);
        self.hold(held, 0, exact)
    }
}
