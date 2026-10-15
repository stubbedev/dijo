//! POI's `RecordFactoryInputStream`: the records of a workbook stream as `HSSFEventFactory` hands
//! them to listeners. RK and MulRK records become number records, DBCell records are dropped,
//! the drawing group records after the first and their continuations join it, and a `CONTINUE`
//! record goes where the record before it takes it, failing after one that takes none.

use std::collections::VecDeque;

use super::records::{read_record, sid, CellRef, Record};
use super::stream::{thrown, Read, RecordStream};

/// What a record before a `CONTINUE` record makes of it, by the class of POI's `_lastRecord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Last {
    /// An object or text object record, whose continuation the drawing record takes.
    ObjectOrText,
    /// A drawing group record, which joins its continuation.
    DrawingGroup,
    /// A drawing record, after which the continuation is handed on.
    Drawing,
    /// A record POI does not parse, after which the continuation is handed on.
    Unknown,
    /// Any other record, after which a continuation fails.
    Other,
}

/// A record handed to the listeners: a record, or a drawing group by its index among the
/// workbook's, whose data later records may still join.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Event {
    Record { sid: u16, record: Record },
    DrawingGroup(usize),
}

/// The records of a workbook stream.
pub(super) struct Records<'a> {
    stream: RecordStream<'a>,
    unread: VecDeque<(u16, Record)>,
    last: Last,
    bof_depth: i32,
    last_was_eof_level_zero: bool,
    /// The data of each drawing group, joined with what followed it.
    pub(super) drawing_groups: Vec<Vec<u8>>,
    /// Whether a `FILEPASS` record follows the first BOF record, after which POI decrypts.
    pub(super) encrypted: bool,
}

fn last_of(record: &Record) -> Last {
    match record {
        Record::Obj | Record::TextObject(_) => Last::ObjectOrText,
        Record::DrawingGroup(_) => Last::DrawingGroup,
        Record::Drawing(_) => Last::Drawing,
        Record::Unknown => Last::Unknown,
        _ => Last::Other,
    }
}

impl<'a> Records<'a> {
    /// `new RecordFactoryInputStream(stream, false)`: the first records read ahead, to find
    /// whether the workbook is encrypted, whose records this port does not read on.
    pub(super) fn new(data: &'a [u8]) -> Read<Self> {
        let mut stream = RecordStream::new(data);
        let mut unread = VecDeque::new();
        stream.next_record()?;
        let first = read_record(&mut stream)?;
        let has_bof = matches!(first, Record::Bof { .. });
        let mut last = first.clone();
        unread.push_back((stream.sid(), first));
        if has_bof && stream.has_next_record()? {
            stream.next_record()?;
            last = read_record(&mut stream)?;
            unread.push_back((stream.sid(), last.clone()));
            if last == Record::WriteProtect && stream.has_next_record()? {
                stream.next_record()?;
                last = read_record(&mut stream)?;
                unread.push_back((stream.sid(), last.clone()));
            }
            if last == Record::Eof {
                return thrown("Nothing between BOF and EOF");
            }
        }
        Ok(Records {
            stream,
            encrypted: last == Record::FilePass,
            unread,
            last: last_of(&last),
            bof_depth: i32::from(has_bof),
            last_was_eof_level_zero: false,
            drawing_groups: Vec::new(),
        })
    }

    /// `nextRecord`: the next record handed to the listeners, `None` after the last.
    pub(super) fn next(&mut self) -> Read<Option<Event>> {
        if let Some((sid, record)) = self.unread.pop_front() {
            return Ok(Some(self.event(sid, record)));
        }
        loop {
            if !self.stream.has_next_record()? {
                return Ok(None);
            }
            if self.last_was_eof_level_zero && self.stream.next_sid() != Some(sid::BOF) {
                return Ok(None);
            }
            self.stream.next_record()?;
            if let Some((sid, record)) = self.read_next()? {
                return Ok(Some(self.event(sid, record)));
            }
        }
    }

    fn event(&mut self, sid: u16, record: Record) -> Event {
        match record {
            Record::DrawingGroup(data) => {
                self.drawing_groups.push(data);
                Event::DrawingGroup(self.drawing_groups.len() - 1)
            }
            record => Event::Record { sid, record },
        }
    }

    /// `readNextRecord`: a record with its type.
    fn read_next(&mut self) -> Read<Option<(u16, Record)>> {
        let record_sid = self.stream.sid();
        let record = read_record(&mut self.stream)?;
        self.last_was_eof_level_zero = false;
        match record {
            Record::Bof { .. } => {
                self.bof_depth += 1;
                return Ok(Some((record_sid, record)));
            }
            Record::Eof => {
                self.bof_depth -= 1;
                if self.bof_depth < 1 {
                    self.last_was_eof_level_zero = true;
                }
                return Ok(Some((record_sid, record)));
            }
            Record::DbCell => return Ok(None),
            Record::Rk { cell, value } => {
                return Ok(Some((sid::NUMBER, Record::Number { cell, value })))
            }
            Record::MulRk {
                row,
                first_column,
                last_column,
                values,
            } => {
                let columns = i32::from(last_column) - i32::from(first_column) + 1;
                if columns < 0 {
                    return thrown(format!(
                        "Cannot create RKRecords with negative number of columns: {columns}"
                    ));
                }
                if columns as usize > values.len() || columns == 0 {
                    return thrown("a MulRK record names more columns than it holds");
                }
                let mut numbers =
                    values
                        .iter()
                        .take(columns as usize)
                        .enumerate()
                        .map(|(k, &(xf, value))| {
                            let cell = CellRef {
                                row,
                                column: (k as i32 + i32::from(first_column)) as i16,
                                xf,
                            };
                            (sid::NUMBER, Record::Number { cell, value })
                        });
                let first = numbers.next();
                self.unread.extend(numbers);
                return Ok(first);
            }
            Record::DrawingGroup(ref data) if self.last == Last::DrawingGroup => {
                self.drawing_groups
                    .last_mut()
                    .expect("a drawing group came before")
                    .extend_from_slice(data);
                return Ok(None);
            }
            Record::Continue(ref data) => {
                return match self.last {
                    Last::ObjectOrText => Ok(None),
                    Last::DrawingGroup => {
                        self.drawing_groups
                            .last_mut()
                            .expect("a drawing group came before")
                            .extend_from_slice(data);
                        Ok(None)
                    }
                    Last::Drawing | Last::Unknown => Ok(Some((record_sid, record))),
                    Last::Other => thrown("Unhandled Continue Record"),
                };
            }
            _ => {}
        }
        self.last = last_of(&record);
        Ok(Some((record_sid, record)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::stream::tests::record;
    use super::*;

    const DIMENSIONS: u16 = 0x0200;

    fn bof_alone(kind: i16) -> Vec<u8> {
        let mut data = 0x0600i16.to_le_bytes().to_vec();
        data.extend(kind.to_le_bytes());
        data.extend([0; 12]);
        record(sid::BOF, &data)
    }

    /// A BOF record and the record POI reads ahead after it, which it hands on as read.
    fn bof(kind: i16) -> Vec<u8> {
        let mut bytes = bof_alone(kind);
        bytes.extend(record(DIMENSIONS, &[0; 14]));
        bytes
    }

    fn events(data: &[u8]) -> Read<Vec<Event>> {
        let mut records = Records::new(data)?;
        let mut events = Vec::new();
        while let Some(event) = records.next()? {
            events.push(event);
        }
        Ok(events)
    }

    fn number(column: i16, value: f64) -> Event {
        Event::Record {
            sid: sid::NUMBER,
            record: Record::Number {
                cell: CellRef {
                    row: 3,
                    column,
                    xf: 15,
                },
                value,
            },
        }
    }

    #[test]
    fn turns_mul_rk_records_into_numbers() {
        let mut data = bof(0x10);
        let mut mul_rk = vec![3, 0, 1, 0];
        for value in [2, 3] {
            mul_rk.extend([15, 0]);
            mul_rk.extend(((value << 2) | 2i32).to_le_bytes());
        }
        mul_rk.extend([2, 0]);
        data.extend(record(sid::MUL_RK, &mul_rk));
        data.extend(record(sid::EOF, &[]));
        let events = events(&data).unwrap();
        assert_eq!(events[2], number(1, 2.0));
        assert_eq!(events[3], number(2, 3.0));
    }

    #[test]
    fn hands_on_the_records_read_ahead_as_read() {
        let mut data = bof_alone(0x10);
        data.extend(record(sid::RK, &[3, 0, 1, 0, 15, 0, 2, 0, 0, 0]));
        let delivered = events(&data).unwrap();
        assert!(matches!(delivered[1], Event::Record { sid: sid::RK, .. }));
        let mut data = bof_alone(0x10);
        data.extend(record(sid::EOF, &[]));
        assert!(events(&data).is_err());
    }

    #[test]
    fn stops_after_the_last_eof_and_fails_on_stray_continues() {
        let mut data = bof(0x10);
        data.extend(record(sid::EOF, &[]));
        data.extend(record(DIMENSIONS, &[0; 14]));
        assert_eq!(events(&data).unwrap().len(), 3);
        let mut data = bof(0x10);
        data.extend(record(sid::PROTECT, &[1, 0]));
        data.extend(record(sid::CONTINUE, &[1]));
        assert!(events(&data).is_err());
        let mut data = bof(0x10);
        data.extend(record(0x7777, &[1]));
        data.extend(record(sid::CONTINUE, &[1]));
        assert_eq!(events(&data).unwrap().len(), 4);
    }

    #[test]
    fn finds_encrypted_workbooks() {
        let mut data = bof_alone(0x05);
        data.extend(record(sid::FILE_PASS, &[0; 6]));
        assert!(Records::new(&data).unwrap().encrypted);
        assert!(!Records::new(&bof(0x05)).unwrap().encrypted);
    }
}
