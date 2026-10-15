//! Binary property lists, as Tika's `PListParser` writes what dd-plist 1.30 reads of them
//! ([`binary`]): the object tree as `plist`, `dict`, `key`, `array`, `string`, `number`,
//! `data`, `date` and `uid` elements, the bytes of each data object also handed to the embedded
//! document extractor. XML and ASCII property lists, sets, keys that are not strings, numbers
//! or UIDs, trees that share objects so often that writing them out would not end, and dates
//! far from ours stay on Tika's path.

pub(crate) mod binary;

use std::collections::HashMap;
use std::rc::Rc;

use binary::{Key, Object, Value};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::data_format::date_format::SimpleDateFormat;
use crate::formats::embedded::Wrapping;
use crate::formats::java_time::TimeZone;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};

const MEDIA_TYPES: [&str; 4] = [
    "application/x-bplist",
    "application/x-bplist-itunes",
    "application/x-bplist-memgraph",
    "application/x-bplist-webarchive",
];
const MAGIC: &[u8] = b"bplist";
/// Milliseconds from the Java epoch to dd-plist's, 2001-01-01.
const EPOCH: i64 = 978_307_200_000;
const DATE_PATTERN: &str = "yyyy-MM-dd'T'HH:mm:ssZ";
/// The most objects the port writes, counting shared ones each time they are written.
const MAX_WRITTEN: u64 = 10_000_000;
/// The furthest from 1970 a date may lie for the port to write it, about 30,000 years.
const MAX_DATE_MILLIS: u64 = 1_000_000_000_000_000;

/// Why the port does not write a property list as Tika does: Tika's path is left to read it,
/// or dd-plist fails on it before the document starts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Problem {
    Unread(&'static str),
    Failed(String),
}

/// The dates of a list, written as `PListParser`'s `SimpleDateFormat` writes them in Java's
/// default time zone.
struct Dates {
    format: SimpleDateFormat,
    zone: &'static TimeZone,
}

impl Dates {
    fn new() -> Result<Self, Problem> {
        Ok(Self {
            format: SimpleDateFormat::new(DATE_PATTERN).expect("the date pattern compiles"),
            zone: TimeZone::java_default().map_err(|_| {
                Problem::Unread("the JVM's default time zone, which dates are written in")
            })?,
        })
    }

    /// `NSDate`'s `Date` of `seconds` since 2001, as the pattern writes it.
    fn text(&self, seconds: f64) -> Result<String, Problem> {
        let millis = EPOCH.wrapping_add((1000.0 * seconds) as i64);
        if millis.unsigned_abs() > MAX_DATE_MILLIS {
            return Err(Problem::Unread(
                "a date further from 1970 than the port writes",
            ));
        }
        self.format
            .format(millis, self.zone)
            .map_err(|_| Problem::Unread("a date the port does not format"))
    }
}

/// How many objects writing `value` writes, shared ones each time they are written, or why the
/// port does not write it.
fn check(
    value: &Value,
    dates: &Dates,
    counts: &mut HashMap<*const Object, u64>,
) -> Result<u64, Problem> {
    let Some(object) = value else {
        return Ok(1);
    };
    if let Some(&count) = counts.get(&Rc::as_ptr(object)) {
        return Ok(count);
    }
    let mut count: u64 = 1;
    let mut add = |more: u64| -> Result<(), Problem> {
        count = count.saturating_add(more);
        if count > MAX_WRITTEN {
            return Err(Problem::Unread("more shared objects than the port writes"));
        }
        Ok(())
    };
    match &**object {
        Object::Dict(dict) => {
            for (key, value) in dict.entries() {
                if matches!(key, Key::Object(_)) {
                    return Err(Problem::Unread(
                        "a key that is not a string, a number or a UID",
                    ));
                }
                add(check(&Some(value.clone()), dates, counts)?)?;
            }
        }
        Object::Array(items) => {
            for item in items {
                add(check(item, dates, counts)?)?;
            }
        }
        Object::Set(_) => return Err(Problem::Unread("a set")),
        Object::Date(seconds) => {
            dates.text(*seconds)?;
        }
        _ => {}
    }
    counts.insert(Rc::as_ptr(object), count);
    Ok(count)
}

/// The root of `input` and the writer of its dates, or why the port does not write it.
fn read(input: &[u8]) -> Result<(Value, Dates), Problem> {
    if !input.starts_with(MAGIC) {
        return Err(Problem::Unread("a property list that is not binary"));
    }
    let dates = Dates::new()?;
    let root = binary::parse(input).map_err(|thrown| match thrown {
        binary::Thrown::Format(reason) | binary::Thrown::Runtime(reason) => Problem::Failed(reason),
    })?;
    check(&root, &dates, &mut HashMap::new())?;
    Ok((root, dates))
}

/// `parseObject` of `PListParser`.
fn write(value: &Value, dates: &Dates, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let Some(object) = value else {
        return Err(Failure::Malformed(
            "NullPointerException: a null object in the property list".to_owned(),
        ));
    };
    let element = |name: &str, text: &str, xhtml: &mut Xhtml| -> Result<(), Failure> {
        xhtml.start_element(name)?;
        xhtml.characters(text)?;
        xhtml.end_element(name)?;
        Ok(())
    };
    match &**object {
        Object::Dict(dict) => {
            xhtml.start_element("dict")?;
            for (key, value) in dict.entries() {
                let Key::Text(key) = key else {
                    return Err(Failure::Malformed(
                        "a key the port does not write".to_owned(),
                    ));
                };
                xhtml.element("key", key)?;
                write(&Some(value.clone()), dates, xhtml)?;
            }
            xhtml.end_element("dict")?;
        }
        Object::Array(items) => {
            xhtml.start_element("array")?;
            for item in items {
                write(item, dates, xhtml)?;
            }
            xhtml.end_element("array")?;
        }
        Object::Set(_) => {
            return Err(Failure::Malformed(
                "a set the port does not write".to_owned(),
            ))
        }
        Object::String(text) => element("string", text, xhtml)?,
        Object::Data(bytes) => {
            xhtml.start_element("data")?;
            xhtml.characters(&base64(bytes))?;
            xhtml.parse_embedded(bytes, &TikaMetadata::new(), Wrapping::PackageEntry)?;
            xhtml.end_element("data")?;
        }
        Object::Date(seconds) => {
            let text = dates
                .text(*seconds)
                .map_err(|problem| Failure::Malformed(format!("{problem:?}")))?;
            element("date", &text, xhtml)?;
        }
        Object::Uid { object, .. } => xhtml.element("uid", &object.to_string())?,
        number @ (Object::Integer(_) | Object::Real(_) | Object::Boolean(_)) => element(
            "number",
            &number.text().expect("a number has a text"),
            xhtml,
        )?,
    }
    Ok(())
}

/// Base64 with padding and no line breaks, as dd-plist's `Base64.encodeBytes` writes it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = chunk
            .iter()
            .enumerate()
            .fold(0u32, |value, (index, &byte)| {
                value | u32::from(byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                text.push(char::from(
                    ALPHABET[(triple >> (18 - 6 * index) & 0x3F) as usize],
                ));
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// Tika's `PListParser`, for binary property lists.
pub(crate) struct PListParser;

impl Parser for PListParser {
    fn name(&self) -> &'static str {
        "extractous::PListParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<String> {
        match read(input) {
            Err(Problem::Unread(why)) => Some(why.to_owned()),
            _ => None,
        }
    }

    fn fails_before_starting(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<Error> {
        match read(input) {
            Err(Problem::Failed(reason)) => {
                Some(Error::ParseError(format!("problem parsing root: {reason}")))
            }
            _ => None,
        }
    }

    fn parse(
        &self,
        input: &[u8],
        _configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let (root, dates) = read(input).map_err(|problem| {
            Failure::Malformed(format!(
                "the property list is not read natively: {problem:?}"
            ))
        })?;
        xhtml.start_element("plist")?;
        write(&root, &dates, xhtml)?;
        xhtml.end_element("plist")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::binary::tests::{bplist, Item};
    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

    const TIKA_NAME: &str = "org.apache.tika.parser.apple.PListParser";

    #[test]
    fn writes_base64_as_dd_plist_does() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(&[0xFF, 0xEE, 0xDD, 0xCC]), "/+7dzA==");
    }

    #[test]
    fn extracts_property_lists_as_tikas_path_does() {
        use Item::*;
        let scratch = Scratch::new("plist");
        let typical = bplist(
            &[
                Dict(vec![
                    (1, 2),
                    (3, 4),
                    (5, 6),
                    (7, 8),
                    (9, 10),
                    (11, 12),
                    (13, 14),
                    (15, 16),
                    (17, 20),
                    (18, 19),
                    (1, 21),
                    (22, 23),
                ]),
                Ascii("name"),
                Ascii("Ada"),
                Ascii("count"),
                Int(42),
                Ascii("ratio"),
                Real(0.1),
                Ascii("single"),
                Float(0.1),
                Ascii("flag"),
                Bool(true),
                Ascii("when"),
                Date(86_400.5),
                Ascii("blob"),
                Data(b"Embedded text in a data object."),
                Ascii("list"),
                Array(vec![24, 25, 26, 27, 28, 29]),
                Int(7),
                Ascii("nothing"),
                Null,
                Ascii("caf\u{e9}"),
                Utf16("replaced"),
                Real(-0.0),
                Bool(false),
                Ascii("a"),
                Uid(3),
                Utf16("Gr\u{fc}\u{df}e \u{1F600}"),
                Utf8("\u{20AC}uro"),
                Real(1e300),
                Dict(vec![(1, 2)]),
            ],
            0,
        );
        let null_in_array = bplist(&[Array(vec![1, 2]), Ascii("before"), Null], 0);
        let set = bplist(&[Set(vec![1]), Ascii("member")], 0);
        let read = [scratch.write("typical.bplist", &typical)];
        let failing = [scratch.write("null-in-array.bplist", &null_in_array)];
        let deferred_set = scratch.write("set.bplist", &set);
        let (parsed, deferred) = documents(&PListParser, &[scratch.directory()]);
        assert_eq!(deferred, [deferred_set]);
        assert_eq!(parsed.len(), read.len() + failing.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_property_lists_as_tikas_path_does() {
        let (parsed, deferred) = documents(&PListParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 3);
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
