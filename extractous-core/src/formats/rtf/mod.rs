//! RTF documents, as Tika's `RTFParser` reads them: the text of the body in paragraphs, list
//! items, bold and italic runs and hyperlinks, decoded with the code pages the document names,
//! and the title, authors, dates and counts of its `\info` group as metadata. A document whose
//! pictures or embedded objects Tika would parse as documents of their own is left to Tika.

mod charsets;
mod embedded;
mod extractor;
mod input;

use crate::config::ParserConfigs;
use crate::formats::java_time::TimeZone;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{ContentHandler, Stop, Xhtml};
use crate::formats::{Failure, Parser};
use extractor::Interrupt;

const MEDIA_TYPE: &str = "application/rtf";
/// The control words that can defer a document, which is then read ahead to find out.
const READ_AHEAD: [&[u8]; 3] = [b"pict", b"objdata", b"ansicpg57002"];

pub(crate) struct RtfParser;

impl Parser for RtfParser {
    fn name(&self) -> &'static str {
        "extractous::RTFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Defers a document with a picture or an embedded object Tika would hand to its embedded
    /// document extractor, or fail to read; with text in the one code page the JDK decodes and
    /// this port does not, ISCII (`\ansicpg57002`); or with a creation date when Java's default
    /// time zone cannot be read. Only a document that names `\pict`, `\objdata`,
    /// `\ansicpg57002` or `\creatim` can, and only such a document is read ahead to find out.
    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        configs: &ParserConfigs,
    ) -> Option<String> {
        let undated = TimeZone::java_default().is_err();
        let read_ahead = input.iter().enumerate().any(|(at, &byte)| {
            let rest = &input[at + 1..];
            byte == b'\\'
                && (READ_AHEAD.iter().any(|word| rest.starts_with(word))
                    || (undated && rest.starts_with(b"creatim")))
        });
        if !read_ahead {
            return None;
        }
        let (_, _, result) = Xhtml::write(&mut Discard, TikaMetadata::new(), configs, |xhtml| {
            extractor::extract(input, xhtml)
        });
        match result {
            Err(Interrupt::Deferred(deferral)) => Some(deferral.reason().to_owned()),
            Ok(()) | Err(Interrupt::Failed(_) | Interrupt::Unchecked(_)) => None,
        }
    }

    /// Parses as `RTFParser.parse`, which ends the document even when the parse fails.
    fn parse(
        &self,
        input: &[u8],
        _configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
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
}

/// Takes a document's XHTML and drops it, for reading a document ahead.
struct Discard;

impl ContentHandler for Discard {
    fn start_document(&mut self) -> Result<(), Stop> {
        Ok(())
    }

    fn start_prefix_mapping(&mut self, _prefix: &str, _uri: &str) -> Result<(), Stop> {
        Ok(())
    }

    fn start_element(&mut self, _name: &str, _attributes: &[(&str, &str)]) -> Result<(), Stop> {
        Ok(())
    }

    fn end_element(&mut self, _name: &str) -> Result<(), Stop> {
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Stop> {
        Ok(())
    }

    fn ignorable_whitespace(&mut self, _text: &str) -> Result<(), Stop> {
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        Ok(())
    }
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
        r"{\rtf1\ansi\deff0{\info{\title Fyrets \u230?bog}{\author A}{\author B}{\keywords k1, k2}{\subject S}{\category Cat}{\comment C1}{\company Co}{\manager M}{\template T.dot}{\doccomm ignored}{\creatim\yr2006\mo3\dy1\hr10\min15}{\revtim\yr2007\mo4\dy2\hr11\min16}{\nofpages3}{\nofwords40}{\nofchars200}}\pard body\par}",
        r"{\rtf1\ansi{\info{\creatim\yr2024\mo3\dy31\hr2\min30}}\pard a\par}",
        r"{\rtf1\ansi{\info{\creatim\yr2024\mo10\dy27\hr2\min30}}\pard a\par}",
        r"{\rtf1\ansi{\info{\creatim\yr1500\mo14\dy-3\hr30\min-5}}\pard a\par}",
        r"{\rtf1\ansi{\info{\creatim}}\pard a\par}",
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

    /// The documents of `paths` the native parser reads, after asserting that it defers the
    /// others only for a creation date where Java's default time zone is unknown, as on
    /// Windows, whose registry lookup is not ported.
    fn read_natively<'p>(paths: &[&'p Path]) -> Vec<&'p Path> {
        let zone_unknown = TimeZone::java_default().is_err();
        let mut read = Vec::new();
        for path in paths {
            let input = std::fs::read(path).unwrap();
            match RtfParser.defers(&input, &TikaMetadata::new(), &ParserConfigs::default()) {
                None => read.push(*path),
                Some(reason) if zone_unknown && reason == embedded::Deferral::TimeZone.reason() => {
                }
                Some(reason) => panic!("{} defers: {reason}", path.display()),
            }
        }
        read
    }

    #[test]
    fn extracts_what_tikas_path_extracts() {
        let documents = read_natively(&[Path::new("../quality/corpus/prose.rtf")]);
        assert_same_as_tika(&documents, TIKA_PARSER);
    }

    #[test]
    fn extracts_snippets_as_tikas_path_does() {
        let snippets = SnippetFiles::new("same", SNIPPETS);
        let paths = snippets.paths();
        let documents = read_natively(&paths);
        if TimeZone::java_default().is_ok() {
            assert_eq!(documents.len(), paths.len());
        }
        assert_same_as_tika(&documents, TIKA_PARSER);
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
