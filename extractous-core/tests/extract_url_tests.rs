use extractous::Extractor;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::thread;

const DOCUMENT: &str = "../test_files/documents/simple.odt";
const CONTENT_TYPE: &str = "application/vnd.oasis.opendocument.text";

fn serve(body: Vec<u8>) -> SocketAddr {
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
                "HTTP/1.1 200 OK\r\nContent-Type: {CONTENT_TYPE}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    address
}

#[test]
fn test_extract_http_url_to_string() {
    let extractor = Extractor::new();
    let (expected, _) = extractor.extract_file_to_string(DOCUMENT).unwrap();
    let address = serve(std::fs::read(DOCUMENT).unwrap());

    let (content, _) = extractor
        .extract_url_to_string(&format!("http://{address}/simple.odt"))
        .unwrap();

    assert_eq!(content, expected);
}
