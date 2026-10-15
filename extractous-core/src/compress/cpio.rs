//! cpio archives in the new ASCII format, with or without checksums, which the cpio crate
//! reads, given as commons-compress's `CpioArchiveInputStream` gives Tika's `PackageParser`
//! their files: up to the `TRAILER!!!` entry, each checksum checked, a directory by its mode.
//!
//! The old ASCII and binary formats, which the crate does not read, stay with Tika's path. The
//! crate reads UTF-8 names, which commons-compress, decoding names as US-ASCII, rejects.

use std::io::Read;

use super::{throws, ArchiveFile, Undecoded, MAX_DECODED};

const NEW: &[u8; 6] = b"070701";
const NEW_CRC: &[u8; 6] = b"070702";
const FILE_TYPE: u32 = 0o170000;
const DIRECTORY: u32 = 0o040000;

fn fail(error: std::io::Error) -> Undecoded {
    throws(error.to_string())
}

/// The files of the cpio archive `data`, or why they are not read natively.
pub(crate) fn files(data: &[u8]) -> Result<Vec<ArchiveFile>, Undecoded> {
    if !data.starts_with(NEW) && !data.starts_with(NEW_CRC) {
        return Err(Undecoded::Unported("the old ASCII and binary cpio formats"));
    }
    let mut input = data;
    let mut files = Vec::new();
    loop {
        let mut reader = cpio::newc::Reader::new(input).map_err(fail)?;
        let entry = reader.entry().clone();
        if entry.is_trailer() {
            return Ok(files);
        }
        let mut content = Vec::new();
        (&mut reader)
            .take(MAX_DECODED as u64 + 1)
            .read_to_end(&mut content)
            .map_err(fail)?;
        if content.len() > MAX_DECODED {
            return Err(Undecoded::TooLarge);
        }
        if content.len() as u64 != u64::from(entry.file_size()) {
            return Err(throws("the input ends inside an entry's data"));
        }
        if let Some(checksum) = entry.checksum() {
            let sum = content
                .iter()
                .fold(0u32, |sum, &b| sum.wrapping_add(u32::from(b)));
            if sum != checksum {
                return Err(throws("CRC Error"));
            }
        }
        input = reader.finish().map_err(fail)?;
        let directory = entry.mode() & FILE_TYPE == DIRECTORY;
        files.push(ArchiveFile {
            name: entry.name().to_owned(),
            size: u64::from(entry.file_size()),
            modified_millis: Some(i64::from(entry.mtime()) * 1000),
            created_millis: None,
            directory,
            content: if directory { Vec::new() } else { content },
        });
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    /// An archive of `entries` in the new ASCII format with checksums, as `cpio -H crc` writes;
    /// a name ending in a slash is a directory's.
    pub(crate) fn cpio(entries: &[(&str, &[u8])]) -> Vec<u8> {
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
        let found: Vec<(String, bool, Vec<u8>)> = files(&data)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.directory, file.content))
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
        assert!(files(&data[..data.len() - 130]).is_err());
        let mut corrupt = data.clone();
        let at = corrupt.windows(7).position(|w| w == b"content").unwrap();
        corrupt[at] = b'C';
        assert!(files(&corrupt).is_err());
        assert!(matches!(files(b"070707rest"), Err(Undecoded::Unported(_))));
    }
}
