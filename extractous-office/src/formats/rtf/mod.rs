//! RTF documents, as Tika's `RTFParser` reads them: the text of the body in paragraphs, list
//! items, bold and italic runs and hyperlinks, decoded with the code pages the document names,
//! the title, authors, dates and counts of its `\info` group as metadata, and its pictures and
//! embedded objects parsed as documents of their own.

mod charsets;
mod compressed;
mod embedded;
mod extractor;
mod html;
mod input;

pub(crate) use compressed::decompress;
pub(crate) use html::decapsulate;

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use extractor::Interrupt;

const MEDIA_TYPE: &str = "application/rtf";

pub struct RtfParser;

impl Parser for RtfParser {
    fn name(&self) -> &'static str {
        "extractous::RTFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.rtf.RTFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// It reads the document once, writing its text as it goes and releasing what it has read.
    /// It holds the state of the groups it is in, the text Tika's `TextExtractor` holds until a
    /// control word or group writes it, and the picture or object being read, spooled to a file
    /// past what is read whole.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        Ok(Box::new(move |xhtml| write(input, pages, xhtml)))
    }
}

/// `RTFParser.parseInline` of `input` into another document's `xhtml`, its header's metadata
/// into fresh metadata.
pub(crate) fn write_inline(input: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
    extractor::extract_inline(input, xhtml, TikaMetadata::new()).map_err(Interrupt::into_failure)
}

/// Parses as `RTFParser.parse`, which ends the document even when the parse fails.
fn write<'a>(input: &'a [u8], pages: Pages<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
    let Err(interrupt) = extractor::extract(input, pages, xhtml) else {
        return Ok(());
    };
    xhtml.end_document()?;
    Err(interrupt.into_failure())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    #[cfg(feature = "tika")]
    const TIKA_PARSER: &str = "org.apache.tika.parser.microsoft.rtf.RTFParser";
    /// Small documents for the corners of `TextExtractor` the corpora do not reach.
    #[cfg(feature = "tika")]
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
        r"{\rtf1\ansi\ansicpg57002\pard \'a4\'a6\'e9 \'b3\'e8\'e8\par}",
    ];

    /// Snippets Tika fails on.
    #[cfg(feature = "tika")]
    const FAILING_SNIPPETS: &[&str] = &[
        r"{\rtf1\ansi\pard a\bin10 abc}",
        r"{\rtf1{\info{\nofpages1}{\nofpages2}}\pard x\par}",
        r"{\rtf1\ansi{\*\listtable{\list{\listlevel\levelnfc23}\listid7}}\pard\ls7\lslvl12 one\par}",
    ];

    /// Snippets with pictures and objects, which Tika parses as documents of their own.
    #[cfg(feature = "tika")]
    const EMBEDDED_SNIPPETS: &[&str] = &[
        r"{\rtf1\ansi\pard x{\pict\pngblip 89504e47}\par y\par}",
        r"{\rtf1\ansi\pard x{\*\shppict{\pict\wmetafile8 0102}}\par}",
        r"{\rtf1\ansi\pard {\b x{\pict\wbitmap0 0102}y}\par}",
        r"{\rtf1\ansi\pard x{\object{\*\objdata 01050000 02000000 07000000 50427275736800 00000000 00000000 01000000 ff}}\par}",
        r"{\rtf1\ansi\pard x{\object{\*\objdata 01050000 02000000 0f000000 576f72642e446f63756d656e742e3800 00000000 00000000 00000000}}\par}",
        r"{\rtf1\ansi\pard x{\object{\*\objdata 01050000 02000000 0f000000 576f72642e446f63756d656e742e3800 00000000 00000000 01004001 00}{\pict 00}}z\par}",
        r"{\rtf1\ansi\pard x{\object{\*\objdata 01050000 02000000 0f000000 576f72642e446f63756d656e742e3800 00000000 00000000 00}{\pict 00}}z\par}",
        r"{\rtf1\ansi\pard x{\object\objemb{\*\objdata 01050000 02000000 0f000000 576f72642e446f63756d656e742e3800 00000000 00000000 05000000 68656c6c6f}{\result{\pict\pngblip 89504e470d0a1a0a}}}\par}",
        r"{\rtf1\ansi\pard {\pict{\*\picprop{\sp{\sn wzDescription}{\sv C:\\pics\\a.txt}}{\sp{\sn wzName}{\sv \'e6\u248?}}}68656c6c6f}\par}",
        r"{\rtf1\ansi\pard {\pict{\*\picprop{\sp{\sn wzDescription}{\sv }}}68656c6c6f}{\pict 68656c6c6f}\par}",
        r"{\rtf1\ansi\pard x{\pict\bin99999999 }\par}",
        r"{\rtf1\ansi\pard x{\pict\bin10 ab}\par}",
        r"{\rtf1\ansi\pard x{\pict\bin3 abc}{\pict}\par}",
        r"{\rtf1\ansi\pard x{\pict 0102\par y}z\par}",
        r"{\rtf1\ansi x{\pict\pngblip 89504e47",
    ];

    /// Snippets written to files of their own, in a directory removed when they are dropped.
    #[cfg(feature = "tika")]
    struct SnippetFiles {
        directory: PathBuf,
        files: Vec<PathBuf>,
    }

    #[cfg(feature = "tika")]
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

    #[cfg(feature = "tika")]
    impl Drop for SnippetFiles {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    #[cfg(feature = "tika")]
    fn as_paths(paths: &[PathBuf]) -> Vec<&Path> {
        paths.iter().map(PathBuf::as_path).collect()
    }

    /// The XHTML the native path extracts from `rtf`.
    fn extract(rtf: &[u8]) -> String {
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(rtf)
            .unwrap()
            .0
    }

    #[cfg(feature = "tika")]
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
        assert_same_as_tika(&undated.paths(), TIKA_PARSER);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_snippets_as_tikas_path_does() {
        let snippets = SnippetFiles::new("same", SNIPPETS);
        assert_same_as_tika(&snippets.paths(), TIKA_PARSER);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn parses_pictures_and_objects_as_tikas_path_does() {
        let snippets = SnippetFiles::new("embedded", EMBEDDED_SNIPPETS);
        assert_same_as_tika(&snippets.paths(), TIKA_PARSER);
    }

    /// An RTF document embedding `data` as an object of `class`, written as hex.
    #[cfg(feature = "tika")]
    fn with_object(class: &str, data: &[u8]) -> Vec<u8> {
        let mut object = Vec::new();
        object.extend_from_slice(&0x0501u32.to_le_bytes());
        object.extend_from_slice(&2u32.to_le_bytes());
        let class = format!("{class}\0");
        for part in [class.as_bytes(), b"", b"", data] {
            object.extend_from_slice(&(part.len() as u32).to_le_bytes());
            object.extend_from_slice(part);
        }
        let hex: String = object.iter().map(|byte| format!("{byte:02x}")).collect();
        format!(r"{{\rtf1\ansi\pard before{{\object\objemb{{\*\objdata {hex}}}}}after\par}}")
            .into_bytes()
    }

    /// An OLE package of the file `name` holding `data`.
    #[cfg(feature = "tika")]
    fn package(name: &str, data: &[u8]) -> Vec<u8> {
        let mut bytes = vec![2, 0];
        for text in [name, "C:\\icon"] {
            bytes.extend_from_slice(text.as_bytes());
            bytes.push(0);
        }
        bytes.extend_from_slice(&[0, 0, 3, 0]);
        let path = format!("C:\\{name}");
        bytes.extend_from_slice(&(path.len() as u32 + 1).to_le_bytes());
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }

    #[cfg(feature = "tika")]
    #[test]
    fn unwraps_objects_as_tikas_path_does() {
        let compound = crate::ole2::tests::compound;
        let scratch = Scratch::new("rtf-objects");
        let documents = [
            scratch.write(
                "contents.rtf",
                &with_object(
                    "Paint.Picture",
                    &compound(&["\u{1}CompObj", "CONTENTS", "\u{3}ObjInfo"], b"plain text"),
                ),
            ),
            scratch.write(
                "storage.rtf",
                &with_object("Word.Document.8", &compound(&["WordDocument"], b"x")),
            ),
            scratch.write(
                "unknown.rtf",
                &with_object("Some.Thing", &compound(&["Stuff"], b"x")),
            ),
            scratch.write(
                "package.rtf",
                &with_object(
                    "Package",
                    &package(
                        "a.html",
                        b"<html>\n\t<body>\n\t\t<h1>Head</h1>\n\t\t<p>x</p>\n\t</body></html>",
                    ),
                ),
            ),
        ];
        let documents: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&documents, TIKA_PARSER);
    }

    /// A document too large to read whole is read once, in place, its text written as it goes
    /// and what it has read released, in memory that does not grow with it.
    #[test]
    fn extracts_documents_too_large_to_read_whole_in_bounded_memory() {
        use crate::large::{bodies_past_limit, LargeFile};
        let filler = "0123456789abcdef".repeat(4096);
        let body = |index: u64| {
            format!("{{\\*\\datastore {filler}}}\\pard Linje {index} i logbogen\\par\n")
                .into_bytes()
        };
        let count = bodies_past_limit(body(0).len());
        let file = LargeFile::new("large.rtf", br"{\rtf1\ansi ", count, body, b"}");
        let (text, _) = file.extract("extractous::RTFParser", |path| {
            crate::Extractor::new().extract_file_to_string(path)
        });
        assert!(text.contains("Linje 0 i logbogen\n"), "{text}");
        let last = format!("Linje {} i logbogen", count - 1);
        assert!(
            text.trim_end().ends_with(&last),
            "{}",
            &text[text.len() - 200..]
        );
    }

    #[test]
    fn parses_pictures_as_embedded_documents() {
        let xhtml = extract(br"{\rtf1\ansi\pard x{\pict\pngblip 89504e470d0a1a0a}y\par}");
        assert!(
            xhtml.contains(r#"<div class="package-entry"><h1>embedded-0.png</h1>"#),
            "{xhtml}"
        );
        assert!(xhtml.contains("xy"), "{xhtml}");
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

    #[cfg(feature = "tika")]
    #[test]
    fn fails_where_tikas_path_fails() {
        assert_fails_as_tika(&SnippetFiles::new("failing", FAILING_SNIPPETS).paths());
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_the_conformance_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&RtfParser, &[&conformance_corpus()]);
        assert_eq!(deferred, Vec::<PathBuf>::new());
        assert!(parsed.len() >= 43, "{} documents", parsed.len());
        assert_same_as_tika(&as_paths(&parsed), TIKA_PARSER);
    }
}
