//! Unix `ar` archives, which the ar crate reads, given as commons-compress's
//! `ArArchiveInputStream` gives Tika's `PackageParser` their files: GNU and BSD long names
//! resolved, names decoded as US-ASCII with U+FFFD for each byte beyond it.
//!
//! The ar crate keeps the GNU and BSD symbol tables to itself, while commons-compress gives them
//! to Tika as files, the GNU one without a name.

use std::io::Read;

use super::{ascii, throws, ArchiveFile, Undecoded, MAX_DECODED};

/// The files of the `ar` archive `data`, or why they are not read natively.
pub(crate) fn files(data: &[u8]) -> Result<Vec<ArchiveFile>, Undecoded> {
    let mut archive = ar::Archive::new(data);
    let mut files = Vec::new();
    while let Some(entry) = archive.next_entry() {
        let mut entry = entry.map_err(|e| throws(e.to_string()))?;
        let header = entry.header();
        let name = ascii(header.identifier());
        let size = header.size();
        let modified = i64::try_from(header.mtime()).unwrap_or(i64::MAX);
        let mut content = Vec::new();
        (&mut entry)
            .take(MAX_DECODED as u64 + 1)
            .read_to_end(&mut content)
            .map_err(|e| throws(e.to_string()))?;
        if content.len() > MAX_DECODED {
            return Err(Undecoded::TooLarge);
        }
        files.push(ArchiveFile {
            name,
            size,
            modified_millis: Some(modified.wrapping_mul(1000)),
            created_millis: None,
            directory: false,
            content,
        });
    }
    Ok(files)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An archive of `entries` in the GNU variant, names longer than 15 bytes in its `//` table.
    pub(crate) fn ar(entries: &[(&str, &[u8])]) -> Vec<u8> {
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
        let data = ar(&[("a.txt", b"odd"), ("a-rather-long-file-name.txt", b"long")]);
        let found: Vec<(String, Vec<u8>)> = files(&data)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.content))
            .collect();
        assert_eq!(
            found,
            [
                ("a.txt".to_owned(), b"odd".to_vec()),
                ("a-rather-long-file-name.txt".to_owned(), b"long".to_vec()),
            ]
        );
        assert!(files(b"<arch>\n").is_err());
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
        let names: Vec<String> = files(&data).unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["a.txt"]);
    }
}
