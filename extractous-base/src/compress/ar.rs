//! Unix `ar` archives, which the ar crate reads, given as commons-compress's
//! `ArArchiveInputStream` gives Tika's `PackageParser` their files: GNU and BSD long names
//! resolved, names decoded as UTF-8 where valid, else as US-ASCII with U+FFFD for each byte
//! beyond it, and modification times in milliseconds that do not wrap around. commons-compress
//! decodes every name as US-ASCII and multiplies the time as an `int`, which wraps (#101).
//!
//! The ar crate keeps the GNU and BSD symbol tables to itself, while commons-compress gives them
//! to Tika as files, the GNU one without a name.

use std::cell::Cell;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::rc::Rc;

use super::{ascii, throws, ArchiveFile, Undecoded};
use crate::input::{Input, Pages};

/// The files of the `ar` archive `data`, whose pages are `pages`, each file's data in place, or
/// why they are not read natively. A file the archive cuts short keeps what it holds of it.
pub fn files<'a>(data: &'a [u8], pages: Pages<'a>) -> Result<Vec<ArchiveFile<'a>>, Undecoded> {
    let position = Rc::new(Cell::new(0));
    let mut archive = ar::Archive::new(Tracked {
        cursor: Cursor::new(data),
        position: Rc::clone(&position),
    });
    let mut files = Vec::new();
    while let Some(entry) = archive.next_entry() {
        let fail = |e: io::Error| throws(e.to_string());
        let mut entry = entry.map_err(fail)?;
        let header = entry.header();
        let name = std::str::from_utf8(header.identifier())
            .map_or_else(|_| ascii(header.identifier()), str::to_owned);
        let size = header.size();
        let modified = i64::try_from(header.mtime()).unwrap_or(i64::MAX);
        let start =
            usize::try_from(position.get()).map_or(data.len(), |start| start.min(data.len()));
        let length = entry.seek(SeekFrom::End(0)).map_err(fail)?;
        let end = usize::try_from(length).map_or(data.len(), |length| {
            start.saturating_add(length).min(data.len())
        });
        files.push(ArchiveFile {
            name,
            size,
            modified_millis: modified.checked_mul(1000),
            created_millis: None,
            directory: false,
            content: Input::Within(&data[start..end], pages),
        });
    }
    Ok(files)
}

/// A reader of an archive that tells where it stands, which is where a file's data starts once
/// the ar crate has read its header.
struct Tracked<'a> {
    cursor: Cursor<&'a [u8]>,
    position: Rc<Cell<u64>>,
}

impl Read for Tracked<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.cursor.read(buffer)?;
        self.position.set(self.cursor.position());
        Ok(read)
    }
}

impl Seek for Tracked<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let position = self.cursor.seek(to)?;
        self.position.set(position);
        Ok(position)
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    #[cfg(test)]
    use super::*;

    /// An archive of `entries` in the GNU variant, names longer than 15 bytes in its `//` table.
    pub fn ar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let identifiers: Vec<Vec<u8>> = entries
            .iter()
            .map(|(name, _)| name.as_bytes().to_vec())
            .collect();
        let mut builder = ar::GnuBuilder::new(Vec::new(), identifiers);
        for (name, content) in entries {
            let mut header = ar::Header::new(name.as_bytes().to_vec(), content.len() as u64);
            header.set_mtime(1_700_000_000);
            header.set_mode(0o644);
            builder.append(&header, *content).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn reads_short_and_long_names() {
        let data = ar(&[
            ("a.txt", b"odd"),
            ("a-rather-long-file-name.txt", b"long"),
            ("caf\u{e9}.txt", b"utf-8"),
        ]);
        let found: Vec<(String, Vec<u8>)> = files(&data, Pages::IN_MEMORY)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.content.to_vec()))
            .collect();
        assert_eq!(
            found,
            [
                ("a.txt".to_owned(), b"odd".to_vec()),
                ("a-rather-long-file-name.txt".to_owned(), b"long".to_vec()),
                ("caf\u{e9}.txt".to_owned(), b"utf-8".to_vec()),
            ]
        );
        assert!(files(b"<arch>\n", Pages::IN_MEMORY).is_err());
    }

    /// commons-compress gives Tika a GNU symbol table as a file without a name.
    #[test]
    fn keeps_the_symbol_table_to_itself() {
        let member = |name: &str, content: &[u8]| {
            let mut bytes = format!(
                "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                0,
                0,
                0,
                644,
                content.len()
            )
            .into_bytes();
            bytes.extend_from_slice(content);
            if content.len() % 2 == 1 {
                bytes.push(b'\n');
            }
            bytes
        };
        let mut data = b"!<arch>\n".to_vec();
        data.extend_from_slice(&member("/", &[0, 0, 0, 0]));
        data.extend_from_slice(&member("a.txt/", b"text"));
        let names: Vec<String> = files(&data, Pages::IN_MEMORY)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(names, ["a.txt"]);
    }
}
