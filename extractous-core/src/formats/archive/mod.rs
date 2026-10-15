//! Archives and compressed files as Tika 4.1's package module reads them, each entry handed to
//! the embedded document extractor: ZIP archives with `ZipParser` ([`zip`]).

mod zip;

pub(crate) use zip::ZipParser;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents};
    use crate::formats::Parser;

    const ROOTS: [&str; 2] = ["../test_files/documents", "../quality/corpus"];

    /// Tika's test archives the native path extracts differently until the framework carries
    /// what Tika's top-level handlers know into the embedded parses it hands to Tika, with why.
    const FRAMEWORK_LIMITED: [(&str, &str); 2] = [
        (
            "droste.zip",
            "nests archives deeper than SecureContentHandler's ten package entries, which Tika \
             counts from the top-level document and the embedded parse from its own start",
        ),
        (
            "testOpenOfficeInAZip.zip",
            "holds a document whose parser ends paragraphs outside the XHTML namespace, which \
             EventRecorder drops, so their replay ends each with a line break",
        ),
    ];

    /// Asserts parity on the documents of `parser` under `roots` it neither defers nor names in
    /// `limited`, each of which it must parse, and returns the names of those it defers.
    fn assert_parity(
        parser: &dyn Parser,
        tika_name: &str,
        roots: &[&Path],
        limited: &[(&str, &str)],
    ) -> Vec<String> {
        let (parsed, deferred) = documents(parser, roots);
        eprintln!(
            "{tika_name}: {} parsed, deferred {deferred:?}",
            parsed.len()
        );
        let (excluded, compared): (Vec<PathBuf>, Vec<PathBuf>) = parsed
            .into_iter()
            .partition(|path| limited.iter().any(|(name, _)| path.ends_with(name)));
        assert_eq!(
            excluded.len(),
            limited.len(),
            "parsed only {excluded:?} of the documents excluded from the comparison"
        );
        let paths: Vec<&Path> = compared.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, tika_name);
        deferred
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn extracts_zip_archives_as_tikas_path_does() {
        let roots: Vec<&Path> = ROOTS.iter().map(Path::new).collect();
        let deferred = assert_parity(
            &ZipParser,
            "org.apache.tika.parser.pkg.ZipParser",
            &roots,
            &[],
        );
        assert_eq!(deferred, ["spanned.z01"]);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_zip_archives_as_tikas_path_does() {
        let deferred = assert_parity(
            &ZipParser,
            "org.apache.tika.parser.pkg.ZipParser",
            &[&conformance_corpus()],
            &FRAMEWORK_LIMITED,
        );
        assert_eq!(
            deferred,
            [
                "moby.zip",
                "test-documents-spanned.z01",
                "test-documents-spanned.zip",
                "testZip_with_DataDescriptor.zip"
            ]
        );
    }
}
