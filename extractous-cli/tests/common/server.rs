use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::DOCUMENTS;

/// An `extractous serve` process listening on a free local port, killed when dropped.
pub struct Server {
    pub child: Child,
    pub address: String,
}

impl Server {
    pub fn start(args: &[&str]) -> Self {
        Self::start_with_env(args, &[])
    }

    pub fn start_with_env(args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_extractous"))
            .args(["serve", "--listen", "127.0.0.1:0"])
            .args(args)
            .envs(env.iter().copied())
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

    pub fn connect(&self) -> TcpStream {
        connect(&self.address)
    }

    pub fn request(
        &self,
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Response {
        request(&self.address, method, target, headers, body)
    }

    pub fn post_file(&self, target: &str, headers: &[(&str, &str)], name: &str) -> Response {
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

/// A connection to a server at `address`, which gives up on an answer after two minutes.
pub fn connect(address: &str) -> TcpStream {
    let stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    stream
}

/// Sends one request to the server at `address`, on a connection of its own.
pub fn request(
    address: &str,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Response {
    let mut stream = connect(address);
    write_head(&mut stream, method, target, headers, body.len());
    stream.write_all(body).unwrap();
    read_response(stream)
}

pub fn write_head(
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

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8(self.body.clone()).unwrap()
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

pub fn read_response(stream: TcpStream) -> Response {
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
