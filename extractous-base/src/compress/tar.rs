//! TAR archives, which the tar crate reads, given as commons-compress's
//! `TarArchiveInputStream` gives Tika's `PackageParser` their files: names decoded as UTF-8
//! with `?` for what is malformed, a name from a GNU long name or a PAX path without its
//! leading slashes, and a directory being an entry of the directory type or a name ending in a
//! slash. What commons-compress reads differently stays with Tika's path: global PAX headers
//! that set more than a comment, and PAX sparse files, which commons-compress reassembles.

use std::io::Cursor;

use super::tar_header::normalize;
use super::zip::decode_name;
use super::{throws, ArchiveFile, Decoding, Limited, Undecoded};
use crate::input::{Input, Pages};

/// The global PAX header key that commons-compress keeps without applying it.
const COMMENT: &str = "comment";
/// The directory GNU tar names PAX sparse files in, `%d/GNUSparseFile.%p/%f`.
const PAX_SPARSE_DIRECTORY: &str = "GNUSparseFile.";

fn fail(error: std::io::Error) -> Undecoded {
    throws(error.to_string())
}

/// The files of the TAR archive `data`, whose pages are `pages`, or why they are not read
/// natively: a file's data in place, a GNU sparse file's reassembled to a spool, up to the
/// [`decode_limit`](super::decode_limit) of the archive, at which the files end.
pub fn files<'a>(
    data: &'a [u8],
    pages: Pages<'a>,
) -> Result<Limited<Vec<ArchiveFile<'a>>>, Undecoded> {
    let mut archive = tar::Archive::new(Cursor::new(data));
    let mut files = Vec::new();
    for entry in archive.entries_with_seek().map_err(fail)? {
        let mut entry = entry.map_err(fail)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            let extensions = entry.pax_extensions().map_err(fail)?;
            for extension in extensions.into_iter().flatten() {
                if extension.map_err(fail)?.key_bytes() != COMMENT.as_bytes() {
                    return Err(Undecoded::Unported(
                        "global PAX headers, which commons-compress applies to every entry",
                    ));
                }
            }
            continue;
        }
        let path = entry.path_bytes().into_owned();
        let mut name = decode_name(&path);
        if path != entry.header().path_bytes().as_ref() {
            name = normalize(&name);
        }
        if name.contains(PAX_SPARSE_DIRECTORY) {
            return Err(Undecoded::Unported("PAX sparse files"));
        }
        let size = if kind.is_gnu_sparse() {
            entry.header().entry_size().map_err(fail)?
        } else {
            entry.size()
        };
        let modified = entry.header().mtime().map_err(fail)?;
        let directory = kind.is_dir() || name.ends_with('/');
        let content = if directory {
            Limited::Whole(Input::Borrowed(&[]))
        } else if kind.is_gnu_sparse() {
            let mut decoding = Decoding::spooled(data.len());
            decoding.read_from(&mut entry)?;
            let content = decoding.finish()?;
            if matches!(&content, Limited::Whole(content) if content.len() as u64 != entry.size()) {
                return Err(truncated());
            }
            content
        } else {
            Limited::Whole(Input::Within(stored(data, &entry)?, pages))
        };
        let file = content.map(|content| ArchiveFile {
            name,
            size,
            modified_millis: Some(
                i64::try_from(modified)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1000),
            ),
            created_millis: None,
            directory,
            content,
        });
        if let Some(cut) = file.add_to(&mut files) {
            return Ok(cut);
        }
    }
    Ok(Limited::Whole(files))
}

fn truncated() -> Undecoded {
    throws("Truncated TAR archive")
}

/// The data of `entry`, a file the archive `data` stores whole, in place.
fn stored<'a>(
    data: &'a [u8],
    entry: &tar::Entry<'_, Cursor<&[u8]>>,
) -> Result<&'a [u8], Undecoded> {
    let start = usize::try_from(entry.raw_file_position()).map_err(|_| truncated())?;
    let length = usize::try_from(entry.size()).map_err(|_| truncated())?;
    start
        .checked_add(length)
        .and_then(|end| data.get(start..end))
        .ok_or_else(truncated)
}

#[cfg(test)]
mod tests {
    use super::super::tar_header::tests::{header, pax, tar};
    use super::*;

    fn names_and_contents(data: &[u8]) -> Vec<(String, bool, Vec<u8>)> {
        files(data, Pages::IN_MEMORY)
            .and_then(Limited::whole)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.directory, file.content.to_vec()))
            .collect()
    }

    #[test]
    fn reads_long_names_and_pax_paths() {
        let long = "d/".repeat(60) + "long.txt";
        let data = tar(&[
            (b"dir/", b'5', b""),
            (b"././@PaxHeader", b'x', &pax(&[("path", "/abs/pax.txt")])),
            (b"short", b'0', b"pax data"),
            (b"././@LongLink", b'L', format!("{long}\0").as_bytes()),
            (b"cut", b'0', b"long data"),
            (b"global", b'g', &pax(&[("comment", "a commit")])),
            (b"/kept", b'0', b"absolute"),
        ]);
        assert_eq!(
            names_and_contents(&data),
            [
                ("dir/".to_owned(), true, Vec::new()),
                ("abs/pax.txt".to_owned(), false, b"pax data".to_vec()),
                (long, false, b"long data".to_vec()),
                ("/kept".to_owned(), false, b"absolute".to_vec()),
            ]
        );
    }

    /// commons-compress does not skip a directory's data, and reads the next header from it.
    #[test]
    fn skips_a_directory_s_data_where_commons_compress_reads_on_in_it() {
        let mut data = header(b"dir/", b'5', 512);
        data.extend_from_slice(&header(b"inside.txt", b'0', 0));
        data.extend_from_slice(&tar(&[(b"after.txt", b'0', b"after")]));
        let names: Vec<String> = files(&data, Pages::IN_MEMORY)
            .and_then(Limited::whole)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(names, ["dir/", "after.txt"]);
    }

    #[test]
    fn leaves_what_commons_compress_reads_differently_to_tika() {
        let global = tar(&[
            (b"global", b'g', &pax(&[("path", "global.txt")])),
            (b"file", b'0', b"data"),
        ]);
        assert!(matches!(
            files(&global, Pages::IN_MEMORY),
            Err(Undecoded::Unported(_))
        ));
        let data = tar(&[(b"a.txt", b'0', b"content"), (b"b.txt", b'0', b"more")]);
        assert!(files(&data[..512 + 3], Pages::IN_MEMORY).is_err());
        let read = files(&data, Pages::IN_MEMORY)
            .and_then(Limited::whole)
            .unwrap();
        let Input::Within(stored, _) = read[1].content else {
            panic!("a stored file is not read in place");
        };
        assert_eq!(stored.as_ptr(), data[1536..].as_ptr());
    }
}
