use std::path::{Path, PathBuf};

use super::*;
use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

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

#[test]
#[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
fn extracts_tikas_test_documents_as_tikas_path_does() {
    let (parsed, deferred) = documents(&SourceCodeParser, &[&conformance_corpus()]);
    assert!(deferred.is_empty(), "{deferred:?}");
    assert_eq!(parsed.len(), 2, "{parsed:?}");
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika(&paths, SourceCodeParser.tika_name());
}
