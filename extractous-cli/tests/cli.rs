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
    assert_eq!(objects[1]["metadata"]["xmpTPg:NPages"][0], "28");
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
