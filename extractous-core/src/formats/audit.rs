//! What of a corpus still reaches Tika's path, and why: for each document, whether routing
//! leaves it to Tika, its native parser defers it, or its native parser reads it, and then
//! which of its embedded documents went over the bridge to Tika. The embedded parses record
//! themselves here as they happen, on the thread of the extraction. Each document is audited
//! in a process of its own, so one that crashes or hangs its extraction is reported as such.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::{Duration, Instant};

use crate::config::{ParserConfigs, PdfParserConfig, TesseractOcrConfig};
use crate::formats::{self, Bridged, Declined, Output, Unrouted};
use crate::tika::Source;

/// How an embedded document of a natively read one reached Tika's path.
#[derive(Debug)]
pub(crate) enum Bridge {
    /// Routing sent it there.
    Routed(Bridged),
    /// Its native parser, of this name, failed on it as Tika's would throw, and Tika's path
    /// parsed it again for the exception Tika records, with why the native parse failed.
    Reparsed(&'static str, String),
}

thread_local! {
    static BRIDGES: RefCell<Vec<Bridge>> = const { RefCell::new(Vec::new()) };
}

/// Records that an embedded document goes to Tika's path, and why.
pub(crate) fn bridged(bridged: Bridged) {
    BRIDGES.with(|bridges| bridges.borrow_mut().push(Bridge::Routed(bridged)));
}

/// Records that Tika's path parses an embedded document again after `parser` failed on it.
pub(crate) fn reparsed(parser: &'static str, reason: &str) {
    BRIDGES.with(|bridges| {
        bridges
            .borrow_mut()
            .push(Bridge::Reparsed(parser, reason.to_owned()))
    });
}

fn take_bridges() -> Vec<Bridge> {
    BRIDGES.with(|bridges| std::mem::take(&mut *bridges.borrow_mut()))
}

/// The longest a reason is quoted in the report.
const REASON_CHARACTERS: usize = 160;

fn short(reason: &str) -> String {
    let line = reason.lines().next().unwrap_or_default();
    let line = line.replace(['\t', FIELDS], " ");
    match line.char_indices().nth(REASON_CHARACTERS) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line,
    }
}

impl Bridge {
    /// The reason, as the report groups it.
    fn reason(&self) -> String {
        match self {
            Bridge::Routed(Bridged::Unrouted(Unrouted::Unparsed(media_type))) => {
                format!("no native parser for {}", base_type(media_type))
            }
            Bridge::Routed(Bridged::Unrouted(unrouted)) => format!("{unrouted:?}"),
            Bridge::Routed(Bridged::Declined(parser, Declined::Deferred(reason))) => {
                format!("{parser} defers: {}", short(reason))
            }
            Bridge::Routed(Bridged::Declined(parser, Declined::Fails(_))) => {
                format!("{parser} fails it before starting: Tika records the exception")
            }
            Bridge::Reparsed(parser, reason) => {
                format!(
                    "{parser} failed, Tika reparses for the exception: {}",
                    short(reason)
                )
            }
        }
    }
}

/// What became of one document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// Its native parser read it without Tika.
    Native,
    /// Its native parser read it, sending embedded documents to Tika's path.
    Bridging,
    /// Its native parser read it and failed, as Tika's would.
    Failed,
    /// Its native parser panicked.
    Panicked,
    /// Its extraction crashed the process.
    Crashed,
    /// Its extraction took longer than [`DOCUMENT_TIMEOUT`].
    TimedOut,
    /// Its native parser deferred it.
    Deferred,
    /// Routing left it to Tika.
    Unrouted,
}

impl Kind {
    const ALL: [Kind; 8] = [
        Kind::Native,
        Kind::Bridging,
        Kind::Failed,
        Kind::Panicked,
        Kind::Crashed,
        Kind::TimedOut,
        Kind::Deferred,
        Kind::Unrouted,
    ];

    fn name(self) -> &'static str {
        match self {
            Kind::Native => "native",
            Kind::Bridging => "native-bridging",
            Kind::Failed => "native-failed",
            Kind::Panicked => "panicked",
            Kind::Crashed => "crashed",
            Kind::TimedOut => "timed-out",
            Kind::Deferred => "deferred",
            Kind::Unrouted => "unrouted",
        }
    }

    /// Whether Tika's path reads some of a document that ended so.
    fn reaches_java(self) -> bool {
        matches!(self, Kind::Bridging | Kind::Deferred | Kind::Unrouted)
    }
}

impl FromStr for Kind {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.name() == name)
            .ok_or_else(|| format!("no outcome is named {name:?}"))
    }
}

/// What the audit of one document found: its type, the native parser routing picked (for a
/// document routing leaves to Tika, the parsers Tika's path picks), how it ended and why.
#[derive(Debug)]
struct Found {
    media_type: String,
    parser: String,
    kind: Kind,
    reasons: Vec<String>,
}

/// Separates the fields of a [`Found`] on the child's output line.
const FIELDS: char = '\u{1f}';
/// Starts the child's output line.
const MARKER: &str = "ROUTING-AUDIT";

impl Found {
    fn line(&self) -> String {
        let mut line = format!(
            "{MARKER}{FIELDS}{}{FIELDS}{}{FIELDS}{}",
            self.media_type,
            self.parser,
            self.kind.name()
        );
        for reason in &self.reasons {
            line.push(FIELDS);
            line.push_str(reason);
        }
        line
    }

    fn parse(output: &str) -> Option<Self> {
        let line = output
            .lines()
            .find_map(|line| line.split_once(MARKER)?.1.strip_prefix(FIELDS))?;
        let mut fields = line.split(FIELDS);
        let media_type = fields.next()?.to_owned();
        let parser = fields.next()?.to_owned();
        let kind = fields.next()?.parse().unwrap();
        Some(Self {
            media_type,
            parser,
            kind,
            reasons: fields.map(str::to_owned).collect(),
        })
    }
}

fn base_type(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// The settings `extractous-cli` extracts with unless told otherwise (`settings::extractor`
/// under `--ocr auto`): OCR in the automatic language, and PDF pages OCRed when more than half
/// of their glyphs map to no text or they have at most 10 characters.
fn cli_settings() -> ParserConfigs {
    ParserConfigs {
        pdf: PdfParserConfig::new().set_ocr_strategy_auto("50%,10"),
        office: Default::default(),
        ocr: TesseractOcrConfig::new().set_language(TesseractOcrConfig::AUTO_LANGUAGE),
    }
}

const SETTINGS: &str = "ROUTING_AUDIT_SETTINGS";

/// The settings [`SETTINGS`] names: the library's defaults or the command line's.
fn settings() -> ParserConfigs {
    let name = std::env::var(SETTINGS)
        .unwrap_or_else(|_| panic!("{SETTINGS} names the settings to audit: library or cli"));
    match name.as_str() {
        "library" => ParserConfigs::default(),
        "cli" => cli_settings(),
        other => panic!("{SETTINGS} is library or cli, not {other:?}"),
    }
}

/// The parsers Tika's path parses the document at `name` with, as its `tk:parsed-by` lists them
/// after its composite parsers, OCR skipped, which picks no other parser; a native parser's
/// name where it replaces Tika's.
fn tika_parsers(name: &str) -> String {
    let configs = ParserConfigs {
        ocr: TesseractOcrConfig::new().set_skip_ocr(true),
        ..Default::default()
    };
    let parsed = crate::tika::parse_to_string(
        crate::isolate::isolate().unwrap(),
        Source::File(name),
        1,
        &configs,
        None,
        false,
    );
    let metadata = match parsed {
        Ok((_, metadata)) => metadata,
        Err(error) => return format!("(Tika fails: {})", short(&error.to_string())),
    };
    let parsers: Vec<String> = metadata
        .get("tk:parsed-by")
        .into_iter()
        .flatten()
        .filter(|parser| !COMPOSITES.contains(&parser.as_str()))
        .map(|parser| {
            formats::PARSERS
                .iter()
                .find(|native| native.tika_name() == parser)
                .map_or_else(|| parser.clone(), |native| native.name().to_owned())
        })
        .collect();
    parsers.join(" > ")
}

/// The composite parsers Tika's path records before the parser it picks.
const COMPOSITES: [&str; 2] = [
    "org.apache.tika.parser.DefaultParser",
    "org.apache.tika.parser.AutoDetectParser",
];

/// Audits the document at `name` in this process.
fn audit(name: &str, configs: &ParserConfigs) -> Found {
    let routed =
        formats::route(Source::File(name)).unwrap_or_else(|e| panic!("cannot route {name}: {e}"));
    let routed = match routed {
        Ok(routed) => routed,
        Err(unrouted) => {
            let (media_type, reason) = match &unrouted {
                Unrouted::Unparsed(media_type) => {
                    (base_type(media_type), "no native parser".to_owned())
                }
                other => ("(not detected in Rust)".to_owned(), format!("{other:?}")),
            };
            return Found {
                media_type,
                parser: tika_parsers(name),
                kind: Kind::Unrouted,
                reasons: vec![reason],
            };
        }
    };
    let parser = routed.parser;
    let parsed = panic::catch_unwind(AssertUnwindSafe(|| {
        let start = routed.detected.metadata_for(parser);
        if let Some(deferral) = parser.defers(&routed.input, &start, configs) {
            return (Kind::Deferred, vec![short(&deferral)]);
        }
        let parsed = formats::parse_to_string(
            parser,
            &routed.input,
            &routed.detected,
            configs,
            usize::MAX,
            Output::Text,
        );
        let bridges: Vec<String> = take_bridges().iter().map(Bridge::reason).collect();
        match parsed {
            Ok(Some(_)) if bridges.is_empty() => (Kind::Native, bridges),
            Ok(Some(_)) => (Kind::Bridging, bridges),
            Ok(None) => panic!("{name} was deferred on its second preparation"),
            Err(error) => (Kind::Failed, vec![short(&error.to_string())]),
        }
    }));
    let (kind, reasons) = parsed.unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default();
        (Kind::Panicked, vec![short(&message)])
    });
    Found {
        media_type: base_type(&routed.detected.media_type),
        parser: parser.name().to_owned(),
        kind,
        reasons,
    }
}

const DOCUMENT: &str = "ROUTING_AUDIT_DOCUMENT";
const CHILD_TEST: &str = "formats::audit::audits_one_document";
/// The longest one document's audit may take.
const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(600);

/// Audits the document at `path` in a child process running [`audits_one_document`].
fn audit_in_child(path: &Path) -> Found {
    let exe = std::env::current_exe().expect("the test binary");
    let mut child = Command::new(exe)
        .args(["--ignored", "--exact", CHILD_TEST, "--nocapture"])
        .args(["--test-threads", "1"])
        .env(DOCUMENT, path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the test binary runs");
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut output = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut output).map(|_| output)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > DOCUMENT_TIMEOUT {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = reader.join().unwrap().unwrap_or_default();
    if let Some(found) = Found::parse(&output) {
        return found;
    }
    let (kind, reason) = match status {
        Some(status) => (Kind::Crashed, format!("{status}")),
        None => (Kind::TimedOut, format!("over {DOCUMENT_TIMEOUT:?}")),
    };
    Found {
        media_type: "(unknown: the audit did not finish)".to_owned(),
        parser: "-".to_owned(),
        kind,
        reasons: vec![reason],
    }
}

/// Audits the document [`DOCUMENT`] names, for [`audits_what_reaches_tikas_path`], printing
/// what it found on one line.
#[test]
#[ignore = "run by audits_what_reaches_tikas_path, one document at a time"]
fn audits_one_document() {
    let path = std::env::var(DOCUMENT).unwrap_or_else(|_| panic!("{DOCUMENT} names a document"));
    println!("{}", audit(&path, &settings()).line());
}

struct Row {
    corpus: &'static str,
    path: PathBuf,
    found: Found,
}

fn files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            let entries = std::fs::read_dir(&path)
                .unwrap_or_else(|e| panic!("cannot list {}: {e}", path.display()));
            pending.extend(entries.map(|entry| entry.unwrap().path()));
        } else if std::fs::metadata(&path).unwrap().len() > 0 {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// Documents and occurrences per reason.
type Tally = BTreeMap<String, (usize, usize)>;

fn tally(tally: &mut Tally, reasons: &[String]) {
    let mut seen: Vec<&String> = Vec::new();
    for reason in reasons {
        let entry = tally.entry(reason.clone()).or_default();
        if !seen.contains(&reason) {
            entry.0 += 1;
            seen.push(reason);
        }
        entry.1 += 1;
    }
}

fn ranked(tally: Tally) -> Vec<(String, usize, usize)> {
    let mut ranked: Vec<_> = tally
        .into_iter()
        .map(|(reason, (documents, occurrences))| (reason, documents, occurrences))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
    ranked
}

fn escaped(text: &str) -> String {
    text.replace('|', "\\|")
}

fn report(rows: &[Row], settings: &str) -> String {
    let mut types: BTreeMap<&str, BTreeMap<Kind, usize>> = BTreeMap::new();
    let mut tallies: BTreeMap<Kind, Tally> = BTreeMap::new();
    let mut corpora: BTreeMap<&str, usize> = BTreeMap::new();
    for Row { corpus, found, .. } in rows {
        *corpora.entry(corpus).or_default() += 1;
        *types
            .entry(&found.media_type)
            .or_default()
            .entry(found.kind)
            .or_default() += 1;
        let reasons: Vec<String> = match found.kind {
            Kind::Native => Vec::new(),
            Kind::Bridging => found.reasons.clone(),
            Kind::Unrouted if found.reasons == ["no native parser"] => {
                vec![format!(
                    "no native parser for {} (Tika: {})",
                    found.media_type, found.parser
                )]
            }
            Kind::Crashed | Kind::TimedOut | Kind::Unrouted => found.reasons.clone(),
            _ => found
                .reasons
                .iter()
                .map(|reason| format!("{}: {reason}", found.parser))
                .collect(),
        };
        tally(tallies.entry(found.kind).or_default(), &reasons);
    }
    let count = |kind: Kind| rows.iter().filter(|row| row.found.kind == kind).count();
    let mut out = String::new();
    writeln!(out, "# Routing audit ({settings} settings)\n").unwrap();
    for (corpus, documents) in &corpora {
        writeln!(out, "- {corpus}: {documents} documents").unwrap();
    }
    writeln!(out, "\n{} documents:\n", rows.len()).unwrap();
    for kind in Kind::ALL {
        writeln!(out, "- {}: {}", kind.name(), count(kind)).unwrap();
    }
    writeln!(out, "\n## Per media type\n").unwrap();
    let mut header = "| Media type | Documents |".to_owned();
    let mut rule = "|---|---:|".to_owned();
    for kind in Kind::ALL {
        write!(header, " {} |", kind.name()).unwrap();
        rule.push_str("---:|");
    }
    writeln!(out, "{header}\n{rule}").unwrap();
    let java = |counts: &BTreeMap<Kind, usize>| {
        counts
            .iter()
            .filter(|(kind, _)| kind.reaches_java())
            .map(|(_, count)| count)
            .sum::<usize>()
    };
    let mut by_java: Vec<_> = types.iter().collect();
    by_java.sort_by(|a, b| java(b.1).cmp(&java(a.1)).then(a.0.cmp(b.0)));
    for (media_type, counts) in by_java {
        write!(out, "| {media_type} | {} |", counts.values().sum::<usize>()).unwrap();
        for kind in Kind::ALL {
            match counts.get(&kind) {
                Some(count) => write!(out, " {count} |").unwrap(),
                None => out.push_str(" |"),
            }
        }
        out.push('\n');
    }
    for (kind, title) in [
        (Kind::Deferred, "Deferral reasons"),
        (Kind::Unrouted, "Why routing leaves documents to Tika"),
        (Kind::Bridging, "Embedded documents sent to Tika"),
        (Kind::Failed, "Native failures, as Tika fails"),
        (Kind::Panicked, "Panics"),
        (Kind::Crashed, "Crashes"),
        (Kind::TimedOut, "Timeouts"),
    ] {
        let Some(tally) = tallies.remove(&kind) else {
            continue;
        };
        writeln!(
            out,
            "\n## {title}\n\n| Documents | Occurrences | Reason |\n|---:|---:|---|"
        )
        .unwrap();
        for (reason, documents, occurrences) in ranked(tally) {
            writeln!(
                out,
                "| {documents} | {occurrences} | {} |",
                escaped(&reason)
            )
            .unwrap();
        }
    }
    let unfinished: Vec<&Row> = rows
        .iter()
        .filter(|row| {
            matches!(
                row.found.kind,
                Kind::Crashed | Kind::TimedOut | Kind::Panicked
            )
        })
        .collect();
    if !unfinished.is_empty() {
        writeln!(out, "\n## Documents whose audit did not finish\n").unwrap();
        for row in unfinished {
            writeln!(
                out,
                "- {} ({}): {}",
                row.path.display(),
                row.found.kind.name(),
                escaped(&row.found.reasons.join("; "))
            )
            .unwrap();
        }
    }
    out
}

fn tsv(rows: &[Row]) -> String {
    let mut out = String::new();
    for Row {
        corpus,
        path,
        found,
    } in rows
    {
        writeln!(
            out,
            "{corpus}\t{}\t{}\t{}\t{}\t{}",
            path.display(),
            found.media_type,
            found.parser,
            found.kind.name(),
            found.reasons.join(" ; ")
        )
        .unwrap();
    }
    out
}

/// Audits the routing of extractous's documents, the quality corpus and Tika's test documents
/// under the settings [`SETTINGS`] names, writing `routing.md` and `documents.tsv` to the
/// directory `ROUTING_AUDIT_DIR` names.
#[test]
#[ignore = "needs Tika's test documents (conformance/prepare.sh), ROUTING_AUDIT_DIR and ROUTING_AUDIT_SETTINGS"]
fn audits_what_reaches_tikas_path() {
    let directory = PathBuf::from(
        std::env::var_os("ROUTING_AUDIT_DIR")
            .expect("ROUTING_AUDIT_DIR names the report's directory"),
    );
    let settings_name = std::env::var(SETTINGS).unwrap();
    settings();
    let corpora: [(&'static str, PathBuf); 3] = [
        (
            "test_files/documents",
            PathBuf::from("../test_files/documents"),
        ),
        ("quality/corpus", PathBuf::from("../quality/corpus")),
        ("conformance", formats::parity::conformance_corpus()),
    ];
    let mut rows = Vec::new();
    for (corpus, root) in corpora {
        for path in files(&root) {
            let found = audit_in_child(&path);
            eprintln!("audit {} {}", found.kind.name(), path.display());
            rows.push(Row {
                corpus,
                path,
                found,
            });
        }
    }
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("routing.md"), report(&rows, &settings_name)).unwrap();
    std::fs::write(directory.join("documents.tsv"), tsv(&rows)).unwrap();
}
