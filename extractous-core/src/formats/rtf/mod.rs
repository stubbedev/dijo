//! RTF documents, as Tika's `RTFParser` reads them: the text of the body in paragraphs, list
//! items, bold and italic runs and hyperlinks, decoded with the code pages the document names,
//! and the title, authors, dates and counts of its `\info` group as metadata. A document whose
//! pictures or embedded objects Tika would parse as documents of their own is left to Tika.

mod charsets;
mod compressed;
mod embedded;
mod extractor;
mod html;
mod input;

pub(crate) use compressed::decompress;
pub(crate) use html::decapsulate;

use crate::config::ParserConfigs;
use crate::formats::embedded::ParseContext;
use crate::formats::handlers::Discard;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Deferral, Failure, Parser, Prepared};
use extractor::Interrupt;

const MEDIA_TYPE: &str = "application/rtf";
/// The control words that can defer a document, which is then read ahead to find out.
const READ_AHEAD: [&[u8]; 3] = [b"pict", b"objdata", b"ansicpg57002"];

pub(crate) struct RtfParser;

impl Parser for RtfParser {
    fn name(&self) -> &'static str {
        "extractous::RTFParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.rtf.RTFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Defers a document with a picture or an embedded object Tika would hand to its embedded
    /// document extractor, or fail to read; with text in the one code page the JDK decodes and
    /// this port does not, ISCII (`\ansicpg57002`). Only a document that names `\pict`,
    /// `\objdata` or `\ansicpg57002` can, and only such a document is read ahead to find out,
    /// which reads it twice.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        if let Some(reason) = deferral(input, configs) {
            return Err(Declined::Deferred(reason));
        }
        Ok(Box::new(move |xhtml| write(input, xhtml)))
    }
}

/// Why Tika's path should extract `input` rather than this parser, if it should.
pub(crate) fn deferral(input: &[u8], configs: &ParserConfigs) -> Option<Deferral> {
    let read_ahead = input.iter().enumerate().any(|(at, &byte)| {
        byte == b'\\'
            && READ_AHEAD
                .iter()
                .any(|word| input[at + 1..].starts_with(word))
    });
    if !read_ahead {
        return None;
    }
    let context = ParseContext::new(configs);
    let (_, _, result) = Xhtml::write(&mut Discard, TikaMetadata::new(), &context, |xhtml| {
        extractor::extract(input, xhtml)
    });
    match result {
        Err(Interrupt::Deferred(deferral)) => Some(deferral.reason().to_owned()),
        Ok(()) | Err(Interrupt::Failed(_) | Interrupt::Unchecked(_)) => None,
    }
}

/// `RTFParser.parseInline` of `input`, which [`deferral`] passed, into another
/// document's `xhtml`, its header's metadata into fresh metadata.
pub(crate) fn write_inline(input: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
    match extractor::extract_inline(input, xhtml, TikaMetadata::new()) {
        Ok(()) => Ok(()),
        Err(Interrupt::Failed(failure) | Interrupt::Unchecked(failure)) => Err(failure),
        Err(Interrupt::Deferred(deferral)) => Err(Failure::Malformed(format!(
            "The RTF document has {}, which leaves it to Tika",
            deferral.reason()
        ))),
    }
}

/// Parses as `RTFParser.parse`, which ends the document even when the parse fails.
fn write(input: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
    xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
    let failure = match extractor::extract(input, xhtml) {
        Ok(()) => return Ok(()),
        Err(Interrupt::Failed(failure) | Interrupt::Unchecked(failure)) => failure,
        Err(Interrupt::Deferred(deferral)) => Failure::Malformed(format!(
            "The RTF document has {}, which leaves it to Tika",
            deferral.reason()
        )),
    };
    xhtml.end_document()?;
    Err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents,
    };
    use std::path::{Path, PathBuf};

    const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.rtf.RTFParser";
    /// The conformance documents with pictures or embedded objects Tika parses.
    const DEFERRED: [&str; 4] = [
        "testBinControlWord.rtf",
        "testRTFEmbeddedFiles.rtf",
        "testRTFEmbeddedLink.rtf",
        "testRTFRegularImages.rtf",
    ];

    /// Small documents for the corners of `TextExtractor` the corpora do not reach.
    const SNIPPETS: &[&str] = &[
        r"{\rtf1\ansi\ansicpg1251\deff0{\fonttbl{\f0\fcharset204 Arial;}}\pard \'cf\'f0\u1080?\uc2\u1074 ab c\uc0\u1099 d\par}",
        r"{\rtf1\ansi{\fonttbl{\f0\fcharset238 A;}{\f1\fcharset0 B;}{\f2\fcharset128 C;}}\pard\f0 \'b9\f1\'b9{\f2\'82\'a0\'83\'4a}\par}",
        r"{\rtf1\ansi\pard \u-10179?\u-8704?\u55357?\par x\u56832?y\u0?\u31?\par}",
        r"{\rtf1\mac\pard \'8a\'9a\par}",
        r"{\rtf1\pc\pard \'81\'94\par}",
        r"{\rtf1\pca\pard \'d5\par}",
        r"{\rtf1\ansi\ansicpg10007\pard \'80\'df\par}",
        r"{\rtf1\ansi\ansicpg57003\pard a\'e8b\par}",
        r"{\rtf1\ansi\ansicpg932{\fonttbl{\f0\fnil\fcharset128 MS;}}\deff0\pard \'93\'fa\'96\'7b\par}",
        r"{\rtf1\ansi\pard \b bold \i both\b0  italic\i0  plain\par {\b grouped {\i nested}} after\par\plain x\b y\plain z\par}",
        r"{\rtf1\ansi\pard {\i a\par b}\par\b c{\b0 d}e\par}",
        r"{\rtf1\ansi{\*\listtable{\list{\listlevel\levelnfc23}{\listlevel\levelnfc0}\listid7}{\list{\listlevel\levelnfc0}\listid8}}{\*\listoverridetable{\listoverride\listid7\ls1}}\pard\ls7 one\par\pard\ls7 two\par\pard\ls8 three\par\pard after\par\pard\ls9 nine\par}",
        r"{\rtf1\ansi{\*\listtable{\list{\listlevel\levelnfc0}\listid3}}\pard{\listtext 1.}\ls3 one\par\pard{\listtext 2.}\ls3\lslvl0 two\par}",
        r#"{\rtf1\ansi\pard see {\field{\*\fldinst HYPERLINK "http://a.dk/"}{\fldrslt here}} now\par}"#,
        r#"{\rtf1\ansi\pard {\field{\*\fldinst { HYPERLINK \\l "anchor" }}{\fldrslt {\b bold} link}}\par}"#,
        r#"{\rtf1\ansi\pard {\field{\*\fldinst HYPERLINK "http://b.dk/"}{\fldrslt a\par b}} c\par}"#,
        r#"{\rtf1\ansi\pard {\field{\*\fldinst HYPERLINK "http://c.dk/"}{\fldrslt x {\field{\*\fldinst PAGEREF _Toc1 \\h}{\fldrslt 3}} y}} z\par}"#,
        r"{\rtf1\ansi\pard {\field{\*\fldinst PAGE}{\fldrslt 7}} page\par}",
        r"{\rtf1\ansi\pard \b {\field{\*\fldinst HYPERLINK http://d.dk}{\fldrslt \i in}} out\par}",
        r"{\rtf1\ansi\deff0{\info{\title Fyrets \u230?bog}{\author A}{\author B}{\keywords k1, k2}{\subject S}{\category Cat}{\comment C1}{\company Co}{\manager M}{\template T.dot}{\doccomm ignored}{\revtim\yr2007\mo4\dy2\hr11\min16}{\nofpages3}{\nofwords40}{\nofchars200}}\pard body\par}",
        r"{\rtf1\ansi\fromhtml1{\*\htmltag <p>}\htmlrtf {\htmlrtf0 visible}\htmlrtf0\par}",
        r"{\rtf1\ansi x\par",
        r"{\rtf1\ansi\pard x\par}trailing}",
        r"}{\rtf1\ansi\pard x\par}",
        r"{\rtf1\ansi\pard x}}}\par y}",
        r"{\rtf1\ansi\pard a\'",
        r"{\rtf1\ansi\pard a\'4",
        r"{\rtf1\ansi\pard a\'zz b\'4g c\par}",
        r"{\rtf1\ansi\pard a{",
        r"{\rtf1\ansi\pard a{\",
        r"{\rtf1\ansi\pard a\",
        r"{\rtf1\ansi\pard \upr{a \'e6}{\*\ud{b \u230?}}\par}",
        r"{\rtf1\ansi\pard text{\*\atnid AB}{\*\atnauthor Anna}\chatn{\*\annotation{\*\atnref 1}\pard note\par}more\par}",
        r"{\rtf1\ansi\pard \{a\}\\b\~c\-d\_e\tab f\line g\page h\column i\bullet\endash\emdash\enspace\qmspace\emspace\lquote\rquote\ldblquote\rdblquote\softline\softpage\softcolumn\par}",
        r"{\rtf1\ansi\trowd\cellx100\cellx200\pard\intbl a\cell b\cell\row\pard after\par}",
        r"{\rtf1\ansi\pard a\bin3 xyzb\par}",
        r"{\rtf1\ansi\pard a\uc-1\u66?bcd{e}f\par}",
        r"{\rtf1\ansi\pard a\u66\'41\'42c\uc2\u67\'41\'42d\par}",
        r"{\rtf1\ansi\pard a{\*\shppict x}{\shptxt shape}b\par}",
        r"{\rtf1\ansi\pard a\u65536?\u-65535?\u2147483647?\u99999999999?\par}",
        r"{\rtf1\ansi{\stylesheet{\s1 Heading;}}{\colortbl;\red0\green0\blue0;}\pard x\par}",
        r"{\rtf1\ansi\pard a\par\par\par b}",
        r"{\rtf1\ansi\pard\b a\par b\b0 c\par}",
        r"{\rtf1\ansi\pard\i a\cell b\i0\par}",
        r"{\rtf1\ansi\pard The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. \'e6\'f8\'e5\par}",
        r"{\rtf1\ansi\pard {\sp{\sn wzDescription}{\sv x.png}} y\par}",
        r"{\rtf1\ansi\pard a{\object\objemb{\*\objdata 01050000 01000000}{\result b}}\par}",
        r"{\rtf1\ansi\pard a{\object{\*\objdata 01050000 02000000 08000000 5061636b61676500 00000000 00000000 0c000000 0200410000430000000001000000}}c\par}",
        r"{\rtf1\ansi\pard \plain\ltrch\rtlch x\sect\sectd y\par}",
        "{\\rtf1\\ansi\\pard a\r\nb\rc\nd\\\r\ne\\\nf\\par}",
        r"{\rtf1\ansi\pard\'e6{\*\generator x}\'f8 \u8364?\'80\par}",
    ];

    /// Snippets Tika fails on.
    const FAILING_SNIPPETS: &[&str] = &[
        r"{\rtf1\ansi\pard a\bin10 abc}",
        r"{\rtf1{\info{\nofpages1}{\nofpages2}}\pard x\par}",
        r"{\rtf1\ansi{\*\listtable{\list{\listlevel\levelnfc23}\listid7}}\pard\ls7\lslvl12 one\par}",
    ];

    /// Snippets with a picture or object Tika parses.
    const DEFERRED_SNIPPETS: &[&str] = &[
        r"{\rtf1\ansi\pard x{\pict\pngblip 89504e47}\par}",
        r"{\rtf1\ansi\pard x{\*\shppict{\pict\wmetafile8 0102}}\par}",
        r"{\rtf1\ansi\pard x{\object{\*\objdata 01050000 02000000 07000000 50427275736800 00000000 00000000 01000000 ff}}\par}",
        r"{\rtf1\ansi\pard x{\pict\bin99999999 }\par}",
        r"{\rtf1\ansi\pard x{\pict\bin10 ab}\par}",
        r"{\rtf1\ansi\ansicpg57002\pard \'a4\par}",
    ];

    /// Snippets written to files of their own, in a directory removed when they are dropped.
    struct SnippetFiles {
        directory: PathBuf,
        files: Vec<PathBuf>,
    }

    impl SnippetFiles {
        fn new(name: &str, snippets: &[&str]) -> Self {
            let directory =
                std::env::temp_dir().join(format!("extractous-rtf-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&directory).unwrap();
            let files = snippets
                .iter()
                .enumerate()
                .map(|(index, snippet)| {
                    let path = directory.join(format!("snippet-{index}.rtf"));
                    std::fs::write(&path, snippet).unwrap();
                    path
                })
                .collect();
            Self { directory, files }
        }

        fn paths(&self) -> Vec<&Path> {
            as_paths(&self.files)
        }
    }

    impl Drop for SnippetFiles {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    fn as_paths(paths: &[PathBuf]) -> Vec<&Path> {
        paths.iter().map(PathBuf::as_path).collect()
    }

    /// Asserts that the native parser reads each of `paths` rather than deferring it.
    fn assert_read_natively(paths: &[&Path]) {
        for path in paths {
            let input = std::fs::read(path).unwrap();
            let deferral =
                RtfParser.defers(&input, &TikaMetadata::new(), &ParserConfigs::default());
            assert_eq!(deferral, None, "{}", path.display());
        }
    }

    /// The XHTML the native path extracts from `rtf`.
    fn extract(rtf: &[u8]) -> String {
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(rtf)
            .unwrap()
            .0
    }

    #[test]
    fn extracts_what_tikas_path_extracts() {
        // The document's creation date is all zeros, which Tika's lenient calendar makes
        // a date in year -0001 in the JVM's zone and the native parser leaves out (#65): it is compared
        // without the date, and read natively with it.
        const ZERO_DATE: &str = r"{\creatim\yr0\mo0\dy0\hr0\min0}";
        let prose = std::fs::read_to_string("../quality/corpus/prose.rtf").unwrap();
        assert!(prose.contains(ZERO_DATE));
        assert!(!extract(prose.as_bytes()).contains("dcterms:created"));
        let undated = SnippetFiles::new("prose", &[&prose.replace(ZERO_DATE, "")]);
        assert_read_natively(&undated.paths());
        assert_same_as_tika(&undated.paths(), TIKA_PARSER);
    }

    #[test]
    fn extracts_snippets_as_tikas_path_does() {
        let snippets = SnippetFiles::new("same", SNIPPETS);
        assert_read_natively(&snippets.paths());
        assert_same_as_tika(&snippets.paths(), TIKA_PARSER);
    }

    #[test]
    fn reads_creation_dates_as_utc_wall_clock_times() {
        // Tika reads them in the JVM's default zone, and makes something of any fields (#65).
        let created = |info: &str| {
            let xhtml = extract(format!(r"{{\rtf1\ansi{{\info{info}}}\pard a\par}}").as_bytes());
            let start = xhtml.find(r#"<meta name="dcterms:created" content=""#)?;
            let value = &xhtml[start + 38..];
            Some(value[..value.find('"').unwrap()].to_owned())
        };
        let date = |text: &str| Some(text.to_owned());
        assert_eq!(
            created(r"{\creatim\yr2006\mo3\dy1\hr10\min15}"),
            date("2006-03-01T10:15:00Z")
        );
        assert_eq!(
            created(r"{\creatim\yr2024\mo3\dy31\hr2\min30}"),
            date("2024-03-31T02:30:00Z")
        );
        assert_eq!(
            created(r"{\creatim\yr2024\mo2\dy29}"),
            date("2024-02-29T00:00:00Z")
        );
        assert_eq!(created(r"{\creatim\yr2023\mo2\dy29}"), None);
        assert_eq!(created(r"{\creatim\yr1500\mo14\dy-3\hr30\min-5}"), None);
        assert_eq!(created(r"{\creatim\yr2024\mo1\dy1\hr24}"), None);
        assert_eq!(created(r"{\creatim}"), None);
        assert_eq!(created(r"{\creatim\yr0\mo0\dy0\hr0\min0}"), None);
        // Each date group names its own fields: the revision date's do not fill in creation's.
        assert_eq!(created(r"{\revtim\yr2007\mo4\dy2}{\creatim\hr1}"), None);
        assert_eq!(
            created(r"{\creatim\yr2007\mo4\dy2}{\revtim\yr2008\mo5\dy3}"),
            date("2007-04-02T00:00:00Z")
        );
    }

    #[test]
    fn fails_where_tikas_path_fails() {
        assert_fails_as_tika(&SnippetFiles::new("failing", FAILING_SNIPPETS).paths());
    }

    #[test]
    fn defers_pictures_objects_and_iscii() {
        for snippet in DEFERRED_SNIPPETS {
            assert!(
                RtfParser
                    .defers(
                        snippet.as_bytes(),
                        &TikaMetadata::new(),
                        &ParserConfigs::default()
                    )
                    .as_deref()
                    .is_some(),
                "{snippet}"
            );
        }
        let unfinished = r"{\rtf1\ansi x{\pict\pngblip 89504e47";
        assert_eq!(
            RtfParser
                .defers(
                    unfinished.as_bytes(),
                    &TikaMetadata::new(),
                    &ParserConfigs::default()
                )
                .as_deref(),
            None
        );
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_the_conformance_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&RtfParser, &[&conformance_corpus()]);
        let mut deferred: Vec<&str> = deferred
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect();
        deferred.sort_unstable();
        let mut expected = DEFERRED;
        expected.sort_unstable();
        assert_eq!(deferred, expected);
        assert!(parsed.len() >= 39, "{} documents", parsed.len());
        assert_same_as_tika(&as_paths(&parsed), TIKA_PARSER);
    }
}
