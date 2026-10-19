//! A port of Apache Tika 4.1's default charset detection (`DefaultEncodingDetector`, as
//! `AutoDetectReader` uses it): an HTML page's meta declaration, a byte order mark and a charset
//! the content type names each report a declared charset, Mojibuster reports what the bytes prove
//! or most likely are, and when the reports disagree the JunkFilter decodes the probe in each and
//! keeps the one that reads most like text; when it does not, the first report stands. Tika runs
//! its detectors in the order of their jars on the class path; the order here is fixed: the
//! HTML declaration, Mojibuster, the byte order mark, the content type. The
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
pub use jdk::Pieces;
use junk_filter::Arbitration;

/// How a detector knows a charset, in the order Mojibuster trusts them: a proof from the
/// bytes' structure, a declaration, or a statistical guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Declarative,
    Structural,
    Statistical,
}

impl Kind {
    /// The name of Tika's `EncodingResult.ResultType`.
    fn tika_name(self) -> &'static str {
        match self {
            Kind::Declarative => "DECLARATIVE",
            Kind::Structural => "STRUCTURAL",
            Kind::Statistical => "STATISTICAL",
        }
    }

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
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// The charset detected.
    pub charset: Charset,
    /// The charset to decode with: the detected one, or the vendor superset of an East Asian
    /// one, which also decodes the vendor's extensions.
    pub decode_as: Charset,
    /// The detector whose report won.
    pub detector: Detector,
    /// The confidence of the first result Tika's detection returns: the JunkFilter's winner
    /// with the highest confidence any detector reported it with
    /// (`EncodingDetectorContext.getTopConfidenceFor`), else the first report's best.
    pub confidence: f32,
    /// How the detection went, as Tika traces it.
    pub trace: Trace,
}

/// What each detector that reported anything reported first, in the order they ran, and how
/// the JunkFilter's arbitration among them ended: Tika's `tk:encoding-detection-trace`.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    reports: Vec<(Detector, Candidate)>,
    arbitration: Arbitration,
}

/// Tika's form of the trace, as `CompositeEncodingDetector.buildTrace` writes it: each report
/// as `detector->charset[KIND]`, a statistical one followed by its confidence to two decimals,
/// separated by `, `, and the arbitration's note in brackets, e.g.
/// `MojibusterEncodingDetector->ISO-8859-16[STATISTICAL](1.00) [junk-filter-selected]`.
impl std::fmt::Display for Trace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, (detector, top)) in self.reports.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(
                f,
                "{}->{}[{}]",
                detector.tika_name(),
                top.charset.name(),
                top.kind.tika_name()
            )?;
            if top.kind == Kind::Statistical {
                write!(f, "({})", java_two_decimals(top.confidence))?;
            }
        }
        if let Some(note) = self.arbitration.note() {
            write!(f, " [{note}]")?;
        }
        Ok(())
    }
}

/// A confidence from 0 to 1 as Java's `String.format("%.2f", confidence)` writes it: the
/// shortest decimal digits that identify the value, rounded half up to two decimals.
fn java_two_decimals(confidence: f32) -> String {
    let shortest = f64::from(confidence).to_string();
    let (whole, fraction) = shortest.split_once('.').unwrap_or((&shortest, ""));
    let digit = |i: usize| {
        fraction
            .as_bytes()
            .get(i)
            .map_or(0, |&d| u32::from(d - b'0'))
    };
    let whole: u32 = whole
        .parse()
        .unwrap_or_else(|_| panic!("A confidence is between 0 and 1, not {shortest}"));
    let hundredths = whole * 100 + digit(0) * 10 + digit(1) + u32::from(digit(2) >= 5);
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}

/// Detects the charset of text in `input`, as Tika does when told its content type.
pub fn detect(input: &[u8], content_type: Option<&str>) -> Detection {
    detect_declared(input, content_type, None, Declarations::All)
}

/// Detects the charset of text in `input` as [`detect`] does, but passing over a declaration
/// [`undecodable_declaration`] finds, which Tika's detection weighs and may decode with: a
/// detection whose charset the port always decodes.
pub fn detect_decodable(input: &[u8], content_type: Option<&str>) -> Detection {
    detect_declared(input, content_type, None, Declarations::Decodable)
}

/// Detects the charset of text in `input` as Tika does without a content type but with a
/// content-type hint (`tk:content-type-hint`), a source's claim about the bytes, whose charset
/// `MetadataCharsetDetector` declares.
pub fn detect_hinted(input: &[u8], hint: &str) -> Detection {
    detect_declared(input, None, Some(hint), Declarations::All)
}

/// The declarations of an HTML page and a content type a detection weighs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declarations {
    All,
    /// Only those naming a charset the port decodes.
    Decodable,
}

impl Declarations {
    fn keeps(self, charset: Charset) -> bool {
        match self {
            Declarations::All => true,
            Declarations::Decodable => decodes(charset),
        }
    }
}

/// Whether the port decodes text detected as `charset`, with its vendor superset.
fn decodes(charset: Charset) -> bool {
    mojibuster::superset(charset).decodes()
}

/// The detection, `MetadataCharsetDetector` declaring the charset of the content type, else of
/// the hint.
fn detect_declared(
    input: &[u8],
    content_type: Option<&str>,
    hint: Option<&str>,
    declarations: Declarations,
) -> Detection {
    let declared = |charset| vec![Candidate::new(charset, 1.0, Kind::Declarative)];
    let probe = probe::probe(input, probe::CONTENT_TARGET, probe::RAW_CAP);
    let mut reports = Vec::with_capacity(4);
    if let Some(charset) = html::detect(input).filter(|&charset| declarations.keeps(charset)) {
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
    let metadata_charset = content_type
        .and_then(content_type_charset)
        .or_else(|| hint.and_then(content_type_charset))
        .filter(|&charset| declarations.keeps(charset));
    if let Some(charset) = metadata_charset {
        reports.push(Report {
            detector: Detector::ContentType,
            candidates: declared(charset),
        });
    }
    let arbitration = junk_filter::arbitrate(&reports, probe);
    let (charset, detector, confidence) = match arbitration.winner() {
        Some(winner) => (
            winner,
            reports
                .iter()
                .find(|report| report.candidates[0].charset == winner)
                .map_or(Detector::JunkFilter, |report| report.detector),
            reports
                .iter()
                .flat_map(|report| &report.candidates)
                .filter(|candidate| candidate.charset == winner)
                .map(|candidate| candidate.confidence)
                .fold(0.0, f32::max),
        ),
        None => (
            reports[0].candidates[0].charset,
            reports[0].detector,
            reports[0].candidates[0].confidence,
        ),
    };
    Detection {
        charset,
        decode_as: mojibuster::superset(charset),
        detector,
        confidence,
        trace: Trace {
            reports: reports
                .iter()
                .map(|report| (report.detector, report.candidates[0]))
                .collect(),
            arbitration,
        },
    }
}

/// A charset that an HTML declaration in `input` or `content_type` names and that the port
/// does not decode, such as an IBM EBCDIC double-byte one, which leaves what Tika makes of the
/// input unknown. Every charset the other detectors name decodes.
pub fn undecodable_declaration(input: &[u8], content_type: Option<&str>) -> Option<Charset> {
    [
        html::detect(input),
        content_type.and_then(content_type_charset),
    ]
    .into_iter()
    .flatten()
    .find(|&charset| !decodes(charset))
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

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    #[cfg(test)]
    use std::io::Read;

    #[cfg(test)]
    use flate2::read::GzDecoder;

    #[cfg(test)]
    use super::*;

    #[cfg(test)]
    const REFERENCE: &[u8] =
        include_bytes!("../../../tests/fixtures/charset/encoding-reference.tsv.gz");

    /// The bytes of base64 text, as the reference generators write input.
    pub fn base64(text: &str) -> Vec<u8> {
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

    #[cfg(test)]
    fn candidates(input: &[u8], content_type: &str) -> Vec<(&'static str, f32, &'static str)> {
        let probe = probe::probe(input, probe::CONTENT_TARGET, probe::RAW_CAP);
        mojibuster::detect(probe, Some(content_type))
            .iter()
            .map(|candidate| {
                (
                    candidate.charset.name(),
                    candidate.confidence,
                    candidate.kind.tika_name(),
                )
            })
            .collect()
    }

    #[test]
    fn passes_over_declarations_it_does_not_decode() {
        let page = b"<meta charset=\"x-IBM930\"> plain text";
        assert_eq!(detect(page, None).charset.name(), "x-IBM930");
        assert_eq!(
            undecodable_declaration(page, None).map(Charset::name),
            Some("x-IBM930")
        );
        assert!(detect_decodable(page, None).decode_as.decodes());
        let text = b"plain text";
        let declared = Some("text/plain; charset=x-IBM933");
        assert_eq!(
            undecodable_declaration(text, declared).map(Charset::name),
            Some("x-IBM933")
        );
        assert!(detect_decodable(text, declared).decode_as.decodes());
        let korean = b"\x1b$)C\x0e\x30\x21\x0f plain text";
        assert_eq!(undecodable_declaration(korean, None), None);
        assert_eq!(detect_decodable(korean, None), detect(korean, None));
    }

    #[test]
    fn writes_confidences_as_java_does() {
        let cases = [
            (1.0, "1.00"),
            (0.1, "0.10"),
            (0.125, "0.13"),
            (0.375, "0.38"),
            (0.995, "1.00"),
            (0.994, "0.99"),
            (0.0, "0.00"),
            (0.7349, "0.73"),
        ];
        for (confidence, expected) in cases {
            assert_eq!(java_two_decimals(confidence), expected, "{confidence}");
        }
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
            let expected_candidates: Vec<(&str, f32, &str)> = fields[5]
                .split(' ')
                .filter(|field| !field.is_empty())
                .map(|field| {
                    let parts: Vec<&str> = field.split('@').collect();
                    let confidence = parts[1]
                        .parse()
                        .unwrap_or_else(|e| panic!("confidence {field}: {e}"));
                    (parts[0], confidence, parts[2])
                })
                .collect();
            let actual_candidates = candidates(&input, content_type);
            if actual_candidates != expected_candidates {
                mojibuster_mismatches.push(format!(
                    "{content_type} {:?}: Tika [{}], port {actual_candidates:?}",
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
