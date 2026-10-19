//! What Tika's composite parser reads, in the configuration dijo's former Java
//! implementation gave it (`TikaNativeMain.createParser`): the types it has a parser for, where
//! [`super::parser_for`], walking a document's types as `CompositeParser.getParser` does, stops
//! for a type no native parser reads.

mod parsers;

use crate::mime::MediaType;
use parsers::TIKA_PARSERS;

/// The types Tika's detection names that its default types do not declare and that no parser of
/// Tika's reads, so that its composite parser falls back to its `EmptyParser`: an MS Graph chart
/// or an equation an Office document embeds, an OOXML package of no more specific type (a theme),
/// and an OS/2 bitmap array.
const UNPARSED_UNDECLARED: [&str; 4] = [
    "application/vnd.ms-equation",
    "application/vnd.ms-graph",
    "application/vnd.openxmlformats-officedocument",
    "image/x-os2-graphics",
];

/// Whether `kind` is one of the [`UNPARSED_UNDECLARED`] types.
pub(super) fn is_unparsed_undeclared(kind: &MediaType) -> bool {
    UNPARSED_UNDECLARED.contains(&kind.as_str())
}

/// Whether Tika's composite parser has a parser of its own for `kind`, rather than the one of a
/// type `kind` specializes.
pub(super) fn tika_reads(kind: &MediaType) -> bool {
    TIKA_PARSERS
        .binary_search_by(|(listed, _)| (*listed).cmp(kind.as_str()))
        .is_ok()
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::formats::{self, PARSERS};
    use crate::mime;

    const EMPTY_PARSER: &str = "org.apache.tika.parser.EmptyParser";

    #[test]
    fn falls_back_to_the_empty_parser_for_a_type_tika_has_no_parser_for() {
        let parser = formats::parser_for("application/x-berkeley-db").unwrap();
        assert_eq!(parser.tika_name(), EMPTY_PARSER);
        let parser = formats::parser_for("not a media type").unwrap();
        assert_eq!(parser.tika_name(), EMPTY_PARSER);
        assert!(PARSERS
            .iter()
            .any(|parser| parser.tika_name() == EMPTY_PARSER));
    }

    /// The undeclared types Tika's detection names that no parser of Tika's reads fall back to
    /// the empty parser, as Tika's composite parser does, rather than fail as unsupported (#119).
    #[test]
    fn falls_back_to_the_empty_parser_for_an_undeclared_type_tika_detects() {
        for kind in UNPARSED_UNDECLARED {
            assert!(!mime::declares(&MediaType::named(kind)), "{kind}");
            assert_eq!(formats::parser_for(kind).unwrap().tika_name(), EMPTY_PARSER);
        }
    }

    /// Tika's test documents of types its detection names that no parser of Tika's reads: an
    /// OOXML theme and an OS/2 bitmap array (#144).
    const UNPARSED_DOCUMENTS: [&str; 2] = [
        "tika-parsers-standard/tika-parsers-standard-modules/tika-parser-microsoft-module/src/test/resources/test-documents/testPPT.thmx",
        "tika-parsers-standard/tika-parsers-standard-integration-tests/src/test/resources/test-documents/testOS2BitmapArray",
    ];

    /// The documents of Tika's that its composite parser reads with the empty parser, as their
    /// detected types say, are read with the empty parser, rather than fail as unsupported (#144).
    #[test]
    #[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
    fn reads_with_the_empty_parser_the_documents_tika_reads_with_none() {
        for document in UNPARSED_DOCUMENTS {
            let data =
                std::fs::read(formats::testing::conformance_corpus().join(document)).unwrap();
            let detected = mime::detect(&data, mime::Origin::Bytes)
                .unwrap()
                .media_type
                .to_string();
            let parser = formats::parser_for(&detected).unwrap();
            assert_eq!(parser.tika_name(), EMPTY_PARSER, "{document}: {detected}");
        }
    }

    #[test]
    fn walks_to_the_supertype_tika_reads_a_type_as() {
        assert_eq!(
            formats::parser_for("text/x-matlab").unwrap().name(),
            formats::parser_for("text/plain").unwrap().name()
        );
        assert!(formats::parser_for("application/x-activemime").is_err());
        assert_eq!(
            formats::parser_for("application/x-hwp-v5").unwrap().name(),
            "dijo::HwpV5Parser"
        );
        assert!(TIKA_PARSERS.is_sorted());
    }
}
