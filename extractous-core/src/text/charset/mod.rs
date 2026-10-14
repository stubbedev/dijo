//! A port of Apache Tika 4.1's default charset detection (`DefaultEncodingDetector`, as
//! `AutoDetectReader` uses it): an HTML page's meta declaration, a byte order mark and a charset
//! the content type names each report a declared charset, Mojibuster reports what the bytes prove
//! or most likely are, and when the reports disagree the JunkFilter decodes the probe in each and
//! keeps the one that reads most like text; when it does not, the first report stands. Tika runs
//! its detectors in the order of their jars on the class path; the order here is the extractous
//! native image's: the HTML declaration, Mojibuster, the byte order mark, the content type. The
//! answers match Tika's own, which `tests/fixtures/charset/encoding-reference.tsv.gz` holds.

mod html;
mod jdk;
mod junk_filter;
mod models;
mod mojibuster;
mod probe;
mod strip;
mod structural;

pub use jdk::Charset;

/// How a detector knows a charset, in the order Mojibuster trusts them: a proof from the
/// bytes' structure, a declaration, or a statistical guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Declarative,
    Structural,
    Statistical,
}

impl Kind {
    fn rank(self) -> u8 {
        match self {
            Kind::Structural => 0,
            Kind::Declarative => 1,
            Kind::Statistical => 2,
        }
    }
}

/// A charset a detector reports, with its confidence from 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Candidate {
    charset: Charset,
    confidence: f32,
    kind: Kind,
}

impl Candidate {
    fn new(charset: Charset, confidence: f32, kind: Kind) -> Self {
        Candidate {
            charset,
            confidence: confidence.clamp(0.0, 1.0),
            kind,
        }
    }
}

/// A detector that took part in a detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detector {
    ByteOrderMark,
    ContentType,
    Html,
    Mojibuster,
    JunkFilter,
}

impl Detector {
    /// The name of the Tika class it ports, which Tika reports as the encoding detector.
    pub fn tika_name(self) -> &'static str {
        match self {
            Detector::ByteOrderMark => "BOMDetector",
            Detector::ContentType => "MetadataCharsetDetector",
            Detector::Html => "HtmlEncodingDetector",
            Detector::Mojibuster => "MojibusterEncodingDetector",
            Detector::JunkFilter => "JunkFilterEncodingDetector",
        }
    }
}

/// What one detector reported, best first.
#[derive(Debug)]
struct Report {
    detector: Detector,
    candidates: Vec<Candidate>,
}

/// The outcome of a detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detection {
    /// The charset detected.
    pub charset: Charset,
    /// The charset to decode with: the detected one, or the vendor superset of an East Asian
    /// one, which also decodes the vendor's extensions.
    pub decode_as: Charset,
    /// The detector whose report won.
    pub detector: Detector,
}

/// Detects the charset of text in `input`, as Tika does when told its content type.
pub fn detect(input: &[u8], content_type: Option<&str>) -> Detection {
    let declared = |charset| vec![Candidate::new(charset, 1.0, Kind::Declarative)];
    let probe = probe::probe(input, probe::CONTENT_TARGET, probe::RAW_CAP);
    let mut reports = Vec::with_capacity(4);
    if let Some(charset) = html::detect(input) {
        reports.push(Report {
            detector: Detector::Html,
            candidates: declared(charset),
        });
    }
    reports.push(Report {
        detector: Detector::Mojibuster,
        candidates: mojibuster::detect(probe, content_type),
    });
    if let Some(charset) = byte_order_mark(input) {
        reports.push(Report {
            detector: Detector::ByteOrderMark,
            candidates: declared(charset),
        });
    }
    if let Some(charset) = content_type.and_then(content_type_charset) {
        reports.push(Report {
            detector: Detector::ContentType,
            candidates: declared(charset),
        });
    }
    let (charset, detector) = match junk_filter::arbitrate(&reports, probe) {
        Some(winner) => (
            winner,
            reports
                .iter()
                .find(|report| report.candidates[0].charset == winner)
                .map_or(Detector::JunkFilter, |report| report.detector),
        ),
        None => (reports[0].candidates[0].charset, reports[0].detector),
    };
    Detection {
        charset,
        decode_as: mojibuster::superset(charset),
        detector,
    }
}

/// The Unicode charset a byte order mark starts the input with.
fn byte_order_mark(input: &[u8]) -> Option<Charset> {
    let name = match input {
        [0xEF, 0xBB, 0xBF, ..] => "UTF-8",
        [0x00, 0x00, 0xFE, 0xFF, ..] => "UTF-32BE",
        [0xFF, 0xFE, 0x00, 0x00, ..] => "UTF-32LE",
        [0xFE, 0xFF, ..] => "UTF-16BE",
        [0xFF, 0xFE, ..] => "UTF-16LE",
        _ => return None,
    };
    Some(Charset::named(name))
}

/// The charset a content type's `charset` parameter names, as Tika's
/// `MetadataCharsetDetector` takes it: ISO-8859-1 and US-ASCII as windows-1252, which WHATWG
/// reads them as.
fn content_type_charset(content_type: &str) -> Option<Charset> {
    let label = content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"').trim())
    })?;
    let charset = Charset::for_name(label)?;
    Some(match charset.name() {
        "ISO-8859-1" | "US-ASCII" => Charset::named("windows-1252"),
        _ => charset,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use flate2::read::GzDecoder;

    use super::*;

    const REFERENCE: &[u8] =
        include_bytes!("../../../tests/fixtures/charset/encoding-reference.tsv.gz");

    fn base64(text: &str) -> Vec<u8> {
        let value = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("not base64: {c}"),
        };
        let digits: Vec<u8> = text.bytes().filter(|&c| c != b'=').map(value).collect();
        digits
            .chunks(4)
            .flat_map(|chunk| {
                let bits = chunk
                    .iter()
                    .enumerate()
                    .fold(0u32, |bits, (i, &d)| bits | (u32::from(d) << (18 - 6 * i)));
                let bytes = bits.to_be_bytes();
                bytes[1..chunk.len()].to_vec()
            })
            .collect()
    }

    fn candidates(input: &[u8], content_type: &str) -> String {
        let probe = probe::probe(input, probe::CONTENT_TARGET, probe::RAW_CAP);
        mojibuster::detect(probe, Some(content_type))
            .iter()
            .map(|candidate| candidate.charset.name())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn detects_as_tika_does() {
        let mut reference = String::new();
        GzDecoder::new(REFERENCE)
            .read_to_string(&mut reference)
            .expect("the reference decompresses");
        let mut count = 0;
        let mut mojibuster_mismatches = Vec::new();
        let mut detection_mismatches = Vec::new();
        for line in reference.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            let input = base64(fields[0]);
            let content_type = fields[1];
            count += 1;
            let expected_candidates: Vec<&str> = fields[5]
                .split(' ')
                .filter(|field| !field.is_empty())
                .map(|field| field.split('@').next().unwrap())
                .collect();
            let actual_candidates = candidates(&input, content_type);
            if actual_candidates != expected_candidates.join(" ") {
                mojibuster_mismatches.push(format!(
                    "{content_type} {:?}: Tika [{}], port [{actual_candidates}]",
                    String::from_utf8_lossy(&input[..input.len().min(60)]),
                    fields[5]
                ));
            }
            let detection = detect(&input, Some(content_type));
            let actual = (
                detection.decode_as.name(),
                detection.charset.name(),
                detection.detector.tika_name(),
            );
            let expected = (fields[2], fields[3], fields[4]);
            if actual != expected {
                detection_mismatches.push(format!(
                    "{content_type} {:?}: Tika {expected:?}, port {actual:?}",
                    String::from_utf8_lossy(&input[..input.len().min(60)])
                ));
            }
        }
        assert!(count > 5_000, "the reference has {count} inputs");
        assert!(
            mojibuster_mismatches.is_empty() && detection_mismatches.is_empty(),
            "Mojibuster differs on {} and the detection on {} of {count} inputs:\n{}\n---\n{}",
            mojibuster_mismatches.len(),
            detection_mismatches.len(),
            mojibuster_mismatches
                .iter()
                .take(15)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
            detection_mismatches
                .iter()
                .take(15)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
