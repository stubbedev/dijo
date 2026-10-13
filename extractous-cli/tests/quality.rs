mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use common::{matched, tokens};

const QUALITY: &str = "../quality";
const JOBS: usize = 4;
const ANCHOR_TOKENS: usize = 4;

/// Names the external programs, such as `ddjvu`, that this environment lacks. The documents
/// that need one are checked for the error naming it instead of being scored.
const MISSING_HELPERS: &str = "QUALITY_MISSING_HELPERS";

/// The categories whose text comes from OCR: scans, and PDFs whose text layer is garbage.
const OCR_CATEGORIES: [&str; 2] = ["ocr", "broken-text-layer"];

/// How much worse than the baseline a score may get before the check fails. OCR depends on the
/// tesseract version of each platform, so it gets more room.
fn tolerance(category: &str) -> f64 {
    if OCR_CATEGORIES.contains(&category) {
        0.05
    } else {
        0.005
    }
}

struct Document {
    file: String,
    truth: String,
    category: String,
    helper: Option<String>,
    arguments: Vec<String>,
}

impl Document {
    /// How the document is named in the baseline and the scoreboard: the file, with its
    /// arguments when it has any, since a file can be scored with different ones.
    fn key(&self) -> String {
        if self.arguments.is_empty() {
            self.file.clone()
        } else {
            format!("{} {}", self.file, self.arguments.join(" "))
        }
    }

    fn pages(&self) -> bool {
        self.arguments
            .windows(2)
            .any(|pair| pair[0] == "--format" && pair[1] == "pages")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Scores {
    character_error_rate: f64,
    token_error_rate: f64,
    recall: f64,
    spurious: f64,
    reading_order: f64,
}

fn manifest() -> Vec<Document> {
    fs::read_to_string(Path::new(QUALITY).join("corpus.tsv"))
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 5, "corpus.tsv line {line:?}");
            Document {
                file: fields[0].to_string(),
                truth: fields[1].to_string(),
                category: fields[2].to_string(),
                helper: (!fields[3].is_empty()).then(|| fields[3].to_string()),
                arguments: fields[4].split_whitespace().map(str::to_string).collect(),
            }
        })
        .collect()
}

/// The helpers named by [`MISSING_HELPERS`], each one a helper some document needs.
fn missing_helpers(documents: &[Document]) -> BTreeSet<String> {
    let missing: BTreeSet<String> = std::env::var(MISSING_HELPERS)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let needed: BTreeSet<&String> = documents.iter().filter_map(|d| d.helper.as_ref()).collect();
    for helper in &missing {
        assert!(
            needed.contains(helper),
            "{MISSING_HELPERS} names {helper}, which no document in corpus.tsv needs"
        );
    }
    missing
}

fn assert_scores_every_document(missing: &BTreeSet<String>, writer: &str) {
    assert!(
        missing.is_empty(),
        "{writer} records every document's scores, so {MISSING_HELPERS} must name no helper"
    );
}

fn run(document: &Document) -> Output {
    Command::new(env!("CARGO_BIN_EXE_extractous"))
        .args(&document.arguments)
        .arg(Path::new(QUALITY).join("corpus").join(&document.file))
        .output()
        .unwrap()
}

fn assert_reports_missing(document: &Document, helper: &str) {
    let output = run(document);
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && errors.contains(&format!("{helper} on the PATH")),
        "{}: without {helper}: {errors}",
        document.file
    );
}

fn extract(document: &Document) -> String {
    let output = run(document);
    assert!(
        output.status.success(),
        "{}: {}",
        document.file,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    if !document.pages() {
        return stdout;
    }
    stdout
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| record["kind"] == "page")
        .map(|record| record["text"].as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn characters(text: &str) -> Vec<char> {
    text.nfc()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect()
}

fn edit_distance<T: PartialEq>(from: &[T], to: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=to.len()).collect();
    let mut current = vec![0; to.len() + 1];
    for (i, a) in from.iter().enumerate() {
        current[0] = i + 1;
        for (j, b) in to.iter().enumerate() {
            let substitution = previous[j] + usize::from(a != b);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[to.len()]
}

fn longest_increasing(positions: &[usize]) -> usize {
    let mut tails: Vec<usize> = Vec::new();
    for &position in positions {
        match tails.binary_search(&position) {
            Ok(_) => {}
            Err(index) if index == tails.len() => tails.push(position),
            Err(index) => tails[index] = position,
        }
    }
    tails.len()
}

/// The share of the truth's paragraphs that appear in the extracted text in their original
/// order, each found by its first few tokens; a paragraph that repeats is matched to the next
/// occurrence of its anchor.
fn reading_order(truth: &str, extracted: &[String]) -> f64 {
    let paragraphs: Vec<Vec<String>> = truth
        .split("\n\n")
        .map(tokens)
        .filter(|paragraph| paragraph.len() >= ANCHOR_TOKENS)
        .collect();
    if paragraphs.is_empty() {
        return 1.0;
    }
    let mut seen: HashMap<&[String], usize> = HashMap::new();
    let positions: Vec<usize> = paragraphs
        .iter()
        .filter_map(|paragraph| {
            let anchor = &paragraph[..ANCHOR_TOKENS];
            let occurrence = seen.entry(anchor).or_default();
            let position = extracted
                .windows(ANCHOR_TOKENS)
                .enumerate()
                .filter(|(_, window)| *window == anchor)
                .nth(*occurrence)
                .map(|(position, _)| position);
            *occurrence += 1;
            position
        })
        .collect();
    longest_increasing(&positions) as f64 / paragraphs.len() as f64
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    numerator as f64 / denominator as f64
}

fn score(truth: &str, extracted: &str) -> Scores {
    let truth_tokens = tokens(truth);
    let extracted_tokens = tokens(extracted);
    let truth_characters = characters(truth);
    let matched = matched(&truth_tokens, &extracted_tokens);
    Scores {
        character_error_rate: ratio(
            edit_distance(&truth_characters, &characters(extracted)),
            truth_characters.len(),
        ),
        token_error_rate: ratio(
            edit_distance(&truth_tokens, &extracted_tokens),
            truth_tokens.len(),
        ),
        recall: ratio(matched, truth_tokens.len()),
        spurious: ratio(extracted_tokens.len() - matched, extracted_tokens.len()),
        reading_order: reading_order(truth, &extracted_tokens),
    }
}

/// The ways `scores` fall short of `baseline` by more than `tolerance`.
fn regressions(scores: &Scores, baseline: &Scores, tolerance: f64) -> Vec<String> {
    let checks = [
        (
            "character error rate",
            scores.character_error_rate - baseline.character_error_rate,
        ),
        (
            "token error rate",
            scores.token_error_rate - baseline.token_error_rate,
        ),
        ("recall", baseline.recall - scores.recall),
        ("spurious text", scores.spurious - baseline.spurious),
        (
            "reading order",
            baseline.reading_order - scores.reading_order,
        ),
    ];
    checks
        .into_iter()
        .filter(|(_, worse_by)| *worse_by > tolerance)
        .map(|(metric, worse_by)| format!("{metric} worse by {worse_by:.3}"))
        .collect()
}

fn truth(document: &Document) -> String {
    fs::read_to_string(
        Path::new(QUALITY)
            .join("truth")
            .join(format!("{}.txt", document.truth)),
    )
    .unwrap()
}

/// Another extractor's text of the corpus: its name and version, and the directory holding its
/// text of each document as `<file>.txt`, where a document it could not read has none.
struct Rival {
    name: String,
    texts: PathBuf,
}

/// The rivals whose texts the subdirectories of `dir` hold, each named after its rival.
fn rivals(dir: &Path) -> Vec<Rival> {
    let mut rivals: Vec<Rival> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("reading {}: {error}", dir.display()))
        .map(|entry| {
            let entry = entry.unwrap();
            Rival {
                name: entry.file_name().into_string().unwrap(),
                texts: entry.path(),
            }
        })
        .collect();
    rivals.sort_by(|one, other| one.name.cmp(&other.name));
    assert!(
        !rivals.is_empty(),
        "{} holds no rival's texts",
        dir.display()
    );
    rivals
}

/// The scores of extractous and of its rivals side by side, for every document extracted with
/// default options.
fn comparison(
    documents: &[&Document],
    results: &BTreeMap<String, Scores>,
    rivals: &[Rival],
) -> String {
    let names: Vec<&str> = rivals.iter().map(|rival| rival.name.as_str()).collect();
    let mut table = format!(
        "# Extractous and other extractors\n\n\
         The quality corpus extracted by extractous, {}, each with its default options and \
         without a language hint. A document an extractor could not read counts as empty \
         text. Each \
         cell is extractous / {}.\n\n\
         | Document | Category | CER | Token error rate | Recall | Spurious |\n\
         |---|---|---|---|---|---|\n",
        names.join(", "),
        names.join(" / "),
    );
    let metrics = |scores: &Scores| {
        [
            scores.character_error_rate,
            scores.token_error_rate,
            scores.recall,
            scores.spurious,
        ]
    };
    let cell = |values: &[f64]| {
        values
            .iter()
            .map(|value| format!("{value:.3}"))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    let mut totals = vec![[0.0; 4]; rivals.len() + 1];
    let mut count = 0;
    for document in documents
        .iter()
        .filter(|document| document.arguments.is_empty())
    {
        let truth = truth(document);
        let mut all = vec![metrics(&results[&document.key()])];
        for rival in rivals {
            let path = rival.texts.join(format!("{}.txt", document.file));
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(error) => panic!("reading {}: {error}", path.display()),
            };
            all.push(metrics(&score(&truth, &text)));
        }
        for (total, scores) in totals.iter_mut().zip(&all) {
            for (sum, value) in total.iter_mut().zip(scores) {
                *sum += value;
            }
        }
        count += 1;
        let columns: Vec<String> = (0..4)
            .map(|metric| cell(&all.iter().map(|scores| scores[metric]).collect::<Vec<_>>()))
            .collect();
        writeln!(
            table,
            "| {} | {} | {} |",
            document.file,
            document.category,
            columns.join(" | ")
        )
        .unwrap();
    }
    let means: Vec<String> = (0..4)
        .map(|metric| {
            cell(
                &totals
                    .iter()
                    .map(|total| total[metric] / f64::from(count))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    writeln!(table, "| **Mean** | | {} |", means.join(" | ")).unwrap();
    table
}

fn scoreboard(documents: &[&Document], results: &BTreeMap<String, Scores>) -> String {
    let mut table = String::from(
        "| Document | Category | CER | Token error rate | Recall | Spurious | Reading order |\n\
         |---|---|---|---|---|---|---|\n",
    );
    for document in documents {
        let scores = &results[&document.key()];
        let _ = writeln!(
            table,
            "| {} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |",
            document.key(),
            document.category,
            scores.character_error_rate,
            scores.token_error_rate,
            scores.recall,
            scores.spurious,
            scores.reading_order
        );
    }
    table
}

#[test]
fn extraction_quality_does_not_regress() {
    let manifest = manifest();
    let missing = missing_helpers(&manifest);
    let (documents, unscored): (Vec<&Document>, Vec<&Document>) =
        manifest.iter().partition(|document| {
            document
                .helper
                .as_ref()
                .is_none_or(|helper| !missing.contains(helper))
        });
    for document in unscored {
        assert_reports_missing(document, document.helper.as_ref().unwrap());
    }
    let results = Mutex::new(BTreeMap::new());
    let next = Mutex::new(documents.iter());
    std::thread::scope(|scope| {
        for _ in 0..JOBS {
            scope.spawn(|| loop {
                let Some(document) = next.lock().unwrap().next() else {
                    return;
                };
                let truth = truth(document);
                let extracted = extract(document);
                if let Some(dir) = std::env::var_os("QUALITY_OUTPUT") {
                    fs::create_dir_all(&dir).unwrap();
                    fs::write(PathBuf::from(dir).join(document.key()), &extracted).unwrap();
                }
                let scores = score(&truth, &extracted);
                results.lock().unwrap().insert(document.key(), scores);
            });
        }
    });
    let results = results.into_inner().unwrap();
    let table = scoreboard(&documents, &results);
    println!("{table}");
    if let Some(path) = std::env::var_os("QUALITY_SCOREBOARD") {
        assert_scores_every_document(&missing, "QUALITY_SCOREBOARD");
        fs::write(PathBuf::from(path), &table).unwrap();
    }
    if let Some(dir) = std::env::var_os("QUALITY_COMPARE") {
        assert_scores_every_document(&missing, "QUALITY_COMPARE");
        let path = std::env::var_os("QUALITY_COMPARISON")
            .expect("QUALITY_COMPARISON names the file to write the comparison to");
        fs::write(
            path,
            comparison(&documents, &results, &rivals(Path::new(&dir))),
        )
        .unwrap();
    }

    let baseline_path = Path::new(QUALITY).join("baseline.json");
    if std::env::var_os("QUALITY_BLESS").is_some() {
        assert_scores_every_document(&missing, "QUALITY_BLESS");
        let json = serde_json::to_string_pretty(&results).unwrap();
        fs::write(&baseline_path, json + "\n").unwrap();
        return;
    }
    let baseline: BTreeMap<String, Scores> =
        serde_json::from_str(&fs::read_to_string(&baseline_path).unwrap()).unwrap();
    let failures: Vec<String> = documents
        .iter()
        .filter_map(|document| {
            let Some(expected) = baseline.get(&document.key()) else {
                return Some(format!("{}: missing from baseline.json", document.key()));
            };
            let found = regressions(
                &results[&document.key()],
                expected,
                tolerance(&document.category),
            );
            (!found.is_empty()).then(|| format!("{}: {}", document.key(), found.join(", ")))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "Extraction quality regressed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn metrics_score_known_differences() {
    let truth = "One two three four.\n\nFive six seven eight.";
    let perfect = score(truth, "One two three four.\n\nFive six seven eight.");
    assert_eq!(perfect.character_error_rate, 0.0);
    assert_eq!(perfect.recall, 1.0);
    assert_eq!(perfect.reading_order, 1.0);

    let swapped = score(truth, "Five six seven eight. One two three four.");
    assert_eq!(swapped.recall, 1.0);
    assert_eq!(swapped.reading_order, 0.5);

    let noisy = score(truth, "One two three four. Five six seven eight. Page 1");
    assert_eq!(noisy.spurious, 2.0 / 10.0);
    let repeated = "One two three four.\n\nFive six seven eight.\n\nOne two three four.";
    assert_eq!(score(repeated, repeated).reading_order, 1.0);
    assert_eq!(tokens("灯台 守り lamp"), ["灯", "台", "守", "り", "lamp"]);
    assert_eq!(tokens("प्रकाशस्तंभ रक्षक"), ["प्रकाशस्तंभ", "रक्षक"]);
}
