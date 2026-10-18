//! MicroStation V8 drawings, as Tika's `DGN8Parser` reads them: the summary information of the
//! compound document as metadata, and an empty body.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::Summaries;
use crate::formats::poi::{Thrown, Unread};
use crate::formats::{Decline, Footprint, Parser, Prepared};
use crate::ole2::{Ole2, Unreadable};

/// Tika's `DGN8Parser`.
pub struct Dgn8Parser;

impl Parser for Dgn8Parser {
    fn name(&self) -> &'static str {
        "extractous::DGN8Parser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.dgn.DGN8Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/vnd.dgn; version=8"]
    }

    /// It reads only the directory of the compound document and its summary information, in
    /// place.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Fails a drawing POI does not open.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let file = Ole2::open(input).map_err(|Unreadable(reason)| {
            Unread::from(Thrown(format!(
                "POIFSFileSystem cannot open the compound document: {reason}"
            )))
            .declined()
        })?;
        let summaries = Summaries::read(&file, file.root())?;
        Ok(Box::new(move |xhtml| {
            summaries.write(xhtml.metadata_mut());
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    #[cfg(feature = "tika")]
    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};
    use crate::ole2::tests::compound;

    const ENTRIES: [&str; 3] = ["DGN~H", "DGN~MF", "DGN~S"];

    #[test]
    fn writes_an_empty_body() {
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&compound(&ENTRIES, b"drawing"))
            .unwrap();
        assert_eq!(text.trim(), "");
        assert_eq!(metadata["Content-Type"], ["image/vnd.dgn; version=8"]);
        assert_eq!(metadata["tk:parsed-by"], ["extractous::DGN8Parser"]);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_drawings_as_tikas_path_does() {
        let scratch = Scratch::new("dgn");
        let drawing = scratch.write("drawing.dgn", &compound(&ENTRIES, b"drawing"));
        let (parsed, deferred) = documents(&Dgn8Parser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed, std::slice::from_ref(&drawing));
        assert_same_as_tika(&[drawing.as_path()], Dgn8Parser.tika_name());
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_drawings_as_tikas_path_does() {
        let (parsed, deferred) = documents(&Dgn8Parser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 1, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, Dgn8Parser.tika_name());
    }
}
