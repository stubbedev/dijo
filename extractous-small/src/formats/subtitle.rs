//! Subtitles (SubRip, WebVTT, SubStation Alpha), as our Java `SubtitleParser` reads them: the
//! text read through an [`AutoDetectReader`], one paragraph per cue, without cue numbers,
//! timings, cue settings, styles, notes or the markup inside cues. The parser sets the
//! `AutoDetectReader`'s metadata and `Content-Encoding` (the decoding charset's name, which
//! Tika's `CharsetUtils.clean` leaves as it is for every name the JDK gives), and leaves
//! `Content-Type` as detected.

use crate::config::ParserConfigs;
use crate::formats::auto_detect_reader::{self, AutoDetectReader};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{unless_different, Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::java::is_blank;
use crate::java::strip;
use crate::mime::MediaType;

const SUBSTATION_DIALOGUE: &str = "Dialogue:";
const SUBSTATION_TEXT_FIELD: usize = 9;

pub struct SubtitleParser;

impl Parser for SubtitleParser {
    fn name(&self) -> &'static str {
        "extractous::SubtitleParser"
    }
    fn tika_name(&self) -> &'static str {
        "ai.yobix.SubtitleParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-subrip", "text/vtt", "text/x-ssa"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        unless_different(
            Box::new(move |xhtml| write(input, pages, xhtml)),
            AutoDetectReader::difference(input, metadata.get("Content-Type")),
        )
    }
}

/// Writes the cues of `input`, decoded as Tika's `AutoDetectReader` decodes it, each as its
/// lines are read, the `pages` of `input` released as they are.
fn write<'a>(input: &'a [u8], pages: Pages<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let content_type = xhtml
        .metadata_mut()
        .get("Content-Type")
        .ok_or_else(|| Failure::Malformed("the document has no Content-Type".into()))?
        .to_owned();
    let substation = MediaType::parse(&content_type)
        .is_some_and(|media_type| media_type.base_type() == MediaType::named("text/x-ssa"));
    let (detection, text) = AutoDetectReader::stream(input, Some(&content_type), pages);
    let metadata = xhtml.metadata_mut();
    auto_detect_reader::set_metadata(&detection, metadata);
    metadata.set("Content-Encoding", detection.decode_as.name());
    let mut paragraph = |cue: &str| xhtml.element("p", cue);
    if substation {
        substation_cues(text.lines(), &mut paragraph)?;
    } else {
        timed_cues(text.lines(), &mut paragraph)?;
    }
    Ok(())
}

/// The text of SubRip and WebVTT cues: the lines after each timing line, up to a blank line.
/// WebVTT headers, notes and style blocks have no timing line, so they are left out. Each cue
/// goes to `each` as it ends.
fn timed_cues(
    lines: impl IntoIterator<Item = impl AsRef<str>>,
    each: &mut impl FnMut(&str) -> Result<(), Stop>,
) -> Result<(), Stop> {
    let mut cue = String::new();
    let mut in_cue = false;
    for line in lines {
        let line = line.as_ref();
        if is_timing(line) {
            in_cue = true;
            continue;
        }
        if is_blank(line) {
            end_cue(&mut cue, each)?;
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
    end_cue(&mut cue, each)
}

/// The text field of each Dialogue line of a SubStation Alpha script, `\N` and `\n` as line
/// breaks. Each cue goes to `each` as it is read.
fn substation_cues(
    lines: impl IntoIterator<Item = impl AsRef<str>>,
    each: &mut impl FnMut(&str) -> Result<(), Stop>,
) -> Result<(), Stop> {
    for line in lines {
        let Some(fields) = line.as_ref().strip_prefix(SUBSTATION_DIALOGUE) else {
            continue;
        };
        let Some(field) = fields
            .splitn(SUBSTATION_TEXT_FIELD + 1, ',')
            .nth(SUBSTATION_TEXT_FIELD)
        else {
            continue;
        };
        let mut cue = clean(&escaped_line_breaks(field));
        end_cue(&mut cue, each)?;
    }
    Ok(())
}

/// Hands `cue`, stripped, to `each` unless it is empty, and clears it.
fn end_cue(cue: &mut String, each: &mut impl FnMut(&str) -> Result<(), Stop>) -> Result<(), Stop> {
    let text = strip(cue);
    if !text.is_empty() {
        each(text)?;
    }
    cue.clear();
    Ok(())
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

    use super::*;

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

    /// The cues `read` hands on of the lines of `text`.
    fn cues(
        text: &str,
        read: impl FnOnce(
            auto_detect_reader::Lines<'_>,
            &mut dyn FnMut(&str) -> Result<(), Stop>,
        ) -> Result<(), Stop>,
    ) -> Vec<String> {
        let mut cues = Vec::new();
        let (_, streamed) =
            AutoDetectReader::stream(text.as_bytes(), Some("text/plain"), Pages::IN_MEMORY);
        read(streamed.lines(), &mut |cue| {
            cues.push(cue.to_owned());
            Ok(())
        })
        .unwrap();
        cues
    }

    fn timed_cues(text: &str) -> Vec<String> {
        cues(text, |lines, mut each| super::timed_cues(lines, &mut each))
    }

    fn substation_cues(text: &str) -> Vec<String> {
        cues(text, |lines, mut each| {
            super::substation_cues(lines, &mut each)
        })
    }

    /// A subtitle file too large to read whole is read a line at a time, each cue written as it
    /// ends.
    #[test]
    fn reads_subtitles_too_large_to_read_whole() {
        let cue = |index: u64| {
            format!("{index}\n00:00:01,000 --> 00:00:02,000\nReplik {index}\n\n").into_bytes()
        };
        let count = extractous_core::large::bodies_past_limit(cue(0).len());
        let file = extractous_core::large::LargeFile::new("long.srt", b"", count, cue, b"");
        let mut cues = 0;
        file.extract_lines(SubtitleParser.name(), |line| {
            if !line.is_empty() {
                assert_eq!(line, format!("Replik {cues}"));
                cues += 1;
            }
        });
        assert_eq!(cues, count);
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
