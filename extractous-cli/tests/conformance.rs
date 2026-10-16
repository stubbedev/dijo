mod common;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use common::{matched, tokens, Scratch};

const CONFORMANCE: &str = "../conformance";
const WORKS: f64 = 0.95;
const TOLERANCE: f64 = 0.05;
const JOBS: &str = "8";
const TIMEOUT_SECONDS: &str = "60";

/// The parsers of ours that read a format differently from Tika on purpose (iWork text Tika
/// leaves out, dropping subtitle timings, notebook JSON, FB2 styles, email headers, the Kindle
/// books and HWPX documents Tika does not read), so their text is not compared with Tika's:
/// tests of their own check it. The native port of one counts as it does.
const OWN_PARSERS: [&str; 14] = [
    "ai.yobix.IworkArchiveParser",
    "extractous::IworkArchiveParser",
    "ai.yobix.SubtitleParser",
    "extractous::SubtitleParser",
    "ai.yobix.NotebookParser",
    "extractous::NotebookParser",
    "ai.yobix.FictionBookParser",
    "extractous::FictionBookParser",
    "ai.yobix.EmlxParser",
    "extractous::EmlxParser",
    "ai.yobix.MobiParser",
    "extractous::MobiParser",
    "ai.yobix.HwpxParser",
    "extractous::HwpxParser",
];

/// How the native extraction of a file compares with Tika on the JVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Outcome {
    /// The native extraction yields no text where Tika does.
    Fails,
    /// Both yield text, but different text.
    Differs,
    /// Both yield the same text, give or take a few tokens.
    Works,
    /// One of our own parsers reads the format, on purpose differently from Tika.
    Replaced,
    /// Neither yields any text.
    BothEmpty,
    /// Only the native extraction yields text.
    NativeMore,
}

impl Outcome {
    fn passes(self) -> bool {
        self >= Outcome::Works
    }

    /// Whether both sides yielded text that was compared, so its similarity means something.
    fn compared(self) -> bool {
        matches!(self, Outcome::Works | Outcome::Differs)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Comparison {
    mime_type: String,
    outcome: Outcome,
    /// The F1 score of the native tokens against Tika's.
    similarity: f64,
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Compares a native record, whose title counts as text since Tika's text output includes
/// the title, with Tika's text.
fn compare(record: &serde_json::Value, tika: &str) -> Comparison {
    let mut native = tokens(record["text"].as_str().unwrap_or_default());
    native.extend(tokens(
        record["metadata"]["title"].as_str().unwrap_or_default(),
    ));
    let tika = tokens(tika);
    let total = native.len() + tika.len();
    let similarity = if total == 0 {
        1.0
    } else {
        2.0 * matched(&tika, &native) as f64 / total as f64
    };
    let replaced = record["raw_metadata"]["tk:parsed-by"]
        .as_array()
        .is_some_and(|parsers| {
            parsers
                .iter()
                .any(|parser| OWN_PARSERS.contains(&parser.as_str().unwrap_or_default()))
        });
    let outcome = match (native.is_empty(), tika.is_empty()) {
        (true, true) => Outcome::BothEmpty,
        (false, true) => Outcome::NativeMore,
        (true, false) => Outcome::Fails,
        (false, false) if replaced => Outcome::Replaced,
        (false, false) if similarity >= WORKS => Outcome::Works,
        (false, false) => Outcome::Differs,
    };
    Comparison {
        mime_type: record["mime_type"]
            .as_str()
            .unwrap_or("undetected")
            .to_string(),
        outcome,
        similarity,
    }
}

fn matrix(comparisons: &BTreeMap<String, Comparison>) -> String {
    let mut by_type: BTreeMap<&str, Vec<(&String, &Comparison)>> = BTreeMap::new();
    for (file, comparison) in comparisons {
        by_type
            .entry(&comparison.mime_type)
            .or_default()
            .push((file, comparison));
    }
    let passing = comparisons
        .values()
        .filter(|comparison| comparison.outcome.passes())
        .count();
    let mut out = format!(
        "# Conformance with Apache Tika\n\n\
         Every test document of Apache Tika's parser modules, extracted natively and with the \
         same Tika version on the JVM, OCR off on both sides. A file works when the native text \
         and title match Tika's text with an F1 score of at least {WORKS} over their tokens, or \
         when one of our own parsers reads its format on purpose differently (subtitles, \
         notebooks, FictionBook, Apple Mail, iWork 2013+). {passing} of {} files work or are empty on both \
         sides.\n\n\
         | Media type | Files | Work | Differ | Fail | Files that differ or fail (F1) |\n\
         |---|---|---|---|---|---|\n",
        comparisons.len()
    );
    for (mime_type, entries) in by_type {
        let count = |outcome: &dyn Fn(Outcome) -> bool| {
            entries
                .iter()
                .filter(|(_, comparison)| outcome(comparison.outcome))
                .count()
        };
        let problems: Vec<String> = entries
            .iter()
            .filter(|(_, comparison)| !comparison.outcome.passes())
            .map(|(file, comparison)| {
                let name = Path::new(file.as_str())
                    .file_name()
                    .unwrap()
                    .to_string_lossy();
                format!("{name} ({:.2})", comparison.similarity)
            })
            .collect();
        writeln!(
            out,
            "| {mime_type} | {} | {} | {} | {} | {} |",
            entries.len(),
            count(&Outcome::passes),
            count(&|outcome| outcome == Outcome::Differs),
            count(&|outcome| outcome == Outcome::Fails),
            problems.join(", ")
        )
        .unwrap();
    }
    out
}

/// Extracts Apache Tika's own test documents natively and compares the text with Tika's on the
/// JVM, as `just conformance` prepares them in CONFORMANCE_CORPUS and CONFORMANCE_JVM. A file
/// that worked must keep working, and a differing one must not get worse; CONFORMANCE_BLESS
/// accepts the current results as the new baseline.
#[test]
#[ignore]
fn native_extraction_matches_tika_on_the_jvm() {
    let corpus = PathBuf::from(std::env::var("CONFORMANCE_CORPUS").expect(
        "CONFORMANCE_CORPUS names Tika's test documents; run `just conformance` to prepare them",
    ));
    let jvm = PathBuf::from(std::env::var("CONFORMANCE_JVM").expect(
        "CONFORMANCE_JVM names Tika's text for each of them; run `just conformance` to prepare them",
    ));
    let scratch = Scratch::new("conformance");
    let output = Command::new(env!("CARGO_BIN_EXE_extractous"))
        .args([
            "--recursive",
            "--format",
            "json",
            "--metadata",
            "--raw-metadata",
            "--raw",
        ])
        .args([
            "--ocr",
            "never",
            "--jobs",
            JOBS,
            "--timeout",
            TIMEOUT_SECONDS,
        ])
        .arg("--output")
        .arg(&scratch.0)
        .arg(corpus.file_name().unwrap())
        .current_dir(corpus.parent().unwrap())
        .output()
        .unwrap();
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let native_root = scratch.0.join(corpus.file_name().unwrap());
    let mut comparisons = BTreeMap::new();
    for json in files(&native_root) {
        let file = json
            .strip_prefix(&native_root)
            .unwrap()
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        let record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&json).unwrap()).unwrap();
        let tika = fs::read(jvm.join(format!("{file}.txt"))).unwrap_or_default();
        comparisons.insert(file, compare(&record, &String::from_utf8_lossy(&tika)));
    }
    assert_eq!(
        comparisons.len(),
        files(&corpus).len(),
        "an input was left without a record"
    );

    let dir = Path::new(CONFORMANCE);
    fs::write(dir.join("MATRIX.md"), matrix(&comparisons)).unwrap();
    let baseline_path = dir.join("baseline.json");
    if std::env::var_os("CONFORMANCE_BLESS").is_some() {
        fs::write(
            &baseline_path,
            serde_json::to_string_pretty(&comparisons).unwrap() + "\n",
        )
        .unwrap();
        return;
    }
    let baseline: BTreeMap<String, Comparison> =
        serde_json::from_str(&fs::read_to_string(&baseline_path).unwrap()).unwrap();
    let regressions: Vec<String> = comparisons
        .iter()
        .filter_map(|(file, now)| {
            let Some(before) = baseline.get(file) else {
                return Some(format!("{file}: missing from baseline.json"));
            };
            let worse = (before.outcome.passes() && !now.outcome.passes())
                || now.outcome < before.outcome
                || (now.outcome.compared()
                    && before.outcome.compared()
                    && now.similarity < before.similarity - TOLERANCE);
            worse.then(|| {
                format!(
                    "{file}: {:?} ({:.2}) was {:?} ({:.2})",
                    now.outcome, now.similarity, before.outcome, before.similarity
                )
            })
        })
        .collect();
    assert!(
        regressions.is_empty(),
        "Conformance regressed:\n{}",
        regressions.join("\n")
    );
}
