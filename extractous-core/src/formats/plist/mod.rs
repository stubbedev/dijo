//! Property lists, as Tika's `PListParser` writes what dd-plist 1.30 reads of them, binary
//! ([`binary`]) and XML ([`xml`]): the object tree as `plist`, `dict`, `key`, `array`, `string`,
//! `number`, `data`, `date` and `uid` elements, the bytes of each data object also handed to the
//! embedded document extractor, and each null as an empty `null` element, where Tika drops a
//! null in a dictionary and fails on one elsewhere after starting the list (#104). An XML list
//! whose root is a dictionary is typed by its keys. Sets, trees that share objects so often
//! that writing them out would not end, dates far from ours, and what [`xml`] leaves out stay
//! on Tika's path. ASCII property lists have no media type of their own in Tika, which detects
//! them as text, so they never reach `PListParser`.

pub(crate) mod binary;
mod xml;

use std::collections::HashMap;
use std::rc::Rc;

use binary::{Key, Object, Value};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java::simple_date_format::SimpleDateFormat;
use crate::java::time::TimeZone;

const MEDIA_TYPES: [&str; 5] = [
    "application/x-bplist",
    "application/x-bplist-itunes",
    "application/x-bplist-memgraph",
    "application/x-bplist-webarchive",
    XML_PLIST,
];
/// The type Tika detects an XML property list as, by its root element.
const XML_PLIST: &str = "application/x-plist";
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
                if let Key::Other(_) = key {
                    return Err(Problem::Failed(
                        "a dictionary key is not a string, which dd-plist names by its \
                         toString"
                            .to_owned(),
                    ));
                }
                add(check(value, dates, counts)?)?;
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

/// The formats `PropertyListParser.determineType` tells apart by the first character after
/// any byte order mark and white space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Binary,
    Xml,
    Other,
}

/// `PropertyListParser.determineType`, for the lists Tika's detection sends to `PListParser`:
/// binary ones start with their magic, XML ones with markup after any byte order mark and
/// white space.
fn format(input: &[u8]) -> Format {
    if input.starts_with(MAGIC) {
        return Format::Binary;
    }
    let boms: [&[u8]; 5] = [
        &[0x00, 0x00, 0xFE, 0xFF],
        &[0xFF, 0xFE, 0x00, 0x00],
        &[0xEF, 0xBB, 0xBF],
        &[0xFE, 0xFF],
        &[0xFF, 0xFE],
    ];
    let unmarked = boms
        .iter()
        .find_map(|bom| input.strip_prefix(*bom))
        .unwrap_or(input);
    match unmarked
        .iter()
        .find(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0C))
    {
        Some(b'<') => Format::Xml,
        _ => Format::Other,
    }
}

/// The root of `input` and the writer of its dates, or why the port does not write it.
fn read(input: &[u8]) -> Result<(Value, Dates), Problem> {
    let root = match format(input) {
        Format::Binary => binary::parse(input).map_err(|thrown| match thrown {
            binary::Thrown::Format(reason) | binary::Thrown::Runtime(reason) => {
                Problem::Failed(reason)
            }
        })?,
        Format::Xml => xml::parse(input)?,
        Format::Other => {
            return Err(Problem::Unread(
                "a property list that is neither binary nor XML",
            ))
        }
    };
    let dates = Dates::new()?;
    check(&root, &dates, &mut HashMap::new())?;
    Ok((root, dates))
}

/// `parseObject` of `PListParser`, with a null written as an empty `null` element.
fn write(value: &Value, dates: &Dates, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let Some(object) = value else {
        xhtml.start_element("null")?;
        xhtml.end_element("null")?;
        return Ok(());
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
                let Key::String(key) = key else {
                    return Err(Failure::Malformed(
                        "a key the port does not write".to_owned(),
                    ));
                };
                xhtml.element("key", key)?;
                write(value, dates, xhtml)?;
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
        Object::Uid(value) => xhtml.element("uid", &value.to_string())?,
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

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.apple.PListParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let (root, dates) = read(input).map_err(|problem| match problem {
            Problem::Unread(why) => Declined::Deferred(why.to_owned()),
            Problem::Failed(reason) => {
                Declined::Fails(Error::ParseError(format!("problem parsing root: {reason}")))
            }
        })?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            if let (Some(XML_PLIST), Some(Object::Dict(dict))) =
                (metadata.get("Content-Type"), root.as_deref())
            {
                metadata.set("Content-Type", dict.kind().xml_type());
            }
            xhtml.start_element("plist")?;
            write(&root, &dates, xhtml)?;
            xhtml.end_element("plist")?;
            Ok(())
        }))
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
                    (1, 21),
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
                // dd-plist names a UID by its object, mis-measures multibyte UTF-8, takes keys
                // of any object and drops null values (#104), which `binary`'s tests and
                // `writes_nulls_as_empty_values` check the port does not.
                Int(3),
                Utf16("Gr\u{fc}\u{df}e \u{1F600}"),
                Utf8("euro"),
                Real(1e300),
                Dict(vec![(1, 2)]),
            ],
            0,
        );
        let set = bplist(&[Set(vec![1]), Ascii("member")], 0);
        let read = [scratch.write("typical.bplist", &typical)];
        let deferred_set = scratch.write("set.bplist", &set);
        let (parsed, deferred) = documents(&PListParser, &[scratch.directory()]);
        assert_eq!(deferred, [deferred_set]);
        assert_eq!(parsed.len(), read.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
    }

    /// The body the native path writes of the property list `input`, or how it fails.
    fn body(input: &[u8]) -> Result<String, String> {
        let (xml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(input)
            .map_err(|error| error.to_string())?;
        let start = xml.find("<plist>").expect("a plist element");
        let end = xml.find("</plist>").expect("the plist's end") + "</plist>".len();
        Ok(xml[start..end].to_owned())
    }

    #[test]
    fn writes_nulls_as_empty_values() {
        // dd-plist drops a null in a dictionary, and Tika fails on one in an array after
        // starting the list (#104).
        use Item::*;
        let null = "<null />";
        let dict = bplist(
            &[
                Dict(vec![(1, 2), (3, 4)]),
                Ascii("a"),
                Null,
                Ascii("b"),
                Int(1),
            ],
            0,
        );
        assert_eq!(
            body(&dict),
            Ok(format!(
                "<plist><dict><key>a</key>{null}<key>b</key><number>1</number></dict></plist>"
            ))
        );
        let array = bplist(&[Array(vec![1, 2]), Ascii("before"), Null], 0);
        assert_eq!(
            body(&array),
            Ok(format!(
                "<plist><array><string>before</string>{null}</array></plist>"
            ))
        );
        let xml = |body: &str| {
            format!("<?xml version=\"1.0\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">{body}</plist>")
        };
        assert_eq!(
            body(
                xml("<dict><key>k</key><string>v</string><key>k</key><unknown/></dict>").as_bytes()
            ),
            Ok(format!("<plist><dict><key>k</key>{null}</dict></plist>"))
        );
        assert_eq!(
            body(xml("<array><string>a</string><unknown/></array>").as_bytes()),
            Ok(format!(
                "<plist><array><string>a</string>{null}</array></plist>"
            ))
        );
    }

    #[test]
    fn fails_on_keys_that_are_not_strings_before_writing() {
        // dd-plist takes any object's `toString` as a key (#104).
        use Item::*;
        let number_key = bplist(&[Dict(vec![(1, 2)]), Int(7), Ascii("seven")], 0);
        let failure = body(&number_key).unwrap_err();
        assert!(failure.contains("is not a string"), "{failure}");
    }

    #[test]
    fn extracts_xml_property_lists_as_tikas_path_does() {
        let scratch = Scratch::new("xml-plist");
        let header = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \
                      \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n";
        let list = |body: &str| format!("{header}<plist version=\"1.0\">\n{body}\n</plist>\n");
        let read = [
            scratch.write(
                "typical.plist",
                list(
                    "<dict>\n  <key>keyA</key>\n  <string>valueA</string>\n  <key>key&amp;B</key>\n  \
                     <string>value&amp;B <![CDATA[<b>]]><!-- c --> f\u{f8}r</string>\n  <key>date</key>\n  \
                     <date>2011-11-28T09:21:30Z</date>\n  <key>data</key>\n  <data>\n  SGVsbG8gZW1i\n  \
                     ZWRkZWQgdGV4dA==\n  </data>\n  <key>array</key>\n  <array>\n    <true/>\n    <false/>\n    \
                     <integer>87</integer>\n    <integer>0x1f</integer>\n    <integer>yes</integer>\n    \
                     <real>3.14159</real>\n    <real>-infinity</real>\n    <real> 2</real>\n    <string/>\n  \
                     </array>\n  <key>nested</key>\n  <dict/>\n</dict>",
                )
                .as_bytes(),
            ),
            scratch.write(
                "webarchive.plist",
                list("<dict><key>WebMainResource</key><dict/></dict>").as_bytes(),
            ),
            scratch.write(
                "itunes.plist",
                list(
                    "<dict><key>Playlists</key><array/><key>Tracks</key><dict/>\
                     <key>Music Folder</key><string>m</string></dict>",
                )
                .as_bytes(),
            ),
            scratch.write("array.plist", list("<array><string>a</string></array>").as_bytes()),
            scratch.write(
                "no-doctype.plist",
                b"<?xml version=\"1.0\"?>\n<plist><dict><key>k</key><integer>1</integer></dict></plist>",
            ),
        ];
        let failing = [
            scratch.write("empty.plist", list("").as_bytes()),
            scratch.write("odd.plist", list("<dict><key>k</key></dict>").as_bytes()),
            scratch.write("number.plist", list("<integer>1x</integer>").as_bytes()),
            scratch.write("data.plist", list("<data>QQ</data>").as_bytes()),
        ];
        let deferred_entity =
            scratch.write("entity.plist", list("<string>&e;</string>").as_bytes());
        let (parsed, deferred) = documents(&PListParser, &[scratch.directory()]);
        assert_eq!(deferred, [deferred_entity]);
        assert_eq!(parsed.len(), read.len() + failing.len(), "{parsed:?}");
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
        // Its embedded keyed archives hold UIDs, which Tika names by their object (#104).
        let (with_uids, paths): (Vec<&Path>, Vec<&Path>) = parsed
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| path.ends_with("testWEBARCHIVE.webarchive"));
        assert_same_as_tika(&paths, TIKA_NAME);
        let [with_uids] = with_uids[..] else {
            panic!("{with_uids:?}");
        };
        let (xml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_file_to_string(with_uids.to_str().unwrap())
            .unwrap();
        assert!(
            xml.contains("<key>WebResourceResponse</key><uid>1</uid>"),
            "{xml}"
        );
    }
}
