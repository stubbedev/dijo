use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::thread;

pub fn parse_metadata_file(file_path: &str) -> HashMap<String, Vec<String>> {
    let expected_metadata_string = std::fs::read_to_string(file_path).unwrap();

    serde_json::from_str(&expected_metadata_string).expect("JSON was not well-formatted")
}

pub fn calculate_similarity_percent(
    expected: &HashMap<String, Vec<String>>,
    current: &HashMap<String, Vec<String>>,
) -> f64 {
    let mut matches = 0;
    let mut total = 0;

    // Iterate over all keys in the 'expected' HashMap
    for (key, value1) in expected {
        if let Some(value2) = current.get(key) {
            total += 1;
            if value1 == value2 {
                matches += 1;
            }
        }
    }
    if total == 0 {
        return 0.0;
    }
    // Return the similarity percentage
    (matches as f64) / (total as f64)
}

pub fn is_expected_metadata_contained(
    expected: &HashMap<String, Vec<String>>,
    current: &HashMap<String, Vec<String>>,
) -> bool {
    // Check if all keys in `expected` are present in `current` and have identical values
    expected.iter().all(|(key, expected_values)| {
        let actual_values_opt = current.get(key);
        match actual_values_opt {
            None => {
                println!("\nexpected key = {key} not found !!");
                false
            }
            Some(actual_values) => {
                if actual_values != expected_values {
                    println!(
                        "\nvalues for key = {key} differ!! expected = {:?} and actual = {:?}",
                        expected_values, actual_values
                    );
                    false
                } else {
                    true
                }
            }
        }
    })
}

/// The `file:` URL of the file at `path`, as `Path.toUri` writes it: absolute, with forward
/// slashes, a slash before a Windows drive letter, and spaces encoded. The path is made absolute
/// without `canonicalize`, whose `\\?\` prefix on Windows no URL path holds.
pub fn file_url(path: &str) -> String {
    let absolute = std::path::absolute(path).unwrap();
    let slashed = absolute
        .to_str()
        .unwrap()
        .replace('\\', "/")
        .replace(' ', "%20");
    match slashed.starts_with('/') {
        true => format!("file://{slashed}"),
        false => format!("file:///{slashed}"),
    }
}

pub fn serve(body: Vec<u8>, content_type: &'static str) -> SocketAddr {
    answer("200 OK", body, content_type)
}

/// Answers every request with `status`, the `body` of `content_type`, and its length.
pub fn answer(status: &'static str, body: Vec<u8>, content_type: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    address
}

/// Accepts connections and never answers on them.
pub fn silent() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let held: Vec<_> = listener.incoming().collect();
        drop(held);
    });
    address
}
