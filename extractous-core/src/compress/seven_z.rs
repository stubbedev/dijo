//! 7z archives, which sevenz-rust2 reads, given as commons-compress's `SevenZFile` gives Tika's
//! `SevenZParser` their files: in the order the archive lists them, with their times, under
//! the memory limit Tika sets for the LZMA and LZMA2 coders. An archive needing a password
//! stays with Tika's path, which fails on it as it has none.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use sevenz_rust2::{ArchiveEntry, ArchiveReader, EncoderMethod, NtTime, Password};

use super::{throws, ArchiveFile, Undecoded, MAX_DECODED, MEMORY_LIMIT_KIB};

/// The milliseconds from 1601, where NTFS times start, to 1970.
const NTFS_EPOCH_MILLIS: i64 = 11_644_473_600_000;

fn fail(error: sevenz_rust2::Error) -> Undecoded {
    throws(error.to_string())
}

/// `FileTimes.ntfsTimeToDate`: the milliseconds since 1970 of an NTFS time.
fn millis(time: NtTime) -> i64 {
    i64::try_from(u64::from(time) / 10_000).unwrap_or(i64::MAX) - NTFS_EPOCH_MILLIS
}

/// The memory in KiB the coders of `reader`'s archive need, as `LZMADecoder` and
/// `LZMA2Decoder` count it, beyond Tika's limit.
fn over_memory_limit(reader: &ArchiveReader<Cursor<&[u8]>>) -> Result<bool, Undecoded> {
    for coder in reader
        .archive()
        .blocks
        .iter()
        .flat_map(|block| &block.coders)
    {
        let properties = coder.properties();
        let needed = match coder.encoder_method_id() {
            EncoderMethod::ID_LZMA if properties.len() >= 5 => {
                let dict_size = u32::from_le_bytes(properties[1..5].try_into().expect("four"));
                lzma_rust2::lzma_get_memory_usage_by_props(dict_size, properties[0])
                    .map_err(|e| throws(e.to_string()))?
            }
            EncoderMethod::ID_LZMA2 if !properties.is_empty() => {
                let bits = u32::from(properties[0]);
                if bits > 40 {
                    return Err(throws("Unsupported LZMA2 property bits"));
                }
                let dict_size = if bits == 40 {
                    u32::MAX
                } else {
                    (2 | (bits & 1)) << (bits / 2 + 11)
                };
                lzma_rust2::lzma2_get_memory_usage(dict_size)
            }
            _ => continue,
        };
        if needed > MEMORY_LIMIT_KIB {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The files of the 7z archive `data`, or why they are not read natively.
pub(crate) fn files(data: &[u8]) -> Result<Vec<ArchiveFile>, Undecoded> {
    let mut reader = ArchiveReader::new(Cursor::new(data), Password::empty()).map_err(fail)?;
    if over_memory_limit(&reader)? {
        return Err(throws("a coder needs more memory than Tika allows"));
    }
    let mut contents: HashMap<*const ArchiveEntry, Vec<u8>> = HashMap::new();
    let mut decoded = 0usize;
    reader
        .for_each_entries(|entry, read| {
            let mut content = Vec::new();
            read.take(MAX_DECODED as u64 + 1)
                .read_to_end(&mut content)?;
            decoded = decoded.saturating_add(content.len());
            contents.insert(entry, content);
            Ok(decoded <= MAX_DECODED)
        })
        .map_err(fail)?;
    if decoded > MAX_DECODED {
        return Err(Undecoded::TooLarge);
    }
    Ok(reader
        .archive()
        .files
        .iter()
        .map(|file| ArchiveFile {
            name: file.name.clone(),
            size: file.size,
            modified_millis: file
                .has_last_modified_date
                .then(|| millis(file.last_modified_date)),
            created_millis: file.has_creation_date.then(|| millis(file.creation_date)),
            directory: file.is_directory,
            content: contents
                .remove(&(file as *const ArchiveEntry))
                .unwrap_or_default(),
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 7z archive of `entries`, LZMA2-compressed; a name ending in a slash is a directory's.
    pub(crate) fn seven_z(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        for (name, content) in entries {
            match name.strip_suffix('/') {
                Some(directory) => {
                    writer
                        .push_archive_entry::<&[u8]>(ArchiveEntry::new_directory(directory), None)
                        .unwrap();
                }
                None => {
                    writer
                        .push_archive_entry(ArchiveEntry::new_file(name), Some(*content))
                        .unwrap();
                }
            }
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn reads_the_files_in_the_order_the_archive_lists_them() {
        let data = seven_z(&[("dir/", b""), ("empty.txt", b""), ("dir/a.txt", b"text")]);
        let found: Vec<(String, bool, Vec<u8>)> = files(&data)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.directory, file.content))
            .collect();
        assert_eq!(
            found,
            [
                ("dir".to_owned(), true, Vec::new()),
                ("empty.txt".to_owned(), false, Vec::new()),
                ("dir/a.txt".to_owned(), false, b"text".to_vec()),
            ]
        );
    }

    #[test]
    fn refuses_what_needs_a_password_or_is_no_archive() {
        let encrypted = std::fs::read("../test_files/documents/encrypted/password.7z").unwrap();
        assert!(files(&encrypted).is_err());
        assert!(files(b"7z\xBC\xAF\x27\x1C\x00\x04 not really").is_err());
    }
}
