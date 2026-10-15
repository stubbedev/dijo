//! The header readers against mime4j's and Tika's own answers:
//! `tests/fixtures/mail/fields.tsv.gz`, written by `just mail-reference`
//! (`MailReference.java` describes its form).

use std::io::Read;

use flate2::read::GzDecoder;

use super::address::{flatten, parse_address_list};
use super::codec::decode_encoded_words;
use super::date::parse_date_lenient;
use super::handler::person_and_email;
use super::structured::{ContentDisposition, ContentType};
use super::Unported;
use crate::formats::java_time::format_instant;
use crate::text::charset::tests::base64;

const REFERENCE: &[u8] = include_bytes!("../../../tests/fixtures/mail/fields.tsv.gz");
const SEPARATOR: char = '\u{1F}';
const PAIR: char = '\u{1E}';

fn text(field: &str) -> String {
    String::from_utf8(base64(field)).expect("the reference holds UTF-8")
}

fn null(value: Option<&str>) -> &str {
    value.unwrap_or("null")
}

fn answer(kind: &str, value: &str) -> Result<Option<String>, Unported> {
    Ok(match kind {
        "date" => parse_date_lenient(value)?.map(format_instant),
        "words" => Some(decode_encoded_words(value)?),
        "ctype" => {
            let parsed = ContentType::parse(value);
            let mut out = [&parsed.mime_type, &parsed.media_type, &parsed.subtype]
                .map(|part| null(part.as_deref()).to_owned())
                .join(&SEPARATOR.to_string());
            let mut parameters: Vec<(&str, &str)> = parsed.parameters().collect();
            parameters.sort_unstable();
            for (name, value) in parameters {
                out.push_str(&format!("{SEPARATOR}{name}{PAIR}{value}"));
            }
            Some(out)
        }
        "cdisp" => {
            let parsed = ContentDisposition::parse(value)?;
            let mut out = parsed.kind.clone();
            for (name, value) in parsed.parameters.iter() {
                out.push_str(&format!("{SEPARATOR}{name}{PAIR}{value}"));
            }
            Some(out)
        }
        "to" => {
            let shown = parse_address_list(value)?
                .iter()
                .map(|address| address.display())
                .collect::<Result<Vec<_>, _>>()?;
            Some(shown.join(&SEPARATOR.to_string()))
        }
        "from" => {
            let mut out = Vec::new();
            for address in flatten(parse_address_list(value)?) {
                let shown = address.display()?;
                let (person, email) = person_and_email(&shown);
                out.push(shown);
                out.push(format!(
                    "{}{PAIR}{}",
                    null(person.as_deref()),
                    null(email.as_deref())
                ));
            }
            Some(out.join(&SEPARATOR.to_string()))
        }
        other => panic!("the reference has an unknown kind {other}"),
    })
}

#[test]
fn reads_headers_as_mime4j_and_tika_do() {
    let mut reference = String::new();
    GzDecoder::new(REFERENCE)
        .read_to_string(&mut reference)
        .expect("the reference decompresses");
    let mut mismatches = Vec::new();
    let mut unported = Vec::new();
    let mut count = 0;
    for line in reference.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [kind, value, expected] = fields[..] else {
            panic!("a reference line has three fields: {line}");
        };
        count += 1;
        let value = text(value);
        let expected = (expected != "null").then(|| text(expected));
        match answer(kind, &value) {
            Ok(actual) if actual == expected => {}
            Ok(actual) => mismatches.push(format!(
                "{kind} {value:?}: expected {expected:?}, got {actual:?}"
            )),
            Err(reason) => unported.push(format!("{kind} {value:?}: {reason:?}")),
        }
    }
    assert!(count > 600, "the reference has {count} values");
    assert!(
        mismatches.is_empty(),
        "{} of {count} values differ:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    let zones = unported
        .iter()
        .filter(|line| line.ends_with("TimeZone"))
        .count();
    let charsets = unported
        .iter()
        .filter(|line| line.ends_with("Charset"))
        .count();
    assert_eq!(
        (zones, charsets),
        (17, 4),
        "values left to Tika: dates naming a time zone, and malformed ISO-2022-JP:\n{}",
        unported.join("\n")
    );
}
