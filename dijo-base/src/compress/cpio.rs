//! cpio archives given as commons-compress's `CpioArchiveInputStream` gives Tika's
//! `PackageParser` their files: up to the `TRAILER!!!` entry, each checksum checked, a directory
//! by its mode. The cpio crate reads the new ASCII format, with or without checksums; the old
//! binary and ASCII formats, which it does not read, are read as `CpioArchiveInputStream` reads
//! them.
//!
//! Names are read as UTF-8, which commons-compress, decoding names as US-ASCII, rejects.

use super::{throws, ArchiveFile, Undecoded};
use crate::input::{Input, Pages};

const NEW: &[u8; 6] = b"070701";
const NEW_CRC: &[u8; 6] = b"070702";
const OLD_ASCII: &[u8; 6] = b"070707";
const TRAILER: &str = "TRAILER!!!";
const FILE_TYPE: u32 = 0o170000;
const DIRECTORY: u32 = 0o040000;

fn fail(error: std::io::Error) -> Undecoded {
    throws(error.to_string())
}

/// The files of the cpio archive `data`, whose pages are `pages`, each file's data in place,
/// or why they are not read natively. Each entry is read in the format its own magic names, as
/// `getNextCPIOEntry` reads it.
pub fn files<'a>(data: &'a [u8], pages: Pages<'a>) -> Result<Vec<ArchiveFile<'a>>, Undecoded> {
    let mut input = data;
    let mut files = Vec::new();
    loop {
        let (entry, rest) = if input.starts_with(NEW) || input.starts_with(NEW_CRC) {
            new_entry(input)?
        } else {
            OldFormat::of(input)?.entry(input)?
        };
        let Some(entry) = entry else {
            return Ok(files);
        };
        files.push(entry.file(pages));
        input = rest;
    }
}

/// An entry of `name`, `mode` and modification time `mtime` in seconds, holding `content`.
struct Entry<'a> {
    name: String,
    mode: u64,
    mtime: i64,
    content: &'a [u8],
}

impl<'a> Entry<'a> {
    /// The entry's file, whose data has the pages `pages`: a directory, by its mode, without it.
    fn file(self, pages: Pages<'a>) -> ArchiveFile<'a> {
        let directory = self.mode & u64::from(FILE_TYPE) == u64::from(DIRECTORY);
        ArchiveFile {
            name: self.name,
            size: self.content.len() as i64,
            modified_millis: Some(self.mtime * 1000),
            created_millis: None,
            directory,
            content: if directory {
                Input::Borrowed(&[])
            } else {
                Input::Within(self.content, pages)
            },
        }
    }
}

/// The entry in the new ASCII format at the start of `input`, none for the trailer, and what
/// follows it.
fn new_entry(input: &[u8]) -> Result<(Option<Entry<'_>>, &[u8]), Undecoded> {
    let mut rest = input;
    let entry = cpio::newc::Reader::new(&mut rest)
        .map_err(fail)?
        .entry()
        .clone();
    if entry.is_trailer() {
        return Ok((None, &[]));
    }
    let size = usize::try_from(entry.file_size()).expect("a 32-bit size fits in usize");
    let ends_inside = || throws("the input ends inside an entry's data");
    let content = rest.get(..size).ok_or_else(ends_inside)?;
    if let Some(checksum) = entry.checksum() {
        let sum = content
            .iter()
            .fold(0u32, |sum, &b| sum.wrapping_add(u32::from(b)));
        if sum != checksum {
            return Err(throws("CRC Error"));
        }
    }
    let rest = rest
        .get(size + (4 - size % 4) % 4..)
        .ok_or_else(ends_inside)?;
    let entry = Entry {
        name: entry.name().to_owned(),
        mode: u64::from(entry.mode()),
        mtime: i64::from(entry.mtime()),
        content,
    };
    Ok((Some(entry), rest))
}

/// The old formats, which the cpio crate does not read: binary, in the byte order of the machine
/// that wrote it, or ASCII with octal numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OldFormat {
    /// Each 16-bit word little-endian, or big-endian where `swapped`, the high word of a 32-bit
    /// number first.
    Binary {
        swapped: bool,
    },
    Ascii,
}

impl OldFormat {
    /// The format whose magic `input` starts with, failing as `getNextCPIOEntry` fails on an
    /// unknown one.
    fn of(input: &[u8]) -> Result<Self, Undecoded> {
        match input {
            [0xC7, 0x71, ..] => Ok(OldFormat::Binary { swapped: false }),
            [0x71, 0xC7, ..] => Ok(OldFormat::Binary { swapped: true }),
            _ if input.starts_with(OLD_ASCII) => Ok(OldFormat::Ascii),
            _ => Err(throws(format!(
                "Unknown magic [{}]",
                super::ascii(&input[..input.len().min(6)])
            ))),
        }
    }

    /// The widths of the header's fields after the magic: device, inode, mode, user, group,
    /// links, remote device, modification time, name size and file size.
    fn widths(self) -> [usize; 10] {
        match self {
            OldFormat::Binary { .. } => [2, 2, 2, 2, 2, 2, 2, 4, 2, 4],
            OldFormat::Ascii => [6, 6, 6, 6, 6, 6, 6, 11, 6, 11],
        }
    }

    fn magic_length(self) -> usize {
        match self {
            OldFormat::Binary { .. } => 2,
            OldFormat::Ascii => OLD_ASCII.len(),
        }
    }

    /// What the header and the name, and the data, are padded to a multiple of.
    fn alignment(self) -> usize {
        match self {
            OldFormat::Binary { .. } => 2,
            OldFormat::Ascii => 1,
        }
    }

    /// `readBinaryLong` or `readAsciiLong` over a field.
    fn number(self, field: &[u8]) -> Result<u64, Undecoded> {
        match self {
            OldFormat::Binary { swapped } => {
                Ok(field.as_chunks::<2>().0.iter().fold(0, |number, &word| {
                    let word = if swapped {
                        u16::from_be_bytes(word)
                    } else {
                        u16::from_le_bytes(word)
                    };
                    number << 16 | u64::from(word)
                }))
            }
            OldFormat::Ascii => {
                let text = super::ascii(field);
                u64::from_str_radix(&text, 8)
                    .map_err(|_| throws(format!("For input string: \"{text}\" under radix 8")))
            }
        }
    }

    /// `readOldBinaryEntry` or `readOldAsciiEntry` and the entry's data at the start of `input`,
    /// none for the trailer, and what follows it.
    fn entry(self, input: &[u8]) -> Result<(Option<Entry<'_>>, &[u8]), Undecoded> {
        let mut cursor = Cursor(input);
        cursor.take(self.magic_length())?;
        let mut fields = [0u64; 10];
        for (field, width) in fields.iter_mut().zip(self.widths()) {
            *field = self.number(cursor.take(width)?)?;
        }
        let [_, _, mode, _, _, _, _, mtime, name_size, file_size] = fields;
        let header_length = input.len() - cursor.0.len();
        let name_size = usize::try_from(name_size)
            .ok()
            .filter(|&size| size > 0)
            .ok_or_else(|| throws("Found illegal entry with zero name length"))?;
        let name = cursor.take(name_size - 1)?;
        cursor.take(1)?;
        let name = String::from_utf8(name.to_vec())
            .map_err(|_| throws("the name of an entry is not UTF-8"))?;
        if mode & u64::from(FILE_TYPE) == 0 && name != TRAILER {
            return Err(throws(format!(
                "Mode 0 only allowed in the trailer. Found entry: {name}"
            )));
        }
        cursor.take(self.padding(header_length + name_size))?;
        if name == TRAILER {
            return Ok((None, &[]));
        }
        let mtime = i64::try_from(mtime).expect("at most 33 bits");
        let size = usize::try_from(file_size).unwrap_or(usize::MAX);
        let content = cursor.take(size)?;
        cursor.take(self.padding(size))?;
        let entry = Entry {
            name,
            mode,
            mtime,
            content,
        };
        Ok((Some(entry), cursor.0))
    }

    /// The bytes that pad `length` bytes to the alignment.
    fn padding(self, length: usize) -> usize {
        (self.alignment() - length % self.alignment()) % self.alignment()
    }
}

/// What is left of an old format's entry to read.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    /// The next `length` bytes, failing where the input ends before them, as `readFully` fails.
    fn take(&mut self, length: usize) -> Result<&'a [u8], Undecoded> {
        if self.0.len() < length {
            return Err(throws("the input ends inside an entry"));
        }
        let (taken, rest) = self.0.split_at(length);
        self.0 = rest;
        Ok(taken)
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use std::io::Write;

    #[cfg(test)]
    use super::*;

    /// An archive of `entries` in the new ASCII format with checksums, as `cpio -H crc` writes;
    /// a name ending in a slash is a directory's.
    pub fn cpio(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, (name, content)) in entries.iter().enumerate() {
            let directory = name.ends_with('/');
            let checksum = content
                .iter()
                .fold(0u32, |sum, &b| sum.wrapping_add(u32::from(b)));
            let builder = cpio::newc::Builder::new(name.trim_end_matches('/'))
                .ino(index as u32 + 1)
                .mode(if directory { 0o040755 } else { 0o100644 })
                .nlink(1)
                .mtime(1_700_000_000);
            let mut writer = builder.write_crc(&mut out, content.len() as u32, checksum);
            writer.write_all(content).unwrap();
            writer.finish().unwrap();
        }
        cpio::newc::trailer(&mut out).unwrap();
        out
    }

    #[test]
    fn reads_entries_up_to_the_trailer() {
        let data = cpio(&[("dir/", b""), ("dir/a.txt", b"odd")]);
        let found: Vec<(String, bool, Vec<u8>)> = files(&data, Pages::IN_MEMORY)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.directory, file.content.to_vec()))
            .collect();
        assert_eq!(
            found,
            [
                ("dir".to_owned(), true, Vec::new()),
                ("dir/a.txt".to_owned(), false, b"odd".to_vec()),
            ]
        );
    }

    #[test]
    fn fails_on_what_is_cut_short_or_corrupt() {
        let data = cpio(&[("a.txt", b"content")]);
        let files = |data| files(data, Pages::IN_MEMORY);
        assert!(files(&data[..data.len() - 130]).is_err());
        let mut corrupt = data.clone();
        let at = corrupt.windows(7).position(|w| w == b"content").unwrap();
        corrupt[at] = b'C';
        assert!(files(&corrupt).is_err());
        assert!(matches!(files(b"070707rest"), Err(Undecoded::Fails(_))));
        assert!(matches!(files(b"070799rest"), Err(Undecoded::Fails(_))));
    }
}

#[cfg(test)]
mod old_format_tests {
    use super::*;

    const ENTRIES: [(&str, u64, &[u8]); 3] = [
        ("dir", 0o040755, b""),
        ("dir/odd.txt", 0o100644, b"odd"),
        ("even.txt", 0o100644, b"even"),
    ];

    fn found(data: &[u8]) -> Vec<(String, bool, Vec<u8>, Option<i64>)> {
        files(data, Pages::IN_MEMORY)
            .unwrap()
            .into_iter()
            .map(|file| {
                (
                    file.name,
                    file.directory,
                    file.content.to_vec(),
                    file.modified_millis,
                )
            })
            .collect()
    }

    fn expected() -> Vec<(String, bool, Vec<u8>, Option<i64>)> {
        ENTRIES
            .iter()
            .map(|(name, mode, content)| {
                (
                    name.to_string(),
                    mode & 0o170000 == 0o040000,
                    if mode & 0o170000 == 0o040000 {
                        Vec::new()
                    } else {
                        content.to_vec()
                    },
                    Some(1_700_000_000_000),
                )
            })
            .collect()
    }

    /// An archive in the old ASCII format, as `cpio -H odc` writes.
    fn odc() -> Vec<u8> {
        let mut out = Vec::new();
        let trailer = (TRAILER, 0, &b""[..]);
        for (name, mode, content) in ENTRIES.into_iter().chain([trailer]) {
            out.extend_from_slice(
                format!(
                    "070707{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:011o}{:06o}{:011o}{name}\0",
                    0,
                    1,
                    mode,
                    0,
                    0,
                    1,
                    0,
                    1_700_000_000,
                    name.len() + 1,
                    content.len()
                )
                .as_bytes(),
            );
            out.extend_from_slice(content);
        }
        out
    }

    /// An archive in the old binary format, as `cpio -H bin` writes on a little-endian machine,
    /// or on a big-endian one where `swapped`.
    fn bin(swapped: bool) -> Vec<u8> {
        let word = |out: &mut Vec<u8>, word: u16| {
            out.extend_from_slice(&if swapped {
                word.to_be_bytes()
            } else {
                word.to_le_bytes()
            })
        };
        let long = |out: &mut Vec<u8>, long: u32| {
            word(out, (long >> 16) as u16);
            word(out, long as u16);
        };
        let mut out = Vec::new();
        let trailer = (TRAILER, 0, &b""[..]);
        for (name, mode, content) in ENTRIES.into_iter().chain([trailer]) {
            for field in [0o070707, 0, 1, mode as u16, 0, 0, 1, 0] {
                word(&mut out, field);
            }
            long(&mut out, 1_700_000_000);
            word(&mut out, name.len() as u16 + 1);
            long(&mut out, content.len() as u32);
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            if out.len() % 2 == 1 {
                out.push(0);
            }
            out.extend_from_slice(content);
            if content.len() % 2 == 1 {
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn reads_the_old_ascii_and_binary_formats() {
        assert_eq!(found(&odc()), expected());
        assert_eq!(found(&bin(false)), expected());
        assert_eq!(found(&bin(true)), expected());
    }

    #[test]
    fn fails_on_old_archives_cut_short() {
        let files = |data| files(data, Pages::IN_MEMORY);
        let odc = odc();
        assert!(matches!(
            files(&odc[..odc.len() - 90]),
            Err(Undecoded::Fails(_))
        ));
        let bin = bin(false);
        let pad = bin.windows(4).position(|w| w == b"odd\0").unwrap() + 4;
        assert!(matches!(files(&bin[..pad - 1]), Err(Undecoded::Fails(_))));
    }
}
