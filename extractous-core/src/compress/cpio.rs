//! cpio archives as commons-compress's `CpioArchiveInputStream` reads them: the new ASCII
//! format, with or without its checksum, the old ASCII format and the old binary format in
//! either byte order, names in US-ASCII, up to the `TRAILER!!!` entry.

use super::{ascii, parse_long};

const TRAILER: &str = "TRAILER!!!";
const OLD_BINARY: u64 = 0o70707;
const FILE_TYPE: i64 = 0o170000;
const DIRECTORY: i64 = 0o040000;
const MAX_SIZE: i64 = 0xFFFF_FFFF;

/// Why commons-compress throws on an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Throws(pub(crate) String);

fn throws(reason: impl Into<String>) -> Throws {
    Throws(reason.into())
}

/// The formats of a cpio header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    New,
    NewCrc,
    OldAscii,
    OldBinary { swap_half_word: bool },
}

impl Format {
    fn header_size(self) -> i64 {
        match self {
            Format::New | Format::NewCrc => 110,
            Format::OldAscii => 76,
            Format::OldBinary { .. } => 26,
        }
    }

    fn alignment(self) -> i64 {
        match self {
            Format::New | Format::NewCrc => 4,
            Format::OldAscii => 0,
            Format::OldBinary { .. } => 2,
        }
    }

    fn padding(self, size: i64) -> usize {
        let alignment = self.alignment();
        if alignment == 0 {
            return 0;
        }
        let remain = size % alignment;
        if remain > 0 {
            (alignment - remain) as usize
        } else {
            0
        }
    }
}

/// A file of the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) size: i64,
    /// `getLastModifiedDate`: the seconds times a thousand, as a long multiplies them.
    pub(crate) modified_millis: i64,
    mode: i64,
    format: Format,
    checksum: i64,
    data_start: usize,
}

impl Entry {
    pub(crate) fn is_directory(&self) -> bool {
        self.mode & FILE_TYPE == DIRECTORY
    }
}

/// A `CpioArchiveInputStream` over the whole input.
pub(crate) struct CpioStream<'a> {
    data: &'a [u8],
    position: usize,
    current: Option<Entry>,
}

/// `CpioUtil.byteArray2long`: big-endian, the bytes of each half word swapped unless
/// `swap_half_word`.
fn binary_number(bytes: &[u8], swap_half_word: bool) -> u64 {
    let mut number = 0u64;
    for pair in bytes.chunks(2) {
        let (high, low) = if swap_half_word {
            (pair[0], pair[1])
        } else {
            (pair[1], pair[0])
        };
        number = number << 16 | u64::from(high) << 8 | u64::from(low);
    }
    number
}

impl<'a> CpioStream<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        CpioStream {
            data,
            position: 0,
            current: None,
        }
    }

    /// The data of `entry`, which must be the entry last read, read to its end.
    pub(crate) fn content(&self, entry: &Entry) -> Result<&'a [u8], Throws> {
        let size = usize::try_from(entry.size).expect("a size within four bytes");
        self.data
            .get(entry.data_start..entry.data_start + size)
            .ok_or_else(|| throws("the input ends inside an entry's data"))
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], Throws> {
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| throws("EOFException"))?;
        let taken = &self.data[self.position..end];
        self.position = end;
        Ok(taken)
    }

    fn ascii_number(&mut self, length: usize, radix: u32) -> Result<i64, Throws> {
        let text = ascii(self.take(length)?);
        parse_long(&text, radix).ok_or_else(|| throws(format!("Unable to parse number: {text}")))
    }

    fn binary_number(&mut self, length: usize, swap_half_word: bool) -> Result<i64, Throws> {
        Ok(binary_number(self.take(length)?, swap_half_word) as i64)
    }

    /// `readCString`: the name and the NUL after it, which is not checked, in US-ASCII.
    fn name(&mut self, size: i64) -> Result<String, Throws> {
        if !(1..=i64::from(i32::MAX)).contains(&size) {
            return Err(throws(format!("an unusual name size {size}")));
        }
        let bytes = self.take(size as usize - 1)?;
        self.take(1)?;
        if !bytes.is_ascii() {
            return Err(throws("a name is not US-ASCII"));
        }
        Ok(ascii(bytes))
    }

    /// `closeEntry`: the current entry's data, its padding and, in the new format with a
    /// checksum, the sum of its bytes checked.
    fn close_entry(&mut self) -> Result<(), Throws> {
        let Some(entry) = self.current.take() else {
            return Ok(());
        };
        let content = self.content(&entry)?;
        self.position = entry.data_start + content.len();
        self.take(entry.format.padding(entry.size))?;
        if entry.format == Format::NewCrc {
            let sum = content
                .iter()
                .fold(0i64, |sum, &b| (sum + i64::from(b)) & 0xFFFF_FFFF);
            if sum != entry.checksum {
                return Err(throws("CRC Error"));
            }
        }
        Ok(())
    }

    /// `getNextCPIOEntry`: `None` at the trailer.
    pub(crate) fn next_entry(&mut self) -> Result<Option<Entry>, Throws> {
        self.close_entry()?;
        let magic = self.take(2)?;
        let format = if binary_number(magic, false) == OLD_BINARY {
            Format::OldBinary {
                swap_half_word: false,
            }
        } else if binary_number(magic, true) == OLD_BINARY {
            Format::OldBinary {
                swap_half_word: true,
            }
        } else {
            let rest = self.take(4)?;
            match [magic, rest].concat().as_slice() {
                b"070701" => Format::New,
                b"070702" => Format::NewCrc,
                b"070707" => Format::OldAscii,
                other => return Err(throws(format!("Unknown magic [{}]", ascii(other)))),
            }
        };
        let (name, size, mode, mtime, checksum, name_size) = match format {
            Format::New | Format::NewCrc => {
                let mut fields = [0i64; 13];
                for field in &mut fields {
                    *field = self.ascii_number(8, 16)?;
                }
                let [_, mode, _, _, _, mtime, size, _, _, _, _, name_size, checksum] = fields;
                check_size(size)?;
                let name = self.name(name_size)?;
                (name, size, mode, mtime, checksum, name_size)
            }
            Format::OldAscii => {
                let mut fields = [0i64; 7];
                for field in &mut fields {
                    *field = self.ascii_number(6, 8)?;
                }
                let mode = fields[2];
                let mtime = self.ascii_number(11, 8)?;
                let name_size = self.ascii_number(6, 8)?;
                let size = self.ascii_number(11, 8)?;
                check_size(size)?;
                let name = self.name(name_size)?;
                (name, size, mode, mtime, 0, name_size)
            }
            Format::OldBinary { swap_half_word } => {
                let mut fields = [0i64; 7];
                for field in &mut fields {
                    *field = self.binary_number(2, swap_half_word)?;
                }
                let mode = fields[2];
                let mtime = self.binary_number(4, swap_half_word)?;
                let name_size = self.binary_number(2, swap_half_word)?;
                let size = self.binary_number(4, swap_half_word)?;
                let name = self.name(name_size)?;
                (name, size, mode, mtime, 0, name_size)
            }
        };
        if name_size < 0 {
            return Err(throws("Found illegal entry with negative name length"));
        }
        if mode & FILE_TYPE == 0 && name != TRAILER {
            return Err(throws("Mode 0 only allowed in the trailer"));
        }
        let header_end = format.header_size() + name_size;
        self.take(format.padding(header_end))?;
        if name == TRAILER {
            return Ok(None);
        }
        let entry = Entry {
            name,
            size,
            modified_millis: mtime.wrapping_mul(1000),
            mode: if mode & FILE_TYPE != 0 { mode } else { 0 },
            format,
            checksum,
            data_start: self.position,
        };
        self.current = Some(entry.clone());
        Ok(Some(entry))
    }
}

/// `setSize`, which takes sizes of four bytes.
fn check_size(size: i64) -> Result<(), Throws> {
    if (0..=MAX_SIZE).contains(&size) {
        Ok(())
    } else {
        Err(throws(format!("Invalid entry size <{size}>")))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An archive of `entries` in the new ASCII format with checksums, as `cpio -H crc` writes.
    pub(crate) fn cpio(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let pad = |out: &mut Vec<u8>| {
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
        };
        let write = |out: &mut Vec<u8>, name: &str, mode: u32, content: &[u8]| {
            let sum = content.iter().map(|&b| u32::from(b)).sum::<u32>();
            out.extend_from_slice(
                format!(
                    "070702{:08X}{mode:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{sum:08X}",
                    1,
                    0,
                    0,
                    1,
                    1_700_000_000,
                    content.len(),
                    0,
                    0,
                    0,
                    0,
                    name.len() + 1
                )
                .as_bytes(),
            );
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            pad(out);
            out.extend_from_slice(content);
            pad(out);
        };
        for (name, content) in entries {
            let mode = if name.ends_with('/') {
                0o040755
            } else {
                0o100644
            };
            write(&mut out, name.trim_end_matches('/'), mode, content);
        }
        write(&mut out, TRAILER, 0, b"");
        out
    }

    fn entries(data: &[u8]) -> Result<Vec<(String, bool, Vec<u8>)>, Throws> {
        let mut stream = CpioStream::new(data);
        let mut found = Vec::new();
        while let Some(entry) = stream.next_entry()? {
            let content = stream.content(&entry)?.to_vec();
            found.push((entry.name.clone(), entry.is_directory(), content));
        }
        Ok(found)
    }

    #[test]
    fn reads_entries_up_to_the_trailer() {
        let data = cpio(&[("dir/", b""), ("dir/a.txt", b"odd")]);
        assert_eq!(
            entries(&data).unwrap(),
            [
                ("dir".to_owned(), true, Vec::new()),
                ("dir/a.txt".to_owned(), false, b"odd".to_vec()),
            ]
        );
    }

    #[test]
    fn fails_where_commons_compress_fails() {
        let data = cpio(&[("a.txt", b"content")]);
        assert!(entries(&data[..data.len() - 130]).is_err());
        let mut corrupt = data.clone();
        let at = corrupt.windows(7).position(|w| w == b"content").unwrap();
        corrupt[at] = b'C';
        assert!(entries(&corrupt).is_err());
        assert!(entries(b"070799").is_err());
    }
}
