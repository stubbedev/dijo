use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const DOCUMENTS: &str = "../test_files/documents";

fn document(name: &str) -> String {
    format!("{DOCUMENTS}/{name}")
}

fn extractous(args: &[&str]) -> Output {
    extractous_with_stdin(args, None)
}

fn extractous_with_stdin(args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_extractous"))
        .args(args)
        .env_remove("EXTRACTOUS_FORMAT")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(bytes) = stdin {
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("extractous-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn extracts_text_to_stdout() {
    let output = extractous(&[&document("simple.odt")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("These are a few of my favorite things"));
}

#[test]
fn extracts_xhtml() {
    let output = extractous(&["--format", "xhtml", &document("simple.odt")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("<html xmlns=\"http://www.w3.org/1999/xhtml\">"));
}

#[test]
fn json_is_one_object_per_input_with_metadata() {
    let output = extractous(&[
        "--format",
        "json",
        "--metadata",
        &document("simple.odt"),
        &document("2022_Q3_AAPL.pdf"),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let objects: Vec<serde_json::Value> = stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(objects.len(), 2);
    assert_eq!(objects[0]["source"]["path"], document("simple.odt"));
    assert_eq!(objects[0]["status"], "complete");
    assert_eq!(objects[1]["metadata"]["page_count"], 28);
}

#[test]
fn text_metadata_goes_to_stderr() {
    let output = extractous(&["--metadata", &document("2022_Q3_AAPL.pdf")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let stderr = stderr(&output);
    assert!(stderr.starts_with(&format!("==> {} <==\n", document("2022_Q3_AAPL.pdf"))));
    assert!(stderr.contains("xmpTPg:NPages: 28\n"), "{stderr}");
}

#[test]
fn environment_variables_set_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_extractous"))
        .arg(document("simple.odt"))
        .env("EXTRACTOUS_FORMAT", "json")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let object: serde_json::Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert_eq!(object["status"], "complete");
}

#[test]
fn reads_standard_input() {
    let bytes = fs::read(document("simple.odt")).unwrap();
    let output = extractous_with_stdin(&["-"], Some(&bytes));
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("These are a few of my favorite things"));
}

#[test]
fn detect_prints_only_the_type_for_one_input() {
    let output = extractous(&["detect", &document("2022_Q3_AAPL.pdf")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "application/pdf\n");
}

#[test]
fn detect_labels_several_inputs() {
    let bytes = fs::read(document("simple.doc")).unwrap();
    let output = extractous_with_stdin(
        &["detect", &document("2022_Q3_AAPL.pdf"), "-"],
        Some(&bytes),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "{}: application/pdf\n-: application/msword\n",
            document("2022_Q3_AAPL.pdf")
        )
    );
}

#[test]
fn parallel_output_keeps_input_order() {
    let inputs: Vec<String> = [
        "2022_Q3_AAPL.pdf",
        "simple.odt",
        "category-level.docx",
        "vodafone.xlsx",
        "simple.doc",
    ]
    .iter()
    .map(|name| document(name))
    .collect();
    let mut sequential_args = vec!["--jobs", "1"];
    sequential_args.extend(inputs.iter().map(String::as_str));
    let mut parallel_args = vec!["--jobs", "4"];
    parallel_args.extend(inputs.iter().map(String::as_str));

    let sequential = extractous(&sequential_args);
    let parallel = extractous(&parallel_args);
    assert!(sequential.status.success(), "{}", stderr(&sequential));
    assert!(parallel.status.success(), "{}", stderr(&parallel));
    assert_eq!(stdout(&parallel), stdout(&sequential));
}

#[test]
fn output_directory_mirrors_recursive_inputs() {
    let scratch = Scratch::new("mirror");
    let output = extractous(&[
        "--recursive",
        "--output",
        scratch.0.to_str().unwrap(),
        &format!("{DOCUMENTS}/ooxml"),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    let written = scratch
        .0
        .join("test_files/documents/ooxml/smartart.pptx.txt");
    assert!(!fs::read_to_string(written).unwrap().trim().is_empty());
}

#[test]
fn a_failed_input_does_not_stop_the_batch() {
    let missing = document("does-not-exist.pdf");
    let output = extractous(&[
        &document("simple.odt"),
        &missing,
        &document("category-level.docx"),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = stdout(&output);
    assert!(stdout.contains("These are a few of my favorite things"));
    assert!(stdout.contains("Call me Ishmael"));
    let stderr = stderr(&output);
    assert!(
        stderr.contains(&format!("extractous: {missing}: ")),
        "{stderr}"
    );
    assert!(stderr.contains("NoSuchFileException"), "{stderr}");
}

#[test]
fn a_directory_needs_recursive() {
    let output = extractous(&[DOCUMENTS]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("pass --recursive"));
}

#[test]
fn usage_errors_exit_with_two() {
    let output = extractous(&["--format", "pdf", &document("simple.odt")]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn ocr_never_skips_images() {
    let output = extractous(&["--ocr", "never", &document("ara-ocr.png")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).trim().is_empty());
}

#[test]
fn ocr_language_reaches_tesseract() {
    let output = extractous(&["--ocr-lang", "ara", &document("ara-ocr.png")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let expected =
        fs::read_to_string(Path::new("../test_files/expected_result/ara-ocr.png.txt")).unwrap();
    let first_line = expected
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap();
    assert!(
        stdout(&output).contains(first_line.trim()),
        "{}",
        stdout(&output)
    );
}

fn json_objects(args: &[&str]) -> Vec<serde_json::Value> {
    let mut all = vec!["--format", "json", "--metadata", "--raw-metadata"];
    all.extend_from_slice(args);
    let output = extractous(&all);
    stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn assert_matches_schema(object: &serde_json::Value) {
    let schema: serde_json::Value =
        serde_json::from_str(&fs::read_to_string("../schema/output-v1.schema.json").unwrap())
            .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(object)
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn json_output_follows_the_published_schema() {
    let inputs: Vec<String> = [
        "2022_Q3_AAPL.pdf",
        "category-level.docx",
        "simple.odt",
        "simple.doc",
        "vodafone.xlsx",
        "simple.pptx",
        "winter-sports.epub",
        "sample.eml",
        "sample.html",
        "ara-ocr.png",
        "partially-damaged.zip",
        "does-not-exist.pdf",
    ]
    .iter()
    .map(|name| document(name))
    .collect();
    let args: Vec<&str> = inputs.iter().map(String::as_str).collect();
    let objects = json_objects(&args);
    assert_eq!(objects.len(), inputs.len());
    for object in &objects {
        assert_matches_schema(object);
    }
    assert_eq!(objects.last().unwrap()["status"], "failed");
    assert_eq!(
        objects.last().unwrap()["errors"][0]["category"],
        "unreadable"
    );
}

#[test]
fn metadata_is_normalised_per_format() {
    let objects = json_objects(&[
        &document("2022_Q3_AAPL.pdf"),
        &document("category-level.docx"),
        &document("simple.pptx"),
        &document("winter-sports.epub"),
        &document("sample.eml"),
        &document("sample.html"),
    ]);
    let metadata = |index: usize| &objects[index]["metadata"];

    assert_eq!(objects[0]["mime_type"], "application/pdf");
    assert_eq!(metadata(0)["page_count"], 28);
    assert_eq!(metadata(0)["encrypted"], true);
    assert_eq!(metadata(0)["created"]["utc"], "2022-07-29T10:03:21Z");
    assert_eq!(
        metadata(0)["authors"][0],
        "EDGAR Online, a division of Donnelley Financial Solutions"
    );

    assert_eq!(metadata(1)["language"][0]["code"], "en-US");
    assert_eq!(metadata(1)["modified"]["utc"], "2023-09-26T15:58:44Z");

    assert_eq!(metadata(2)["title"], "Title Slide");
    assert_eq!(metadata(2)["page_count"], 2);

    assert_eq!(metadata(3)["created"]["utc"], "2023-02-26T09:19:44.326437Z");
    assert_eq!(metadata(3)["language"][0]["code"], "en");

    assert_eq!(objects[4]["mime_type"], "message/rfc822");
    assert_eq!(metadata(4)["title"], "Notes on the Analytical Engine");
    assert_eq!(metadata(4)["created"]["utc"], "2024-03-05T13:30:00Z");

    assert_eq!(objects[5]["mime_type"], "text/html");
    assert_eq!(metadata(5)["title"], "Sample page");
    assert_eq!(metadata(5)["authors"][0], "Grace Hopper");
}

#[test]
fn damaged_parts_make_a_partial_result() {
    let objects = json_objects(&[&document("partially-damaged.zip")]);
    let object = &objects[0];
    assert_eq!(object["status"], "partial");
    assert!(object["text"]
        .as_str()
        .unwrap()
        .contains("This part of the archive extracts fine."));
    assert_eq!(object["errors"][0]["category"], "corrupt");
    assert_eq!(object["errors"][0]["location"], "embedded document");
}

#[test]
fn partial_results_succeed_unless_strict() {
    let lenient = extractous(&[&document("partially-damaged.zip")]);
    assert!(lenient.status.success(), "{}", stderr(&lenient));
    assert!(stderr(&lenient).contains("Error creating OOXML extractor"));

    let strict = extractous(&["--strict", &document("partially-damaged.zip")]);
    assert_eq!(strict.status.code(), Some(1));
}

fn records(format: &str, input: &str) -> Vec<serde_json::Value> {
    let output = extractous(&["--format", format, "--metadata", input]);
    assert!(output.status.success(), "{}", stderr(&output));
    let records: Vec<serde_json::Value> = stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for record in &records {
        assert_matches_schema(record);
    }
    records
}

#[test]
fn pages_are_streamed_in_order_then_closed_by_the_document() {
    let records = records("pages", &document("2022_Q3_AAPL.pdf"));
    let (document_record, pages) = records.split_last().unwrap();
    assert_eq!(pages.len(), 28);
    for (index, page) in pages.iter().enumerate() {
        assert_eq!(page["kind"], "page");
        assert_eq!(page["page"], index + 1);
    }
    assert!(pages[0]["text"]
        .as_str()
        .unwrap()
        .starts_with("UNITED STATES"));
    assert_eq!(document_record["kind"], "document");
    assert_eq!(document_record["status"], "complete");
    assert_eq!(document_record["metadata"]["page_count"], 28);
    assert!(document_record.get("text").is_none());
}

#[test]
fn slides_are_pages() {
    let records = records("pages", &document("simple.pptx"));
    let slides: Vec<_> = records
        .iter()
        .filter(|record| record.get("page").is_some())
        .collect();
    assert_eq!(slides.len(), 2);
    assert_eq!(slides[1]["page"], 2);
    assert!(slides[1]["text"]
        .as_str()
        .unwrap()
        .starts_with("Things to think about"));
}

#[test]
fn an_unpaged_document_is_one_page_record() {
    let records = records("pages", &document("category-level.docx"));
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["kind"], "page");
    assert!(records[0].get("page").is_none());
}

#[test]
fn elements_carry_their_structure() {
    let records = records("elements", &document("category-level.docx"));
    let heading = records
        .iter()
        .find(|record| record["element"] == "heading")
        .unwrap();
    assert_eq!(heading["level"], 1);
    assert_eq!(heading["text"], "A Heading 1");
    assert!(records.iter().any(|record| record["element"] == "paragraph"
        && record["text"]
            .as_str()
            .unwrap()
            .starts_with("Call me Ishmael")));
    assert_eq!(records.last().unwrap()["kind"], "document");
}

#[test]
fn a_closed_stdout_stops_quietly() {
    use std::io::Read;

    let mut child = Command::new(env!("CARGO_BIN_EXE_extractous"))
        .args(["--jobs", "1"])
        .args(std::iter::repeat_n(document("2022_Q3_AAPL.pdf"), 20))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first_bytes = [0; 16];
    child
        .stdout
        .take()
        .unwrap()
        .read_exact(&mut first_bytes)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), "");
}

#[test]
fn encrypted_inputs_are_reported_as_encrypted() {
    let objects = json_objects(&[&format!("{DOCUMENTS}/encrypted/password.docx")]);
    assert_eq!(objects[0]["status"], "encrypted");
    assert_eq!(objects[0]["errors"][0]["category"], "encrypted");
    assert_matches_schema(&objects[0]);
}

#[test]
fn an_encrypted_archive_entry_leaves_the_rest_extracted() {
    let objects = json_objects(&[&format!("{DOCUMENTS}/encrypted/encrypted-entry.zip")]);
    assert_matches_schema(&objects[0]);
    assert_eq!(objects[0]["status"], "partial");
    assert!(objects[0]["text"].as_str().unwrap().contains("hello world"));
    assert_eq!(objects[0]["errors"][0]["category"], "encrypted");
    assert!(objects[0]["errors"][0]["message"]
        .as_str()
        .unwrap()
        .contains("encrypted.txt"));
}

#[test]
fn password_file_decrypts_without_leaking_the_password() {
    let scratch = Scratch::new("passwords");
    let password_file = scratch.0.join("passwords");
    fs::write(&password_file, "not-it\nextractous\n").unwrap();
    let output = extractous(&[
        "--format",
        "json",
        "--password-file",
        password_file.to_str().unwrap(),
        &format!("{DOCUMENTS}/encrypted/user-password.pdf"),
        &format!("{DOCUMENTS}/encrypted/password.docx"),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = stdout(&output);
    assert!(!stdout.contains("extractous\""));
    let objects: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(objects.iter().all(|object| object["status"] == "complete"));
    let decrypted = json_objects(&[
        "--password-file",
        password_file.to_str().unwrap(),
        &format!("{DOCUMENTS}/encrypted/password.docx"),
    ]);
    assert_eq!(decrypted[0]["metadata"]["encrypted"], true);
    assert_eq!(decrypted[0]["metadata"]["decrypted"], true);
    assert_matches_schema(&decrypted[0]);
    assert!(objects[1]["text"]
        .as_str()
        .unwrap()
        .contains("Call me Ishmael"));
}

#[test]
fn a_wrong_password_fails_with_status_one() {
    let output = extractous(&[
        "--password",
        "not-it",
        &format!("{DOCUMENTS}/encrypted/password.7z"),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!stderr(&output).contains("not-it"));
}

#[test]
fn a_timeout_keeps_the_text_extracted_before_it() {
    let started = std::time::Instant::now();
    let objects = json_objects(&[
        "--timeout",
        "3",
        "--ocr",
        "always",
        &document("2022_Q3_AAPL.pdf"),
    ]);
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    assert_matches_schema(&objects[0]);
    assert_eq!(objects[0]["status"], "partial");
    assert!(objects[0]["text"]
        .as_str()
        .unwrap()
        .contains("UNITED STATES"));
    assert_eq!(objects[0]["errors"][0]["category"], "timeout");
}

fn embedded_paths(objects: &[serde_json::Value]) -> Vec<String> {
    objects
        .iter()
        .map(|object| {
            object["source"]["embedded_path"]
                .as_str()
                .unwrap_or("<root>")
                .to_string()
        })
        .collect()
}

#[test]
fn separate_embedded_documents_carry_their_paths() {
    let objects = json_objects(&["--embedded", "separate", &document("nested.zip")]);
    for object in &objects {
        assert_matches_schema(object);
    }
    assert_eq!(
        embedded_paths(&objects),
        [
            "<root>",
            "mail.eml",
            "mail.eml/numbers.xlsx",
            "mail.eml/numbers.xlsx/image1.png",
            "copy-a.txt",
            "copy-b.txt",
            "inner.zip",
            "inner.zip/notes.txt",
        ]
    );
    assert_eq!(objects[1]["mime_type"], "message/rfc822");
    assert!(objects[1]["text"]
        .as_str()
        .unwrap()
        .contains("The spreadsheet is attached."));
    assert!(!objects[0]["text"]
        .as_str()
        .unwrap()
        .contains("The spreadsheet is attached."));
    assert_eq!(objects[5]["duplicate_of"], "copy-a.txt");
    assert_eq!(
        objects[5]["source"]["blake3"],
        objects[4]["source"]["blake3"]
    );
    assert!(objects[7]["text"]
        .as_str()
        .unwrap()
        .contains("Notes inside the inner archive."));
}

#[test]
fn separate_text_output_labels_each_embedded_document() {
    let output = extractous(&["--embedded", "separate", &document("nested.zip")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("=== mail.eml/numbers.xlsx ==="));
    assert!(text.contains("=== copy-b.txt (same as copy-a.txt) ==="));
}

#[test]
fn embedded_limits_skip_subtrees_as_limit_exceeded() {
    let deep = json_objects(&[
        "--embedded",
        "separate",
        "--max-depth",
        "1",
        &document("nested.zip"),
    ]);
    let skipped = deep
        .iter()
        .find(|object| object["source"]["embedded_path"] == "mail.eml/numbers.xlsx")
        .unwrap();
    assert_eq!(skipped["status"], "failed");
    assert_eq!(skipped["errors"][0]["category"], "limit-exceeded");
    assert_eq!(deep[0]["status"], "complete");
    let strict = extractous(&[
        "--embedded",
        "separate",
        "--max-depth",
        "1",
        "--strict",
        &document("nested.zip"),
    ]);
    assert_eq!(strict.status.code(), Some(1));
    assert!(stderr(&strict).contains("Nested deeper than 1 levels"));
    assert!(!embedded_paths(&deep).contains(&"mail.eml/numbers.xlsx/image1.png".to_string()));

    let few = json_objects(&[
        "--embedded",
        "separate",
        "--max-embedded",
        "2",
        &document("nested.zip"),
    ]);
    assert_eq!(
        few.iter()
            .filter(|object| object["errors"][0]["category"] == "limit-exceeded")
            .count(),
        4
    );

    let small = json_objects(&[
        "--embedded",
        "separate",
        "--max-expanded-bytes",
        "100",
        &document("nested.zip"),
    ]);
    assert!(small
        .iter()
        .any(|object| object["errors"][0]["category"] == "limit-exceeded"));
    for object in deep.iter().chain(&few).chain(&small) {
        assert_matches_schema(object);
    }
}

#[test]
fn spool_dir_holds_the_temporary_files() {
    let scratch = Scratch::new("spool-dir");
    let bytes = fs::read(document("simple.odt")).unwrap();
    let output = extractous_with_stdin(
        &[
            "--spool-dir",
            scratch.0.to_str().unwrap(),
            "--jobs",
            "2",
            "-",
            &document("simple.doc"),
        ],
        Some(&bytes),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("These are a few of my favorite things"));
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);

    let missing = extractous(&["--spool-dir", "/nonexistent/spool", &document("simple.odt")]);
    assert_eq!(missing.status.code(), Some(1));
}

#[test]
fn licenses_prints_the_licence_and_third_party_notices() {
    let output = extractous(&["--licenses"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("Apache License"));
    assert!(text.contains("| `clap` |"));
    assert!(text.contains("| `org.apache.tika:tika-core` |"));

    let with_input = extractous(&["--licenses", &document("simple.odt")]);
    assert_eq!(with_input.status.code(), Some(2));
}

#[test]
fn text_is_normalised_unless_raw() {
    let input = document("unnormalized.txt");
    let normalised = stdout(&extractous(&[&input]));
    assert!(normalised
        .starts_with("The final office report on caf\u{e9} extraction.\n\nEnd of file.\n"));

    let raw = stdout(&extractous(&["--raw", &input]));
    assert!(raw.contains(
        "The \u{fb01}nal o\u{fb03}ce\u{a0}\u{a0}report on cafe\u{301} extrac\u{ad}tion."
    ));
    assert!(raw.contains("End\u{200b} of  file."));

    let keep_ligatures = stdout(&extractous(&[
        "--no-normalize",
        "ligatures,whitespace",
        &input,
    ]));
    assert!(keep_ligatures
        .contains("The \u{fb01}nal o\u{fb03}ce\u{a0}\u{a0}report on caf\u{e9} extraction."));

    let json = stdout(&extractous(&["--format", "json", &input]));
    let document: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(document["text"]
        .as_str()
        .unwrap()
        .starts_with("The final office report"));

    let conflicting = extractous(&["--raw", "--no-normalize", "unicode", &input]);
    assert_eq!(conflicting.status.code(), Some(2));
}

fn cache_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    for shard in fs::read_dir(dir).unwrap() {
        let shard = shard.unwrap().path();
        for entry in fs::read_dir(&shard).unwrap() {
            entries.push(entry.unwrap().path());
        }
    }
    entries.sort();
    entries
}

#[test]
fn the_cache_replays_complete_extractions() {
    let scratch = Scratch::new("cache");
    let cache = scratch.0.join("cache");
    let cache_arg = cache.to_str().unwrap();
    let input = document("simple.odt");

    let first = extractous(&["--cache-dir", cache_arg, &input]);
    assert!(first.status.success(), "{}", stderr(&first));
    let entries = cache_entries(&cache);
    assert_eq!(entries.len(), 1);

    let stored = fs::read(&entries[0]).unwrap();
    let trailer_length =
        u64::from_le_bytes(stored[stored.len() - 8..].try_into().unwrap()) as usize;
    let output_length = stored.len() - 8 - trailer_length;
    assert_eq!(&stored[..output_length], first.stdout.as_slice());
    let mut replaced = b"served from the cache\n".to_vec();
    replaced.extend_from_slice(&stored[output_length..]);
    fs::write(&entries[0], replaced).unwrap();

    let second = extractous(&["--cache-dir", cache_arg, &input]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(stdout(&second), "served from the cache\n");

    let raw = extractous(&["--cache-dir", cache_arg, "--raw", &input]);
    assert!(stdout(&raw).contains("These are a few of my favorite things"));
    assert_eq!(cache_entries(&cache).len(), 2);

    let failed = extractous(&[
        "--cache-dir",
        cache_arg,
        &format!("{DOCUMENTS}/encrypted/password.docx"),
    ]);
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(cache_entries(&cache).len(), 2);

    let evicted = extractous(&["--cache-dir", cache_arg, "--cache-max-size", "0", &input]);
    assert!(evicted.status.success(), "{}", stderr(&evicted));
    assert!(cache_entries(&cache).is_empty());
}

#[test]
fn the_cache_refuses_passwords_and_separate_embedded_documents() {
    let input = document("simple.odt");
    let with_password = extractous(&["--cache-dir", "unused", "--password", "x", &input]);
    assert_eq!(with_password.status.code(), Some(2));
    let separate = extractous(&["--cache-dir", "unused", "--embedded", "separate", &input]);
    assert_eq!(separate.status.code(), Some(2));
    assert!(stderr(&separate).contains("--embedded separate"));
    assert!(!Path::new("unused").exists());
}
