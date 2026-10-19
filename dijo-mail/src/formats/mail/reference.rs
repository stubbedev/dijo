//! The header readers against mime4j's and Tika's own answers:
//! `tests/fixtures/mail/fields.tsv.gz`, written by `just mail-reference`
//! (`MailReference.java` describes its form).

use std::io::Read;

use flate2::read::GzDecoder;

use super::address::Address;
use super::address::{flatten, parse_address_list};
use super::codec::decode_encoded_words;
use super::date::{parse_date, MailDate};
use super::structured::{ContentDisposition, ContentType};
use super::Unported;
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
        "date" => parse_date(value)?.map(MailDate::format),
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
            let shown: Vec<String> = parse_address_list(value)?
                .iter()
                .map(Address::display)
                .collect();
            Some(shown.join(&SEPARATOR.to_string()))
        }
        "from" => {
            let mut out = Vec::new();
            for address in flatten(parse_address_list(value)?) {
                let Address::Mailbox(mailbox) = &address else {
                    unreachable!("a flattened address is a mailbox");
                };
                let (person, email) = (mailbox.name(), mailbox.email());
                out.push(address.display());
                out.push(format!("{}{PAIR}{}", null(person), null(email.as_deref())));
            }
            Some(out.join(&SEPARATOR.to_string()))
        }
        other => panic!("the reference has an unknown kind {other}"),
    })
}

/// The reference's values the port reads differently from mime4j on purpose, by the issue that
/// names mime4j's behaviour.
const DEPARTURES: &[(&str, &str, &str)] = &[
    ("cdisp", "inline; filename=a/b.txt", "#75"),
    ("cdisp", "inline; filename=a/b.tt", "#75"),
    ("cdisp", "inline; filename=a/b.taxt", "#75"),
    ("cdisp", "inline; filename=a/b.txxt", "#75"),
    ("cdisp", "INLINE; FILENAME=A/B.TXT", "#75"),
    ("cdisp", "inline; filename=a/b.t", "#75"),
    ("cdisp", "attachment; filename=\"Szőlő.pdf\"", "#75"),
    ("cdisp", "attachment; filename=\"szőlő.pdf\"", "#75"),
    ("cdisp", "attachment; fileame=\"Szőlő.pdf\"", "#75"),
    ("cdisp", "attachment; fileőname=\"Szőlő.pdf\"", "#75"),
    (
        "cdisp",
        "attachment; filename=\"Szőname=\"Szőlő.pdf\"",
        "#75",
    ),
    ("ctype", "text/plain; a=1; name=\"Ā\"; b=2", "#75"),
    ("ctype", "text/plain; a=1; name=\"Ā\"; b=", "#75"),
    ("ctype", "text/plain; a=1; name=\"Ā\"; b=[2", "#75"),
    ("ctype", "text/plain; a=1; name=\"Ā\"; b=22", "#75"),
    ("ctype", "text/plain; a=1; name=\"ā\"; b=2", "#75"),
    (
        "ctype",
        "multipart/őalternative; boundary=\"_=ExampleMailer=_0123456789abcdef\"",
        "#75",
    ),
    (
        "cdisp",
        "Attachment; filename*0*=utf-8''caf%C3%A9; file=utf-8''caf%C3%A9; filename*1=.txt; size=12",
        "#76",
    ),
    (
        "cdisp",
        "attachment; filename=a+b.txt; name=utf-8''x",
        "#76",
    ),
    ("words", "=?utf-8?b?w6k?=", "#72"),
    ("words", "=?UTF-8?B?W6K?=", "#72"),
    ("words", "=?shift_jis?b?k/qW4zq?=", "#72"),
    ("words", "=?shift_jis?b?k/qWe4e4zq?=", "#72"),
    ("date", "9 May 69 01:32:00 +0000", "#80"),
    ("date", "9 May 916 01:32:00 +0000", "#80"),
    ("date", "9 May 1 01:32:00 +0000", "#80"),
    ("date", "09 MAY 2016 01:32:00.12345 +0000", "#80"),
    ("date", "9 May 2016 25:32:00 +0000", "#80"),
    ("date", "31 Feb 2021 10:00", "#80"),
    ("date", "0 Jan 2021 10:00", "#80"),
    ("date", "9 J 2016 01:32:00 +0000", "#80"),
    ("date", "9 5 2016 01:32:00 +0000", "#80"),
    ("date", "9 13 2016 01:32:00 +0000", "#80"),
    ("date", "July 9 2012 12:10:10 PM", "#80"),
    ("date", "July 9 2012 12:10:10 AM", "#80"),
    ("date", "2012-10-10", "#80"),
    ("date", "2012-1-10", "#80"),
    ("date", "7/9/2012", "#80"),
    ("date", "13/9/2012", "#80"),
    ("date", "31/12/99", "#80"),
    ("date", "12/31/99", "#80"),
    ("date", "99/12/31", "#80"),
    ("date", "2012/12/31", "#80"),
    ("date", "Jul 9 2012", "#80"),
    ("date", "9 Jul 2012", "#80"),
    ("date", "9 Jul 12", "#80"),
    ("date", "Jul 9, 2012", "#80"),
    ("date", "Thu, 9 Jul 2012 10:10:10 +1", "#80"),
    ("date", "10-10-2000 10:10 -0500", "#80"),
    ("date", "+5 Jan 2020 10:00", "#80"),
    ("date", "-5 Jan 2020 10:00", "#80"),
    ("date", "5 Jan 2020 +10:00", "#80"),
    ("date", "Thu Jul 9 2012", "#80"),
    ("date", "Fri, 27 Oct 2023 ", "#80"),
    ("date", "Thu, 7 Jun 200 02:15:00 -0700 (PDT)", "#80"),
    ("date", "Thu, 7 Jun 20011 02:15:00 -0700 (PDT)", "#80"),
    ("date", "Thu, 7 Jun 200", "#80"),
    ("date", "Mon, 16 May 2016 09:030:32 +0100", "#80"),
    ("date", "Fri, 16 Jan 20159 07:35:46 +0000", "#80"),
    ("date", "Fri, 16 Jan 2015", "#80"),
    ("date", "Fri, 15 Feb 2008 15:01:15 -15 -0800", "#80"),
    ("date", "Fri, 26 Nov 2010 19:57:53 0000", "#80"),
    ("date", "Mon, 9 May 016 7:32:00 UTC+0600 (BST)", "#80"),
    ("date", "Mon, 9 May 2016 732:00 GMT-0330", "#80"),
    ("date", "9 May 2016 01:32:00 00 GMT", "#80"),
    ("date", "9 May 2016 ", "#80"),
    ("date", "9 May 206 01:32:00 z", "#80"),
    ("date", "9 May 20", "#80"),
    ("date", "9 May 2016 01:32:0000", "#80"),
    ("date", "9 May 016 1:2", "#80"),
    ("date", "9 May 16 001:32:00 +0000", "#80"),
    ("date", "9 May 16 ", "#80"),
    ("date", "9 MAY 69 01:32:00 +0000", "#80"),
    ("date", "9 May 916 01:32:00 +000", "#80"),
    ("date", "9 May 916 01:32:00 +00000", "#80"),
    ("date", "9 may 916 01:32:00 +0000", "#80"),
    ("date", "9 May 916 01:32:00 +00", "#80"),
    ("date", "9 May 1 1:32:00 +0000", "#80"),
    ("date", "9 may 1 01:32:00 +0000", "#80"),
    ("date", "9 May 1 ", "#80"),
    ("date", "09 MAY 2016 01:32:0.12345 +0000", "#80"),
    ("date", "09 MAY 2016 01:32:00.12345 +0000.12345 +0000", "#80"),
    ("date", "9 May 2016 25:32:00 +000", "#80"),
    ("date", "9 May 2016 25:32:00 +00:00", "#80"),
    ("date", "9 May 2016 25:32:00 +00000", "#80"),
    ("date", "9 may 2016 25:32:00 +0000", "#80"),
    ("date", "9 May 2016 25:32:00 +00", "#80"),
    ("date", "31 Feb 021 10:00", "#80"),
    ("date", "31 Feb 22021 10:00", "#80"),
    ("date", "31 feb 2021 10:00", "#80"),
    ("date", "0 JAN 2021 10:00", "#80"),
    ("date", "9 J 2016 01:32:00 0000", "#80"),
    ("date", "9 J 2016 01:32:00 ", "#80"),
    ("date", "9 5 2016 01:32:00 +000", "#80"),
    ("date", "9 5 2016 01:32:00 +000)0", "#80"),
    ("date", "9 5 2016 01:32:00 +00000", "#80"),
    ("date", "9 13 2016", "#80"),
    ("date", "July 9 2012 10:10:010am", "#80"),
    ("date", "July 9 2012 12:10:10PM", "#80"),
    ("date", "July 9 2012 12:10:100 PM", "#80"),
    ("date", "July 9 2012 12:10:10  PM", "#80"),
    ("date", "JULY 9 2012 12:10:10 PM", "#80"),
    ("date", "JULY 9 2012 12:10:10 AM", "#80"),
    ("date", "July 9 2012 109:10 p", "#80"),
    ("date", "Jul 9 2012 10:10:1010", "#80"),
    ("date", "2012-10-10 10:100:10", "#80"),
    ("date", "202-10-10", "#80"),
    ("date", "2012-91-10", "#80"),
    ("date", "2012-11-10", "#80"),
    ("date", "7/9/2012 10:1010 PM", "#80"),
    ("date", "13/9/012", "#80"),
    ("date", "1/12/99", "#80"),
    ("date", "12/31/999", "#80"),
    ("date", "99/12/1", "#80"),
    ("date", "012/12/31", "#80"),
    ("date", "Jul 9 212", "#80"),
    ("date", "Jul 9 2012012", "#80"),
    ("date", "JUL 9 2012", "#80"),
    ("date", "Jul 9 2", "#80"),
    ("date", "9 jul 2012", "#80"),
    ("date", "9 JUL 12", "#80"),
    ("date", "Jul 9, 202", "#80"),
    ("date", "Jul 9, 20112", "#80"),
    ("date", "JUL 9, 2012", "#80"),
    ("date", "Jul 9, 20", "#80"),
    ("date", "Thursday, July -9 2012 10:10 +0100", "#80"),
    ("date", "Thu, 9 Jul 2012", "#80"),
    ("date", "Thu, 9 Jul 2012 10:1010 PST", "#80"),
    ("date", "Thu, 9 Jul 2", "#80"),
    ("date", "Thu, 9 Jul 2012 10:0:10 +1", "#80"),
    ("date", "THU, 9 JUL 2012 10:10:10 +1", "#80"),
    ("date", "10-10-2000 1:10 -0500", "#80"),
    ("date", "10-10-2000 10:100:10 -0500", "#80"),
    ("date", "+,5 Jan 2020 10:00", "#80"),
    ("date", "+5 JAN 2020 10:00", "#80"),
    ("date", "-5 jan 2020 10:00", "#80"),
    ("date", "5 Jan +2", "#80"),
    ("date", "5 JAN 2020 +10:00", "#80"),
    ("date", "Mon, 5 Jan 202 10:00 +0100\n", "#80"),
    ("date", "Mon, 5 Jan 202", "#80"),
    ("date", "Thu Jul 9 212", "#80"),
    ("date", "Thu Jul 9 201012", "#80"),
    ("date", "THU JUL 9 2012", "#80"),
    ("date", "Thu Jul 9 2", "#80"),
    ("from", "\"Novell GroupWise\" <$$$.$$$.$$$>", "#77"),
    ("from", "\"a@b\" <c@d>", "#77"),
    ("from", "Novell GroupWise <$$$.$$$.$$$>", "#77"),
    ("from", "Tim Allis>on <tallison@apache.org>", "#77"),
    ("from", "\"Vladimir L.\" <vladimir_l@example.covladimir_l@example.com>", "#77"),
    ("from", "Hari Seldon <hseldon@example.comdon <hseldon@example.com>", "#77"),
    ("from", "\"Novell GroupWse\" <$$$.$$$.$$$>", "#77"),
    ("from", "\"Novell GroupWéise\" <$$$.$$$.$$$>", "#77"),
    ("from", "\"Novell GroupWise\" <$$$.$$$.$ise\" <$$$.$$$.$$$>", "#77"),
    ("from", "\"NOVELL GROUPWISE\" <$$$.$$$.$$$>", "#77"),
    ("from", "\"Julien N\"ioche (JIRA)\" <jira@apache.org>", "#77"),
    ("from", "\"Julien Nioche (JIRA)\" <jira@apioche (JIRA)\" <jira@apache.org>", "#77"),
    ("from", "fooooooo.baaaaar@foobaaaaar@fooo.ba", "#77"),
    ("from", "John Doe <john.doe@gmail.com>il.com>", "#77"),
    ("from", "John Smith john.smith@domain.com>", "#77"),
    ("from", "John Smith ><john.smith@domain.com>", "#77"),
    ("from", "John Smith <john.smith<john.smith@domain.com>", "#77"),
    ("from", "DigitalPebble <juPebble <julien@digitalpebble.com>", "#77"),
    ("from", "Nowhere nowhere@nowhere.com>", "#77"),
    ("from", "Nowhere <nowhere@nowhere.<nowhere@nowhere.com>", "#77"),
    ("from", "Joe SixPack <joixPack <joe@football.example.com>", "#77"),
    ("from", "=?utf-8?Q?Philipp_Ste<inkr=C3=BCger?= <philipp.steinkrueger@uni-koeln.de>", "#77"),
    ("from", "=?utf-8?Q?Philipp_Steinkr=C3=BCger?= <philipp.steininkr=C3=BCger?= <philipp.steinkrueger@uni-koeln.de>", "#77"),
    ("from", "Juha Haaga <juha.haaga@gmail.com>>", "#77"),
    ("from", "=?ISO-8859-1?Q?Keld_J=F8rn_Simonsen?= <keld@eld_J=F8rn_Simonsen?= <keld@dkuug.dk>", "#77"),
    ("from", "Another Person <anr Person <another.person@another-example.com>", "#77"),
    ("from", "ada@ex@example.org", "#77"),
    ("from", "a@b\" <c@d>", "#77"),
    ("from", "9\"a@b\" <c@d>", "#77"),
    ("from", "\"a@b\" <c@d>\"a@b\" <c@d>", "#77"),
    ("from", "x <a@b> <c@d>c@d>", "#77"),
    ("from", "Novell GrouWise <$$$.$$$.$$$>", "#77"),
    ("from", "Novell Grou\\pWise <$$$.$$$.$$$>", "#77"),
    ("from", "Novell GroupWise <$$$.$$$.pWise <$$$.$$$.$$$>", "#77"),
    ("from", "NOVELL GROUPWISE <$$$.$$$.$$$>", "#77"),
    ("to", "\"=?utf-8?q?=3D=3Futf-8=3Fq=3Fx=3F=3D?=\" <r@e>", "#77"),
    ("to", "\"=?utf\"-8?q?Ren=C3=A9?=\" <r@e>", "#77"),
    ("to", "\"=?utf-8?q?=3D=3Futf-8=3Fq=3FxFq=3Fx=3F=3D?=\" <r@e>", "#77"),
    ("to", "\"=?UTF-8?Q?=3D=3FUTF-8=3FQ=3FX=3F=3D?=\" <R@E>", "#77"),
];

#[test]
fn reads_headers_as_mime4j_and_tika_do() {
    let mut reference = String::new();
    GzDecoder::new(REFERENCE)
        .read_to_string(&mut reference)
        .expect("the reference decompresses");
    let mut mismatches = Vec::new();
    let mut unported = Vec::new();
    let mut departed = Vec::new();
    let mut count = 0;
    for line in reference.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [kind, value, expected] = fields[..] else {
            panic!("a reference line has three fields: {line}");
        };
        count += 1;
        let value = text(value);
        let expected = (expected != "null").then(|| text(expected));
        if let Some(departure) = DEPARTURES
            .iter()
            .find(|(departed, input, _)| *departed == kind && *input == value)
        {
            departed.push(departure);
            continue;
        }
        match answer(kind, &value) {
            Ok(actual) if actual == expected => {}
            Ok(actual) => mismatches.push(format!(
                "{kind} {value:?}: expected {expected:?}, got {actual:?}"
            )),
            Err(reason) => unported.push(format!("{kind} {value:?}: {reason:?}")),
        }
    }
    assert!(count > 600, "the reference has {count} values");
    assert_eq!(
        departed.len(),
        DEPARTURES.len(),
        "departures not in the reference"
    );
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
        (16, 4),
        "values the port leaves out: dates naming a time zone, and malformed ISO-2022-JP:\n{}",
        unported.join("\n")
    );
}
