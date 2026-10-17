//! Documents named by a URL, opened as Tika's `TikaInputStream.get(URL, metadata)` opens them:
//! a `file:` URL of a regular file as that file, an `http:` or `https:` URL fetched, with what
//! the server says of the document recorded as Tika records it. The native image Tika runs in
//! enables no other protocol.

use std::io;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ureq::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use ureq::http::{HeaderMap, HeaderName, Uri};
use ureq::Agent;

use crate::errors::{Error, ExtractResult};
use crate::input::{Input, Spool};
use crate::mime::uri;

/// How long connecting, the TLS handshake included, may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the server may take to answer once asked, until its response's headers are in.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a whole fetch may take when its extraction sets no time limit of its own.
const FETCH_LIMIT: Duration = Duration::from_secs(600);
/// The redirects followed, as many as Java's `HttpURLConnection` follows (`http.maxRedirects`).
const MAX_REDIRECTS: u32 = 20;

/// Where a URL leads.
#[derive(Debug)]
pub(crate) enum Located {
    /// A regular file, which Tika reads as the file at this path.
    File(String),
    /// A document fetched from a server.
    Fetched(Fetched),
}

/// A document a server sent.
#[derive(Debug)]
pub(crate) struct Fetched {
    pub(crate) headers: Headers,
    pub(crate) body: Input<'static>,
}

/// What `TikaInputStream.get(URL, metadata)` records of a fetched document, each header's value
/// as `URLConnection` reads it: the last of its kind, its bytes as ISO-8859-1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Headers {
    /// The last segment of the URL's path, still percent-encoded, unless the path ends in a
    /// slash.
    pub(crate) name: Option<String>,
    pub(crate) content_type: Option<String>,
    pub(crate) content_encoding: Option<String>,
    /// `URLConnection.getContentLength`: the `Content-Length`, where it is a whole number that
    /// fits in a Java `int`.
    pub(crate) content_length: Option<u32>,
}

/// Where `url` leads: the file it names, or the document fetched from it. A fetch is bounded by
/// `deadline`, the extraction's time limit, if it has one, and fails with [`Error::Timeout`]
/// past it; any other failure to open the document is an [`Error::IoError`], as on Tika's path.
pub(crate) fn locate(url: &str, deadline: Option<Instant>) -> ExtractResult<Located> {
    let unopened = |reason: &dyn std::fmt::Display| {
        Error::IoError(format!("Could not open the input {url}: {reason}"))
    };
    let scheme = scheme(url).ok_or_else(|| unopened(&"it is not an absolute URL"))?;
    if scheme.eq_ignore_ascii_case("file") {
        return file_path(url).map(Located::File).map_err(|e| unopened(&e));
    }
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return Err(unopened(&format_args!(
            "the {scheme} protocol is not supported"
        )));
    }
    fetch(url, deadline).map(Located::Fetched)
}

/// `URI`'s scheme: the letters, digits, `+`, `-` and `.` before the first colon, starting with a
/// letter, if a colon comes before any `/`, `?` or `#`.
fn scheme(url: &str) -> Option<&str> {
    let end = url.find([':', '/', '?', '#'])?;
    let scheme = &url[..end];
    let mut chars = scheme.chars();
    (url[end..].starts_with(':')
        && chars.next()?.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)))
    .then_some(scheme)
}

/// The path of the regular file a `file:` URL names, as `Paths.get(url.toURI())` reads it.
fn file_path(url: &str) -> Result<String, String> {
    let parsed = uri::parse(url).ok_or("it is not a valid URI")?;
    let rest = &url["file:".len()..];
    if let Some(after) = rest.strip_prefix("//") {
        if !after.starts_with('/') {
            return Err("a file URL with an authority component names no local file".to_owned());
        }
    }
    let path = parsed.path.ok_or("it is not hierarchical")?;
    if parsed.query.is_some() || parsed.fragment.is_some() {
        return Err("a file URL with a query or fragment names no file".to_owned());
    }
    let decoded = uri::decode(path);
    let local = local_path(&decoded);
    match std::fs::metadata(local) {
        Ok(found) if found.is_file() => Ok(local.to_owned()),
        Ok(_) => Err(format!("{local} is not a regular file")),
        Err(e) => Err(format!("{local}: {e}")),
    }
}

/// A URI's decoded path as a path of this platform: on Windows without the slash before a drive
/// letter.
fn local_path(path: &str) -> &str {
    let drive = path.as_bytes();
    if cfg!(windows) && drive.len() >= 3 && drive[0] == b'/' && drive[2] == b':' {
        return &path[1..];
    }
    path
}

fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .max_redirects(MAX_REDIRECTS)
            .build()
            .new_agent()
    })
}

/// Fetches the document at an `http:` or `https:` `url`, following redirects.
fn fetch(url: &str, deadline: Option<Instant>) -> ExtractResult<Fetched> {
    let failed = |error: ureq::Error| match error {
        ureq::Error::Timeout(ureq::Timeout::Global) if deadline.is_some() => Error::Timeout,
        ureq::Error::StatusCode(status) => Error::IoError(format!(
            "Could not open the input {url}: the server answered with HTTP status {status}"
        )),
        error => Error::IoError(format!("Could not open the input {url}: {error}")),
    };
    let uri: Uri = url.parse().map_err(|e| {
        Error::IoError(format!(
            "Could not open the input {url}: it is not a valid URL: {e}"
        ))
    })?;
    let name = resource_name(uri.path());
    let limit = match deadline {
        Some(deadline) => deadline.saturating_duration_since(Instant::now()),
        None => FETCH_LIMIT,
    };
    let response = agent()
        .get(uri)
        .config()
        .timeout_global(Some(limit))
        .build()
        .call()
        .map_err(failed)?;
    let fields = response.headers();
    let headers = Headers {
        name,
        content_type: last(fields, &CONTENT_TYPE),
        content_encoding: last(fields, &CONTENT_ENCODING),
        content_length: last(fields, &CONTENT_LENGTH).and_then(|length| java_int_length(&length)),
    };
    let mut body = response
        .into_body()
        .into_with_config()
        .limit(u64::MAX)
        .reader();
    let mut spool = Spool::new(headers.content_length.map(u64::from));
    io::copy(&mut body, &mut spool).map_err(|e| failed(e.into()))?;
    let body = spool
        .finish()
        .map_err(|e| Error::IoError(format!("Could not keep the document from {url}: {e}")))?;
    Ok(Fetched { headers, body })
}

/// The resource name `TikaInputStream.get(URL, metadata)` takes from a URL's path: what follows
/// its last slash, unless nothing does.
fn resource_name(path: &str) -> Option<String> {
    let name = &path[path.rfind('/').map_or(0, |slash| slash + 1)..];
    (!name.is_empty()).then(|| name.to_owned())
}

/// The value of the last `name` header, its bytes read as ISO-8859-1.
fn last(fields: &HeaderMap, name: &HeaderName) -> Option<String> {
    fields
        .get_all(name)
        .iter()
        .next_back()
        .map(|value| value.as_bytes().iter().copied().map(char::from).collect())
}

/// `URLConnection.getContentLength`: `Long.parseLong` of the header, where it is not negative and
/// fits in an `int`.
fn java_int_length(value: &str) -> Option<u32> {
    let digits = value.strip_prefix('+').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits
        .parse::<u32>()
        .ok()
        .filter(|&length| i32::try_from(length).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn reads_schemes_as_java_uris_do() {
        assert_eq!(scheme("http://example.com/a.pdf"), Some("http"));
        assert_eq!(scheme("HTTPS://example.com"), Some("HTTPS"));
        assert_eq!(scheme("svn+ssh://host/x"), Some("svn+ssh"));
        assert_eq!(scheme("example.com/a:b"), None);
        assert_eq!(scheme("/tmp/a.pdf"), None);
        assert_eq!(scheme("1http://x"), None);
    }

    #[test]
    fn names_documents_by_the_last_segment_of_their_path() {
        assert_eq!(
            resource_name("/docs/a%20b.pdf"),
            Some("a%20b.pdf".to_owned())
        );
        assert_eq!(resource_name("/docs/"), None);
        assert_eq!(resource_name("/"), None);
        assert_eq!(resource_name(""), None);
    }

    #[test]
    fn reads_content_lengths_as_java_does() {
        assert_eq!(java_int_length("1234"), Some(1234));
        assert_eq!(java_int_length("+12"), Some(12));
        assert_eq!(java_int_length("2147483647"), Some(i32::MAX as u32));
        assert_eq!(java_int_length("2147483648"), None);
        assert_eq!(java_int_length("-1"), None);
        assert_eq!(java_int_length("12 "), None);
        assert_eq!(java_int_length(""), None);
    }

    #[test]
    fn opens_file_urls_of_regular_files_as_those_files() {
        let path =
            std::env::temp_dir().join(format!("extractous fetch {}.txt", std::process::id()));
        std::fs::write(&path, "text").unwrap();
        let name = path.to_str().unwrap();
        let encoded = name.replace(' ', "%20");
        for url in [format!("file:{encoded}"), format!("file://{encoded}")] {
            match locate(&url, None).unwrap() {
                Located::File(found) => assert_eq!(found, name),
                Located::Fetched(_) => panic!("{url} was fetched"),
            }
        }
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            locate(&format!("file:{encoded}"), None),
            Err(Error::IoError(_))
        ));
        let directory = std::env::temp_dir();
        assert!(matches!(
            locate(&format!("file:{}", directory.to_str().unwrap()), None),
            Err(Error::IoError(_))
        ));
        assert!(matches!(
            locate("file://host/tmp/x.txt", None),
            Err(Error::IoError(_))
        ));
    }

    /// Answers the first request to a local port with `answer`, or, where it is empty, holds
    /// the connection open without answering.
    fn serve_once(answer: Vec<u8>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = BufReader::new(&connection);
            let mut line = String::new();
            while request.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            connection.write_all(&answer).unwrap();
            if answer.is_empty() {
                thread::sleep(Duration::from_secs(10));
            }
        });
        address
    }

    #[test]
    fn follows_redirects_from_http_to_https() {
        let tls = TcpListener::bind("127.0.0.1:0").unwrap();
        let tls_address = tls.local_addr().unwrap();
        let (hello, greeted) = mpsc::channel();
        thread::spawn(move || {
            let (mut connection, _) = tls.accept().unwrap();
            let mut record = [0; 1];
            connection.read_exact(&mut record).unwrap();
            hello.send(record[0]).unwrap();
        });
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: https://{tls_address}/document.txt\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let address = serve_once(redirect.into_bytes());
        let deadline = Instant::now() + Duration::from_secs(10);
        let fetched = locate(&format!("http://{address}/document.txt"), Some(deadline));
        let handshake = 0x16;
        assert_eq!(
            greeted.recv_timeout(Duration::from_secs(10)),
            Ok(handshake),
            "the redirect to https is followed with a TLS handshake; the fetch gave {fetched:?}"
        );
    }

    #[test]
    fn times_out_a_server_that_never_answers_within_the_extractions_limit() {
        let address = serve_once(Vec::new());
        let started = Instant::now();
        let deadline = started + Duration::from_millis(300);
        let fetched = locate(&format!("http://{address}/document.txt"), Some(deadline));
        assert!(matches!(fetched, Err(Error::Timeout)), "{fetched:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn fails_urls_it_cannot_open_as_inputs_it_cannot_open() {
        for url in [
            "example.com/a.pdf",
            "ftp://example.com/a.pdf",
            "jar:file:/a.jar!/b",
        ] {
            assert!(matches!(locate(url, None), Err(Error::IoError(_))), "{url}");
        }
    }
}
