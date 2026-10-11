use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DOCUMENTS: &str = "../test_files/documents";

struct Server {
    child: Child,
    address: String,
}

impl Server {
    fn start(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_extractous"))
            .args(["serve", "--listen", "127.0.0.1:0"])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let mut line = String::new();
        let address = loop {
            line.clear();
            assert!(
                stderr.read_line(&mut line).unwrap() > 0,
                "the server exited before listening"
            );
            if let Some(url) = line.trim().strip_prefix("extractous: listening on http://") {
                break url.to_string();
            }
        };
        std::thread::spawn(move || {
            let mut sink = String::new();
            while stderr.read_line(&mut sink).unwrap_or(0) > 0 {
                sink.clear();
            }
        });
        Self { child, address }
    }

    fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .unwrap();
        stream
    }

    fn request(
        &self,
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Response {
        let mut stream = self.connect();
        write_head(&mut stream, method, target, headers, body.len());
        stream.write_all(body).unwrap();
        read_response(stream)
    }

    fn post_file(&self, target: &str, headers: &[(&str, &str)], name: &str) -> Response {
        let body = std::fs::read(format!("{DOCUMENTS}/{name}")).unwrap();
        self.request("POST", target, headers, &body)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn write_head(
    stream: &mut TcpStream,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    length: usize,
) {
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Length: {length}\r\n"
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn text(&self) -> String {
        String::from_utf8(self.body.clone()).unwrap()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

fn read_response(stream: TcpStream) -> Response {
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.push((name.to_string(), value.trim().to_string()));
    }
    let chunked = headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("transfer-encoding") && value == "chunked");
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            if size == 0 {
                break;
            }
            let mut chunk = vec![0; size + 2];
            reader.read_exact(&mut chunk).unwrap();
            body.extend_from_slice(&chunk[..size]);
        }
    } else {
        reader.read_to_end(&mut body).unwrap();
    }
    Response {
        status,
        headers,
        body,
    }
}

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
    let rejected = loop {
        let response = server.request("POST", "/extract", &[], b"x");
        if response.status == 503 || started.elapsed() > Duration::from_secs(10) {
            break response;
        }
    };
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
    let killed = Command::new("kill")
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
