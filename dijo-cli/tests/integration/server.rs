use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use crate::common::server::{read_response, write_head, Response, Server};
use crate::common::{Scratch, DOCUMENTS};

#[test]
fn health_and_readiness() {
    let server = Server::start(&[]);
    assert_eq!(server.request("GET", "/healthz", &[], b"").text(), "ok\n");
    assert_eq!(server.request("GET", "/readyz", &[], b"").status, 200);
    assert_eq!(server.request("GET", "/nope", &[], b"").status, 404);
}

/// Runs `dijo health` with `args` and `DIJO_LISTEN` as given, or unset.
fn health(args: &[&str], listen: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dijo"));
    command.arg("health").args(args).env_remove("DIJO_LISTEN");
    if let Some(listen) = listen {
        command.env("DIJO_LISTEN", listen);
    }
    command.output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn the_health_command_passes_silently_against_a_running_server() {
    let server = Server::start(&[]);
    for output in [
        health(&["--listen", &server.address], None),
        health(&["--listen", &server.address, "--ready"], None),
        health(&[], Some(&server.address)),
    ] {
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }
}

#[test]
fn the_health_command_reaches_a_server_listening_on_every_address() {
    let server = Server::start(&[]);
    let port = server.address.rsplit_once(':').unwrap().1;
    let output = health(&[], Some(&format!("0.0.0.0:{port}")));
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn the_health_command_fails_without_a_server() {
    let address = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .to_string();
    let output = health(&["--listen", &address], None);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains(&format!("http://{address}/healthz: Failed to connect")),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_health_command_fails_on_a_server_that_does_not_answer() {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = silent.local_addr().unwrap().to_string();
    let started = Instant::now();
    let output = health(&["--listen", &address, "--timeout", "1"], None);
    assert!(!output.status.success());
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        stderr(&output).contains("did not answer within 1s"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn extracts_text_json_pages_and_detects() {
    let server = Server::start(&["--workers", "2"]);

    let text = server.post_file(
        "/extract",
        &[("X-Dijo-Filename", "simple.odt")],
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

/// Requests as a Tika client sends them, here Kontainer's text pass and its OCR pass.
#[test]
fn tika_clients_are_answered_unchanged() {
    let server = Server::start(&[]);
    let hello = server.request("GET", "/tika", &[], b"");
    assert_eq!(hello.status, 200);
    assert!(hello.text().ends_with("Please PUT\n"));

    let tika = |headers: &[(&str, &str)], name: &str| {
        let body = std::fs::read(format!("{DOCUMENTS}/{name}")).unwrap();
        let common = [
            ("Accept", "text/plain"),
            ("Connection", "close"),
            ("X-Tika-Timeout-Millis", "120000"),
        ];
        server.request("PUT", "/tika", &[&common[..], headers].concat(), &body)
    };
    let text_pass = tika(
        &[
            ("Content-Type", "application/pdf"),
            ("X-Tika-PDFOcrStrategy", "no_ocr"),
        ],
        "2022_Q3_AAPL.pdf",
    );
    assert_eq!(text_pass.status, 200);
    assert_eq!(
        text_pass.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        text_pass.text(),
        server.post_file("/extract", &[], "2022_Q3_AAPL.pdf").text()
    );

    let office = tika(
        &[
            ("Content-Type", "application/vnd.oasis.opendocument.text"),
            ("X-Tika-OCRskipOcr", "true"),
        ],
        "simple.odt",
    );
    assert!(office
        .text()
        .contains("These are a few of my favorite things"));

    let scan = tika(
        &[
            ("Content-Type", "application/pdf"),
            ("X-Tika-OCRLanguage", "eng"),
            ("X-Tika-PDFocrDPI", "150"),
        ],
        "eng-ocr.pdf",
    );
    assert_eq!(scan.status, 200);
    assert_eq!(
        scan.text(),
        server
            .post_file("/extract?ocr_lang=eng", &[], "eng-ocr.pdf")
            .text()
    );

    let locked = tika(&[], "encrypted/password.docx");
    assert_eq!(locked.status, 422);
    let unlocked = tika(&[("Password", "extractous")], "encrypted/password.docx");
    assert!(unlocked.text().contains("Call me Ishmael"));

    assert_eq!(
        server
            .request("PUT", "/tika", &[("Accept", "image/png")], b"x")
            .status,
        406
    );
    assert_eq!(
        tika(&[("X-Tika-PDFextractInlineImages", "true")], "simple.odt").status,
        400
    );
    assert_eq!(
        tika(&[("X-Tika-OCRLanguage", "klingon")], "simple.odt").status,
        400
    );
    assert_eq!(server.request("PUT", "/tika?x=1", &[], b"x").status, 400);
}

fn put_file(server: &Server, target: &str, headers: &[(&str, &str)], name: &str) -> Response {
    let body = std::fs::read(format!("{DOCUMENTS}/{name}")).unwrap();
    server.request("PUT", target, headers, &body)
}

/// tika-server's content endpoints answer what `/extract` answers in the matching format, under
/// tika-server's media types.
#[test]
fn tika_content_endpoints_answer_each_format() {
    let server = Server::start(&[]);
    let cases = [
        ("/tika/text", "text", "text/plain; charset=utf-8"),
        ("/tika/html", "xhtml", "text/html; charset=utf-8"),
        ("/tika/xml", "xhtml", "text/xml; charset=utf-8"),
        ("/tika/md", "markdown", "text/plain; charset=utf-8"),
    ];
    for (target, format, media_type) in cases {
        let answer = put_file(&server, target, &[], "simple.odt");
        assert_eq!(answer.status, 200, "{target}");
        assert_eq!(answer.header("content-type"), Some(media_type), "{target}");
        let expected = server
            .post_file(&format!("/extract?format={format}"), &[], "simple.odt")
            .text();
        assert_eq!(answer.text(), expected, "{target}");
    }
    let markdown = put_file(&server, "/tika/md", &[], "simple.odt").text();
    assert!(markdown.starts_with("# These are a few of my favorite things:\n\n"));
    assert!(markdown.contains("\n- Hockey\n"));

    let html = put_file(&server, "/tika", &[("Accept", "text/html")], "simple.odt");
    assert_eq!(
        html.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(html.text().contains("<html"));
}

#[test]
fn tika_metadata_endpoints_answer_tikas_objects() {
    let server = Server::start(&[]);
    let json = put_file(&server, "/tika/json/text", &[], "simple.odt").json();
    assert_eq!(
        json["Content-Type"],
        "application/vnd.oasis.opendocument.text"
    );
    assert_eq!(json["tk:content-handler-type"], "TEXT");
    assert!(json["tk:content"]
        .as_str()
        .unwrap()
        .contains("These are a few of my favorite things"));
    assert_eq!(json["tk:parsed-by"], "dijo::OpenDocumentParser");
    assert!(json.get("tk:resource-name").is_none());

    let meta = put_file(
        &server,
        "/meta",
        &[("Content-Disposition", "attachment; filename=\"a.odt\"")],
        "simple.odt",
    )
    .json();
    assert!(meta.get("tk:content").is_none());
    assert_eq!(meta["tk:resource-name"], "a.odt");

    let csv = put_file(&server, "/meta", &[("Accept", "text/csv")], "simple.odt");
    assert_eq!(csv.header("content-type"), Some("text/csv; charset=utf-8"));
    assert!(csv.text().contains("Content-Length,7649\n"));
    let field = put_file(
        &server,
        "/meta/Content-Type",
        &[("Accept", "text/plain")],
        "simple.odt",
    );
    assert_eq!(field.text(), "application/vnd.oasis.opendocument.text");
    assert_eq!(
        put_file(&server, "/meta/nope", &[], "simple.odt").status,
        404
    );

    let locked = put_file(&server, "/tika/json", &[], "encrypted/password.docx");
    assert_eq!(locked.status, 200);
    assert!(locked.json()["tk:exception:container-exception"]
        .as_str()
        .unwrap()
        .contains("encrypted"));
    assert_eq!(
        put_file(
            &server,
            "/meta/Content-Type",
            &[],
            "encrypted/password.docx"
        )
        .status,
        422
    );
}

/// The keys a tika-server 3 client reads a PDF's content and page counts by.
const TIKA_3_PDF_KEYS: &[&str] = &[
    "X-TIKA:content",
    "X-TIKA:Parsed-By",
    "pdf:charsPerPage",
    "pdf:unmappedUnicodeCharsPerPage",
    "access_permission:can_print",
];

fn assert_tika_3_pdf_keys(object: &serde_json::Value, context: &str) {
    for key in TIKA_3_PDF_KEYS {
        assert!(
            object.get(key).is_some(),
            "{context} has no {key}: {object}"
        );
    }
    assert_eq!(
        object["pdf:charsPerPage"].as_array().map(Vec::len),
        Some(28),
        "{context}"
    );
}

/// A tika-server 3 client gets what tika-server 3 answered: `PUT /tika/{handler}` with
/// `Accept: application/json` is one JSON object in tika-server 3's keys, the content as the
/// handler writes it, and no 4.x key that has a 3.x one.
#[test]
fn tika_3_clients_get_tika_3s_json_object() {
    let server = Server::start(&[]);
    let accept_json = [
        ("Accept", "application/json"),
        ("Content-Type", "application/pdf"),
    ];
    for (target, marker) in [
        ("/tika/text", "Apple Inc."),
        ("/tika/html", "<html"),
        ("/tika/xml", "<html"),
        ("/tika", "<html"),
    ] {
        let answer = put_file(&server, target, &accept_json, "2022_Q3_AAPL.pdf");
        assert_eq!(answer.status, 200, "{target}");
        assert_eq!(
            answer.header("content-type"),
            Some("application/json"),
            "{target}"
        );
        let object = answer.json();
        assert_tika_3_pdf_keys(&object, target);
        assert!(
            object["X-TIKA:content"].as_str().unwrap().contains(marker),
            "{target}"
        );
        for key in ["tk:content", "tk:parsed-by", "pdf:chars-per-page"] {
            assert!(object.get(key).is_none(), "{target} has {key}");
        }
    }
    let text = put_file(&server, "/tika/text", &accept_json, "2022_Q3_AAPL.pdf").json();
    assert!(!text["X-TIKA:content"].as_str().unwrap().contains("<html"));

    let plain = put_file(&server, "/tika/text", &[], "2022_Q3_AAPL.pdf");
    assert_eq!(
        plain.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        plain.text(),
        server.post_file("/extract", &[], "2022_Q3_AAPL.pdf").text()
    );
    assert_eq!(
        put_file(
            &server,
            "/tika/text",
            &[("Accept", "image/png")],
            "simple.odt"
        )
        .status,
        406
    );
}

/// The endpoints both tika-server 3 and 4 serve answer each value under its 4.x key and its
/// 3.x key; those only tika-server 4 serves, under its keys alone.
#[test]
fn endpoints_both_tika_servers_serve_answer_both_keys() {
    let server = Server::start(&[]);
    let records = put_file(&server, "/rmeta/text", &[], "2022_Q3_AAPL.pdf").json();
    let upload = &records[0];
    assert_tika_3_pdf_keys(upload, "/rmeta/text");
    for (v4, v3) in [
        ("tk:content", "X-TIKA:content"),
        ("tk:parsed-by", "X-TIKA:Parsed-By"),
        ("tk:embedded-depth", "X-TIKA:embedded_depth"),
        ("pdf:chars-per-page", "pdf:charsPerPage"),
        ("access-permission:can-print", "access_permission:can_print"),
    ] {
        assert_eq!(upload[v4], upload[v3], "{v4} and {v3}");
    }

    let named = [("Content-Disposition", "attachment; filename=\"a.pdf\"")];
    let meta = put_file(&server, "/meta", &named, "2022_Q3_AAPL.pdf").json();
    assert_eq!(meta["tk:resource-name"], "a.pdf");
    assert_eq!(meta["resourceName"], "a.pdf");
    assert_eq!(meta["pdf:charsPerPage"], meta["pdf:chars-per-page"]);
    let field = put_file(
        &server,
        "/meta/pdf:charsPerPage",
        &[("Accept", "text/plain")],
        "2022_Q3_AAPL.pdf",
    );
    assert_eq!(field.status, 200);
    assert_eq!(
        field.text(),
        meta["pdf:chars-per-page"][0].as_str().unwrap()
    );

    let json = put_file(&server, "/tika/json/text", &[], "2022_Q3_AAPL.pdf").json();
    assert!(json["tk:content"].is_string());
    for key in TIKA_3_PDF_KEYS {
        assert!(json.get(key).is_none(), "/tika/json/text has {key}");
    }
}

/// `dijo serve` answers a tika-server 3 client as tika-server 3 does: the same status, media
/// type and keys for the content and the page counts. Needs tika-server 3 at
/// TIKA3_SERVER_ADDRESS, such as `docker run -p 9998:9998 apache/tika:3.3.1.0-full`.
#[test]
#[ignore = "needs a running tika-server 3 at TIKA3_SERVER_ADDRESS"]
fn dijo_answers_tika_3_clients_as_tika_server_3_does() {
    let tika = std::env::var("TIKA3_SERVER_ADDRESS").expect("TIKA3_SERVER_ADDRESS");
    let server = Server::start(&[]);
    let body = std::fs::read(format!("{DOCUMENTS}/2022_Q3_AAPL.pdf")).unwrap();
    let headers = [
        ("Accept", "application/json"),
        ("Content-Type", "application/pdf"),
    ];
    let media_type = |response: &Response| {
        response
            .header("content-type")
            .and_then(|value| value.split(';').next())
            .map(str::to_string)
    };
    for target in ["/tika/text", "/tika/html", "/tika/xml", "/rmeta/text"] {
        let theirs = crate::common::server::request(&tika, "PUT", target, &headers, &body);
        let ours = server.request("PUT", target, &headers, &body);
        assert_eq!(ours.status, theirs.status, "{target}");
        assert_eq!(media_type(&ours), media_type(&theirs), "{target}");
        let first = |response: &Response| match response.json() {
            serde_json::Value::Array(mut records) => records.swap_remove(0),
            object => object,
        };
        let (theirs, ours) = (first(&theirs), first(&ours));
        for key in TIKA_3_PDF_KEYS
            .iter()
            .chain(&["pdf:ocrPageCount", "resourceName"])
        {
            assert_eq!(
                ours.get(key).is_some(),
                theirs.get(key).is_some(),
                "{target}: {key}"
            );
        }
        assert_eq!(
            ours["pdf:charsPerPage"].as_array().map(Vec::len),
            theirs["pdf:charsPerPage"].as_array().map(Vec::len),
            "{target}"
        );
    }
}

/// Every document of the upload has an object, numbered in the order it is found, duplicates
/// too, and `/unpack` names each file by its number.
const BOUNDARY: &str = "dijo-test-boundary";

/// A part of a `multipart/form-data` body: its name, file name, media type and content.
struct FormPart<'a> {
    name: Option<&'a str>,
    file_name: Option<&'a str>,
    content_type: &'a str,
    content: Vec<u8>,
}

/// The document `name` as the part named `file`, as Tika clients send it.
fn file_part(name: &str, content_type: &'static str) -> FormPart<'static> {
    FormPart {
        name: Some("file"),
        file_name: None,
        content_type,
        content: std::fs::read(format!("{DOCUMENTS}/{name}")).unwrap(),
    }
}

fn config_part(json: &str) -> FormPart<'static> {
    FormPart {
        name: Some("config"),
        file_name: None,
        content_type: "application/json",
        content: json.as_bytes().to_vec(),
    }
}

fn post_form(
    server: &Server,
    target: &str,
    headers: &[(&str, &str)],
    parts: &[FormPart<'_>],
) -> Response {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(
            format!("--{BOUNDARY}\r\nContent-Disposition: form-data").as_bytes(),
        );
        if let Some(name) = part.name {
            body.extend_from_slice(format!("; name=\"{name}\"").as_bytes());
        }
        if let Some(file_name) = part.file_name {
            body.extend_from_slice(format!("; filename=\"{file_name}\"").as_bytes());
        }
        body.extend_from_slice(
            format!("\r\nContent-Type: {}\r\n\r\n", part.content_type).as_bytes(),
        );
        body.extend_from_slice(&part.content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
    server.request(
        "POST",
        target,
        &[&[("Content-Type", content_type.as_str())], headers].concat(),
        &body,
    )
}

/// Kontainer's requests to tika-server 4.2's `/tika/config` endpoints: a text pass, an OCR pass
/// and office documents, as plain text and as JSON (#394).
#[test]
fn tika_config_endpoints_answer_kontainers_requests() {
    let server = Server::start(&[]);
    let text_config = r#"{"pdf-parser": {"pages": {"text": "EXTRACT"}},
        "timeout-limits": {"totalTaskTimeoutMillis": 120000, "progressTimeoutMillis": 120000}}"#;
    let pdf = || file_part("2022_Q3_AAPL.pdf", "application/pdf");

    let text = post_form(
        &server,
        "/tika/config/text",
        &[],
        &[pdf(), config_part(text_config)],
    );
    assert_eq!(text.status, 200, "{}", text.text());
    assert_eq!(
        text.header("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(
        text.text(),
        server.post_file("/extract", &[], "2022_Q3_AAPL.pdf").text()
    );

    let json = post_form(
        &server,
        "/tika/config/json/text",
        &[],
        &[pdf(), config_part(text_config)],
    );
    assert_eq!(json.status, 200, "{}", json.text());
    let object = json.json();
    assert_eq!(object["tk:content-handler-type"], "TEXT");
    assert_eq!(object["tk:content"].as_str(), Some(text.text().as_str()));
    assert_eq!(
        object["pdf:chars-per-page"].as_array().map(Vec::len),
        Some(28)
    );

    let ocr_config = r#"{"tesseract-ocr-parser": {"language": "eng"}, "pdf-parser": {"pages": {"render": {"dpi": 150}}}}"#;
    let scan = post_form(
        &server,
        "/tika/config/text",
        &[],
        &[
            file_part("eng-ocr.pdf", "application/pdf"),
            config_part(ocr_config),
        ],
    );
    assert_eq!(scan.status, 200, "{}", scan.text());
    assert_eq!(
        scan.text(),
        server
            .post_file("/extract?ocr_lang=eng", &[], "eng-ocr.pdf")
            .text()
    );

    let office = "application/vnd.oasis.opendocument.text";
    for config in [
        r#"{"tesseract-ocr-parser": {"skipOcr": true}}"#,
        r#"{"tesseract-ocr-parser": {"language": "eng+dan", "minFileSizeToOcr": 100000}}"#,
    ] {
        let answer = post_form(
            &server,
            "/tika/config/json/text",
            &[],
            &[file_part("simple.odt", office), config_part(config)],
        );
        assert_eq!(answer.status, 200, "{config}: {}", answer.text());
        assert!(answer.json()["tk:content"]
            .as_str()
            .unwrap()
            .contains("These are a few of my favorite things"));
    }

    let unconfigured = post_form(
        &server,
        "/tika/config/md",
        &[],
        &[file_part("simple.odt", office)],
    );
    assert_eq!(
        unconfigured.text(),
        server
            .post_file("/extract?format=markdown", &[], "simple.odt")
            .text()
    );
    let configured_xml = post_form(
        &server,
        "/tika/config",
        &[],
        &[
            file_part("simple.odt", office),
            config_part(r#"{"basic-content-handler-factory": {"type": "XML"}}"#),
        ],
    );
    assert_eq!(configured_xml.status, 200);
    assert!(configured_xml.text().contains("<html"));
}

#[test]
fn tika_config_endpoints_refuse_what_they_cannot_honour() {
    let server = Server::start(&[]);
    let office = "application/vnd.oasis.opendocument.text";
    let document = || file_part("simple.odt", office);
    let refused = |target: &str, headers: &[(&str, &str)], parts: &[FormPart<'_>]| {
        let answer = post_form(&server, target, headers, parts);
        (
            answer.status,
            answer.json()["error"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    };
    assert_eq!(
        refused("/tika/config/text", &[], &[document(), config_part("{")]).0,
        400
    );
    assert_eq!(
        refused(
            "/tika/config/text",
            &[],
            &[document(), config_part(r#"{"unpack-config": {}}"#)]
        ),
        (
            400,
            "The Tika config component unpack-config is not supported".to_string()
        )
    );
    assert_eq!(
        refused(
            "/tika/config/xml",
            &[],
            &[
                document(),
                config_part(r#"{"basic-content-handler-factory": {"type": "TEXT"}}"#)
            ]
        )
        .0,
        400
    );
    assert_eq!(
        refused(
            "/tika/config/text",
            &[("X-Tika-OCRLanguage", "eng")],
            &[
                document(),
                config_part(r#"{"tesseract-ocr-parser": {"language": "dan"}}"#)
            ]
        )
        .0,
        400
    );
    assert_eq!(
        refused("/tika/config/text", &[], &[config_part("{}")]),
        (
            400,
            "Missing file attachment (use name='file' or send single unnamed attachment)"
                .to_string()
        )
    );
    let raw = put_file(&server, "/tika/config/text", &[], "simple.odt");
    assert_eq!(raw.status, 404);
    let unformed = std::fs::read(format!("{DOCUMENTS}/simple.odt")).unwrap();
    let not_multipart = server.request(
        "POST",
        "/tika/config/text",
        &[("Content-Type", office)],
        &unformed,
    );
    assert_eq!(not_multipart.status, 415);
}

/// The `/form` and `/config` uploads answer as the `PUT` endpoints they mirror.
#[test]
fn tika_multipart_uploads_answer_as_their_put_endpoints() {
    let server = Server::start(&[]);
    let zip = || file_part("nested.zip", "application/zip");

    let put_rmeta = put_file(&server, "/rmeta/ignore", &[], "nested.zip").json();
    let form_rmeta = post_form(&server, "/rmeta/form/ignore", &[], &[zip()]);
    assert_eq!(form_rmeta.status, 200, "{}", form_rmeta.text());
    assert_eq!(form_rmeta.json(), put_rmeta);
    let config_rmeta = post_form(
        &server,
        "/rmeta/config/text",
        &[],
        &[zip(), config_part("{}")],
    )
    .json();
    assert_eq!(config_rmeta[4]["tk:content-handler-type"], "TEXT");
    let body_rmeta = put_file(&server, "/rmeta/body", &[], "nested.zip").json();
    assert_eq!(body_rmeta[4]["tk:content-handler-type"], "BODY");

    let named = FormPart {
        file_name: Some("a.odt"),
        ..file_part("simple.odt", "application/vnd.oasis.opendocument.text")
    };
    let meta = post_form(&server, "/meta/form", &[], &[named]);
    assert_eq!(meta.status, 200, "{}", meta.text());
    assert_eq!(meta.json()["tk:resource-name"], "a.odt");
    let csv = post_form(
        &server,
        "/meta/config",
        &[("Accept", "text/csv")],
        &[file_part(
            "simple.odt",
            "application/vnd.oasis.opendocument.text",
        )],
    );
    assert!(
        csv.text().contains("Content-Length,7649\n"),
        "{}",
        csv.text()
    );

    let rdf = [("Accept", "application/rdf+xml")];
    let xmp = put_file(&server, "/meta", &rdf, "simple.odt");
    assert_eq!(
        xmp.header("content-type"),
        Some("application/rdf+xml; charset=utf-8")
    );
    assert!(xmp
        .text()
        .contains(r#"dc:format="application/vnd.oasis.opendocument.text""#));
    let form_xmp = post_form(
        &server,
        "/meta/form",
        &rdf,
        &[file_part(
            "simple.odt",
            "application/vnd.oasis.opendocument.text",
        )],
    );
    assert_eq!(form_xmp.text(), xmp.text());
    let field_xmp = put_file(&server, "/meta/Content-Type", &rdf, "simple.odt").text();
    assert!(field_xmp.contains("dc:format=") && !field_xmp.contains("xmp:CreateDate"));
    let config_xmp = post_form(
        &server,
        "/meta/config",
        &rdf,
        &[file_part(
            "simple.odt",
            "application/vnd.oasis.opendocument.text",
        )],
    );
    assert_eq!(config_xmp.status, 406);

    let names = |archive: &[u8]| -> Vec<String> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive.to_vec())).unwrap();
        (0..zip.len())
            .map(|index| zip.by_index(index).unwrap().name().unwrap().to_string())
            .collect()
    };
    let unpacked = post_form(&server, "/unpack", &[], &[zip()]);
    assert_eq!(unpacked.status, 200);
    assert_eq!(
        names(&unpacked.body),
        names(&put_file(&server, "/unpack", &[], "nested.zip").body)
    );
    let sidecar = |archive: &[u8]| -> serde_json::Value {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive.to_vec())).unwrap();
        let mut json = String::new();
        zip.by_name("4.txt.metadata.json")
            .unwrap()
            .read_to_string(&mut json)
            .unwrap();
        serde_json::from_str(&json).unwrap()
    };
    let all_text = post_form(
        &server,
        "/unpack/all/config/text",
        &[],
        &[zip(), config_part("{}")],
    );
    assert_eq!(names(&all_text.body).len(), 16);
    assert_eq!(sidecar(&all_text.body)["tk:content-handler-type"], "TEXT");
    let put_all_text = put_file(&server, "/unpack/all/text", &[], "nested.zip");
    assert_eq!(
        sidecar(&put_all_text.body)["tk:content-handler-type"],
        "TEXT"
    );
    assert!(
        sidecar(&put_file(&server, "/unpack/all", &[], "nested.zip").body)
            .get("tk:content")
            .is_none()
    );
}

/// The routes of tika-server 4.2 that answer without extracting: its welcome, its status, and
/// the presets and pipes that a tika-server without configuration does not serve.
#[test]
fn tika_servers_other_routes_answer_as_it_does_unconfigured() {
    let server = Server::start(&[]);
    let html = server.request("GET", "/", &[], b"");
    assert_eq!(html.status, 200);
    assert_eq!(
        html.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(html
        .text()
        .contains("<b>POST</b> <i>/tika/config/json/{handler}</i>"));
    let plain = server.request("GET", "/", &[("Accept", "text/plain")], b"");
    assert!(plain
        .text()
        .contains("POST /tika/config/text\n => text/plain\n"));

    let status = server.request("GET", "/status", &[], b"").json();
    assert_eq!(status["status"], "OPERATING");
    assert_eq!(status["files_processed"], 0);
    put_file(&server, "/tika", &[], "simple.odt");
    let status = server.request("GET", "/status", &[], b"").json();
    assert_eq!(status["files_processed"], 1);
    assert_eq!(status["active_tasks"], 0);
    assert!(status["millis_since_last_parse_started"].is_u64());

    for target in [
        "/tika/preset/fast",
        "/rmeta/preset/fast/text",
        "/unpack/preset/fast/all",
    ] {
        let preset = put_file(&server, target, &[], "simple.odt");
        assert_eq!(preset.status, 404, "{target}");
        assert_eq!(preset.json()["error"], "No such preset: fast");
    }
    for target in ["/pipes", "/async"] {
        assert_eq!(server.request("POST", target, &[], b"{}").status, 404);
    }
}

/// A forked parser's death is answered as tika-server answers it, which clients read the cause
/// from (#394).
#[cfg(target_os = "linux")]
#[test]
fn a_dead_worker_process_is_answered_as_a_dead_forked_parser() {
    let server = Server::start(&[
        "--isolation",
        "process",
        "--workers",
        "1",
        "--worker-max-rss-bytes",
        "1048576",
    ]);
    let stopped = put_file(&server, "/tika/json", &[], "2022_Q3_AAPL.pdf");
    assert_eq!(stopped.status, 503);
    assert_eq!(stopped.json(), serde_json::json!({ "status": "OOM" }));
    let text = post_form(
        &server,
        "/tika/config/text",
        &[],
        &[file_part("2022_Q3_AAPL.pdf", "application/pdf")],
    );
    assert_eq!(text.status, 503);
    assert_eq!(text.json()["status"], "OOM");
    assert_eq!(
        server.post_file("/extract", &[], "2022_Q3_AAPL.pdf").status,
        413
    );
}

#[test]
fn tika_reads_and_unpacks_embedded_documents() {
    let server = Server::start(&[]);
    let records = put_file(&server, "/rmeta/ignore", &[], "nested.zip").json();
    let places: Vec<(String, String, String)> = records
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|record| {
            let field = |key: &str| record[key].as_str().unwrap().to_string();
            (
                field("tk:embedded-id"),
                field("tk:embedded-resource-path"),
                field("tk:embedded-depth"),
            )
        })
        .collect();
    let expected = [
        ("1", "/mail.eml", "1"),
        ("2", "/mail.eml/numbers.xlsx", "2"),
        ("3", "/mail.eml/numbers.xlsx/image1.png", "3"),
        ("4", "/copy-a.txt", "1"),
        ("5", "/copy-b.txt", "1"),
        ("6", "/inner.zip", "1"),
        ("7", "/inner.zip/notes.txt", "2"),
    ]
    .map(|(id, path, depth)| (id.to_string(), path.to_string(), depth.to_string()));
    assert_eq!(places, expected);
    assert_eq!(records[0]["tk:embedded-depth"], "0");
    assert!(records[0].get("tk:content").is_none());

    let text = put_file(&server, "/rmeta/text", &[], "nested.zip").json();
    assert_eq!(text[4]["tk:content-handler-type"], "TEXT");
    assert!(text[4]["tk:content"].is_string());

    let unpacked = put_file(&server, "/unpack", &[], "nested.zip");
    assert_eq!(unpacked.status, 200);
    assert_eq!(unpacked.header("content-type"), Some("application/zip"));
    let names = |archive: &[u8]| -> Vec<String> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive.to_vec())).unwrap();
        (0..zip.len())
            .map(|index| zip.by_index(index).unwrap().name().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        names(&unpacked.body),
        ["1.eml", "2.xlsx", "3.png", "4.txt", "5.txt", "6.zip", "7.txt"]
    );
    let all = names(&put_file(&server, "/unpack/all", &[], "nested.zip").body);
    assert_eq!(all[..2], ["0.zip", "0.zip.metadata.json"]);
    assert_eq!(all.len(), 16);

    assert_eq!(put_file(&server, "/unpack", &[], "simple.odt").status, 204);
    assert_eq!(
        put_file(&server, "/unpack", &[], "encrypted/password.docx").status,
        422
    );
}

#[test]
fn tika_detects_types_and_languages_and_lists_what_it_reads() {
    let server = Server::start(&[]);
    for target in ["/detect", "/detect/stream"] {
        assert_eq!(
            put_file(&server, target, &[], "simple.doc").text(),
            "application/msword"
        );
    }
    let danish = "Dette er en dansk tekst om vejret i København i dag.".as_bytes();
    for (method, target) in [
        ("PUT", "/language"),
        ("POST", "/language/string"),
        ("PUT", "/language/stream"),
    ] {
        assert_eq!(server.request(method, target, &[], danish).text(), "da");
    }

    assert!(server
        .request("GET", "/version", &[], b"")
        .text()
        .starts_with("dijo "));
    let parsers = server
        .request(
            "GET",
            "/parsers/details",
            &[("Accept", "application/json")],
            b"",
        )
        .json();
    assert_eq!(parsers["name"], "dijo::DefaultParser");
    assert!(parsers["children"]
        .as_array()
        .unwrap()
        .iter()
        .any(|parser| parser["name"] == "dijo::OpenDocumentParser"
            && parser["supportedTypes"]
                .as_array()
                .unwrap()
                .contains(&"application/vnd.oasis.opendocument.text".into())));
    let detectors = server.request("GET", "/detectors", &[], b"").text();
    assert!(detectors.starts_with("dijo::DefaultDetector (Composite Detector):\n"));
    let types = server
        .request("GET", "/mime-types", &[("Accept", "application/json")], b"")
        .json();
    assert_eq!(types["application/pdf"]["alias"][0], "application/x-pdf");
    assert_eq!(
        types["application/atom+xml"]["supertype"],
        "application/xml"
    );
    let pdf = server
        .request("GET", "/mime-types/application/x-pdf", &[], b"")
        .json();
    assert_eq!(pdf["type"], "application/pdf");
    assert_eq!(pdf["defaultExtension"], ".pdf");
    assert_eq!(
        server
            .request("GET", "/mime-types/no/such", &[], b"")
            .status,
        404
    );
}

#[test]
fn encrypted_documents_need_the_password_header() {
    let server = Server::start(&[]);
    let locked = server.post_file("/extract", &[], "encrypted/password.docx");
    assert_eq!(locked.status, 422);
    assert_eq!(locked.json()["status"], "encrypted");

    let unlocked = server.post_file(
        "/extract",
        &[("X-Dijo-Password", "extractous")],
        "encrypted/password.docx",
    );
    assert_eq!(unlocked.status, 200);
    assert!(unlocked.text().contains("Call me Ishmael"));
}

/// A document its parser fails on is the client's to fix, not a fault of the server (#314).
#[test]
fn damaged_documents_are_unprocessable() {
    let server = Server::start(&[]);
    let mut png = std::fs::read(format!("{DOCUMENTS}/ara-ocr.png")).unwrap();
    png[33..37].copy_from_slice(&0xFFFF_FFF0_u32.to_be_bytes());
    let damaged = server.request("POST", "/extract", &[], &png);
    assert_eq!(damaged.status, 422);
    let document = damaged.json();
    assert_eq!(document["errors"][0]["category"], "corrupt");
    assert_eq!(
        document["errors"][0]["message"],
        "image/png parse error: Invalid chunk length -16"
    );
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

/// A request rejected before its body is read is answered once the body is, so that the
/// connection closing after the answer cannot reset it and lose the answer, as macOS loses it.
#[test]
fn rejections_are_answered_once_the_body_is_read() {
    let server = Server::start(&[]);
    for (method, target) in [
        ("POST", "/extract?format=pdf"),
        ("POST", "/detect?x=1"),
        ("PUT", "/tika?x=1"),
        ("POST", "/nope"),
    ] {
        let mut stream = server.connect();
        write_head(&mut stream, method, target, &[], 5);
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut early = [0; 1];
        let answered_early = stream.read(&mut early).is_ok_and(|read| read > 0);
        assert!(
            !answered_early,
            "{target} was answered before its body was sent"
        );
        stream.set_read_timeout(None).unwrap();
        stream.write_all(b"hello").unwrap();
        let answer = read_response(stream);
        assert!(
            matches!(answer.status, 400 | 404),
            "{target}: {}",
            answer.status
        );
    }
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
    assert!(health(&["--listen", &server.address], None)
        .status
        .success());
    let unready = health(&["--listen", &server.address, "--ready"], None);
    assert!(!unready.status.success());
    assert!(
        stderr(&unready).contains("/readyz answered 503 Service Unavailable"),
        "{}",
        stderr(&unready)
    );

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
    assert!(metrics.contains("dijo_requests_total{endpoint=\"extract\",status=\"200\"} 1"));
    assert!(metrics.contains("dijo_requests_total{endpoint=\"extract\",status=\"422\"} 1"));
    assert!(metrics.contains("dijo_failures_total{category=\"encrypted\"} 1"));
    assert!(metrics.contains("dijo_workers "));
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

/// The parse a worker process stopped waiting for may run on: the process is replaced, not
/// handed the next job.
#[test]
fn a_worker_process_whose_extraction_timed_out_is_replaced() {
    let server = Server::start(&["--isolation", "process", "--workers", "1", "--timeout", "3"]);
    stop_the_worker_by_its_timeout(&server);
    let next = server.post_file("/extract", &[], "simple.odt");
    assert!(next
        .text()
        .contains("These are a few of my favorite things"));
    let metrics = server.request("GET", "/metrics", &[], b"").text();
    assert!(metrics.contains("dijo_workers 1"), "{metrics}");
}

/// Times out the only worker process of `server`, whose timeout is 3 seconds, and waits until it
/// is stopped.
fn stop_the_worker_by_its_timeout(server: &Server) {
    let document = server
        .post_file("/extract?format=json&ocr=always", &[], "2022_Q3_AAPL.pdf")
        .json();
    assert_eq!(document["errors"][0]["category"], "timeout");
    let started = Instant::now();
    loop {
        let metrics = server.request("GET", "/metrics", &[], b"").text();
        if metrics.contains("dijo_workers 0") {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the worker process was not stopped: {metrics}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A worker process is replaced from the program the server runs, though its file was replaced
/// or removed meanwhile, as an upgrade under a running server does (#312).
#[cfg(target_os = "linux")]
#[test]
fn a_worker_process_is_replaced_once_the_servers_file_is_gone() {
    let scratch = Scratch::new("replaced-executable");
    let program = scratch.0.join("dijo");
    std::fs::copy(env!("CARGO_BIN_EXE_dijo"), &program).unwrap();
    let server = Server::start_program(
        &program,
        &["--isolation", "process", "--workers", "1", "--timeout", "3"],
        &[],
    );
    std::fs::remove_file(&program).unwrap();
    stop_the_worker_by_its_timeout(&server);
    let next = server.post_file("/extract", &[], "simple.odt");
    assert_eq!(next.status, 200, "{}", next.text());
    assert!(next
        .text()
        .contains("These are a few of my favorite things"));
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
        &[("X-Dijo-Password", "extractous")],
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
    assert!(metrics.contains("dijo_cache_hits_total 1"), "{metrics}");
    assert!(metrics.contains("dijo_cache_misses_total 1"), "{metrics}");
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
        &[("X-Dijo-Password", "extractous")],
        "encrypted/password.docx",
    );
    assert!(unlocked.text().contains("Call me Ishmael"));

    let detected = server.post_file("/detect", &[], "simple.doc");
    assert_eq!(detected.text(), "application/msword\n");

    let metrics = server.request("GET", "/metrics", &[], b"").text();
    assert!(metrics.contains("dijo_workers 2"), "{metrics}");
    assert!(
        metrics.contains("dijo_failures_total{category=\"encrypted\"} 1"),
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
        metrics.contains("dijo_failures_total{category=\"limit-exceeded\"} 1"),
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
            &[("Content-Type", "text/csv"), ("X-Dijo-Filename", "t.txt")],
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
    let body = std::fs::read(crate::common::document("simple.odt")).unwrap();
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
