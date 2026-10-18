//! 7z archives, which sevenz-rust2 reads, given as commons-compress's `SevenZFile` gives Tika's
//! `SevenZParser` their files: in the order the archive lists them, with their times, under
//! the memory limit Tika sets for the LZMA and LZMA2 coders, encrypted ones decrypted with the
//! password the extraction was given. One that needs a password none was given for fails, as
//! Tika's path does; one a password does not decrypt stays with Tika's path, whose decoders
//! fail on its garbled data otherwise than sevenz-rust2's.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use sevenz_rust2::{ArchiveEntry, ArchiveReader, EncoderMethod, NtTime, Password};

use super::{throws, ArchiveFile, Decoding, Limited, Undecoded, CHUNK, MEMORY_LIMIT_KIB};
use crate::input::Input;

/// The milliseconds from 1601, where NTFS times start, to 1970.
const NTFS_EPOCH_MILLIS: i64 = 11_644_473_600_000;

fn fail(error: sevenz_rust2::Error) -> Undecoded {
    match error {
        sevenz_rust2::Error::PasswordRequired => Undecoded::Encrypted,
        sevenz_rust2::Error::MaybeBadPassword(_) => Undecoded::BadPassword,
        error => throws(error.to_string()),
    }
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

/// `FILE_ATTRIBUTE_UNIX_EXTENSION`: the attributes of an entry made on Unix, its mode in their
/// high 16 bits.
const UNIX_EXTENSION: u32 = 0x8000;

/// An entry's name: as stored when the entry was made on Unix, where `\` is a character of the
/// name, else with its backslashes turned into slashes, as Windows separates paths with them.
/// Tika turns every backslash of every name (#101).
fn entry_name(file: &ArchiveEntry) -> String {
    let unix = file.has_windows_attributes && file.windows_attributes & UNIX_EXTENSION != 0;
    if unix {
        file.name.clone()
    } else {
        file.name.replace('\\', "/")
    }
}

/// The files of the 7z archive `data`, decrypted with `password` where it is encrypted, each
/// decoded to a spool up to the [`decode_limit`](super::decode_limit) of the archive, or why
/// they are not read natively. The files end at one that decodes into more, which is cut there.
pub fn files(
    data: &[u8],
    password: Option<&str>,
) -> Result<Limited<Vec<ArchiveFile<'static>>>, Undecoded> {
    let password = password.map_or_else(Password::empty, Password::new);
    let mut reader = ArchiveReader::new(Cursor::new(data), password).map_err(fail)?;
    if over_memory_limit(&reader)? {
        return Err(throws("a coder needs more memory than Tika allows"));
    }
    let mut contents: HashMap<*const ArchiveEntry, Limited<Input<'static>>> = HashMap::new();
    let mut unspooled = None;
    reader
        .for_each_entries(|entry, read| {
            let decoded = decode(read, data.len())?;
            let content = match decoded.and_then(Decoding::finish) {
                Ok(content) => content,
                Err(undecoded) => {
                    unspooled = Some(undecoded);
                    return Ok(false);
                }
            };
            let whole = matches!(content, Limited::Whole(_));
            contents.insert(entry, content);
            Ok(whole)
        })
        .map_err(fail)?;
    if let Some(undecoded) = unspooled {
        return Err(undecoded);
    }
    let mut files = Vec::new();
    for file in &reader.archive().files {
        let content = contents
            .remove(&(file as *const ArchiveEntry))
            .unwrap_or(Limited::Whole(Input::Borrowed(&[])));
        let file = content.map(|content| ArchiveFile {
            name: entry_name(file),
            size: file.size,
            modified_millis: file
                .has_last_modified_date
                .then(|| millis(file.last_modified_date)),
            created_millis: file.has_creation_date.then(|| millis(file.creation_date)),
            directory: file.is_directory,
            content,
        });
        if let Some(cut) = file.add_to(&mut files) {
            return Ok(cut);
        }
    }
    Ok(Limited::Whole(files))
}

/// What `read`, an entry of an archive of `archive` bytes, decodes into, or why it is not
/// spooled. Its decoder's failures are the reader's, which sevenz-rust2 tells a bad password by.
fn decode(
    read: &mut dyn Read,
    archive: usize,
) -> Result<Result<Decoding<crate::input::Spool>, Undecoded>, sevenz_rust2::Error> {
    let mut decoding = Decoding::spooled(archive);
    let mut chunk = vec![0; CHUNK];
    loop {
        let length = read.read(&mut chunk)?;
        if length == 0 {
            return Ok(Ok(decoding));
        }
        if let Err(undecoded) = decoding.write(&chunk[..length]) {
            return Ok(Err(undecoded));
        }
        if decoding.is_cut() {
            return Ok(Ok(decoding));
        }
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use super::*;

    /// A 7z archive of `entries`, LZMA2-compressed; a name ending in a slash is a directory's.
    pub fn seven_z(entries: &[(&str, &[u8])]) -> Vec<u8> {
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
        let found: Vec<(String, bool, Vec<u8>)> = files(&data, None)
            .and_then(Limited::whole)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.directory, file.content.to_vec()))
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
    fn decrypts_with_the_password_and_refuses_what_is_no_archive() {
        let encrypted = std::fs::read("../test_files/documents/encrypted/password.7z").unwrap();
        assert_eq!(files(&encrypted, None), Err(Undecoded::Encrypted));
        assert_eq!(
            files(&encrypted, Some("wrong")),
            Err(Undecoded::BadPassword)
        );
        assert!(!files(&encrypted, Some("extractous"))
            .and_then(Limited::whole)
            .unwrap()
            .is_empty());
        assert!(files(b"7z\xBC\xAF\x27\x1C\x00\x04 not really", None).is_err());
    }
}
