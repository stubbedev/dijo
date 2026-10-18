//! The owner files Office leaves beside a document it has open, `~$name`, as Tika's
//! `MSOwnerFileParser` reads them: the user's name as the last author, first in ASCII and then
//! in UTF-16, which must be as long.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::java;
use crate::text::charset::Charset;

/// `ASCII_CHUNK_LENGTH`: the length byte and the ASCII name it counts.
const ASCII_CHUNK_LENGTH: usize = 54;
/// `TikaCoreProperties.MODIFIER`.
const MODIFIER: &str = "meta:last-author";

/// Tika's `MSOwnerFileParser`.
pub struct MsOwnerFileParser;

impl Parser for MsOwnerFileParser {
    fn name(&self) -> &'static str {
        "extractous::MSOwnerFileParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.MSOwnerFileParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-ms-owner"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let (names, thrown) = names(input);
        Ok(Box::new(move |xhtml| {
            for name in &names {
                xhtml.metadata_mut().set(MODIFIER, name.as_str());
            }
            thrown.map_or(Ok(()), |thrown| Err(Failure::Malformed(thrown.to_owned())))
        }))
    }
}

/// The names `parse` sets, in order, and the exception that ends it where one does.
fn names(input: &[u8]) -> (Vec<String>, Option<&'static str>) {
    let Some(chunk) = input.get(..ASCII_CHUNK_LENGTH) else {
        return (
            Vec::new(),
            Some("EOFException: the ASCII chunk is cut short"),
        );
    };
    let length = chunk[0] as i8;
    let Ok(length) = usize::try_from(length) else {
        return (Vec::new(), Some("ascii name length must be >= 0"));
    };
    if length > ASCII_CHUNK_LENGTH {
        return (Vec::new(), Some("ascii name length must be < 55"));
    }
    let Some(ascii) = chunk.get(1..1 + length) else {
        return (
            Vec::new(),
            Some("StringIndexOutOfBoundsException: the ASCII name runs past its chunk"),
        );
    };
    let names = vec![java::ascii(ascii)];
    if input.get(ASCII_CHUNK_LENGTH).copied().map(usize::from) != Some(length) {
        return (
            names,
            Some("Ascii name length should be the same as the unicode length"),
        );
    }
    let start = ASCII_CHUNK_LENGTH + 2;
    let Some(unicode) = input.get(start..start + length * 2) else {
        return (names, Some("EOFException: the UTF-16 name is cut short"));
    };
    let unicode = Charset::named("UTF-16LE")
        .decode(unicode)
        .expect("the JDK decoders decode UTF-16LE");
    (vec![names[0].clone(), unicode], None)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

    fn owner(ascii: &[u8], unicode: &str, unicode_length: u8) -> Vec<u8> {
        let mut file = vec![0u8; ASCII_CHUNK_LENGTH];
        file[0] = u8::try_from(ascii.len()).unwrap();
        file[1..=ascii.len()].copy_from_slice(ascii);
        file.extend_from_slice(&[unicode_length, 0]);
        file.extend(unicode.encode_utf16().flat_map(u16::to_le_bytes));
        file.resize(162, 0x20);
        file
    }

    #[test]
    fn reads_owner_names_as_tika_does() {
        assert_eq!(
            names(&owner(b"S\xf8ren", "Søren", 5)),
            (vec!["S\u{FFFD}ren".to_owned(), "Søren".to_owned()], None)
        );
        let mut cut = owner(b"heidi", "heidi", 5);
        cut.truncate(60);
        assert_eq!(names(&cut).0, ["heidi"]);
        assert!(names(&cut).1.is_some());
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_owner_files_as_tikas_path_does() {
        let scratch = Scratch::new("ms-owner");
        let read = scratch.write(
            "~$rapport.docx",
            &owner(b"Hans-Henrik\xf8", "Hans-Henrikø", 12),
        );
        let failing = scratch.write("~$notat.docx", &owner(b"Mette", "Mette", 5)[..60]);
        let (parsed, deferred) = documents(&MsOwnerFileParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        assert_same_as_tika(&[read.as_path()], MsOwnerFileParser.tika_name());
        assert_fails_as_tika(&[failing.as_path()]);
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_owner_file_as_tikas_path_does() {
        let (parsed, deferred) = documents(&MsOwnerFileParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 1, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, MsOwnerFileParser.tika_name());
    }
}
