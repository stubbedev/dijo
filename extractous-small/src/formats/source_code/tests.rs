#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

use super::*;
#[cfg(feature = "tika")]
use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

#[cfg(feature = "tika")]
const JAVA: &str = r#"/*
 * Licensed to the Apache Software Foundation (ASF).
 */
package dk.havn;

import java.util.List;

/**
 * A harbour's ships.
 *
 * @author Ane Bøgh
 * @AUTHOR   Bo Øster
 */
public final class Havn<T extends Skib> {
	private static final int MAX = 0x1F;
	private final List<T> skibe = List.of();
	// en linjekommentar
	public double last(long tons, char c) {
		assert tons > 0L : "tons";
		String s = "streng med \"citat\" og \\ backslash";
		return tons * 1.5e3f + 'x' + MAX + 017 + .5;
	}
	/* uafsluttet
	   kommentar */ int x;
	/**/ int y = a & b | c ^ d % e;
	@Override public String toString() { return "Havn<" + skibe + ">"; }
}
"#;

#[cfg(feature = "tika")]
const GROOVY: &str = r#"package dk.havn

/** Groovy-script
 * @author Carl Hansen
 */
def skibe = ['Dana', "Nordlys"] as List
def tekst = """flere
linjer"""
assert skibe.size() == 2
skibe.each { println "Skib: ${it}" }
"#;

#[cfg(feature = "tika")]
const CPP: &str = r#"namespace havn {
/// Doxygen: a ship
/// @param navn the name
class Skib {
public:
    explicit Skib(const char* navn) : navn_(navn) {}
    virtual ~Skib() = default;
private:
    const char* navn_ = "Dana";
    int last = 0x1F + 'c' + 1.5e3;
};
}
#define MAKS 10
/* afsluttet */ /** doxygen
 * blok */
"#;

#[test]
fn scans_tokens_as_jhighlights_scanners_do() {
    let scanner = Language::Java.scanner();
    let units: Vec<u16> = "public int x = 0x1F;\n".encode_utf16().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < units.len() {
        let (action, length) = scanner.next(&units, index, 0).unwrap();
        tokens.push((Language::Java.action(action).0, length));
        index += length;
    }
    assert_eq!(
        tokens,
        [
            (2, 6),
            (1, 1),
            (3, 3),
            (1, 1),
            (1, 1),
            (1, 1),
            (4, 1),
            (1, 1),
            (6, 4),
            (5, 1),
            (1, 1)
        ]
    );
    assert_eq!(author(" * @Author  Ane  "), Some("Ane"));
    assert_eq!(author("no author here"), None);
    assert_eq!(tabs_to_spaces("a\tbc\td"), "a   bc  d");
}

/// The code walked a chunk of lines at a time is the code walked whole: each line's markup,
/// parsed inside the `code` element alone, is what it is inside all of them.
#[test]
fn walks_code_a_chunk_at_a_time_as_whole() {
    let code = "/** Kode\u{0} med\t\"citat\" */\nint x = 1 < 2 & 3 > 4;\n\n/* uafsluttet\nkommentar */ ok\n"
        .repeat(CHUNK / 16);
    let mut renderer = Renderer::new(Language::Java);
    let mut whole: Vec<u16> = Vec::new();
    for line in auto_detect_reader::lines(&code) {
        renderer.line(line, &mut whole);
    }
    let mut framed = frame("Kode.java");
    framed.insert_str(
        framed.find("<code>").unwrap() + "<code>".len(),
        &text_of(whole),
    );
    let mut whole = Recorder::default();
    walk_parsed(&framed, &mut whole).unwrap();
    let mut chunked = Recorder::default();
    let mut lines = auto_detect_reader::lines(&code).map(str::to_owned);
    let code = Code {
        renderer: Renderer::new(Language::Java),
        lines: &mut lines,
    };
    walk_parsed(
        &frame("Kode.java"),
        &mut Framed {
            walk: &mut chunked,
            code: Some(code),
        },
    )
    .unwrap();
    assert!(whole.0.len() > 1000);
    assert_eq!(chunked.0, whole.0);
}

/// The nodes a walk reports, adjacent text joined.
#[derive(Default)]
struct Recorder(Vec<String>);

impl Visitor for Recorder {
    type Stop = Failure;

    fn start(&mut self, node: &ParsedNode<'_>) -> Result<(), Failure> {
        let attributes = match node {
            ParsedNode::Element { attributes, .. } => attributes
                .iter()
                .map(|attribute| format!(" {}={:?}", attribute.key, attribute.value))
                .collect(),
            _ => String::new(),
        };
        self.0.push(format!("<{}{attributes}>", node_name(node)));
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), Failure> {
        match self.0.last_mut() {
            Some(last) if last.starts_with('"') => last.push_str(text),
            _ => self.0.push(format!("\"{text}")),
        }
        Ok(())
    }

    fn end(&mut self, node: &ParsedNode<'_>) -> Result<(), Failure> {
        self.0.push(format!("</{}>", node_name(node)));
        Ok(())
    }
}

/// Source code too large to read whole is read a line at a time, twice, and highlighted a chunk
/// of lines at a time, under a budget far below it.
#[test]
fn highlights_source_code_too_large_to_read_whole_a_chunk_at_a_time() {
    let line = |index: u64| format!("int x{index} = {index}; // Linje\n").into_bytes();
    let count = extractous_core::large::bodies_past_limit(line(1_000_000).len());
    let file = extractous_core::large::LargeFile::new(
        "Large.java",
        b"/** @author Ane */\n",
        count,
        line,
        b"// the end\n",
    );
    let mut lines = 0u64;
    let mut last = String::new();
    let metadata = file.extract_lines(SourceCodeParser.name(), |text| {
        if text.contains("Linje") {
            lines += 1;
        }
        if !text.trim().is_empty() {
            last = text.trim().to_owned();
        }
    });
    assert_eq!(lines, count);
    assert_eq!(last, "//\u{a0}the\u{a0}end");
    assert_eq!(metadata["LoC"], [(count + 2).to_string()]);
    assert_eq!(metadata["dc:creator"], ["Ane */"]);
}

#[cfg(feature = "tika")]
#[test]
fn extracts_source_code_as_tikas_path_does() {
    let roots = [
        Path::new("../test_files/documents"),
        Path::new("../quality/corpus"),
    ];
    let (parsed, deferred) = documents(&SourceCodeParser, &roots);
    assert!(
        parsed.is_empty() && deferred.is_empty(),
        "{parsed:?} {deferred:?}"
    );

    let scratch = Scratch::new("source-code");
    let java = scratch.write("Havn.java", JAVA.as_bytes());
    let groovy = scratch.write("havn.groovy", GROOVY.as_bytes());
    let cpp = scratch.write("havn.cc", CPP.as_bytes());
    let named = scratch.write(
        "Ḟærge.java",
        b"/** F\xe6rge, \xe5 og \xf8 i Latin-1. */\npublic class F {}\n",
    );
    let (parsed, deferred) = documents(&SourceCodeParser, &[scratch.directory()]);
    assert_eq!(deferred, [named]);
    assert_eq!(parsed, [java.clone(), cpp.clone(), groovy.clone()]);
    assert_same_as_tika(
        &[java.as_path(), groovy.as_path(), cpp.as_path()],
        SourceCodeParser.tika_name(),
    );
}

#[cfg(feature = "tika")]
#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&SourceCodeParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 2, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, SourceCodeParser.tika_name());
}
