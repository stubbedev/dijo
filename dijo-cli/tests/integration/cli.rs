use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::common::{document, text_pdf, Scratch, StoredZip, DOCUMENTS};

fn dijo(args: &[&str]) -> Output {
    dijo_with_stdin(args, None)
}

fn dijo_with_stdin(args: &[&str], stdin: Option<&[u8]>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dijo"))
        .args(args)
        .env_remove("DIJO_FORMAT")
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

/// The text a successful extraction with `args` writes.
fn extracted(args: &[&str]) -> String {
    let output = dijo(args);
    assert!(output.status.success(), "{args:?}: {}", stderr(&output));
    stdout(&output)
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn extracts_text_to_stdout() {
    let output = dijo(&[&document("simple.odt")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("These are a few of my favorite things"));
}

#[test]
fn extracts_xhtml() {
    let output = dijo(&["--format", "xhtml", &document("simple.odt")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("<html xmlns=\"http://www.w3.org/1999/xhtml\">"));
}

#[test]
fn json_is_one_object_per_input_with_metadata() {
    let output = dijo(&[
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
    let output = dijo(&["--metadata", &document("2022_Q3_AAPL.pdf")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let stderr = stderr(&output);
    assert!(stderr.starts_with(&format!("==> {} <==\n", document("2022_Q3_AAPL.pdf"))));
    assert!(stderr.contains("xmpTPg:NPages: 28\n"), "{stderr}");
}

#[test]
fn environment_variables_set_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_dijo"))
        .arg(document("simple.odt"))
        .env("DIJO_FORMAT", "json")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let object: serde_json::Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert_eq!(object["status"], "complete");
}

#[test]
fn reads_standard_input() {
    let bytes = fs::read(document("simple.odt")).unwrap();
    let output = dijo_with_stdin(&["-"], Some(&bytes));
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("These are a few of my favorite things"));
}

#[test]
fn detect_prints_only_the_type_for_one_input() {
    let output = dijo(&["detect", &document("2022_Q3_AAPL.pdf")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "application/pdf\n");
}

#[test]
fn detect_labels_several_inputs() {
    let bytes = fs::read(document("simple.doc")).unwrap();
    let output = dijo_with_stdin(
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

    let sequential = dijo(&sequential_args);
    let parallel = dijo(&parallel_args);
    assert!(sequential.status.success(), "{}", stderr(&sequential));
    assert!(parallel.status.success(), "{}", stderr(&parallel));
    assert_eq!(stdout(&parallel), stdout(&sequential));
}

#[test]
fn output_directory_mirrors_recursive_inputs() {
    let scratch = Scratch::new("mirror");
    let output = dijo(&[
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
    let output = dijo(&[
        &document("simple.odt"),
        &missing,
        &document("category-level.docx"),
    ]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = stdout(&output);
    assert!(stdout.contains("These are a few of my favorite things"));
    assert!(stdout.contains("Call me Ishmael"));
    let stderr = stderr(&output);
    assert!(stderr.contains(&format!("dijo: {missing}: ")), "{stderr}");
    let not_found = std::fs::read(&missing).unwrap_err().to_string();
    assert!(stderr.contains(&not_found), "{stderr}");
}

#[test]
fn worker_processes_give_the_same_output_in_input_order() {
    let missing = document("does-not-exist.pdf");
    let inputs = [
        document("simple.odt"),
        "-".to_string(),
        missing.clone(),
        document("category-level.docx"),
        document("2022_Q3_AAPL.pdf"),
        "../quality/corpus/danish.docx".to_string(),
        "../quality/corpus/prose.rtf".to_string(),
    ];
    let run = |jobs: &str| {
        let mut args = vec!["--format", "json", "--metadata", "--jobs", jobs];
        args.extend(inputs.iter().map(String::as_str));
        dijo_with_stdin(&args, Some(b"plain text from standard input\n"))
    };
    let in_process = run("1");
    let in_workers = run("6");
    assert_eq!(in_workers.status.code(), Some(1));
    assert_eq!(in_process.status.code(), in_workers.status.code());
    assert_eq!(stdout(&in_process), stdout(&in_workers));
    assert!(stdout(&in_workers).contains("plain text from standard input"));
    assert_eq!(stdout(&in_workers).lines().count(), inputs.len());
    assert!(stderr(&in_workers).contains(&format!("dijo: {missing}: ")));
}

#[cfg(target_os = "linux")]
#[test]
fn a_killed_worker_process_does_not_lose_its_documents() {
    let scratch = Scratch::new("killed-worker");
    let inputs: Vec<String> = (0..96)
        .map(|index| {
            let path = scratch.0.join(format!("report-{index:02}.pdf"));
            fs::copy(document("2022_Q3_AAPL.pdf"), &path).unwrap();
            path.to_str().unwrap().to_string()
        })
        .collect();
    let mut child = Command::new(env!("CARGO_BIN_EXE_dijo"))
        .args(["--format", "json", "--jobs", "4"])
        .args(&inputs)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let parent = child.id().to_string();
    let worker = loop {
        let found = fs::read_dir("/proc").unwrap().find_map(|entry| {
            let pid = entry.ok()?.file_name().into_string().ok()?;
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let after_name = &stat[stat.rfind(')')? + 2..];
            let ppid = after_name.split(' ').nth(1)?;
            (ppid == parent).then_some(pid)
        });
        if let Some(pid) = found {
            break pid;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "no worker process started"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut written = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut lines = String::new();
    std::io::BufRead::read_line(&mut written, &mut lines).unwrap();
    assert!(Command::new("kill")
        .args(["-KILL", &worker])
        .status()
        .unwrap()
        .success());
    std::io::Read::read_to_string(&mut written, &mut lines).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let records: Vec<serde_json::Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), inputs.len());
    for (record, input) in records.iter().zip(&inputs) {
        assert_eq!(record["source"]["path"], input.as_str());
        assert_eq!(record["status"], "complete", "{record}");
    }
}

#[test]
fn a_directory_needs_recursive() {
    let output = dijo(&[DOCUMENTS]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("pass --recursive"));
}

#[test]
fn usage_errors_exit_with_two() {
    let output = dijo(&["--format", "pdf", &document("simple.odt")]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn ocr_never_skips_images() {
    let output = dijo(&["--ocr", "never", &document("ara-ocr.png")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).trim().is_empty());
}

#[test]
fn ocr_language_reaches_tesseract() {
    let output = dijo(&["--ocr-lang", "ara", &document("ara-ocr.png")]);
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

#[test]
fn an_ocr_language_that_is_not_installed_is_a_usage_error_naming_those_that_are() {
    let output = dijo(&["--ocr-lang", "eng+xyz", &document("ara-ocr.png")]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(
        message.contains("The OCR language xyz is not installed"),
        "{message}"
    );
    assert!(message.contains("spa"), "{message}");
    assert!(!message.contains("Exception"), "{message}");
    assert!(stdout(&output).is_empty());
}

#[test]
fn every_national_language_of_europe_is_an_ocr_language() {
    let output = dijo(&["--ocr-lang", "spa+fra+ita+pol", &document("eng-ocr.pdf")]);
    assert!(output.status.success(), "{}", stderr(&output));
}

fn json_objects(args: &[&str]) -> Vec<serde_json::Value> {
    let mut all = vec!["--format", "json", "--metadata", "--raw-metadata"];
    all.extend_from_slice(args);
    let output = dijo(&all);
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
fn a_sqlite_database_is_read_for_its_tables() {
    let objects = json_objects(&[&document("notes.sqlite")]);
    assert_ne!(objects[0]["status"], "failed", "{}", objects[0]);
    assert_eq!(objects[0]["mime_type"], "application/x-sqlite3");
    let text = objects[0]["text"].as_str().unwrap();
    assert!(
        text.contains("The lamp was lit at seven minutes past five."),
        "{text}"
    );
    assert!(text.contains("Harbour"), "{text}");
}

#[test]
fn djvu_documents_are_read_for_their_text_layers() {
    let objects = json_objects(&[&document("ledger.djvu"), &document("storm.djvu")]);
    assert_ne!(objects[0]["status"], "failed", "{}", objects[0]);
    assert_eq!(objects[0]["mime_type"], "image/vnd.djvu");
    assert_eq!(objects[0]["metadata"]["page_count"], 3);
    assert_eq!(objects[1]["text"], "The second page holds the storm.\n");
    let pages = page_records(&[], &document("ledger.djvu"));
    let texts: Vec<&str> = pages
        .iter()
        .map(|page| page["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            "The keeper climbed the hundred and twelve steps.\n\
             He wrote one line in the ledger.\nShips passed before midnight.",
            "The second page holds the storm.",
            ""
        ]
    );
}

#[test]
fn hwpx_documents_are_read_as_paragraphs_and_tables() {
    let scratch = Scratch::new("hwpx");
    let path = scratch.0.join("report.hwpx");
    let paragraph = "http://www.hancom.co.kr/hwpml/2011/paragraph";
    let section = "http://www.hancom.co.kr/hwpml/2011/section";
    let run = |text: &str| format!("<hp:run><hp:t>{text}</hp:t></hp:run>");
    let in_cell = |text: &str| {
        format!(
            "<hp:tc><hp:subList><hp:p>{}</hp:p></hp:subList></hp:tc>",
            run(text)
        )
    };
    let body = [
        format!(
            "<hp:p><hp:run><hp:ctrl><hp:header><hp:subList><hp:p>{}</hp:p></hp:subList>\
             </hp:header></hp:ctrl><hp:t>Lighthouse<hp:tab/>report<hp:lineBreak/>second line</hp:t>\
             </hp:run></hp:p>",
            run("Running header")
        ),
        format!(
            "<hp:p>{}<hp:run><hp:tbl><hp:tr>{}{}</hp:tr></hp:tbl></hp:run>{}</hp:p>",
            run("Before the table."),
            in_cell("Cell A"),
            in_cell("Cell B"),
            run("After the table.")
        ),
        format!(
            "<hp:p>{}<hp:run><hp:ctrl><hp:footNote><hp:subList><hp:p>{}</hp:p></hp:subList>\
             </hp:footNote></hp:ctrl></hp:run></hp:p>",
            run("Noted text."),
            run("A footnote.")
        ),
    ]
    .concat();
    let parts = [
        ("mimetype", "application/hwp+zip".to_string()),
        (
            "META-INF/container.xml",
            "<?xml version=\"1.0\"?><ocf:container xmlns:ocf=\"urn:oasis:names:tc:opendocument:xmlns:container\">\
             <ocf:rootfiles><ocf:rootfile full-path=\"Contents/content.hpf\" media-type=\"application/hwpml-package+xml\"/>\
             </ocf:rootfiles></ocf:container>"
                .to_string(),
        ),
        (
            "Contents/content.hpf",
            "<?xml version=\"1.0\"?><opf:package xmlns:opf=\"http://www.idpf.org/2007/opf/\"><opf:metadata>\
             <opf:title>Lighthouse report</opf:title><opf:language>ko</opf:language>\
             <opf:meta name=\"creator\" content=\"text\">Keeper</opf:meta></opf:metadata><opf:manifest>\
             <opf:item id=\"header\" href=\"Contents/header.xml\" media-type=\"application/xml\"/>\
             <opf:item id=\"section0\" href=\"Contents/section0.xml\" media-type=\"application/xml\"/>\
             </opf:manifest><opf:spine><opf:itemref idref=\"header\"/><opf:itemref idref=\"section0\"/>\
             </opf:spine></opf:package>"
                .to_string(),
        ),
        (
            "Contents/header.xml",
            "<?xml version=\"1.0\"?><hh:head xmlns:hh=\"http://www.hancom.co.kr/hwpml/2011/head\">\
             <hh:fontface>Style names are not text</hh:fontface></hh:head>"
                .to_string(),
        ),
        (
            "Contents/section0.xml",
            format!(
                "<?xml version=\"1.0\"?><hs:sec xmlns:hs=\"{section}\" xmlns:hp=\"{paragraph}\">{body}</hs:sec>"
            ),
        ),
    ];
    let mut zip = StoredZip::new(fs::File::create(&path).unwrap());
    for (name, content) in &parts {
        zip.entry(name, |out| out.write_all(content.as_bytes()))
            .unwrap();
    }
    zip.finish().unwrap();

    let objects = json_objects(&[path.to_str().unwrap()]);
    let object = &objects[0];
    assert_eq!(object["status"], "complete", "{object}");
    assert_eq!(object["mime_type"], "application/hwp+zip");
    assert_eq!(object["metadata"]["title"], "Lighthouse report");
    assert_eq!(object["metadata"]["authors"][0], "Keeper");
    let text = object["text"].as_str().unwrap();
    assert!(
        text.starts_with(
            "Running header\nLighthouse\treport\nsecond line\nBefore the table.\nCell A\tCell B\n"
        ),
        "{text}"
    );
    assert!(text.contains("Cell A\tCell B"), "{text}");
    assert!(text.contains("After the table."), "{text}");
    assert!(text.contains("A footnote."), "{text}");
    assert!(!text.contains("Style names"), "{text}");
}

#[test]
fn partial_results_succeed_unless_strict() {
    let lenient = dijo(&[&document("partially-damaged.zip")]);
    assert!(lenient.status.success(), "{}", stderr(&lenient));
    assert!(stderr(&lenient).contains("Truncated ZIP file"));

    let strict = dijo(&["--strict", &document("partially-damaged.zip")]);
    assert_eq!(strict.status.code(), Some(1));
}

fn records(format: &str, input: &str) -> Vec<serde_json::Value> {
    let output = dijo(&["--format", format, "--metadata", input]);
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

    let mut child = Command::new(env!("CARGO_BIN_EXE_dijo"))
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
    let output = dijo(&[
        "--format",
        "json",
        "--password-file",
        password_file.to_str().unwrap(),
        &format!("{DOCUMENTS}/encrypted/user-password.pdf"),
        &format!("{DOCUMENTS}/encrypted/password.docx"),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = stdout(&output);
    assert!(!stdout.contains("dijo\""));
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
    let output = dijo(&[
        "--password",
        "not-it",
        &format!("{DOCUMENTS}/encrypted/password.7z"),
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!stderr(&output).contains("not-it"));
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
    let output = dijo(&["--embedded", "separate", &document("nested.zip")]);
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
    let strict = dijo(&[
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
    let output = dijo_with_stdin(
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

    let missing_dir = Path::new("/nonexistent/spool");
    let missing = dijo(&[
        "--spool-dir",
        missing_dir.to_str().unwrap(),
        &document("simple.odt"),
    ]);
    assert_eq!(missing.status.code(), Some(1));
    let expected = format!(
        "Failed to create the spool directory {}",
        missing_dir.join("dijo-").display()
    );
    assert!(stderr(&missing).contains(&expected), "{}", stderr(&missing));
}

#[test]
fn licenses_prints_the_licence_and_third_party_notices() {
    let output = dijo(&["--licenses"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("Apache License"));
    assert!(text.contains("| `clap` |"));
    assert!(
        !text.contains("| `org.apache.tika:tika-core` |"),
        "no release holds Tika, so no Java library's licence is printed"
    );

    let with_input = dijo(&["--licenses", &document("simple.odt")]);
    assert_eq!(with_input.status.code(), Some(2));
}

#[test]
fn text_is_normalised_unless_raw() {
    let input = document("unnormalized.txt");
    let normalised = extracted(&[&input]);
    assert!(normalised
        .starts_with("The final office report on caf\u{e9} extraction.\n\nEnd of file.\n"));

    let raw = extracted(&["--raw", &input]);
    assert!(raw.contains(
        "The \u{fb01}nal o\u{fb03}ce\u{a0}\u{a0}report on cafe\u{301} extrac\u{ad}tion."
    ));
    assert!(raw.contains("End\u{200b} of  file."));

    let keep_ligatures = extracted(&["--no-normalize", "ligatures,whitespace", &input]);
    assert!(keep_ligatures
        .contains("The \u{fb01}nal o\u{fb03}ce\u{a0}\u{a0}report on caf\u{e9} extraction."));

    let json = extracted(&["--format", "json", &input]);
    let document: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(document["text"]
        .as_str()
        .unwrap()
        .starts_with("The final office report"));

    let conflicting = dijo(&["--raw", "--no-normalize", "unicode", &input]);
    assert_eq!(conflicting.status.code(), Some(2));
}

fn cache_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    for shard in fs::read_dir(dir).unwrap() {
        let shard = shard.unwrap().path();
        if shard.file_name().unwrap() == "paths" {
            continue;
        }
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

    let first = dijo(&["--cache-dir", cache_arg, &input]);
    assert!(first.status.success(), "{}", stderr(&first));
    let entries = cache_entries(&cache);
    assert_eq!(entries.len(), 1);

    let stored = fs::read(&entries[0]).unwrap();
    let trailer_length =
        u64::from_le_bytes(stored[stored.len() - 8..].try_into().unwrap()) as usize;
    assert_eq!(
        &stored[..stored.len() - 8 - trailer_length],
        first.stdout.as_slice()
    );
    rewrite_cache_entry(&entries[0], |_| "served from the cache\n".to_string());

    let second = dijo(&["--cache-dir", cache_arg, &input]);
    assert!(second.status.success(), "{}", stderr(&second));
    assert_eq!(stdout(&second), "served from the cache\n");

    let raw = dijo(&["--cache-dir", cache_arg, "--raw", &input]);
    assert!(stdout(&raw).contains("These are a few of my favorite things"));
    assert_eq!(cache_entries(&cache).len(), 2);

    let failed = dijo(&[
        "--cache-dir",
        cache_arg,
        &format!("{DOCUMENTS}/encrypted/password.docx"),
    ]);
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(cache_entries(&cache).len(), 2);

    let evicted = dijo(&["--cache-dir", cache_arg, "--cache-max-size", "0", &input]);
    assert!(evicted.status.success(), "{}", stderr(&evicted));
    assert!(cache_entries(&cache).is_empty());
}

#[test]
fn the_cache_refuses_passwords() {
    let input = document("simple.odt");
    let with_password = dijo(&["--cache-dir", "unused", "--password", "x", &input]);
    assert_eq!(with_password.status.code(), Some(2));
    assert!(stderr(&with_password).contains("--password"));
    assert!(!Path::new("unused").exists());
}

/// Replaces the output stored in a cache entry, keeping its trailer.
fn rewrite_cache_entry(entry: &Path, rewrite: impl FnOnce(&str) -> String) {
    let stored = fs::read(entry).unwrap();
    let trailer_length =
        u64::from_le_bytes(stored[stored.len() - 8..].try_into().unwrap()) as usize;
    let output_length = stored.len() - 8 - trailer_length;
    let mut replaced = rewrite(std::str::from_utf8(&stored[..output_length]).unwrap()).into_bytes();
    replaced.extend_from_slice(&stored[output_length..]);
    fs::write(entry, replaced).unwrap();
}

#[test]
fn the_cache_extracts_an_embedded_document_once_wherever_it_is_embedded() {
    let scratch = Scratch::new("cache-embedded");
    let cache = scratch.0.join("cache");
    let first = scratch.0.join("first.zip");
    let second = scratch.0.join("second.zip");
    fs::copy(document("nested.zip"), &first).unwrap();
    fs::copy(document("nested.zip"), &second).unwrap();
    let run = |input: &Path| {
        let output = dijo(&[
            "--cache-dir",
            cache.to_str().unwrap(),
            "--embedded",
            "separate",
            "--format",
            "json",
            input.to_str().unwrap(),
        ]);
        assert!(output.status.success(), "{}", stderr(&output));
        stdout(&output)
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>()
    };

    let first_records = run(&first);
    let entries = cache_entries(&cache);
    assert_eq!(
        entries.len(),
        3,
        "one entry per document without embedded ones"
    );
    let notes = entries
        .iter()
        .find(|entry| String::from_utf8_lossy(&fs::read(entry).unwrap()).contains("Notes inside"))
        .unwrap();
    rewrite_cache_entry(notes, |output| {
        output.replace("Notes inside", "Cached notes inside")
    });

    let second_records = run(&second);
    assert_eq!(cache_entries(&cache).len(), 3);
    assert_eq!(first_records.len(), second_records.len());
    for (first_record, second_record) in first_records.iter().zip(&second_records) {
        assert_matches_schema(second_record);
        assert_eq!(second_record["source"]["path"], second.to_str().unwrap());
        assert_eq!(
            second_record["source"]["embedded_path"],
            first_record["source"]["embedded_path"]
        );
        let expected = if second_record["source"]["embedded_path"] == "inner.zip/notes.txt" {
            serde_json::json!("Cached notes inside the inner archive.\n")
        } else {
            first_record["text"].clone()
        };
        assert_eq!(second_record["text"], expected);
    }
}

#[test]
fn a_heap_cap_fails_one_extraction_and_not_the_next() {
    let hungry = document("many-shared-strings.xlsx");
    let uncapped = json_objects(&[&hungry]);
    assert_eq!(uncapped[0]["status"], "complete");

    let capped = json_objects(&[
        "--max-heap-bytes",
        "67108864",
        "--jobs",
        "1",
        &hungry,
        &document("simple.odt"),
    ]);
    assert_matches_schema(&capped[0]);
    assert_eq!(capped[0]["status"], "failed");
    assert_eq!(capped[0]["errors"][0]["category"], "limit-exceeded");
    assert!(capped[0]["errors"][0]["message"]
        .as_str()
        .unwrap()
        .contains("exceeded its memory budget of 67108864 bytes"));
    assert_eq!(capped[1]["status"], "complete");
}

#[test]
fn the_cache_keeps_partial_results_only_when_asked() {
    let scratch = Scratch::new("cache-partial");
    let cache = scratch.0.join("cache");
    let cache_arg = cache.to_str().unwrap();
    let damaged = document("partially-damaged.zip");

    let without = json_objects(&["--cache-dir", cache_arg, &damaged]);
    assert_eq!(without[0]["status"], "partial");
    assert!(cache_entries(&cache).is_empty());

    let first = json_objects(&["--cache-dir", cache_arg, "--cache-partial", &damaged]);
    assert_eq!(cache_entries(&cache).len(), 1);
    let replayed = json_objects(&["--cache-dir", cache_arg, "--cache-partial", &damaged]);
    assert_eq!(replayed, first);
    assert_eq!(replayed[0]["status"], "partial");
    assert!(!replayed[0]["errors"].as_array().unwrap().is_empty());
}

#[test]
fn trusted_modification_times_skip_rehashing() {
    let scratch = Scratch::new("cache-mtime");
    let cache = scratch.0.join("cache");
    let cache_arg = cache.to_str().unwrap();
    let input = scratch.0.join("note.txt");
    fs::write(&input, "first version\n").unwrap();
    let modified = fs::metadata(&input).unwrap().modified().unwrap();
    let input_arg = input.to_str().unwrap();

    let first = dijo(&["--cache-dir", cache_arg, "--cache-trust-mtime", input_arg]);
    assert!(stdout(&first).contains("first version"));

    fs::write(&input, "other version\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&input)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let trusted = dijo(&["--cache-dir", cache_arg, "--cache-trust-mtime", input_arg]);
    assert!(stdout(&trusted).contains("first version"));
    let hashed = dijo(&["--cache-dir", cache_arg, input_arg]);
    assert!(stdout(&hashed).contains("other version"));
}

#[test]
fn the_language_is_detected_per_document_and_page() {
    for (name, code, script) in [
        ("german.docx", "deu", "Latin"),
        ("arabic.docx", "ara", "Arabic"),
        ("hebrew.docx", "heb", "Hebrew"),
        ("hindi.docx", "hin", "Devanagari"),
    ] {
        let objects = json_objects(&[&format!("../quality/corpus/{name}")]);
        assert_matches_schema(&objects[0]);
        let detected = &objects[0]["detected_language"];
        assert_eq!(detected["code"], code, "{name}");
        assert_eq!(detected["script"], script, "{name}");
        assert_eq!(detected["reliable"], true, "{name}");
    }
    let pages = dijo(&["--format", "pages", "../quality/corpus/report.pdf"]);
    let records: Vec<serde_json::Value> = stdout(&pages)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records[0]["detected_language"]["code"], "eng");
    assert!(records.iter().all(|record| {
        assert_matches_schema(record);
        true
    }));
}

fn page_records(args: &[&str], input: &str) -> Vec<serde_json::Value> {
    let mut all = vec!["--format", "pages"];
    all.extend_from_slice(args);
    all.push(input);
    extracted(&all)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| record["page"].is_number())
        .collect()
}

#[test]
fn page_furniture_a_pdf_marks_leaves_the_page_text() {
    let report = "../quality/corpus/report.pdf";
    let pages = page_records(&[], report);
    assert_eq!(pages.len(), 2);
    for (number, page) in (1..).zip(&pages) {
        assert_matches_schema(page);
        assert_eq!(
            page["artifacts"],
            format!("Lighthouse Ledger Study\nPage {number}")
        );
        let text = page["text"].as_str().unwrap();
        assert!(!text.contains("Lighthouse Ledger Study") && !text.contains("Page "));
    }
    let kept = page_records(&["--no-normalize", "headers"], report);
    assert!(kept[0].get("artifacts").is_none());
    assert!(kept[0]["text"]
        .as_str()
        .unwrap()
        .ends_with("Lighthouse Ledger Study\nPage 1"));
}

#[test]
fn running_headers_and_footers_leave_the_page_text() {
    let scratch = Scratch::new("running-lines");
    let pdf = scratch.0.join("generated.pdf");
    fs::write(&pdf, text_pdf(3)).unwrap();
    let pdf = pdf.to_str().unwrap();
    let first = "Line 0 of a long generated document, written to keep the parser busy.";
    let last = "Line 59 of a long generated document, written to keep the parser busy.";
    let pages = page_records(&[], pdf);
    assert_eq!(pages.len(), 3);
    for page in &pages {
        assert_matches_schema(page);
        assert_eq!(page["header"], first);
        assert_eq!(page["footer"], last);
        let text = page["text"].as_str().unwrap();
        assert!(!text.contains(first) && !text.contains(last));
    }
    let kept = page_records(&["--no-normalize", "headers"], pdf);
    assert!(kept[0].get("header").is_none());
    assert!(kept[0]["text"].as_str().unwrap().starts_with(first));
}

#[cfg(unix)]
#[test]
fn no_ocr_means_no_tesseract_run() {
    let scratch = Scratch::new("no-ocr");
    let log = scratch.0.join("calls");
    let path = scratch.path_with_tesseract(&format!("echo \"$*\" >> '{}'", log.display()));
    let output = Command::new(env!("CARGO_BIN_EXE_dijo"))
        .args(["--ocr", "never", &document("science-exploration-1p.pptx")])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.lines().all(|line| !line.contains("--psm")
            && !line.contains(".png")
            && !line.contains("apache-tika")),
        "tesseract ran on an image although OCR is off:\n{calls}"
    );
}

#[test]
fn a_worker_memory_limit_needs_process_isolation() {
    let output = dijo(&["serve", "--worker-max-rss-bytes", "1000000"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("--worker-max-rss-bytes requires --isolation process"));
}

fn assert_text_layer_replaced(pdf: &str, fault: &str, ocr_text: &str, layer_text: &str) {
    let objects = json_objects(&[pdf]);
    let document = &objects[0];
    assert_matches_schema(document);
    assert!(
        document["text"].as_str().unwrap().contains(ocr_text),
        "{document}"
    );
    assert_eq!(
        document["metadata"]["rejected_text_layers"],
        serde_json::json!([{ "page": 1, "fault": fault }])
    );

    let kept = extracted(&["--ocr", "never", pdf]);
    assert!(kept.contains(layer_text), "{kept}");
}

#[test]
fn a_garbage_pdf_text_layer_is_replaced_by_ocr() {
    assert_text_layer_replaced(
        "../quality/corpus/prose-broken-offset.pdf",
        "too-few-vowels",
        "Every evening at dusk the keeper climbed",
        "7KH /LJKWKRXVH",
    );
}

#[test]
fn a_text_layer_splitting_arabic_words_is_replaced_by_ocr() {
    assert_text_layer_replaced(
        "../quality/corpus/arabic.pdf",
        "fragmented-words",
        "كان الحارس يصعد درج البرج كل مساء",
        "يصعد درج الب",
    );
}

#[test]
fn a_pdf_page_drawn_out_of_order_is_read_in_reading_order() {
    let pdf = "../quality/corpus/prose-columns-reversed.pdf";
    let title = "The Lighthouse Keeper's Ledger";
    let left = "Every evening at dusk the keeper climbed";
    let right = "that no ships passed. The next morning,";
    let position = |text: &str, line: &str| text.find(line).unwrap();

    let read = extracted(&["--ocr", "never", pdf]);
    assert!(position(&read, title) < position(&read, left), "{read}");
    assert!(position(&read, left) < position(&read, right), "{read}");

    for args in [
        &["--ocr", "never", "--no-normalize", "reading-order", pdf][..],
        &["--ocr", "never", "--raw", pdf],
        &["--ocr", "never", "--format", "xhtml", pdf],
    ] {
        let drawn = extracted(args);
        assert!(position(&drawn, right) < position(&drawn, left), "{args:?}");
        assert!(position(&drawn, left) < position(&drawn, title), "{args:?}");
    }
}

#[test]
fn pdf_bookmarks_are_left_out_unless_asked_for() {
    let pdf = "../quality/corpus/prose.pdf";
    let title = "The Lighthouse Keeper's Ledger";
    let text = extracted(&["--ocr", "never", pdf]);
    assert_eq!(text.matches(title).count(), 1, "{text}");
    for args in [
        &["--ocr", "never", "--no-normalize", "bookmarks", pdf][..],
        &["--ocr", "never", "--raw", pdf],
    ] {
        let kept = extracted(args);
        assert_eq!(kept.matches(title).count(), 2, "{args:?}: {kept}");
    }
}

#[test]
fn a_page_written_vertically_is_read_column_by_column() {
    let pdf = "../quality/corpus/japanese-vertical.pdf";
    let text = extracted(&["--ocr", "never", pdf]);
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("灯台守の記録"), "{text}");
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("毎日夕暮れになると、灯台守は塔の"),
        "{text}"
    );

    let drawn = extracted(&["--ocr", "never", "--no-normalize", "reading-order", pdf]);
    assert!(drawn.starts_with("灯\n台\n守\n"), "{drawn}");
}

fn notes_text(extension: &str) -> Vec<String> {
    let path = format!("../quality/corpus/notes.{extension}");
    [
        &["--ocr", "never", path.as_str()][..],
        &["--ocr", "never", "--no-normalize", "headers", &path],
    ]
    .iter()
    .map(|args| extracted(args))
    .collect()
}

#[test]
fn table_cells_holding_paragraphs_share_their_row() {
    for extension in ["odt", "docx", "doc", "pdf"] {
        for text in notes_text(extension) {
            let rows: Vec<&str> = text.lines().map(str::trim).collect();
            assert!(rows.contains(&"Year\tShips"), "{extension}: {text}");
            assert!(rows.contains(&"1921\t412"), "{extension}: {text}");
        }
    }
}

#[test]
fn a_footnote_reference_keeps_its_sentence_whole() {
    for extension in ["odt", "docx", "doc"] {
        for text in notes_text(extension) {
            assert!(
                text.contains("every ship he recorded in the ledger."),
                "{extension}: {text}"
            );
        }
    }
}

#[test]
fn comments_follow_the_sentence_they_are_anchored_in() {
    for extension in ["odt", "docx", "doc"] {
        let path = format!("../quality/corpus/revisions.{extension}");
        for args in [&[path.as_str()][..], &["--no-normalize", "headers", &path]] {
            let text = extracted(args);
            let sentence = text
                .find("Historians found that the plain entries told a richer story.")
                .unwrap_or_else(|| panic!("{extension} {args:?}: {text}"));
            let comment = text.find("Cite the archive catalogue here.").unwrap();
            assert!(sentence < comment, "{extension} {args:?}: {text}");
            assert!(!text.contains('\u{fffd}'), "{extension} {args:?}: {text}");
        }
    }
}

#[test]
fn images_tesseract_cannot_read_are_decoded_for_it() {
    let scans = ["psd", "pcx", "tga", "jb2"]
        .map(|extension| format!("../quality/corpus/prose-scan.{extension}"));
    let objects = json_objects(&scans.each_ref().map(String::as_str));
    for (scan, read) in scans.iter().zip(&objects) {
        assert_matches_schema(read);
        assert_eq!(read["status"], "complete", "{scan}: {read}");
        assert!(
            read["text"].as_str().unwrap().contains("Lighthouse Keeper"),
            "{scan}: {read}"
        );
        let reports = read["metadata"]["ocr"].as_array().unwrap();
        assert_eq!(reports.len(), 1, "{scan}: {read}");
        assert_eq!(reports[0]["prepared"], true, "{scan}: {read}");
    }

    let scratch = Scratch::new("damaged-tga");
    let damaged = scratch.0.join("damaged.tga");
    let tga = fs::read(&scans[2]).unwrap();
    fs::write(&damaged, &tga[..4096]).unwrap();
    let read = &json_objects(&[damaged.to_str().unwrap()])[0];
    assert_matches_schema(read);
    assert_eq!(read["status"], "failed", "{read}");
}

#[test]
fn each_ocred_image_reports_how_it_was_read() {
    let objects = json_objects(&[
        "../quality/corpus/prose-scan-upside-down.png",
        "../quality/corpus/arabic-scan-300dpi.png",
        "../quality/corpus/report-scan.tif",
        "../quality/corpus/prose-photo-perspective.jpg",
        "../quality/corpus/prose-scan-inverted.png",
        "../quality/corpus/danish-scan-fax.png",
    ]);
    for object in &objects {
        assert_matches_schema(object);
    }
    let reports = |index: usize| {
        objects[index]["metadata"]["ocr"]
            .as_array()
            .unwrap()
            .clone()
    };

    let upside_down = &reports(0)[0];
    assert_eq!(upside_down["turned_degrees"], 180, "{upside_down}");
    assert_eq!(upside_down["script"], "Latin", "{upside_down}");
    assert_eq!(upside_down["prepared"], true, "{upside_down}");
    assert_eq!(upside_down["flattened"], false, "{upside_down}");
    assert_eq!(upside_down["inverted"], false, "{upside_down}");
    assert_eq!(upside_down["despeckled"], false, "{upside_down}");

    let arabic = &reports(1)[0];
    assert_eq!(arabic["script"], "Arabic", "{arabic}");
    assert_eq!(arabic["language"], "ara", "{arabic}");

    let pages: Vec<_> = reports(2)
        .iter()
        .map(|report| report["page"].clone())
        .collect();
    assert_eq!(pages, [1, 2]);

    let photo = &reports(3)[0];
    assert_eq!(photo["flattened"], true, "{photo}");
    assert_eq!(photo["inverted"], false, "{photo}");

    let inverted = &reports(4)[0];
    assert_eq!(inverted["inverted"], true, "{inverted}");
    assert!(
        objects[4]["text"]
            .as_str()
            .unwrap()
            .starts_with("The Lighthouse Keeper's Ledger"),
        "{}",
        objects[4]
    );

    let fax = &reports(5)[0];
    assert_eq!(fax["despeckled"], true, "{fax}");
    assert!(
        objects[5]["text"]
            .as_str()
            .unwrap()
            .starts_with("Fyrmesterens logbog"),
        "{}",
        objects[5]
    );
}

#[test]
fn scripts_detection_confuses_are_read_in_their_own() {
    let objects = json_objects(&[
        "../quality/corpus/georgian-scan.png",
        &document("malayalam-scan.png"),
    ]);
    for (object, script, opening) in [
        (&objects[0], "Georgian", "ადამიანთა უფლებების"),
        (&objects[1], "Malayalam", "മനുഷ്യരെല്ലാവരും"),
    ] {
        assert_eq!(object["metadata"]["ocr"][0]["script"], script, "{object}");
        assert!(
            object["text"].as_str().unwrap().starts_with(opening),
            "{object}"
        );
    }
}

#[test]
fn a_clean_pdf_text_layer_is_kept() {
    let objects = json_objects(&["../quality/corpus/hindi.pdf", &document("2022_Q3_AAPL.pdf")]);
    for document in &objects {
        assert!(
            document["metadata"].get("rejected_text_layers").is_none(),
            "{document}"
        );
    }
}

#[test]
fn every_page_of_a_skewed_multi_page_tiff_is_ocred_as_a_page() {
    let output = dijo(&[
        "--format",
        "pages",
        "../quality/corpus/report-scan-skewed.tif",
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    let pages: Vec<serde_json::Value> = stdout(&output)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|record: &serde_json::Value| record["kind"] == "page")
        .collect();
    assert_eq!(pages.len(), 2, "{pages:?}");
    assert_eq!(pages[1]["page"], 2);
    assert!(
        pages[1]["text"].as_str().unwrap().contains("Part Three"),
        "{pages:?}"
    );
    assert_eq!(pages[1]["footer"], "Page 2");
}

#[test]
fn the_pages_of_a_scan_read_alike_whichever_finishes_first() {
    let scan = document("greek-danish-greek-scan.tif");
    let pages = page_records(&[], &scan);
    let languages: Vec<_> = pages
        .iter()
        .map(|page| page["detected_language"]["code"].clone())
        .collect();
    assert_eq!(languages, ["ell", "dan", "ell"], "{pages:?}");
    assert_eq!(page_records(&[], &scan), pages);
}

#[test]
fn tiff_images_with_exif_directories_are_ocred() {
    let output = dijo(&["../quality/corpus/report-scan.tif"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("Part Three"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn an_entity_expansion_bomb_stops_at_a_limit() {
    let scratch = Scratch::new("bomb");
    let mut bomb = String::from("<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol0 \"lol\">");
    for level in 1..10 {
        bomb.push_str(&format!(
            "<!ENTITY lol{level} \"{}\">",
            format!("&lol{};", level - 1).repeat(10)
        ));
    }
    bomb.push_str("]><lolz>&lol9;</lolz>");
    let path = scratch.0.join("bomb.xml");
    fs::write(&path, bomb).unwrap();
    let started = std::time::Instant::now();
    let objects = json_objects(&[path.to_str().unwrap()]);
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
    assert_eq!(objects[0]["status"], "partial");
    assert!(objects[0]["text"].as_str().unwrap().len() < 1000);
    assert_eq!(objects[0]["errors"][0]["category"], "limit-exceeded");
}

#[test]
fn a_cancelled_pdf_stops_without_parsing_the_rest() {
    use std::io::Read;

    let scratch = Scratch::new("cancelled-pdf");
    let pdf = scratch.0.join("long.pdf");
    fs::write(&pdf, text_pdf(3000)).unwrap();
    let full = std::time::Instant::now();
    let complete = dijo(&["--ocr", "never", pdf.to_str().unwrap()]);
    assert!(complete.status.success(), "{}", stderr(&complete));
    let full = full.elapsed();

    let mut child = Command::new(env!("CARGO_BIN_EXE_dijo"))
        .args(["--ocr", "never", pdf.to_str().unwrap()])
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
    let closed = std::time::Instant::now();
    let output = child.wait_with_output().unwrap();
    assert!(
        closed.elapsed() < full / 2,
        "stopping took {:?}, the whole document {full:?}",
        closed.elapsed()
    );
    assert_eq!(stderr(&output), "");
}

#[test]
fn a_signature_that_encapsulates_its_content_is_extracted() {
    let objects = json_objects(&[&document("signed-content.xml.p7m")]);
    assert_eq!(objects[0]["status"], "complete");
    assert_eq!(objects[0]["mime_type"], "application/pkcs7-mime");
    assert!(
        objects[0]["text"].as_str().unwrap().contains("link.pdf"),
        "{}",
        objects[0]
    );
}

#[test]
fn source_code_is_extracted() {
    let scratch = Scratch::new("source-code");
    let source = scratch.0.join("Hello.java");
    fs::write(
        &source,
        "public class Hello { String greeting = \"lighthouse keeper\"; }\n",
    )
    .unwrap();
    let output = dijo(&[source.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("lighthouse keeper"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn subtitles_are_read_for_their_spoken_text() {
    for name in ["subtitles.srt", "subtitles.vtt", "subtitles.ass"] {
        let text = extracted(&[&document(name)]);
        assert!(text.contains("Every evening at dusk"), "{name}: {text}");
        assert!(text.contains("the keeper climbed"), "{name}: {text}");
        for noise in [
            "-->", "00:00", "<i>", "<v ", "{\\i1}", "WEBVTT", "Dialogue", "Default",
        ] {
            assert!(!text.contains(noise), "{name} kept {noise:?}: {text}");
        }
    }
}

#[test]
fn notebooks_are_read_cell_by_cell() {
    let objects = json_objects(&[&document("notebook.ipynb")]);
    assert_eq!(objects[0]["mime_type"], "application/x-ipynb+json");
    let text = objects[0]["text"].as_str().unwrap();
    for expected in [
        "Lighthouse analysis",
        "The keeper recorded ships every evening.",
        "print('eleven volumes')",
        "eleven volumes",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    assert!(!text.contains("cell_type"), "{text}");
}

#[test]
fn fiction_books_keep_paragraphs_apart_and_their_metadata() {
    let objects = json_objects(&[&document("book.fb2")]);
    let text = objects[0]["text"].as_str().unwrap();
    assert!(
        text.contains("Chapter One\nEvery evening at dusk the keeper climbed the tower.\nHe wrote"),
        "{text}"
    );
    assert!(!text.contains("cover.jpg"), "{text}");
    assert_eq!(objects[0]["metadata"]["title"], "The Lighthouse Ledger");
    assert_eq!(
        objects[0]["metadata"]["authors"],
        serde_json::json!(["Ada Keeper"])
    );
}

#[test]
fn apple_mail_messages_are_read_as_email() {
    let objects = json_objects(&[&document("mail.emlx")]);
    assert_eq!(objects[0]["mime_type"], "message/x-emlx");
    let text = objects[0]["text"].as_str().unwrap();
    assert!(text.contains("The lamp was lit at dusk."), "{text}");
    assert!(!text.contains("plist"), "{text}");
    assert_eq!(objects[0]["metadata"]["title"], "Ledger entry");
}

#[test]
fn iwork_documents_from_2013_on_are_read_for_their_text() {
    let pages = extracted(&["--ocr", "never", &document("pages2013.pages")]);
    for expected in [
        "Some plain text to parse.",
        "A text box with text.",
        "Cell one",
        "Column three",
    ] {
        assert!(pages.contains(expected), "{pages}");
    }
    let keynote = extracted(&["--ocr", "never", &document("keynote2018.key")]);
    assert!(keynote.contains("Open in Libreoffice 6.2"), "{keynote}");
}

#[test]
fn kindle_books_are_read_with_their_metadata() {
    let objects = json_objects(&[&document("yellow-wallpaper.mobi")]);
    assert_eq!(objects[0]["status"], "complete");
    assert_eq!(objects[0]["mime_type"], "application/x-mobipocket-ebook");
    assert!(
        objects[0]["text"]
            .as_str()
            .unwrap()
            .contains("John is a physician"),
        "{}",
        objects[0]
    );
    assert_eq!(objects[0]["metadata"]["title"], "The Yellow Wallpaper");
    assert_eq!(
        objects[0]["metadata"]["authors"],
        serde_json::json!(["Charlotte Perkins Gilman"])
    );
}
