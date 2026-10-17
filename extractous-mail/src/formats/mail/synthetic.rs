//! Synthetic messages that exercise what the corpora's messages do not, compared with Tika's path
//! as the corpora's are: transfer encodings, charsets, multipart structures, boundaries at the
//! edges of mime4j's buffers, headers and the parts handed to the embedded document extractor.

#[cfg(feature = "tika")]
use std::path::{Path, PathBuf};

#[cfg(feature = "tika")]
use super::EmlxParser;
use super::Rfc822Parser;
#[cfg(feature = "tika")]
use super::DEPARTURES;
#[cfg(feature = "tika")]
use crate::formats::parity::{
    assert_fails_as_tika, assert_same_as_tika_departing, documents, Scratch,
};
use crate::formats::Parser;

#[cfg(feature = "tika")]
const HEADER: &str = "From: Ada Lovelace <ada@example.org>\r\nTo: Charles Babbage <charles@example.org>\r\nSubject: Notes\r\nDate: Tue, 05 Mar 2024 14:30:00 +0100\r\nMIME-Version: 1.0\r\n";

#[cfg(feature = "tika")]
fn message(headers: &str, body: &str) -> Vec<u8> {
    format!("{HEADER}{headers}\r\n{body}").into_bytes()
}

#[cfg(feature = "tika")]
fn multipart(subtype: &str, boundary: &str, parts: &[&str]) -> String {
    let mut body =
        format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\"\r\n\r\npreamble\r\n");
    for part in parts {
        body.push_str(&format!("--{boundary}\r\n{part}\r\n"));
    }
    body.push_str(&format!("--{boundary}--\r\nepilogue\r\n"));
    body
}

#[cfg(feature = "tika")]
fn lines(count: usize, width: usize) -> String {
    (0..count)
        .map(|line| format!("{line:05} {}\r\n", "x".repeat(width)))
        .collect()
}

/// The messages, by file name.
#[cfg(feature = "tika")]
fn messages() -> Vec<(String, Vec<u8>)> {
    let plain = "Content-Type: text/plain; charset=utf-8\r\n\r\nPlain part, caf\u{e9}.";
    let html = "Content-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>HTML <b>part</b> &amp; more</p></body></html>";
    let pdf_like = "Content-Type: application/octet-stream; name=\"data.bin\"\r\nContent-Disposition: attachment; filename=\"data.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\nAAECAwQFBgcICQ==";
    let attached_text = "Content-Type: text/plain\r\nContent-Disposition: attachment; filename*=UTF-8''r%C3%A9sum%C3%A9.txt\r\n\r\nAttached text, not inline.";
    let mut messages: Vec<(String, Vec<u8>)> = vec![
        ("plain.eml".into(), message("Content-Type: text/plain; charset=UTF-8\r\n", "Hello, world.\r\nSecond line.\r\n")),
        ("no-content-type.eml".into(), message("", "Body without a content type.\r\n")),
        ("latin1.eml".into(), {
            let mut bytes = message("Content-Type: text/plain; charset=iso-8859-1\r\n", "");
            bytes.extend_from_slice(b"Caf\xe9 cr\xe8me br\xfbl\xe9e.\r\n");
            bytes
        }),
        ("quoted-printable.eml".into(), message(
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n",
            "Caf=C3=A9 with a soft=\r\n break and trailing space   \r\nan escaped =3D sign, a broken =ZZ escape, ==\r\nand a lone =\r\n",
        )),
        ("base64.eml".into(), message(
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n",
            "SGVsbG8gZnJvbSBiYXNlNjQsIGNhZsOpLg==\r\n",
        )),
        ("html.eml".into(), message(
            "Content-Type: text/html; charset=windows-1252\r\n",
            "<html><head><title>Inner</title><meta name=\"author\" content=\"x\"></head><body><h1>Title</h1><p>Text &euro; <a href=\"http://example.org\">link</a></p><ul><li>one</li><li>two</li></ul></body></html>",
        )),
        ("csv-body.eml".into(), message(
            "Content-Type: text/plain\r\n",
            "a,b,c\r\n1,2,3\r\n4,5,6\r\n7,8,9\r\n",
        )),
        ("encoded-headers.eml".into(), "From: =?utf-8?q?Ren=C3=A9_Descartes?= <rene@example.org>\r\nTo: \"Babbage, Charles\" <c@example.org>, Team: a@x.org, b@y.org;\r\nCc: =?iso-8859-1?q?J=F6rg?= <j@example.org>\r\nBcc: hidden@example.org\r\nSubject: =?utf-8?b?w4lsw6h2ZQ==?= =?utf-8?q?_et_ma=C3=AEtre?=\r\nDate: Mon, 9 May 2016 7:32:00 UTC+0600 (BST)\r\nX-Folded: first\r\n  second\r\n\tthird\r\nX-Empty:\r\nX-Spaces:   \r\nContent-Type: text/plain\r\nbogus header line\r\n\r\nBody.\r\n".as_bytes().to_vec()),
        ("repeated-headers.eml".into(), "From: a@example.org\r\nFrom: B <b@example.org>\r\nSubject: first\r\nSubject: second\r\nDate: not a date\r\nTo: x@y\r\nTo: z@w\r\nReceived: one\r\nReceived: two\r\n\r\nBody.\r\n".to_owned().into_bytes()),
        ("date-forms.eml".into(), "From: a@example.org\r\nDate: July 9 2012 10:10:10am\r\n\r\nBody.\r\n".to_owned().into_bytes()),
        ("alternative.eml".into(), message("", &multipart("alternative", "alt", &[plain, html]))),
        ("alternative-rtf.eml".into(), message("", &multipart("alternative", "alt", &[plain, "Content-Type: application/rtf\r\n\r\n{\\rtf1 Rich}"]))),
        ("mixed.eml".into(), message("", &multipart("mixed", "mix", &[plain, pdf_like, attached_text, html]))),
        ("related-in-alternative.eml".into(), {
            let related = multipart("related", "rel", &[html, "Content-Type: image/png\r\nContent-ID: <img1>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo="]);
            message("", &multipart("alternative", "alt", &[plain, &related]))
        }),
        ("alternative-in-mixed.eml".into(), {
            let alternative = multipart("alternative", "alt", &[plain, html]);
            message("", &multipart("mixed", "mix", &[&alternative, pdf_like]))
        }),
        ("nested-mixed.eml".into(), {
            let inner = multipart("mixed", "inner", &[plain, html]);
            message("", &multipart("mixed", "outer", &[&inner, plain]))
        }),
        ("attached-message.eml".into(), {
            let attached = "Content-Type: message/rfc822\r\n\r\nFrom: inner@example.org\r\nSubject: Inner\r\n\r\nInner body.\r\n";
            message("", &multipart("mixed", "mix", &[plain, attached]))
        }),
        ("digest.eml".into(), message("", &multipart("digest", "dig", &["\r\nFrom: d1@example.org\r\nSubject: One\r\n\r\nFirst.", "\r\nFrom: d2@example.org\r\nSubject: Two\r\n\r\nSecond."]))),
        ("calendar.eml".into(), message("", &multipart("mixed", "mix", &[plain, "Content-Type: text/calendar; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nEND:VCALENDAR"]))),
        ("unterminated.eml".into(), message("Content-Type: multipart/mixed; boundary=b\r\n", "--b\r\nContent-Type: text/plain\r\n\r\nNever closed.\r\n--b\r\n\r\nSecond part")),
        ("boundary-without-dashes.eml".into(), message("Content-Type: multipart/mixed; boundary=b\r\n", "--b\r\n\r\nBody --b inside a line\r\n--bx not a boundary\r\n--b \r\n\r\nafter\r\n--b--")),
        ("multipart-without-boundary.eml".into(), message("Content-Type: multipart/mixed\r\n", "--b\r\n\r\nNot a part.\r\n")),
        ("empty-boundary.eml".into(), message("Content-Type: multipart/mixed; boundary=\"\"\r\n", "--\r\n\r\nPart.\r\n----\r\n")),
        ("lf-only.eml".into(), "From: a@example.org\nSubject: LF\nContent-Type: multipart/alternative; boundary=b\n\n--b\nContent-Type: text/plain\n\nplain\n--b\nContent-Type: text/html\n\n<p>html</p>\n--b--\n".to_owned().into_bytes()),
        ("headers-only.eml".into(), "From: a@example.org\r\nSubject: Nothing else".to_owned().into_bytes()),
        ("inline-image.eml".into(), message("", &multipart("mixed", "mix", &[plain, "Content-Type: image/gif\r\nContent-Disposition: inline; filename=dot.gif\r\nContent-Transfer-Encoding: base64\r\n\r\nR0lGODlhAQABAAAAACw="]))),
        ("disposition-dates.eml".into(), message("", &multipart("mixed", "mix", &[plain, "Content-Type: text/plain\r\nContent-Disposition: attachment; filename=\"a.txt\"; creation-date=\"Tue, 05 Mar 2024 14:30:00 +0100\"; modification-date=\"Wed, 06 Mar 2024 14:30:00 +0100\"; size=3\r\n\r\nabc"]))),
    ];
    for offset in [4050, 4060, 4070, 4080, 4085, 4090, 4096, 4100, 8180, 8190] {
        let filler = "y".repeat(offset);
        let body = multipart("mixed", "edge-boundary", &[
            &format!("Content-Type: text/plain\r\n\r\n{filler}"),
            &format!("Content-Type: text/plain\r\n\r\n{filler}--edge-boundary inside\r\n--edge-boundary-not\r\nend"),
            plain,
        ]);
        messages.push((format!("buffer-edge-{offset}.eml"), message("", &body)));
    }
    let long = multipart(
        "mixed",
        "long",
        &[
            &format!(
                "Content-Type: text/plain\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{}",
                "caf=C3=A9 ==\r\nline=\r\n ".repeat(900)
            ),
            &format!("Content-Type: text/plain\r\n\r\n{}", lines(800, 70)),
        ],
    );
    messages.push(("long-parts.eml".into(), message("", &long)));
    messages.push((
        "long-header.eml".into(),
        message(&format!("X-Long: {}\r\n", "z".repeat(99_000)), "Body.\r\n"),
    ));
    messages
}

#[cfg(feature = "tika")]
#[test]
fn extracts_synthetic_messages_as_tikas_path_does() {
    let scratch = Scratch::new("mail-synthetic");
    for (name, bytes) in messages() {
        scratch.write(&name, &bytes);
    }
    let (parsed, deferred) = documents(&Rfc822Parser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "deferred: {deferred:?}");
    let names: Vec<String> = parsed
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    let missing: Vec<String> = messages()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| !names.contains(name))
        .collect();
    assert_eq!(
        missing,
        Vec::<String>::new(),
        "messages Tika does not detect as email"
    );
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_same_as_tika_departing(
        &paths,
        "org.apache.tika.parser.mail.RFC822Parser",
        DEPARTURES,
    );
    let emlx = {
        let inner = message("Content-Type: text/plain\r\n", "Apple Mail body.\r\n");
        let mut bytes = format!("{}      \n", inner.len()).into_bytes();
        bytes.extend_from_slice(&inner);
        bytes.extend_from_slice(b"<?xml version=\"1.0\"?><plist/>");
        bytes
    };
    scratch.write("apple.emlx", &emlx);
    let (parsed, _) = documents(&EmlxParser, &[scratch.directory()]);
    assert_eq!(parsed.len(), 1);
    assert_same_as_tika_departing(&[parsed[0].as_path()], "ai.yobix.EmlxParser", DEPARTURES);
}

#[test]
fn defers_dates_naming_a_time_zone() {
    let message = b"From: a@example.org\r\nDate: Thu, 9 Jul 2012 10:10:10 PST\r\n\r\nBody.\r\n";
    let reason = Rfc822Parser.defers(
        message,
        &crate::formats::metadata::TikaMetadata::new(),
        &crate::config::ParserConfigs::default(),
    );
    assert!(reason.is_some_and(|reason| reason.contains("time zone")));
}

#[cfg(feature = "tika")]
#[test]
fn fails_where_tikas_path_fails() {
    let scratch = Scratch::new("mail-failures");
    let failing = [
        (
            "line-too-long.eml",
            message(&format!("X-Long: {}\r\n", "z".repeat(100_000)), "Body.\r\n"),
        ),
        (
            "header-too-long.eml",
            message(
                &format!(
                    "X-Long: a\r\n{}",
                    " folded line of a long header\r\n".repeat(3500)
                ),
                "Body.\r\n",
            ),
        ),
    ];
    for (name, bytes) in &failing {
        scratch.write(name, bytes);
    }
    let (parsed, deferred) = documents(&Rfc822Parser, &[scratch.directory()]);
    assert!(deferred.is_empty(), "deferred: {deferred:?}");
    assert_eq!(parsed.len(), failing.len());
    let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
    assert_fails_as_tika(&paths);
}
