//! Subtitles (SubRip, WebVTT, SubStation Alpha), as our Java `SubtitleParser` reads them: the
//! text read through an [`AutoDetectReader`], one paragraph per cue, without cue numbers,
//! timings, cue settings, styles, notes or the markup inside cues. The parser sets the
//! `AutoDetectReader`'s metadata and `Content-Encoding` (the decoding charset's name, which
//! Tika's `CharsetUtils.clean` leaves as it is for every name the JDK gives), and leaves
//! `Content-Type` as detected.

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, lines, AutoDetectReader};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};
use crate::java::is_blank;
use crate::java::strip;
use crate::mime::MediaType;

const SUBSTATION_DIALOGUE: &str = "Dialogue:";
const SUBSTATION_TEXT_FIELD: usize = 9;

pub(crate) struct SubtitleParser;

impl Parser for SubtitleParser {
    fn name(&self) -> &'static str {
        "extractous::SubtitleParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "ai.yobix.SubtitleParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-subrip", "text/vtt", "text/x-ssa"]
    }

    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<String> {
        AutoDetectReader::defers(input).map(str::to_owned)
    }

    fn parse(
        &self,
        input: &[u8],
        _configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let content_type = xhtml
            .metadata_mut()
            .get("Content-Type")
            .ok_or_else(|| Failure::Malformed("the document has no Content-Type".into()))?
            .to_owned();
        let substation = MediaType::parse(&content_type)
            .is_some_and(|media_type| media_type.base_type() == MediaType::named("text/x-ssa"));
        let reader = AutoDetectReader::read(input, Some(&content_type))?;
        let metadata = xhtml.metadata_mut();
        auto_detect_reader::set_metadata(&reader.detection, metadata);
        metadata.set("Content-Encoding", reader.detection.decode_as.name());
        let cues = if substation {
            substation_cues(&reader.text)
        } else {
            timed_cues(&reader.text)
        };
        for cue in cues {
            xhtml.element("p", &cue)?;
        }
        Ok(())
    }
}

/// The text of SubRip and WebVTT cues: the lines after each timing line, up to a blank line.
/// WebVTT headers, notes and style blocks have no timing line, so they are left out.
fn timed_cues(text: &str) -> Vec<String> {
    let mut cues = Vec::new();
    let mut cue = String::new();
    let mut in_cue = false;
    for line in lines(text) {
        if is_timing(line) {
            in_cue = true;
            continue;
        }
        if is_blank(line) {
            add_cue(&mut cues, &mut cue);
            in_cue = false;
            continue;
        }
        if !in_cue {
            continue;
        }
        if !cue.is_empty() {
            cue.push('\n');
        }
        cue.push_str(&clean(line));
    }
    add_cue(&mut cues, &mut cue);
    cues
}

/// The text field of each Dialogue line of a SubStation Alpha script, `\N` and `\n` as line
/// breaks.
fn substation_cues(text: &str) -> Vec<String> {
    let mut cues = Vec::new();
    for line in lines(text) {
        let Some(fields) = line.strip_prefix(SUBSTATION_DIALOGUE) else {
            continue;
        };
        let Some(field) = fields
            .splitn(SUBSTATION_TEXT_FIELD + 1, ',')
            .nth(SUBSTATION_TEXT_FIELD)
        else {
            continue;
        };
        let mut cue = clean(&escaped_line_breaks(field));
        add_cue(&mut cues, &mut cue);
    }
    cues
}

fn add_cue(cues: &mut Vec<String>, cue: &mut String) {
    let text = strip(cue);
    if !text.is_empty() {
        cues.push(text.to_owned());
    }
    cue.clear();
}

/// A line without its markup, `<…>` tags and `{\…}` override blocks, and stripped.
fn clean(line: &str) -> String {
    let mut cleaned = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        let close = match c {
            '<' => rest.find('>'),
            '{' if rest[1..].starts_with('\\') => rest[2..].find('}').map(|end| end + 2),
            _ => None,
        };
        match close {
            Some(end) => rest = &rest[end + 1..],
            None => {
                cleaned.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    strip(&cleaned).to_owned()
}

/// The text with each `\N` or `\n` as a line break.
fn escaped_line_breaks(text: &str) -> String {
    let mut replaced = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && matches!(chars.peek(), Some('N' | 'n')) {
            chars.next();
            replaced.push('\n');
        } else {
            replaced.push(c);
        }
    }
    replaced
}

/// Java's `\s`.
fn is_java_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{B}' | '\u{C}' | '\r')
}

/// Whether a line is a cue timing, as
/// `^\s*(\d+:)?\d{1,2}:\d{2}[.,]\d{1,3}\s*-->\s*(\d+:)?\d{1,2}:\d{2}[.,]\d{1,3}.*$` matches it
/// whole: Java's `.` takes no line terminator, which within a line leaves U+0085, U+2028 and
/// U+2029.
fn is_timing(line: &str) -> bool {
    let start = line.trim_start_matches(is_java_space);
    timestamp_ends(start).into_iter().flatten().any(|rest| {
        let Some(rest) = rest.trim_start_matches(is_java_space).strip_prefix("-->") else {
            return false;
        };
        let rest = rest.trim_start_matches(is_java_space);
        timestamp_ends(rest)
            .into_iter()
            .flatten()
            .any(|tail| !tail.contains(['\u{85}', '\u{2028}', '\u{2029}']))
    })
}

/// What follows a timestamp `(\d+:)?\d{1,2}:\d{2}[.,]\d{1,3}` at the start of `text`, with the
/// hours and without, in the order the regular expression tries them.
fn timestamp_ends(text: &str) -> [Option<&str>; 2] {
    let with_hours = digits(text)
        .filter(|&count| count > 0)
        .and_then(|count| text[count..].strip_prefix(':'))
        .and_then(minutes_seconds);
    [with_hours, minutes_seconds(text)]
}

/// What follows `\d{1,2}:\d{2}[.,]\d{1,3}` at the start of `text`, the last digits taken
/// greedily.
fn minutes_seconds(text: &str) -> Option<&str> {
    let minutes = digits(text)?;
    if !(1..=2).contains(&minutes) {
        return None;
    }
    let rest = text[minutes..].strip_prefix(':')?;
    if digits(rest)? < 2 {
        return None;
    }
    let rest = rest[2..].strip_prefix(['.', ','])?;
    let fraction = digits(rest)?.min(3);
    (fraction > 0).then(|| &rest[fraction..])
}

/// The ASCII digits `text` starts with, as Java's `\d` counts them.
fn digits(text: &str) -> Option<usize> {
    Some(text.bytes().take_while(u8::is_ascii_digit).count())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents};

    const TIKA_NAME: &str = "ai.yobix.SubtitleParser";

    #[test]
    fn extracts_what_tikas_path_extracts() {
        let (parsed, deferred) = documents(
            &SubtitleParser,
            &[
                Path::new("../test_files/documents"),
                Path::new("../quality/corpus"),
            ],
        );
        assert!(deferred.is_empty(), "deferred: {deferred:?}");
        let names: Vec<_> = parsed.iter().filter_map(|path| path.file_name()).collect();
        assert_eq!(names, ["subtitles.ass", "subtitles.srt", "subtitles.vtt"]);
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_what_tikas_path_extracts_from_tikas_test_documents() {
        let (parsed, deferred) = documents(&SubtitleParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "deferred: {deferred:?}");
        assert_eq!(parsed.len(), 5, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    fn recognises_timings_as_java_does() {
        for timing in [
            "00:00:01,000 --> 00:00:04,000",
            "  00:01.5-->00:02.25 align:start",
            "1:02:03.4 --> 1:02:05.123456",
            "12:34:56.789 --> 99:59.9\u{a0}x",
        ] {
            assert!(is_timing(timing), "{timing:?}");
        }
        for line in [
            "00:00:01 --> 00:00:04",
            "0:00:01,1234 --> 0:00:02,0",
            "123:4.5 --> 00:01.0",
            "00:01.0 -> 00:02.0",
            "00:01.0 --> 00:02.0\u{2028}",
            "\u{a0}00:01.0 --> 00:02.0",
            "１0:01.0 --> 00:02.0",
        ] {
            assert!(!is_timing(line), "{line:?}");
        }
    }

    #[test]
    fn reads_cues_without_markup() {
        let srt = "1\r\n00:00:01,000 --> 00:00:02,000\r\n<i>Hello</i> {\\an8}there\r\n  second line \r\n\r\n2\n00:00:03,000 --> 00:00:04,000\n<b>\n\n";
        assert_eq!(timed_cues(srt), ["Hello there\nsecond line"]);
        let vtt = "WEBVTT\n\nNOTE a note\n\n00:01.000 --> 00:02.000\n<v A>a < b\n";
        assert_eq!(timed_cues(vtt), ["a < b"]);
        let ass = "Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\i1}One,{\\i0}\\Ntwo\\n\nDialogue: too,few,fields\nComment: 0,0,0,0,0,0,0,0,0,no\n";
        assert_eq!(substation_cues(ass), ["One,\ntwo"]);
    }
}
