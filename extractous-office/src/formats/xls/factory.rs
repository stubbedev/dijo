//! POI's `RecordFactoryInputStream`: the records of a workbook stream as `HSSFEventFactory` hands
//! them to listeners. RK and MulRK records become number records, DBCell records are dropped,
//! the drawing group records after the first and their continuations join it, and a `CONTINUE`
//! record goes where the record before it takes it, failing after one that takes none.
//!
//! The records read ahead, to find whether the workbook is encrypted, are handed on as the
//! others are. POI hands them on as read: an RK record as such, and a MulRK record, which no
//! listener takes, not at all (#107).

use std::collections::VecDeque;

use super::records::{read_record, sid, CellRef, Record};
use super::stream::RecordStream;
use crate::config::ParserConfigs;
use crate::formats::crypt::xor::{ExcelXorKey, Xor};
use crate::formats::crypt::{self, ChunkedKeyStream, EncryptionInfo, Mode, Rc4Keys, Unopened};
use crate::formats::poi::{thrown, Read};
use crate::formats::{Decline, Unported};

/// `Biff8DecryptingStream.RC4_REKEYING_INTERVAL`: the bytes each block key decrypts.
const RC4_REKEYING_INTERVAL: usize = 1024;
/// `isNeverEncryptedRecord`: the records whose data an encrypted workbook keeps plain, BOF,
/// `INTERFACEHDR` and `FILEPASS`.
const NEVER_ENCRYPTED: [u16; 3] = [sid::BOF, 0x00E1, sid::FILE_PASS];
/// `FilePassRecord`'s encryption types.
const XOR_OBFUSCATION: u16 = 0;
const RC4_ENCRYPTION: u16 = 1;

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
    /// The data of the `FILEPASS` record the first records end with, after which the stream is
    /// encrypted.
    file_pass: Option<Vec<u8>>,
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
    /// `new RecordFactoryInputStream(stream, false)` up to its decrypting stream: the first
    /// records read ahead, to find whether the workbook is encrypted.
    pub(super) fn new(data: &'a [u8]) -> Read<Self> {
        let mut stream = RecordStream::new(data);
        let mut unread = VecDeque::new();
        stream.next_record()?;
        let first = read_record(&mut stream)?;
        let has_bof = matches!(first, Record::Bof { .. });
        let mut last = first.clone();
        unread.extend(cells(stream.sid(), first)?);
        if has_bof && stream.has_next_record()? {
            stream.next_record()?;
            last = read_record(&mut stream)?;
            unread.extend(cells(stream.sid(), last.clone())?);
            if last == Record::WriteProtect && stream.has_next_record()? {
                stream.next_record()?;
                last = read_record(&mut stream)?;
                unread.extend(cells(stream.sid(), last.clone())?);
            }
            if last == Record::Eof {
                return thrown("Nothing between BOF and EOF");
            }
        }
        let file_pass = match &last {
            Record::FilePass(data) => Some(data.clone()),
            _ => None,
        };
        Ok(Records {
            stream,
            file_pass,
            unread,
            last: last_of(&last),
            bof_depth: i32::from(has_bof),
            last_was_eof_level_zero: false,
            drawing_groups: Vec::new(),
        })
    }

    /// `StreamEncryptionInfo.createDecryptingStream`, where the first records end with a
    /// `FILEPASS` record: the encryption opened with the password, and the records after it
    /// decrypted as `Biff8DecryptingStream` reads them, each record's type and length plain and
    /// the data of every record but those `isNeverEncryptedRecord` names decrypted by its
    /// position in the stream; failing as Tika's path does where the password does not open
    /// the encryption. The position is where the records are; POI measures the records before
    /// as it would write them, a `WRITEPROTECT` of four bytes and a missing CSP name as an empty
    /// one, and decrypts out of step where they are otherwise (#119).
    pub(super) fn decrypt(&mut self, configs: &ParserConfigs) -> Result<(), Decline<'static>> {
        let Some(file_pass) = &self.file_pass else {
            return Ok(());
        };
        let info = file_pass_encryption(file_pass)?;
        let start = self.stream.position();
        let password = crypt::password(configs);
        let key = info.verify(password).ok_or_else(|| {
            let given = if password == crypt::DEFAULT_PASSWORD {
                "Default"
            } else {
                "Supplied"
            };
            Decline::Fails(crypt::encrypted(Some(&format!(
                "org.apache.poi.EncryptedDocumentException: {given} password is invalid for \
                 salt/verifier/verifierHash"
            ))))
        })?;
        decrypt_records(&key, self.stream.data_mut(), start);
        Ok(())
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
            Record::Rk { .. } | Record::MulRk { .. } => {
                let mut numbers = cells(record_sid, record)?.into_iter();
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

/// The records POI hands on for `record` of type `record_sid`: a number record for an RK
/// record, one for each cell of a MulRK record, and any other record as it is.
fn cells(record_sid: u16, record: Record) -> Read<Vec<(u16, Record)>> {
    match record {
        Record::Rk { cell, value } => Ok(vec![(sid::NUMBER, Record::Number { cell, value })]),
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
            Ok(values
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
                })
                .collect())
        }
        record => Ok(vec![(record_sid, record)]),
    }
}

/// The encryption of a workbook, as its `FILEPASS` record describes it.
enum Encryption {
    Rc4(EncryptionInfo),
    Xor(Xor),
}

impl Encryption {
    /// The key, where the password opens the encryption.
    fn verify(&self, password: &str) -> Option<WorkbookKey> {
        match self {
            Encryption::Rc4(info) => info.verify(password).map(WorkbookKey::Rc4),
            Encryption::Xor(xor) => xor.verify_excel(password).map(WorkbookKey::Xor),
        }
    }
}

/// What decrypts the records of a workbook.
pub(super) enum WorkbookKey {
    Rc4(Rc4Keys),
    Xor(ExcelXorKey),
}

/// `FilePassRecord`'s encryption, which a workbook reads as CryptoAPI where its version allows.
///
/// A workbook encrypted with AES, which no Office application writes, fails as encrypted
/// whatever the password. Tika's path fails on it with the `IllegalArgumentException` POI
/// throws, in a message that names a Java object's hash (#166).
fn file_pass_encryption(data: &[u8]) -> Result<Encryption, Decline<'static>> {
    let kind = data
        .get(..2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .ok_or_else(|| Unported("POI throws on a FILEPASS record without its type".to_owned()))?;
    let info = &data[2..];
    match kind {
        XOR_OBFUSCATION => Xor::read(info).map(Encryption::Xor),
        RC4_ENCRYPTION => EncryptionInfo::read(info, Mode::CryptoApi).map(Encryption::Rc4),
        _ => {
            return Err(Unported(
                "a FILEPASS record of an encryption type POI throws on, which fails Tika's parse"
                    .to_owned(),
            )
            .into())
        }
    }
    .map_err(|unopened| match unopened {
        Unopened::Disallowed(_) => Decline::Fails(crypt::encrypted(None)),
        unopened => unopened.unported().into(),
    })
}

/// The records of `data` from `start` decrypted in place as `Biff8DecryptingStream` reads them:
/// with RC4, each byte by its position in the stream; with XOR, each byte of a record by its
/// position and the record's length.
pub(super) fn decrypt_records(key: &WorkbookKey, data: &mut [u8], start: usize) {
    enum Decrypting<'k> {
        Rc4(ChunkedKeyStream<'k, Rc4Keys>),
        Xor(&'k ExcelXorKey),
    }
    let mut decrypting = match key {
        WorkbookKey::Rc4(keys) => {
            Decrypting::Rc4(ChunkedKeyStream::new(keys, RC4_REKEYING_INTERVAL))
        }
        WorkbookKey::Xor(key) => Decrypting::Xor(key),
    };
    let mut at = start;
    while at + 4 <= data.len() {
        let sid = u16::from_le_bytes([data[at], data[at + 1]]);
        let length = usize::from(u16::from_le_bytes([data[at + 2], data[at + 3]]));
        let body = at + 4;
        let end = (body + length).min(data.len());
        if !NEVER_ENCRYPTED.contains(&sid) {
            let record = &mut data[body..end];
            match &mut decrypting {
                Decrypting::Rc4(key_stream) => key_stream.decrypt(record, body),
                Decrypting::Xor(key) => key.decrypt_record(record, body, length),
            }
        }
        at = body + length;
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
    fn hands_on_the_records_read_ahead_as_the_others() {
        let mut data = bof_alone(0x10);
        data.extend(record(sid::RK, &[3, 0, 1, 0, 15, 0, 2, 0, 0, 0]));
        let delivered = events(&data).unwrap();
        assert_eq!(delivered[1], number(1, 0.0));
        let mut data = bof_alone(0x10);
        let mut mul_rk = vec![3, 0, 1, 0];
        for value in [2, 3] {
            mul_rk.extend([15, 0]);
            mul_rk.extend(((value << 2) | 2i32).to_le_bytes());
        }
        mul_rk.extend([2, 0]);
        data.extend(record(sid::MUL_RK, &mul_rk));
        let delivered = events(&data).unwrap();
        assert_eq!(delivered[1..], [number(1, 2.0), number(2, 3.0)]);
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
        assert!(Records::new(&data).unwrap().file_pass.is_some());
        assert!(Records::new(&bof(0x05)).unwrap().file_pass.is_none());
    }
}
