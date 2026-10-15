//! Unix `ar` archives as commons-compress's `ArArchiveInputStream` reads them: 60-byte headers
//! of ASCII fields, GNU long names from the `//` string table and BSD long names before the
//! data, an entry's data read as far as the input goes.

use std::ops::Range;

use super::{ascii, java_trim, parse_long};

const SIGNATURE: &[u8; 8] = b"!<arch>\n";
const NAME: Range<usize> = 0..16;
const MODIFIED: Range<usize> = 16..28;
const USER_ID: Range<usize> = 28..34;
const GROUP_ID: Range<usize> = 34..40;
const MODE: Range<usize> = 40..48;
const LENGTH: Range<usize> = 48..58;
const TRAILER: &[u8; 2] = b"`\n";
const STRING_TABLE: &str = "//";
const BSD_PREFIX: &str = "#1/";

/// Why commons-compress throws on an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Throws(pub(crate) String);

fn throws(reason: impl Into<String>) -> Throws {
    Throws(reason.into())
}

/// A file of the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) length: i64,
    /// `getLastModifiedDate`: the seconds times a thousand, as a long multiplies them.
    pub(crate) modified_millis: i64,
    /// Where its data lies, which the input may cut short.
    data: Range<usize>,
}

/// An `ArArchiveInputStream` over the whole input.
pub(crate) struct ArStream<'a> {
    data: &'a [u8],
    position: usize,
    string_table: Option<&'a [u8]>,
    /// Where the current entry's data ends.
    entry_end: Option<i64>,
}

/// `asInt` and `asLong`: an ASCII field, trimmed, as a number of `radix`, blank as zero when
/// `blank_is_zero`.
fn field_number(field: &[u8], radix: u32, blank_is_zero: bool) -> Result<i64, Throws> {
    let text = ascii(field);
    let text = java_trim(&text);
    if text.is_empty() && blank_is_zero {
        return Ok(0);
    }
    parse_long(text, radix).ok_or_else(|| throws(format!("Unable to parse number: {text}")))
}

fn int_field(field: &[u8], radix: u32, blank_is_zero: bool) -> Result<i64, Throws> {
    let number = field_number(field, radix, blank_is_zero)?;
    i32::try_from(number)
        .map(i64::from)
        .map_err(|_| throws(format!("Unable to parse int: {number}")))
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

impl<'a> ArStream<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        ArStream {
            data,
            position: 0,
            string_table: None,
            entry_end: None,
        }
    }

    /// The data of `entry`, which must be the entry last read, as far as the input goes.
    pub(crate) fn content(&self, entry: &Entry) -> &'a [u8] {
        let end = entry.data.end.min(self.data.len());
        &self.data[entry.data.start.min(end)..end]
    }

    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let taken = self
            .data
            .get(self.position..self.position.checked_add(length)?)?;
        self.position += length;
        Some(taken)
    }

    /// `getNextArEntry`: `None` at the end of the archive.
    pub(crate) fn next_entry(&mut self) -> Result<Option<Entry>, Throws> {
        if let Some(end) = self.entry_end.take() {
            let end = usize::try_from(end.max(0)).unwrap_or(usize::MAX);
            self.position = self.position.max(end.min(self.data.len()));
        }
        if self.position == 0 {
            let signature = self
                .take(SIGNATURE.len())
                .ok_or_else(|| throws("Failed to read header"))?;
            if signature != SIGNATURE {
                return Err(throws("Invalid header"));
            }
        }
        if !self.position.is_multiple_of(2) {
            if self.position >= self.data.len() {
                return Ok(None);
            }
            self.position += 1;
        }
        let rest = self.data.len() - self.position;
        if rest == 0 {
            return Ok(None);
        }
        let header = self
            .take(LENGTH.end)
            .ok_or_else(|| throws("Truncated ar archive"))?;
        if self.take(TRAILER.len()) != Some(TRAILER) {
            return Err(throws("Invalid entry trailer"));
        }
        let mut entry_offset = self.position;
        let name_field = ascii(&header[NAME]);
        let mut name = java_trim(&name_field).to_owned();
        if name == STRING_TABLE {
            let length = int_field(&header[LENGTH], 10, false)?;
            let length = usize::try_from(length).map_err(|_| throws("a negative // length"))?;
            let table = self
                .take(length)
                .ok_or_else(|| throws("Failed to read complete // record"))?;
            self.string_table = Some(table);
            self.entry_end = Some((entry_offset + length) as i64);
            return self.next_entry();
        }
        let mut length = field_number(&header[LENGTH], 10, false)?;
        if let Some(stripped) = name.strip_suffix('/') {
            name = stripped.to_owned();
        } else if name.strip_prefix('/').is_some_and(is_digits) {
            let offset = int_field(&name.as_bytes()[1..], 10, false)?;
            name = self.extended_name(offset)?;
        } else if name.strip_prefix(BSD_PREFIX).is_some_and(is_digits) {
            let name_length = int_field(&name.as_bytes()[BSD_PREFIX.len()..], 10, false)?;
            let name_length =
                usize::try_from(name_length).map_err(|_| throws("a negative name length"))?;
            let bytes = self
                .take(name_length)
                .ok_or_else(|| throws("the input ends inside a BSD long name"))?;
            name = ascii(bytes);
            length -= name_length as i64;
            entry_offset += name_length;
        }
        if length < 0 {
            return Err(throws("broken archive, entry with negative size"));
        }
        int_field(&header[USER_ID], 10, true)?;
        int_field(&header[GROUP_ID], 10, true)?;
        int_field(&header[MODE], 8, false)?;
        let modified = field_number(&header[MODIFIED], 10, false)?;
        let end = (entry_offset as i64).saturating_add(length);
        self.entry_end = Some(end);
        Ok(Some(Entry {
            name,
            length,
            modified_millis: modified.wrapping_mul(1000),
            data: entry_offset..usize::try_from(end).unwrap_or(usize::MAX),
        }))
    }

    /// `getExtendedName`: the name at `offset` of the string table, up to a line feed or NUL,
    /// without a slash before it.
    fn extended_name(&self, offset: i64) -> Result<String, Throws> {
        let table = self
            .string_table
            .ok_or_else(|| throws("Cannot process GNU long file name as no // record was found"))?;
        let failed = || throws(format!("Failed to read entry: {offset}"));
        let start = usize::try_from(offset).map_err(|_| failed())?;
        let Some(found) = table
            .get(start..)
            .and_then(|rest| rest.iter().position(|&b| b == b'\n' || b == 0))
        else {
            return Err(failed());
        };
        let mut end = start + found;
        if end == 0 {
            return Err(failed());
        }
        if table[end - 1] == b'/' {
            end -= 1;
        }
        if end > start {
            Ok(ascii(&table[start..end]))
        } else {
            Err(failed())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An archive of `entries`, GNU-style names longer than 15 bytes in a `//` table.
    pub(crate) fn ar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut table = Vec::new();
        let mut names = Vec::new();
        for (name, _) in entries {
            if name.len() > 15 {
                names.push(format!("/{}", table.len()));
                table.extend_from_slice(format!("{name}/\n").as_bytes());
            } else {
                names.push(format!("{name}/"));
            }
        }
        let mut out = SIGNATURE.to_vec();
        let header = |out: &mut Vec<u8>, name: &str, length: usize| {
            out.extend_from_slice(
                format!(
                    "{name:<16}{:<12}{:<6}{:<6}{:<8}{length:<10}`\n",
                    1_700_000_000, 0, 0, 644
                )
                .as_bytes(),
            );
        };
        if !table.is_empty() {
            header(&mut out, STRING_TABLE, table.len());
            out.extend_from_slice(&table);
            if table.len() % 2 == 1 {
                out.push(b'\n');
            }
        }
        for ((_, content), name) in entries.iter().zip(&names) {
            header(&mut out, name, content.len());
            out.extend_from_slice(content);
            if content.len() % 2 == 1 {
                out.push(b'\n');
            }
        }
        out
    }

    fn entries(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, Throws> {
        let mut stream = ArStream::new(data);
        let mut found = Vec::new();
        while let Some(entry) = stream.next_entry()? {
            found.push((entry.name.clone(), stream.content(&entry).to_vec()));
        }
        Ok(found)
    }

    #[test]
    fn reads_short_and_long_names() {
        let data = ar(&[("a.txt", b"odd"), ("a-rather-long-file-name.txt", b"long")]);
        assert_eq!(
            entries(&data).unwrap(),
            [
                ("a.txt".to_owned(), b"odd".to_vec()),
                ("a-rather-long-file-name.txt".to_owned(), b"long".to_vec()),
            ]
        );
    }

    #[test]
    fn reads_a_cut_entry_as_far_as_it_goes() {
        let data = ar(&[("a.txt", b"content")]);
        assert_eq!(
            entries(&data[..data.len() - 4]).unwrap(),
            [("a.txt".to_owned(), b"cont".to_vec())]
        );
        assert!(entries(b"!<arch>\nshort").is_err());
        assert!(entries(b"<arch>\n").is_err());
    }
}
