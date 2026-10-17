//! TAR archives, which the tar crate reads, given as commons-compress's
//! `TarArchiveInputStream` gives Tika's `PackageParser` their files: names decoded as UTF-8
//! with `?` for what is malformed, a name from a GNU long name or a PAX path without its
//! leading slashes, and a directory being an entry of the directory type or a name ending in a
//! slash. What commons-compress reads differently stays with Tika's path: global PAX headers
//! that set more than a comment, and PAX sparse files, which commons-compress reassembles.

use std::io::Read;

use super::tar_header::normalize;
use super::zip::decode_name;
use super::{throws, ArchiveFile, Undecoded, MAX_DECODED};

/// The global PAX header key that commons-compress keeps without applying it.
const COMMENT: &str = "comment";
/// The directory GNU tar names PAX sparse files in, `%d/GNUSparseFile.%p/%f`.
const PAX_SPARSE_DIRECTORY: &str = "GNUSparseFile.";

fn fail(error: std::io::Error) -> Undecoded {
    throws(error.to_string())
}

/// The files of the TAR archive `data`, or why they are not read natively.
pub fn files(data: &[u8]) -> Result<Vec<ArchiveFile>, Undecoded> {
    let mut archive = tar::Archive::new(data);
    let mut files = Vec::new();
    for entry in archive.entries().map_err(fail)? {
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
        let mut content = Vec::new();
        if !directory {
            (&mut entry)
                .take(MAX_DECODED as u64 + 1)
                .read_to_end(&mut content)
                .map_err(fail)?;
            if content.len() > MAX_DECODED {
                return Err(Undecoded::TooLarge);
            }
            if content.len() as u64 != entry.size() {
                return Err(throws("Truncated TAR archive"));
            }
        }
        files.push(ArchiveFile {
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
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::super::tar_header::tests::{header, pax, tar};
    use super::*;

    fn names_and_contents(data: &[u8]) -> Vec<(String, bool, Vec<u8>)> {
        files(data)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.directory, file.content))
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
        let names: Vec<String> = files(&data).unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["dir/", "after.txt"]);
    }

    #[test]
    fn leaves_what_commons_compress_reads_differently_to_tika() {
        let global = tar(&[
            (b"global", b'g', &pax(&[("path", "global.txt")])),
            (b"file", b'0', b"data"),
        ]);
        assert!(matches!(files(&global), Err(Undecoded::Unported(_))));
        let data = tar(&[(b"a.txt", b'0', b"content"), (b"b.txt", b'0', b"more")]);
        assert!(files(&data[..512 + 3]).is_err());
    }
}
