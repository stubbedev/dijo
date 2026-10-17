//! Documents fetched from a URL extract natively as Tika's path extracts them from the same URL:
//! with the resource name, `Content-Type`, `Content-Length` and `Content-Encoding` the server
//! gives, the type it sends as the detection's hint, and the length learned where it sends none.
//! A local server sends the documents with and without the headers.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::thread;

use crate::config::ParserConfigs;
use crate::formats::parity::{as_tika, comparable, sorted, tika_names, Extracted, Outcome};
use crate::formats::{self, Output, StreamOptions};
use crate::input::Source;
use crate::isolate::isolate;
use crate::tika;
use crate::{CharSet, TesseractOcrConfig};

const DOCUMENTS: &str = "../test_files/documents";

/// How the server sends a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sending {
    /// With the type Tika detects in the file, and its length.
    Typed,
    /// With its length but no type.
    Untyped,
    /// As `application/octet-stream`, without a length, the body ending with the connection.
    Unsized,
    /// With a type and an encoding the server claims, and its length.
    Encoded,
}

impl Sending {
    const ALL: [Sending; 4] = [
        Sending::Typed,
        Sending::Untyped,
        Sending::Unsized,
        Sending::Encoded,
    ];

    fn path(self) -> &'static str {
        match self {
            Sending::Typed => "typed",
            Sending::Untyped => "untyped",
            Sending::Unsized => "unsized",
            Sending::Encoded => "encoded",
        }
    }

    fn of(path: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|sending| sending.path() == path)
            .unwrap_or_else(|| panic!("no way of sending {path}"))
    }

    fn headers(self, file: &Path, body: &[u8]) -> String {
        match self {
            Sending::Typed => format!(
                "Content-Type: {}\r\nContent-Length: {}\r\n",
                formats::detect(Source::File(file.to_str().unwrap())).unwrap(),
                body.len()
            ),
            Sending::Untyped => format!("Content-Length: {}\r\n", body.len()),
            Sending::Unsized => "Content-Type: application/octet-stream\r\n".to_owned(),
            Sending::Encoded => format!(
                "Content-Type: text/plain; charset=ISO-8859-1\r\nContent-Encoding: identity\r\n\
                 Content-Length: {}\r\n",
                body.len()
            ),
        }
    }
}

/// Serves the files under `root` at `/<sending>/<name>`, each request on a connection of its
/// own, closed once the body is sent.
fn serve(root: PathBuf) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let target = request.split(' ').nth(1).unwrap();
            let (sending, name) = target[1..].split_once('/').unwrap();
            let file = root.join(name);
            let body = std::fs::read(&file).unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\n{}Connection: close\r\n\r\n",
                Sending::of(sending).headers(&file, &body)
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    address
}

/// What Tika's path streams from `source`, as plain UTF-8 text.
fn tika_stream(source: Source<'_>, configs: &ParserConfigs) -> Outcome {
    let options = tika::StreamOptions {
        char_set: CharSet::UTF_8,
        as_xml: false,
        timeout: None,
        embedded: None,
    };
    let (mut reader, _) = tika::parse_to_stream(isolate().unwrap(), source, configs, &options)
        .map_err(|e| e.to_string())?;
    let mut text = Vec::new();
    reader
        .read_to_end(&mut text)
        .map_err(|e| format!("reading failed: {e}"))?;
    Ok(Extracted {
        text,
        metadata: sorted(reader.metadata().map_err(|e| e.to_string())?),
    })
}

/// What the native path streams from `source`, as plain UTF-8 text, in Tika's names, or `None`
/// where it leaves the document to Tika.
fn native_stream(source: Source<'_>, configs: &ParserConfigs) -> Option<Outcome> {
    let document = match formats::open(source, None) {
        Ok(document) => document,
        Err(error) => return Some(Err(error.to_string())),
    };
    let routed = formats::route(&document).ok()?;
    let names = tika_names(routed.parser.name(), routed.parser.tika_name(), &[]);
    let options = StreamOptions {
        char_set: CharSet::UTF_8,
        output: Output::Text,
        deadline: None,
        spool: None,
    };
    let streamed = formats::parse_to_stream(
        routed.parser,
        routed.input.to_static(),
        routed.detected,
        configs.clone(),
        options,
    );
    let (mut stream, _) = match streamed {
        Ok(Ok(streamed)) => streamed,
        Ok(Err(_)) => return None,
        Err(error) => return Some(Err(error.to_string())),
    };
    let mut text = Vec::new();
    if let Err(e) = stream.read_to_end(&mut text) {
        return Some(Err(format!("reading failed: {e}")));
    }
    Some(
        stream
            .metadata()
            .map_err(|e| e.to_string())
            .map(|metadata| {
                as_tika(
                    Extracted {
                        text,
                        metadata: sorted(metadata),
                    },
                    &names,
                    CharSet::UTF_8,
                )
            }),
    )
}

/// How the native outcome differs from Tika's: where their texts part, and the metadata whose
/// values differ.
fn difference(native: &Outcome, tika: &Outcome) -> String {
    let (native, tika) = match (native, tika) {
        (Ok(native), Ok(tika)) => (native, tika),
        _ => return format!("native {native:?}, Tika {tika:?}"),
    };
    let mut parts = Vec::new();
    if native.text != tika.text {
        let at = native
            .text
            .iter()
            .zip(&tika.text)
            .take_while(|(a, b)| a == b)
            .count();
        let around = |text: &[u8]| {
            String::from_utf8_lossy(&text[at.saturating_sub(40)..text.len().min(at + 40)])
                .into_owned()
        };
        parts.push(format!(
            "texts part at {at}: native {:?}, Tika {:?}",
            around(&native.text),
            around(&tika.text)
        ));
    }
    let names: std::collections::BTreeSet<&String> = native
        .metadata
        .iter()
        .chain(&tika.metadata)
        .map(|(name, _)| name)
        .collect();
    let value = |extracted: &Extracted, name: &String| {
        extracted
            .metadata
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, values)| values.clone())
    };
    for name in names {
        let (n, t) = (value(native, name), value(tika, name));
        if n != t {
            parts.push(format!("{name}: native {n:?}, Tika {t:?}"));
        }
    }
    parts.join("; ")
}

const CONTENT_LENGTH: &str = "Content-Length";

/// The native metadata without the `Content-Length` of a document the server sent without one,
/// where Tika's has none: the native path records the length of what was sent, which Tika's
/// records only once a detector or parser happens to spool the stream to a file. The native
/// value must be the document's `length`.
fn without_learned_length(native: &mut Extracted, tika: &Extracted, length: &str) {
    if tika.metadata.iter().any(|(name, _)| name == CONTENT_LENGTH) {
        return;
    }
    let position = native
        .metadata
        .iter()
        .position(|(name, _)| name == CONTENT_LENGTH)
        .expect("the native path records every fetched document's length");
    let (_, values) = native.metadata.remove(position);
    assert_eq!(values, [length], "the length the native path records");
}

fn comparable_outcome(outcome: Outcome) -> Outcome {
    outcome.map(|extracted| comparable(extracted, CharSet::UTF_8))
}

#[test]
fn fetched_documents_extract_as_tikas_url_path_extracts_them() {
    let configs = ParserConfigs {
        ocr: TesseractOcrConfig::new().set_skip_ocr(true),
        ..ParserConfigs::default()
    };
    let address = serve(PathBuf::from(DOCUMENTS));
    let mut names: Vec<String> = std::fs::read_dir(DOCUMENTS)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect();
    names.sort();
    let (mut compared, mut unlike_files) = (0, Vec::new());
    let mut mismatches = Vec::new();
    for name in &names {
        let file = Path::new(DOCUMENTS).join(name);
        let file = file.to_str().unwrap();
        let Some(native_file) = native_stream(Source::File(file), &configs) else {
            continue;
        };
        if comparable_outcome(native_file)
            != comparable_outcome(tika_stream(Source::File(file), &configs))
        {
            unlike_files.push(name.clone());
            continue;
        }
        for sending in Sending::ALL {
            let url = format!("http://{address}/{}/{name}", sending.path());
            let Some(native) = native_stream(Source::Url(&url), &configs) else {
                continue;
            };
            let tika = tika_stream(Source::Url(&url), &configs);
            let (mut native, tika) = (comparable_outcome(native), comparable_outcome(tika));
            if let (Ok(native), Ok(tika)) = (&mut native, &tika) {
                let length = std::fs::metadata(file).unwrap().len().to_string();
                without_learned_length(native, tika, &length);
            }
            if native != tika {
                mismatches.push(format!("{url}: {}", difference(&native, &tika)));
            }
            compared += 1;
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    assert!(
        compared >= 100,
        "only {compared} fetches compared; unlike as files: {unlike_files:?}"
    );
}
