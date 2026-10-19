use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

mod common;

use common::server::{read_response, write_head, Server};
use common::Scratch;

#[test]
fn health_and_readiness() {
    let server = Server::start(&[]);
    assert_eq!(server.request("GET", "/healthz", &[], b"").text(), "ok\n");
    assert_eq!(server.request("GET", "/readyz", &[], b"").status, 200);
    assert_eq!(server.request("GET", "/nope", &[], b"").status, 404);
}

#[test]
fn extracts_text_json_pages_and_detects() {
    let server = Server::start(&["--workers", "2"]);

    let text = server.post_file(
        "/extract",
        &[("X-Extractous-Filename", "simple.odt")],
        "simple.odt",
    );
    assert_eq!(text.status, 200);
    assert_eq!(
        text.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert!(text
        .text()
        .contains("These are a few of my favorite things"));

    let json = server.post_file(
        "/extract?format=json&metadata=true",
        &[],
        "2022_Q3_AAPL.pdf",
    );
    assert_eq!(json.status, 200);
    let document = json.json();
    assert_eq!(document["status"], "complete");
    assert_eq!(document["metadata"]["page_count"], 28);
    assert_eq!(document["source"]["path"], "upload");

    let pages = server.post_file("/extract?format=pages", &[], "2022_Q3_AAPL.pdf");
    assert_eq!(pages.header("content-type"), Some("application/x-ndjson"));
    assert_eq!(pages.text().lines().count(), 29);

    let detected = server.post_file("/detect", &[], "simple.doc");
    assert_eq!(detected.text(), "application/msword\n");
}

#[test]
fn encrypted_documents_need_the_password_header() {
    let server = Server::start(&[]);
    let locked = server.post_file("/extract", &[], "encrypted/password.docx");
    assert_eq!(locked.status, 422);
    assert_eq!(locked.json()["status"], "encrypted");

    let unlocked = server.post_file(
        "/extract",
        &[("X-Extractous-Password", "extractous")],
        "encrypted/password.docx",
    );
    assert_eq!(unlocked.status, 200);
    assert!(unlocked.text().contains("Call me Ishmael"));
}

#[test]
fn bad_requests_are_rejected() {
    let server = Server::start(&["--max-input-bytes", "1000"]);
    assert_eq!(
        server
            .request("POST", "/extract?format=pdf", &[], b"x")
            .status,
        400
    );
    assert_eq!(
        server.request("POST", "/extract?bogus=1", &[], b"x").status,
        400
    );
    let too_large = server.post_file("/extract", &[], "2022_Q3_AAPL.pdf");
    assert_eq!(too_large.status, 413);
}

#[test]
fn a_full_server_answers_503() {
    let server = Server::start(&["--workers", "1", "--queue", "0"]);
    let mut slow_upload = server.connect();
    write_head(&mut slow_upload, "POST", "/extract", &[], 10);
    slow_upload.write_all(b"hello").unwrap();
    let started = Instant::now();
    while server.request("GET", "/readyz", &[], b"").status != 503 {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the slow upload was never admitted"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let rejected = server.request("POST", "/extract", &[], b"x");
    assert_eq!(rejected.status, 503);
    assert_eq!(rejected.header("retry-after"), Some("1"));

    slow_upload.write_all(b"world").unwrap();
    assert_eq!(read_response(slow_upload).status, 200);
}

#[test]
fn a_slow_upload_times_out() {
    let server = Server::start(&["--timeout", "1"]);
    let mut slow_upload = server.connect();
    write_head(&mut slow_upload, "POST", "/extract", &[], 10);
    slow_upload.write_all(b"hello").unwrap();
    assert_eq!(read_response(slow_upload).status, 408);
}

#[test]
fn metrics_count_requests_and_failures() {
    let server = Server::start(&[]);
    server.post_file("/extract", &[], "simple.odt");
    server.post_file("/extract", &[], "encrypted/password.docx");
    let metrics = server.request("GET", "/metrics", &[], b"").text();
    assert!(metrics.contains("extractous_requests_total{endpoint=\"extract\",status=\"200\"} 1"));
    assert!(metrics.contains("extractous_requests_total{endpoint=\"extract\",status=\"422\"} 1"));
    assert!(metrics.contains("extractous_failures_total{category=\"encrypted\"} 1"));
    assert!(metrics.contains("extractous_workers "));
}

#[cfg(unix)]
#[test]
fn sigterm_shuts_down_cleanly() {
    let mut server = Server::start(&[]);
    assert_eq!(server.request("GET", "/healthz", &[], b"").status, 200);
    let killed = std::process::Command::new("kill")
        .args(["-TERM", &server.child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the server did not exit"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success());
}

#[test]
fn a_slow_extraction_ends_with_a_timeout_in_the_document() {
    let server = Server::start(&["--timeout", "3"]);
    let started = Instant::now();
    let response = server.post_file("/extract?format=json&ocr=always", &[], "2022_Q3_AAPL.pdf");
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(response.status, 200);
    let document = response.json();
    assert_eq!(document["status"], "partial");
    assert_eq!(document["errors"][0]["category"], "timeout");
}

#[test]
fn separate_embedded_documents_stream_as_records() {
    let server = Server::start(&[]);
    let response = server.post_file("/extract?format=json&embedded=separate", &[], "nested.zip");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("application/x-ndjson")
    );
    let paths: Vec<String> = response
        .text()
        .lines()
        .map(|line| {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            record["source"]["embedded_path"]
                .as_str()
                .unwrap_or("<root>")
                .to_string()
        })
        .collect();
    assert_eq!(paths.len(), 8);
    assert!(paths.contains(&"mail.eml/numbers.xlsx/image1.png".to_string()));
}

#[test]
fn text_is_normalised_unless_raw() {
    let server = Server::start(&[]);
    let normalised = server.post_file("/extract", &[], "unnormalized.txt");
    assert!(normalised
        .text()
        .starts_with("The final office report on caf\u{e9} extraction."));

    let raw = server.post_file("/extract?raw=true", &[], "unnormalized.txt");
    assert!(raw.text().starts_with("The \u{fb01}nal"));

    let skipped = server.post_file("/extract?no_normalize=ligatures", &[], "unnormalized.txt");
    assert!(skipped
        .text()
        .starts_with("The \u{fb01}nal o\u{fb03}ce report"));

    let conflicting = server.post_file(
        "/extract?raw&no_normalize=ligatures",
        &[],
        "unnormalized.txt",
    );
    assert_eq!(conflicting.status, 400);
    let unknown = server.post_file("/extract?no_normalize=bogus", &[], "unnormalized.txt");
    assert_eq!(unknown.status, 400);
}

#[test]
fn a_heap_cap_fails_one_request_and_not_the_next() {
    let server = Server::start(&["--max-heap-bytes", "67108864", "--workers", "1"]);
    let hungry = server.post_file("/extract", &[], "many-shared-strings.xlsx");
    assert_eq!(hungry.status, 413);
    assert_eq!(hungry.json()["errors"][0]["category"], "limit-exceeded");
    let next = server.post_file("/extract", &[], "simple.odt");
    assert_eq!(next.status, 200);
    assert!(next
        .text()
        .contains("These are a few of my favorite things"));
}

/// With embedded documents as separate records, a document that overruns the cap is a failed
/// record of its own, not an empty response.
#[test]
fn a_heap_cap_fails_a_separate_request_and_not_the_next() {
    let server = Server::start(&["--max-heap-bytes", "67108864", "--workers", "1"]);
    let hungry = server.post_file(
        "/extract?format=json&embedded=separate",
        &[],
        "many-shared-strings.xlsx",
    );
    assert_eq!(hungry.status, 200);
    let records: Vec<serde_json::Value> = hungry
        .text()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["status"], "failed");
    assert_eq!(records[0]["errors"][0]["category"], "limit-exceeded");
    let next = server.post_file("/extract?format=json&embedded=separate", &[], "nested.zip");
    assert_eq!(next.status, 200);
    assert_eq!(next.text().lines().count(), 8);
}

#[test]
fn the_cache_answers_repeated_uploads() {
    let scratch = Scratch::new("server-cache");
    let server = Server::start(&["--cache-dir", scratch.0.to_str().unwrap()]);
    let first = server.post_file("/extract", &[], "simple.odt");
    let second = server.post_file("/extract", &[], "simple.odt");
    assert_eq!(first.status, 200);
    assert_eq!(second.status, 200);
    assert_eq!(first.text(), second.text());
    assert!(first
        .text()
        .contains("These are a few of my favorite things"));

    let with_password = server.post_file(
        "/extract",
        &[("X-Extractous-Password", "extractous")],
        "encrypted/password.docx",
    );
    assert_eq!(with_password.status, 200);

    let separate = "/extract?format=json&embedded=separate";
    let first_tree = server.post_file(separate, &[], "nested.zip");
    let second_tree = server.post_file(separate, &[], "nested.zip");
    assert_eq!(first_tree.status, 200);
    assert_eq!(first_tree.text(), second_tree.text());
    assert!(first_tree
        .text()
        .contains("Notes inside the inner archive."));

    let metrics = server.request("GET", "/metrics", &[], b"").text();
    assert!(
        metrics.contains("extractous_cache_hits_total 1"),
        "{metrics}"
    );
    assert!(
        metrics.contains("extractous_cache_misses_total 1"),
        "{metrics}"
    );
    drop(server);
}

#[test]
fn worker_processes_serve_extractions() {
    let server = Server::start(&["--isolation", "process", "--workers", "2"]);
    let text = server.post_file("/extract", &[], "simple.odt");
    assert_eq!(text.status, 200);
    assert!(text
        .text()
        .contains("These are a few of my favorite things"));

    let json = server.post_file(
        "/extract?format=json&metadata=true",
        &[],
        "2022_Q3_AAPL.pdf",
    );
    assert_eq!(json.status, 200);
    let document = json.json();
    assert_eq!(document["status"], "complete");
    assert_eq!(document["metadata"]["page_count"], 28);

    let locked = server.post_file("/extract", &[], "encrypted/password.docx");
    assert_eq!(locked.status, 422);
    let unlocked = server.post_file(
        "/extract",
        &[("X-Extractous-Password", "extractous")],
        "encrypted/password.docx",
    );
    assert!(unlocked.text().contains("Call me Ishmael"));

    let detected = server.post_file("/detect", &[], "simple.doc");
    assert_eq!(detected.text(), "application/msword\n");

    let metrics = server.request("GET", "/metrics", &[], b"").text();
    assert!(metrics.contains("extractous_workers 2"), "{metrics}");
    assert!(
        metrics.contains("extractous_failures_total{category=\"encrypted\"} 1"),
        "{metrics}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_worker_process_over_its_memory_limit_is_stopped() {
    let server = Server::start(&[
        "--isolation",
        "process",
        "--workers",
        "1",
        "--worker-max-rss-bytes",
        "1048576",
    ]);
    let stopped = server.post_file("/extract", &[], "2022_Q3_AAPL.pdf");
    assert_eq!(stopped.status, 413);
    let message = stopped.json()["error"].as_str().unwrap().to_string();
    assert!(message.contains("--worker-max-rss-bytes"), "{message}");
    assert_eq!(server.request("GET", "/healthz", &[], b"").status, 200);
    let metrics = server.request("GET", "/metrics", &[], b"").text();
    assert!(
        metrics.contains("extractous_failures_total{category=\"limit-exceeded\"} 1"),
        "{metrics}"
    );
}

/// Sends `head` as it is, from a thread of its own, and returns the status line the server
/// answers it with, or None when the server reset the connection before all of it could be read.
fn raw_status_line(server: &Server, head: Vec<u8>) -> Option<String> {
    let mut stream = server.connect();
    let mut writer = stream.try_clone().unwrap();
    let writing = std::thread::spawn(move || {
        let _ = writer.write_all(&head);
    });
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => line.push(byte[0]),
            _ => break,
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    writing.join().unwrap();
    let complete = line.ends_with(b"\r\n");
    complete.then(|| String::from_utf8(line).unwrap().trim_end().to_string())
}

#[test]
fn oversized_heads_are_refused_and_the_server_stays_up() {
    const REFUSED: &str = "HTTP/1.1 431 Request Header Fields Too Large";
    let server = Server::start(&[]);
    let flood: String = (0..2000)
        .map(|index| format!("X-Flood-{index}: v\r\n"))
        .collect();
    assert_eq!(
        raw_status_line(
            &server,
            format!("GET /healthz HTTP/1.1\r\nHost: test\r\n{flood}\r\n").into_bytes()
        )
        .as_deref(),
        Some(REFUSED)
    );
    let oversized = [
        format!(
            "GET /healthz HTTP/1.1\r\nHost: test\r\nX-Big: {}\r\n\r\n",
            "a".repeat(1 << 20)
        ),
        format!(
            "GET /healthz?{} HTTP/1.1\r\nHost: test\r\n\r\n",
            "a=b&".repeat(1 << 18)
        ),
    ];
    for head in oversized {
        let answer = raw_status_line(&server, head.into_bytes());
        assert!(
            answer.is_none() || answer.as_deref() == Some(REFUSED),
            "{answer:?}"
        );
    }
    assert_eq!(server.request("GET", "/healthz", &[], b"").status, 200);
}

#[test]
fn slow_clients_do_not_starve_the_others() {
    let server = Server::start(&["--client-timeout", "2"]);
    let mut slow: Vec<TcpStream> = (0..200)
        .map(|_| {
            let mut stream = server.connect();
            stream
                .write_all(b"POST /extract HTTP/1.1\r\nHost: test\r\n")
                .unwrap();
            stream
        })
        .collect();
    let started = Instant::now();
    assert_eq!(server.request("GET", "/readyz", &[], b"").status, 200);
    assert!(started.elapsed() < Duration::from_secs(1));
    for stream in &mut slow {
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        assert!(!String::from_utf8_lossy(&answer).contains(" 200 "));
    }
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[test]
fn clients_that_go_silent_are_cut_off() {
    let server = Server::start(&["--client-timeout", "1"]);

    let mut late_headers = server.connect();
    late_headers
        .write_all(b"POST /extract HTTP/1.1\r\nHost: localhost\r\n")
        .unwrap();
    let started = Instant::now();
    let mut answer = Vec::new();
    let _ = late_headers.read_to_end(&mut answer);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!String::from_utf8_lossy(&answer).contains(" 200 "));

    let mut stalled_upload = server.connect();
    write_head(&mut stalled_upload, "POST", "/extract", &[], 10);
    stalled_upload.write_all(b"hello").unwrap();
    let started = Instant::now();
    let response = read_response(stalled_upload);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(response.status, 408);
    assert!(response.text().contains("--client-timeout"));

    assert_eq!(server.request("GET", "/readyz", &[], b"").status, 200);
}

/// The records of nested.zip's documents, extracted separately by a server started with `args`,
/// and how many of them were skipped for a limit.
fn separate_records(args: &[&str]) -> (Vec<serde_json::Value>, usize) {
    let server = Server::start(args);
    let response = server.post_file("/extract?format=json&embedded=separate", &[], "nested.zip");
    assert_eq!(response.status, 200);
    let records: Vec<serde_json::Value> = response
        .text()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let skipped = records
        .iter()
        .filter(|record| record["errors"][0]["category"] == "limit-exceeded")
        .inspect(|record| assert_eq!(record["status"], "failed", "{record}"))
        .count();
    (records, skipped)
}

#[test]
fn embedded_document_limits_are_configurable() {
    let (records, skipped) = separate_records(&[]);
    assert_eq!((records.len(), skipped), (8, 0));

    let (records, skipped) = separate_records(&["--max-embedded", "2"]);
    assert_eq!((records.len(), skipped), (7, 4), "{records:?}");
    let in_processes = ["--isolation", "process", "--workers", "1"];
    let (records, skipped) =
        separate_records(&[&in_processes[..], &["--max-embedded", "2"]].concat());
    assert_eq!((records.len(), skipped), (7, 4), "{records:?}");

    let (records, skipped) = separate_records(&["--max-depth", "1"]);
    assert!(skipped > 0, "{records:?}");
    for record in &records {
        let depth = record["source"]["embedded_path"]
            .as_str()
            .map_or(0, |path| path.split('/').count());
        assert_eq!(depth > 1, record["status"] == "failed", "{record}");
    }

    let (records, skipped) = separate_records(&["--max-expanded-bytes", "100"]);
    assert!(skipped > 0, "{records:?}");
}

#[test]
fn output_is_cut_short_at_the_character_limit() {
    let unlimited = Server::start(&[]);
    let full_text = unlimited
        .post_file("/extract", &[], "2022_Q3_AAPL.pdf")
        .text();
    let full_xhtml = unlimited
        .post_file("/extract?format=xhtml", &[], "2022_Q3_AAPL.pdf")
        .text();
    drop(unlimited);

    let server = Server::start(&["--max-output-chars", "500"]);
    let text = server.post_file("/extract", &[], "2022_Q3_AAPL.pdf");
    assert_eq!(text.status, 200);
    let text = text.text();
    assert_eq!(text.trim_end().chars().count(), 500, "{text}");
    assert!(full_text.starts_with(text.trim_end()));

    let json = server
        .post_file("/extract?format=json", &[], "2022_Q3_AAPL.pdf")
        .json();
    assert_eq!(json["status"], "partial");
    assert_eq!(json["errors"][0]["category"], "limit-exceeded");
    assert_eq!(
        json["text"].as_str().unwrap().trim_end().chars().count(),
        500
    );

    let pages = server.post_file("/extract?format=pages", &[], "2022_Q3_AAPL.pdf");
    let records: Vec<serde_json::Value> = pages
        .text()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let (document, pages) = records.split_last().unwrap();
    let characters: usize = pages
        .iter()
        .map(|page| page["text"].as_str().unwrap().chars().count())
        .sum();
    assert!(characters <= 500, "{characters}");
    assert!(pages.len() < 28);
    assert_eq!(document["status"], "partial");
    assert_eq!(document["errors"][0]["category"], "limit-exceeded");

    let xhtml = server
        .post_file("/extract?format=xhtml", &[], "2022_Q3_AAPL.pdf")
        .text();
    assert!(xhtml.len() < full_xhtml.len());
    assert!(xhtml.ends_with("</body></html>"), "{xhtml}");
    let cut = xhtml.rfind("</p>").unwrap();
    assert!(full_xhtml.starts_with(&xhtml[..cut]));

    let short = server
        .post_file("/extract?format=json", &[], "simple.odt")
        .json();
    assert_eq!(short["status"], "complete");
    drop(server);

    let in_processes = Server::start(&[
        "--isolation",
        "process",
        "--workers",
        "1",
        "--max-output-chars",
        "500",
    ]);
    let json = in_processes
        .post_file("/extract?format=json", &[], "2022_Q3_AAPL.pdf")
        .json();
    assert_eq!(json["status"], "partial");
    assert_eq!(
        json["text"].as_str().unwrap().trim_end().chars().count(),
        500
    );
}

#[test]
fn a_declared_content_type_hints_at_the_type() {
    let server = Server::start(&[]);
    let csv = "table-multi-row-column-cells-actual.csv";
    let untyped = server.post_file("/detect", &[], csv).text();
    let typed = server
        .post_file("/detect", &[("Content-Type", "text/csv")], csv)
        .text();
    assert_ne!(untyped, typed);
    assert!(typed.starts_with("text/csv"), "{typed}");

    let named = server
        .post_file(
            "/detect",
            &[
                ("Content-Type", "text/csv"),
                ("X-Extractous-Filename", "t.txt"),
            ],
            csv,
        )
        .text();
    assert_eq!(named, untyped);

    let json = server
        .post_file(
            "/extract?format=json",
            &[("Content-Type", "application/pdf")],
            "2022_Q3_AAPL.pdf",
        )
        .json();
    assert_eq!(json["status"], "complete");
    assert_eq!(json["source"]["path"], "upload");
}

/// Every spooled upload is removed, however its request ends.
#[test]
fn no_request_leaves_files_behind() {
    let scratch = Scratch::new("server-spool");
    let server = Server::start(&[
        "--spool-dir",
        scratch.0.to_str().unwrap(),
        "--max-input-bytes",
        "200000",
        "--timeout",
        "2",
    ]);
    assert_eq!(server.post_file("/extract", &[], "simple.odt").status, 200);
    assert_eq!(
        server
            .post_file("/extract", &[], "encrypted/password.docx")
            .status,
        422
    );
    assert_eq!(
        server.post_file("/extract", &[], "2022_Q3_AAPL.pdf").status,
        413
    );
    assert_eq!(
        server
            .post_file("/extract?format=json&embedded=separate", &[], "nested.zip")
            .status,
        200
    );
    assert_eq!(server.post_file("/detect", &[], "simple.doc").status, 200);
    let mut slow_upload = server.connect();
    write_head(&mut slow_upload, "POST", "/extract", &[], 10);
    slow_upload.write_all(b"hello").unwrap();
    assert_eq!(read_response(slow_upload).status, 408);
    let mut abandoned = server.connect();
    write_head(&mut abandoned, "POST", "/extract", &[], 10);
    abandoned.write_all(b"hello").unwrap();
    drop(abandoned);

    let leftovers = || -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&scratch.0)
            .unwrap()
            .flat_map(|spool| std::fs::read_dir(spool.unwrap().path()).unwrap())
            .map(|entry| entry.unwrap().path())
            .collect()
    };
    let started = Instant::now();
    while !leftovers().is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            leftovers()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(server);
}

#[cfg(unix)]
#[test]
fn sigterm_lets_running_requests_finish() {
    let mut server = Server::start(&[]);
    let body = std::fs::read(common::document("simple.odt")).unwrap();
    let mut running = server.connect();
    write_head(&mut running, "POST", "/extract", &[], body.len());
    running.write_all(&body[..100]).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let killed = std::process::Command::new("kill")
        .args(["-TERM", &server.child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    std::thread::sleep(Duration::from_millis(200));
    assert!(server.child.try_wait().unwrap().is_none());
    assert!(TcpStream::connect(&server.address).is_err());

    running.write_all(&body[100..]).unwrap();
    let response = read_response(running);
    assert_eq!(response.status, 200);
    assert!(response
        .text()
        .contains("These are a few of my favorite things"));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            break status;
        }
        assert!(started.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success());
}
