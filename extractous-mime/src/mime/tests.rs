//! Parity with Tika's own detection, as `just mime-reference` records it in
//! `tests/fixtures/mime/mime-reference.tsv`: for every document of the parity corpora, the media
//! type and the metadata the detection sets, for the file and for its bytes alone.

use std::path::{Path, PathBuf};

use super::*;

const REFERENCE: &str = include_str!("../../tests/fixtures/mime/mime-reference.tsv");

/// A line of the reference.
struct Case<'a> {
    corpus: &'a str,
    name: &'a str,
    variant: &'a str,
    expected: String,
}

fn cases() -> impl Iterator<Item = Case<'static>> {
    REFERENCE.lines().map(|line| {
        let mut fields = line.splitn(4, '\t');
        let mut next = || fields.next().expect("a complete reference line");
        Case {
            corpus: next(),
            name: next(),
            variant: next(),
            expected: next().to_owned(),
        }
    })
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// What the port answers, in the reference's form: the media type, then each metadata entry.
fn answer(data: &[u8], file_name: &str, variant: &str) -> String {
    let origin = match variant {
        "file" => Origin::File { name: file_name },
        "bytes" => Origin::Bytes,
        other => panic!("unknown variant {other}"),
    };
    match detect(data, origin) {
        Ok(detection) => std::iter::once(detection.media_type.to_string())
            .chain(
                detection
                    .metadata
                    .iter()
                    .map(|entry| format!("{}={}", entry.key.name(), entry.value)),
            )
            .collect::<Vec<_>>()
            .join("\t"),
        Err(error) => format!("error: {error}"),
    }
}

fn file_name(name: &str) -> &str {
    Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .expect("a file name")
}

/// Runs every case of the given corpora against the documents under their roots, and returns
/// how many matched; panics listing every mismatch.
fn check(corpora: &[(&str, PathBuf)]) -> usize {
    let mut matched = 0;
    let mut mismatches = Vec::new();
    for case in cases() {
        let Some((_, root)) = corpora.iter().find(|(corpus, _)| *corpus == case.corpus) else {
            continue;
        };
        let path = root.join(case.name);
        let data = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        let actual = answer(&data, file_name(case.name), case.variant);
        if actual == case.expected {
            matched += 1;
        } else {
            mismatches.push(format!(
                "{}/{} ({}): Tika {}, port {actual}",
                case.corpus, case.name, case.variant, case.expected
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} detections differ from Tika's:\n{}",
        mismatches.len(),
        matched + mismatches.len(),
        mismatches.join("\n")
    );
    matched
}

/// The files under `root`, by their path relative to it, as `Files.walk` finds them, with `/`
/// between the names on every platform, as the reference writes them.
fn files(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("cannot list {}: {error}", directory.display()));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                directories.push(path);
            } else {
                let relative = path.strip_prefix(root).expect("under the root");
                let names: Vec<&str> = relative
                    .components()
                    .map(|name| name.as_os_str().to_str().expect("a UTF-8 name"))
                    .collect();
                found.push(names.join("/"));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn the_reference_covers_the_repository_corpora() {
    for (corpus, root) in [
        ("documents", workspace().join("test_files/documents")),
        ("quality", workspace().join("quality/corpus")),
    ] {
        for variant in ["file", "bytes"] {
            let mut referenced: Vec<String> = cases()
                .filter(|case| case.corpus == corpus && case.variant == variant)
                .map(|case| case.name.to_owned())
                .collect();
            referenced.sort();
            assert_eq!(
                referenced,
                files(&root),
                "{corpus} ({variant}) changed: run `just mime-reference`"
            );
        }
    }
}

#[test]
fn detects_as_tika_does() {
    let matched = check(&[
        ("documents", workspace().join("test_files/documents")),
        ("quality", workspace().join("quality/corpus")),
    ]);
    assert!(matched > 600, "only {matched} cases ran");
}

#[test]
#[ignore = "needs Tika's test documents, which `conformance/prepare.sh` fetches; run with \
            CONFORMANCE_CORPUS set as it prints"]
fn detects_as_tika_does_on_tikas_corpus() {
    let corpus = std::env::var_os("CONFORMANCE_CORPUS")
        .expect("CONFORMANCE_CORPUS names the directory of Tika's test documents");
    let matched = check(&[("tika", PathBuf::from(corpus))]);
    assert!(matched > 2000, "only {matched} cases ran");
}
