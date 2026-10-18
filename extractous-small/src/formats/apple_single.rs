//! AppleSingle files, as Tika's `AppleSingleFileParser` reads them: the real name kept as the
//! original resource name, and the data fork handed to the embedded document extractor. Each
//! entry is read at its offset, where Tika's reads them in turn and fails on entries that
//! overlap (#118).

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Opened, Parser, Prepared};
use crate::java;

const DATA_FORK: u64 = 1;
const REAL_NAME: u64 = 3;
/// The version of AppleSingle Tika reads.
const VERSION: i32 = 0x0002_0000;
/// `TikaCoreProperties.ORIGINAL_RESOURCE_NAME`.
const ORIGINAL_RESOURCE_NAME: &str = "tk:orig-resource-name";

/// An entry of the descriptor: its ID and the range of its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    id: u64,
    offset: u64,
    length: u64,
}

/// What the parse hands the embedded document extractor: the data fork, and the real name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Content<'a> {
    data_fork: Option<&'a [u8]>,
    original_name: Option<String>,
}

/// Tika's `AppleSingleFileParser`.
pub struct AppleSingleFileParser;

impl Parser for AppleSingleFileParser {
    fn name(&self) -> &'static str {
        "extractous::AppleSingleFileParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.apple.AppleSingleFileParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/applefile"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let content = read(input).map_err(|thrown| Decline::Fails(Error::ParseError(thrown)))?;
        Ok(Box::new(move |xhtml| {
            let Some(data_fork) = content.data_fork else {
                return Ok(());
            };
            let mut metadata = TikaMetadata::new();
            if let Some(name) = &content.original_name {
                metadata.set(ORIGINAL_RESOURCE_NAME, name.as_str());
            }
            xhtml.parse_embedded_opened(data_fork, &metadata, Wrapping::PackageEntry, Opened::Alone)
        }))
    }
}

/// The stream Tika reads, which fails as `EndianUtils` and `IOUtils` fail where it ends short.
struct Stream<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Stream<'a> {
    fn take(&mut self, count: u64) -> Result<&'a [u8], String> {
        let end = usize::try_from(count)
            .ok()
            .and_then(|count| self.at.checked_add(count))
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| "EOFException: the AppleSingle file ends early".to_owned())?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N as u64)?.try_into().expect("N bytes"))
    }
}

/// `parse` up to the start of the document: the header and its entries, the real name of the last
/// real-name entry by offset, which must lie in the file, and the data fork, as much of it as
/// there is; the exception Tika throws before it writes anything where they do not read.
fn read(input: &[u8]) -> Result<Content<'_>, String> {
    let mut stream = Stream {
        bytes: input,
        at: 0,
    };
    stream.array::<4>()?;
    let version = i32::from_be_bytes(stream.array()?);
    if version != VERSION {
        return Err(format!(
            "Version should have been 0x00020000, but was:{version}"
        ));
    }
    stream.take(16)?;
    let count = i16::from_be_bytes(stream.array()?);
    let Ok(count) = u16::try_from(count) else {
        return Err(format!(
            "IllegalArgumentException: Illegal Capacity: {count}"
        ));
    };
    let mut entries = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let mut field =
            || -> Result<u64, String> { Ok(u64::from(u32::from_be_bytes(stream.array()?))) };
        let (id, offset, length) = (field()?, field()?, field()?);
        entries.push(Entry { id, offset, length });
    }
    if entries.is_empty() {
        return Err("AppleSingleFile missing field info".to_owned());
    }
    entries.sort_by_key(|entry| entry.offset);
    let original_name = entries
        .iter()
        .rfind(|entry| entry.id == REAL_NAME)
        .map(|entry| {
            usize::try_from(entry.offset)
                .ok()
                .zip(usize::try_from(entry.length).ok())
                .and_then(|(offset, length)| input.get(offset..offset.checked_add(length)?))
                .map(java::ascii)
                .ok_or_else(|| {
                    format!(
                        "EOFException: the real name at {} of {} bytes runs past the end of the \
                         AppleSingle file",
                        entry.offset, entry.length
                    )
                })
        })
        .transpose()?;
    let data_fork = entries
        .iter()
        .find(|entry| entry.id == DATA_FORK)
        .map(|entry| {
            let start = usize::try_from(entry.offset)
                .unwrap_or(usize::MAX)
                .min(input.len());
            let end = usize::try_from(entry.length)
                .ok()
                .and_then(|length| start.checked_add(length))
                .unwrap_or(usize::MAX)
                .min(input.len());
            &input[start..end]
        });
    Ok(Content {
        data_fork,
        original_name,
    })
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

    fn apple_single(entries: &[(u32, &[u8])], data_fork_length: Option<u32>) -> Vec<u8> {
        let mut file = vec![0x00, 0x05, 0x16, 0x00, 0x00, 0x02, 0x00, 0x00];
        file.extend_from_slice(&[0; 16]);
        file.extend_from_slice(&u16::try_from(entries.len()).unwrap().to_be_bytes());
        let mut offset = 26 + 12 * entries.len();
        let mut data = Vec::new();
        for (id, bytes) in entries.iter().rev() {
            let length = match data_fork_length {
                Some(length) if *id == 1 => length,
                _ => u32::try_from(bytes.len()).unwrap(),
            };
            file.extend_from_slice(&id.to_be_bytes());
            file.extend_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
            file.extend_from_slice(&length.to_be_bytes());
            offset += bytes.len();
            data.extend_from_slice(bytes);
        }
        file.extend(data);
        file
    }

    #[test]
    fn extracts_apple_single_files_as_tikas_path_does() {
        let scratch = Scratch::new("apple-single");
        let text = "Sm\u{f8}rrebr\u{f8}d og kaffe\n".as_bytes();
        let read = [
            scratch.write(
                "named.txt",
                &apple_single(&[(3, b"Frokost\xe6.txt"), (9, &[0; 32]), (1, text)], None),
            ),
            scratch.write("overlong.txt", &apple_single(&[(1, text)], Some(4000))),
            scratch.write("forkless", &apple_single(&[(3, b"intet.txt")], None)),
        ];
        let mut cut = apple_single(&[(3, b"navn.txt"), (1, text)], None);
        cut.truncate(40);
        let failing = [
            scratch.write("cut", &cut),
            scratch.write("empty", &apple_single(&[], None)),
        ];
        let (parsed, deferred) = documents(&AppleSingleFileParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len() + failing.len(), "{parsed:?}");
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            AppleSingleFileParser.tika_name(),
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    #[test]
    fn reads_entries_that_overlap() {
        let data = b"named.txt and its text";
        let mut file = vec![0x00, 0x05, 0x16, 0x00, 0x00, 0x02, 0x00, 0x00];
        file.extend_from_slice(&[0; 16]);
        file.extend_from_slice(&2u16.to_be_bytes());
        let start = 26 + 2 * 12;
        for (id, length) in [(DATA_FORK, data.len()), (REAL_NAME, 9)] {
            for field in [id, start, u64::try_from(length).unwrap()] {
                file.extend_from_slice(&u32::try_from(field).unwrap().to_be_bytes());
            }
        }
        file.extend_from_slice(data);
        let content = read(&file).unwrap();
        assert_eq!(content.original_name.as_deref(), Some("named.txt"));
        assert_eq!(content.data_fork, Some(&data[..]));
        file.truncate(file.len() - data.len() + 4);
        assert!(read(&file).unwrap_err().contains("runs past the end"));
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_apple_single_file_as_tikas_path_does() {
        let (parsed, deferred) = documents(&AppleSingleFileParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 1, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, AppleSingleFileParser.tika_name());
    }
}
