//! The parser against Tika's own answers: `tests/fixtures/text/text-reference.tsv.gz`, written by
//! `just text-reference` (`TextReference.java` there describes its form). Each input is parsed
//! with the content type Tika's parser was given, through `Xhtml`, and the body's events are
//! recorded as the generator records them.

use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::GzDecoder;

use super::TextAndCsvParser;
use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{ContentHandler, Stop, Xhtml};
use crate::formats::{Failure, Parser};
use crate::text::charset::tests::base64;

const REFERENCE: &[u8] = include_bytes!("../../../tests/fixtures/text/text-reference.tsv.gz");

/// One input of the reference and what Tika made of it.
struct Case {
    label: String,
    input: Vec<u8>,
    content_type: String,
    metadata: BTreeMap<String, String>,
    events: String,
    counts: String,
}

fn cases() -> Vec<Case> {
    let mut reference = String::new();
    GzDecoder::new(REFERENCE)
        .read_to_string(&mut reference)
        .expect("the text reference decompresses");
    reference
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 9, "a reference line has nine fields: {line}");
            assert_eq!(fields[4], "ok", "Tika parsed {}", fields[0]);
            assert!(
                matches!(fields[3], "auto" | "direct"),
                "{} was parsed {}",
                fields[0],
                fields[3]
            );
            Case {
                label: fields[0].to_string(),
                input: base64(fields[1]),
                content_type: fields[2].to_string(),
                metadata: fields[5]
                    .split('\u{1F}')
                    .map(|entry| {
                        let (name, value) = entry
                            .split_once('=')
                            .unwrap_or_else(|| panic!("metadata entry {entry}"));
                        (name.to_string(), value.to_string())
                    })
                    .collect(),
                events: fields[6].to_string(),
                counts: fields[7].to_string(),
            }
        })
        .collect()
}

/// The body's events in the reference's canonical form, and how many of each element it holds.
#[derive(Default)]
struct Recorder {
    in_body: bool,
    events: String,
    text: String,
    counts: BTreeMap<String, usize>,
}

impl Recorder {
    fn flush(&mut self) {
        if !self.text.is_empty() {
            self.events
                .push_str(&format!("T {}\n{}", self.text.len(), self.text));
            self.text.clear();
        }
    }

    /// The CRC-32 and length of the events, and the element counts, as the reference writes them.
    fn summary(mut self) -> (String, String) {
        self.flush();
        let mut crc = flate2::Crc::new();
        crc.update(self.events.as_bytes());
        let counts = self
            .counts
            .iter()
            .map(|(name, count)| format!("{name}:{count}"))
            .collect::<Vec<_>>()
            .join(" ");
        (
            format!("{:08x}:{}", crc.sum(), self.events.len()),
            if counts.is_empty() {
                "-".into()
            } else {
                counts
            },
        )
    }
}

impl ContentHandler for Recorder {
    fn start_document(&mut self) -> Result<(), Stop> {
        Ok(())
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        if name == "body" {
            self.in_body = true;
            return Ok(());
        }
        if !self.in_body {
            return Ok(());
        }
        self.flush();
        self.events.push_str(&format!("S {name}"));
        for (attribute, value) in attributes {
            self.events.push_str(&format!(" {attribute}={value}"));
        }
        self.events.push('\n');
        *self.counts.entry(name.to_string()).or_insert(0) += 1;
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        if !self.in_body {
            return Ok(());
        }
        self.flush();
        if name == "body" {
            self.in_body = false;
        } else {
            self.events.push_str(&format!("E {name}\n"));
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        if self.in_body {
            self.text.push_str(text);
        }
        Ok(())
    }

    fn ignorable_whitespace(&mut self, _text: &str) -> Result<(), Stop> {
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        Ok(())
    }
}

#[test]
fn parses_as_tika_does() {
    let cases = cases();
    let mut mismatches = Vec::new();
    for case in &cases {
        let mut recorder = Recorder::default();
        let mut given = TikaMetadata::new();
        given.set("Content-Type", case.content_type.clone());
        let configs = ParserConfigs::default();
        let (metadata, _, result) = Xhtml::write(&mut recorder, given, &configs, |xhtml| {
            TextAndCsvParser.parse(&case.input, &configs, xhtml)
        });
        result.unwrap_or_else(|e: Failure| panic!("{}: {e:?}", case.label));
        let metadata: BTreeMap<String, String> = metadata
            .names()
            .map(|name| (name.to_string(), metadata.values(name).join("|")))
            .collect();
        if metadata != case.metadata {
            mismatches.push(format!(
                "{}: metadata\n  Tika {:?}\n  port {metadata:?}",
                case.label, case.metadata
            ));
        }
        let (events, counts) = recorder.summary();
        if (&events, &counts) != (&case.events, &case.counts) {
            mismatches.push(format!(
                "{}: events {} {}, port {events} {counts}",
                case.label, case.events, case.counts
            ));
        }
    }
    assert!(
        cases.len() > 200,
        "the reference has {} inputs",
        cases.len()
    );
    assert!(
        mismatches.is_empty(),
        "{} of {} inputs differ:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n")
    );
}
