//! Native parsers against Tika's own answers: the references the generators in
//! `tests/fixtures/formats/` write (`ParserReference.java` describes their form). Each input is
//! parsed with the content type Tika's parser was given, through `Xhtml`, and the body's events
//! are recorded as the generators record them.

use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::GzDecoder;

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{ContentHandler, Stop, Xhtml};
use crate::formats::Parser;
use crate::text::charset::tests::base64;

/// The columns every reference line starts with.
const COLUMNS: usize = 9;
const RESOURCE_NAME: &str = "tk:resource-name";

/// One input of a reference and what Tika made of it.
pub(crate) struct Case {
    pub(crate) label: String,
    pub(crate) input: Vec<u8>,
    pub(crate) content_type: String,
    /// Whether Tika parsed the input through AutoDetectParser from a file named as the label.
    pub(crate) detected: bool,
    pub(crate) metadata: BTreeMap<String, Vec<String>>,
    events: String,
    counts: String,
    /// The columns a generator adds after the common ones.
    pub(crate) extra: Vec<String>,
}

impl Case {
    /// The file name Tika was told, for inputs parsed from files.
    fn resource_name(&self) -> Option<&str> {
        self.detected
            .then(|| self.label.rsplit('/').next().unwrap_or(&self.label))
    }
}

/// The cases of a gzipped reference.
pub(crate) fn cases(reference: &[u8]) -> Vec<Case> {
    let mut text = String::new();
    GzDecoder::new(reference)
        .read_to_string(&mut text)
        .expect("the reference decompresses");
    text.lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert!(
                fields.len() >= COLUMNS,
                "a reference line has nine fields: {line}"
            );
            assert_eq!(fields[4], "ok", "Tika parsed {}", fields[0]);
            let detected = match fields[3] {
                "auto" => true,
                "direct" => false,
                other => panic!("{} was parsed {other}", fields[0]),
            };
            let mut metadata: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for entry in fields[5].split('\u{1F}').filter(|entry| !entry.is_empty()) {
                let (name, value) = entry
                    .split_once('=')
                    .unwrap_or_else(|| panic!("metadata entry {entry}"));
                metadata
                    .entry(name.to_string())
                    .or_default()
                    .push(value.to_string());
            }
            Case {
                label: fields[0].to_string(),
                input: base64(fields[1]),
                content_type: fields[2].to_string(),
                detected,
                metadata,
                events: fields[6].to_string(),
                counts: fields[7].to_string(),
                extra: fields[COLUMNS..]
                    .iter()
                    .map(|field| field.to_string())
                    .collect(),
            }
        })
        .collect()
}

/// How `parser` differs from Tika on a case: its metadata, or its body's events.
pub(crate) fn difference(parser: &dyn Parser, case: &Case) -> Option<String> {
    let mut recorder = Recorder::default();
    let mut given = TikaMetadata::new();
    if let Some(name) = case.resource_name() {
        given.set(RESOURCE_NAME, name);
    }
    given.set("Content-Type", case.content_type.clone());
    let configs = ParserConfigs::default();
    let prepared = match parser.prepare(&case.input, &given, &configs) {
        Ok(prepared) => prepared,
        Err(reason) => return Some(format!("{}: deferred: {reason}", case.label)),
    };
    let (metadata, _, result) = Xhtml::write(&mut recorder, given, &configs, prepared);
    if let Err(failure) = result {
        return Some(format!("{}: failed: {failure:?}", case.label));
    }
    let metadata: BTreeMap<String, Vec<String>> = metadata
        .names()
        .filter(|&name| name != RESOURCE_NAME)
        .map(|name| (name.to_string(), metadata.values(name).to_vec()))
        .collect();
    if metadata != case.metadata {
        return Some(format!(
            "{}: metadata\n  Tika {:?}\n  port {metadata:?}",
            case.label, case.metadata
        ));
    }
    let (events, counts) = recorder.summary();
    ((&events, &counts) != (&case.events, &case.counts)).then(|| {
        format!(
            "{}: events {} {}, port {events} {counts}",
            case.label, case.events, case.counts
        )
    })
}

/// A text's CRC-32 and UTF-8 length, as the generators write them.
pub(crate) fn digest(text: &str) -> String {
    let mut crc = flate2::Crc::new();
    crc.update(text.as_bytes());
    format!("{:08x}:{}", crc.sum(), text.len())
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

    fn summary(mut self) -> (String, String) {
        self.flush();
        let counts = self
            .counts
            .iter()
            .map(|(name, count)| format!("{name}:{count}"))
            .collect::<Vec<_>>()
            .join(" ");
        (
            digest(&self.events),
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

    fn start_prefix_mapping(&mut self, _prefix: &str, _uri: &str) -> Result<(), Stop> {
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
