//! Property lists, as Tika's `PListParser` writes what dd-plist 1.30 reads of them, binary
//! ([`binary`]) and XML ([`xml`]): the object tree as `plist`, `dict`, `key`, `array`, `string`,
//! `number`, `data`, `date` and `uid` elements, the bytes of each data object also handed to the
//! embedded document extractor, and each null as an empty `null` element, where Tika drops a
//! null in a dictionary and fails on one elsewhere after starting the list (#104). An XML list
//! whose root is a dictionary is typed by its keys. Sets, trees that share objects so often
//! that writing them out would not end, dates far from ours, and what [`xml`] leaves out are not
//! read. ASCII property lists have no media type of their own in Tika, which detects
//! them as text, so they never reach `PListParser`.
//!
//! Neither kind of list is read into a tree: a binary list's objects are read in place as they
//! are written, an XML list's as its elements come, so that the memory a list takes is bounded
//! whatever its size.

mod xml;

use crate::bplist as binary;

use binary::{Node, Number, ObjectId, Reader};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages};
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
/// The most objects beyond those of a binary list the port writes, writing shared ones each
/// time they are written.
const MAX_REWRITTEN: u64 = 10_000_000;
/// The furthest from 1970 a date may lie for the port to write it, about 30,000 years.
const MAX_DATE_MILLIS: u64 = 1_000_000_000_000_000;
/// The bytes of a string or data object decoded or encoded at a time: a multiple of three, so
/// that the base64 of the pieces is that of the whole.
const PIECE: usize = 3 << 14;

/// Why the port does not write a property list as Tika does: the port does not read it, or
/// dd-plist fails on it before the document starts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Problem {
    Unread(&'static str),
    Failed(String),
}

impl From<binary::Thrown> for Problem {
    fn from(thrown: binary::Thrown) -> Self {
        match thrown {
            binary::Thrown::Format(reason) | binary::Thrown::Runtime(reason) => {
                Problem::Failed(reason)
            }
        }
    }
}

/// What dd-plist throws on a binary list read a second time, which it read the first.
fn malformed(thrown: binary::Thrown) -> Failure {
    match thrown {
        binary::Thrown::Format(reason) | binary::Thrown::Runtime(reason) => {
            Failure::Malformed(reason)
        }
    }
}

/// The dates of a list, written as `PListParser`'s `SimpleDateFormat` writes them, in UTC.
/// Tika writes them in the JVM's default time zone, so the same list reads differently on
/// machines in different zones.
struct Dates {
    format: SimpleDateFormat,
    zone: TimeZone,
}

impl Dates {
    fn new() -> Self {
        Self {
            format: SimpleDateFormat::new(DATE_PATTERN).expect("the date pattern compiles"),
            zone: TimeZone::fixed(0),
        }
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
            .format(millis, &self.zone)
            .map_err(|_| Problem::Unread("a date the port does not format"))
    }
}

/// Fails where the port does not write `object` of the binary list `reader` as Tika does, or
/// where writing it, with the `written` objects before it, writes more than [`MAX_REWRITTEN`]
/// objects beyond the list's own.
fn check(
    reader: &Reader,
    object: ObjectId,
    dates: &Dates,
    written: &mut u64,
) -> Result<(), Problem> {
    *written += 1;
    if *written > reader.objects().saturating_add(MAX_REWRITTEN) {
        return Err(Problem::Unread("more shared objects than the port writes"));
    }
    match reader.node(object)? {
        Node::Dict(pairs) => {
            for (key, value) in reader.entries(pairs)? {
                if !matches!(reader.node(key)?, Node::String(_)) {
                    return Err(Problem::Failed(
                        "a dictionary key is not a string, which dd-plist names by its \
                         toString"
                            .to_owned(),
                    ));
                }
                check(reader, value, dates, written)?;
            }
        }
        Node::Array(members) => {
            for member in reader.members(members) {
                check(reader, member, dates, written)?;
            }
        }
        Node::Set(_) => return Err(Problem::Unread("a set")),
        Node::Date(seconds) => {
            dates.text(seconds)?;
        }
        Node::Null | Node::Number(_) | Node::Data(_) | Node::String(_) | Node::Uid(_) => {}
    }
    Ok(())
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

/// An element of `name` holding `text`, written even when the text is empty.
fn text_element(xhtml: &mut Xhtml, name: &str, text: &str) -> Result<(), Stop> {
    xhtml.start_element(name)?;
    xhtml.characters(text)?;
    xhtml.end_element(name)
}

/// A null, as an empty `null` element.
fn write_null(xhtml: &mut Xhtml) -> Result<(), Stop> {
    xhtml.start_element("null")?;
    xhtml.end_element("null")
}

fn write_number(xhtml: &mut Xhtml, number: Number) -> Result<(), Stop> {
    text_element(xhtml, "number", &number.text())
}

fn write_date(xhtml: &mut Xhtml, dates: &Dates, seconds: f64) -> Result<(), Failure> {
    let text = dates
        .text(seconds)
        .map_err(|problem| Failure::Malformed(format!("{problem:?}")))?;
    Ok(text_element(xhtml, "date", &text)?)
}

/// Hands a data object's bytes to the embedded document extractor.
fn embed(xhtml: &mut Xhtml, data: &Input<'_>) -> Result<(), Failure> {
    xhtml.parse_embedded_input(data, &TikaMetadata::new(), Wrapping::PackageEntry)
}

/// A data object: its bytes in base64, then handed to the embedded document extractor.
fn write_data(xhtml: &mut Xhtml, data: &Input<'_>) -> Result<(), Failure> {
    xhtml.start_element("data")?;
    let mut base64 = Base64::default();
    let mut text = String::new();
    for piece in data.chunks(PIECE) {
        text.clear();
        base64.encode(piece, &mut text);
        xhtml.characters(&text)?;
    }
    text.clear();
    base64.finish(&mut text);
    xhtml.characters(&text)?;
    embed(xhtml, data)?;
    xhtml.end_element("data")?;
    Ok(())
}

/// Base64 with padding and no line breaks, as dd-plist's `Base64.encodeBytes` writes it, of
/// bytes that come in pieces: the last one or two bytes of a piece wait for the next.
#[derive(Default)]
struct Base64 {
    carried: [u8; 2],
    carrying: usize,
}

impl Base64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    /// Writes onto `text` the base64 of `bytes` after the bytes before them, all but the
    /// bytes that do not fill a group of three.
    fn encode(&mut self, mut bytes: &[u8], text: &mut String) {
        if self.carrying > 0 {
            let mut group = [0u8; 3];
            group[..self.carrying].copy_from_slice(&self.carried[..self.carrying]);
            let taken = (3 - self.carrying).min(bytes.len());
            group[self.carrying..self.carrying + taken].copy_from_slice(&bytes[..taken]);
            bytes = &bytes[taken..];
            let filled = self.carrying + taken;
            if filled < 3 {
                self.carried[..filled].copy_from_slice(&group[..filled]);
                self.carrying = filled;
                return;
            }
            Self::group(&group, text);
            self.carrying = 0;
        }
        let whole = bytes.len() / 3 * 3;
        for group in bytes[..whole].chunks(3) {
            Self::group(group, text);
        }
        let rest = &bytes[whole..];
        self.carried[..rest.len()].copy_from_slice(rest);
        self.carrying = rest.len();
    }

    /// Writes onto `text` the base64 of the bytes still waiting, padded.
    fn finish(self, text: &mut String) {
        if self.carrying > 0 {
            Self::group(&self.carried[..self.carrying], text);
        }
    }

    /// The four characters of a group of one to three bytes, padded with `=`.
    fn group(group: &[u8], text: &mut String) {
        let triple = group
            .iter()
            .enumerate()
            .fold(0u32, |value, (index, &byte)| {
                value | u32::from(byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= group.len() {
                text.push(char::from(
                    Self::ALPHABET[(triple >> (18 - 6 * index) & 0x3F) as usize],
                ));
            } else {
                text.push('=');
            }
        }
    }
}

/// `parseObject` of `PListParser`, of `object` of the binary list `reader`, with a null written
/// as an empty `null` element.
fn write_binary(
    reader: &Reader,
    object: ObjectId,
    dates: &Dates,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    match reader.node(object).map_err(malformed)? {
        Node::Null => write_null(xhtml)?,
        Node::Dict(pairs) => {
            xhtml.start_element("dict")?;
            for (key, value) in reader.entries(pairs).map_err(malformed)? {
                let Node::String(key) = reader.node(key).map_err(malformed)? else {
                    return Err(Failure::Malformed(
                        "a key the port does not write".to_owned(),
                    ));
                };
                xhtml.element("key", &key.decode())?;
                write_binary(reader, value, dates, xhtml)?;
            }
            xhtml.end_element("dict")?;
        }
        Node::Array(members) => {
            xhtml.start_element("array")?;
            for member in reader.members(members) {
                write_binary(reader, member, dates, xhtml)?;
            }
            xhtml.end_element("array")?;
        }
        Node::Set(_) => {
            return Err(Failure::Malformed(
                "a set the port does not write".to_owned(),
            ))
        }
        Node::String(text) => {
            xhtml.start_element("string")?;
            for piece in text.pieces(PIECE) {
                xhtml.characters(&piece)?;
            }
            xhtml.end_element("string")?;
        }
        Node::Data(bytes) => write_data(xhtml, &Input::Within(bytes, reader.pages()))?,
        Node::Date(seconds) => write_date(xhtml, dates, seconds)?,
        Node::Uid(value) => xhtml.element("uid", &value.to_string())?,
        Node::Number(number) => write_number(xhtml, number)?,
    }
    Ok(())
}

/// Types the document by the keys of its root dictionary, of `kind`, where Tika's detection
/// typed it as an XML property list.
fn type_by_keys(
    xhtml: &mut Xhtml,
    kind: impl FnOnce() -> Result<Option<binary::ListKind>, Failure>,
) -> Result<(), Failure> {
    if xhtml.metadata_mut().get("Content-Type") != Some(XML_PLIST) {
        return Ok(());
    }
    if let Some(kind) = kind()? {
        xhtml.metadata_mut().set("Content-Type", kind.xml_type());
    }
    Ok(())
}

/// The binary list `input`, whose pages are `pages`, checked as dd-plist parses it: what
/// writes it.
fn binary_list<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<Prepared<'a>, Problem> {
    let reader = Reader::open(input, pages)?;
    reader.validate()?;
    let dates = Dates::new();
    check(&reader, reader.top(), &dates, &mut 0)?;
    Ok(Box::new(move |xhtml| {
        type_by_keys(xhtml, || {
            match reader.node(reader.top()).map_err(malformed)? {
                Node::Dict(pairs) => Ok(Some(reader.kind(pairs).map_err(malformed)?)),
                _ => Ok(None),
            }
        })?;
        xhtml.start_element("plist")?;
        write_binary(&reader, reader.top(), &dates, xhtml)?;
        xhtml.end_element("plist")?;
        Ok(())
    }))
}

/// The XML list `input`, whose pages are `pages`, read once as dd-plist reads it: what reads
/// and writes it again.
fn xml_list<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<Prepared<'a>, Problem> {
    let dates = Dates::new();
    let survey = xml::survey(input, pages, &dates)?;
    survey.checked()?;
    Ok(Box::new(move |xhtml| {
        type_by_keys(xhtml, || Ok(survey.root_kind()))?;
        xhtml.start_element("plist")?;
        xml::write(input, pages, &survey, &dates, xhtml)?;
        xhtml.end_element("plist")?;
        Ok(())
    }))
}

/// Tika's `PListParser`.
pub struct PListParser;

impl Parser for PListParser {
    fn name(&self) -> &'static str {
        "dijo::PListParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.apple.PListParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
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
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let prepared = match format(input) {
            Format::Binary => binary_list(input, pages),
            Format::Xml => xml_list(input, pages),
            Format::Other => Err(Problem::Unread(
                "a property list that is neither binary nor XML",
            )),
        };
        prepared.map_err(|problem| match problem {
            Problem::Unread(why) => Decline::Unread(why.to_owned()),
            Problem::Failed(reason) => {
                Decline::Fails(Error::ParseError(format!("problem parsing root: {reason}")))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::binary::tests::{bplist, Item};
    use super::*;
    use crate::formats::testing::{documents, Scratch};

    #[test]
    fn writes_base64_as_dd_plist_does() {
        let base64 = |pieces: &[&[u8]]| {
            let mut base64 = Base64::default();
            let mut text = String::new();
            for piece in pieces {
                base64.encode(piece, &mut text);
            }
            base64.finish(&mut text);
            text
        };
        assert_eq!(base64(&[b""]), "");
        assert_eq!(base64(&[b"f"]), "Zg==");
        assert_eq!(base64(&[b"fo"]), "Zm8=");
        assert_eq!(base64(&[b"foo"]), "Zm9v");
        assert_eq!(base64(&[&[0xFF, 0xEE, 0xDD, 0xCC]]), "/+7dzA==");
        assert_eq!(base64(&[b"f", b"", b"oob", b"a", b"r"]), "Zm9vYmFy");
        assert_eq!(base64(&[b"fo", b"o", b"b"]), "Zm9vYg==");
    }

    /// A binary property list of every kind of object the parser reads, and one of a set, which
    /// it does not read, written to `scratch`.
    fn typical_and_set(scratch: &Scratch) -> (PathBuf, PathBuf) {
        use Item::*;
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
        (
            scratch.write("typical.bplist", &typical),
            scratch.write("set.bplist", &set),
        )
    }

    #[test]
    fn reads_every_kind_of_object_but_sets() {
        let scratch = Scratch::new("plist-kinds");
        let (typical, set) = typical_and_set(&scratch);
        let (parsed, shortfalls) = documents(&PListParser, &[scratch.directory()]);
        assert_eq!(shortfalls, [set]);
        assert_eq!(parsed, [typical]);
    }

    /// The extraction fails a document its native parser does not read with the parser's reason.
    #[test]
    fn fails_sets_with_why() {
        let set = bplist(&[Item::Set(vec![1]), Item::Ascii("member")], 0);
        let failed = crate::Extractor::new()
            .extract_bytes_to_string(&set)
            .unwrap_err();
        assert!(
            matches!(
                &failed,
                Error::Unsupported(crate::Unsupported::Unread {
                    parser: "dijo::PListParser",
                    ..
                })
            ),
            "{failed:?}"
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
    fn writes_a_key_put_again_in_its_first_place_with_its_last_value() {
        use Item::*;
        let list = bplist(
            &[
                Dict(vec![(1, 2), (3, 4), (1, 5)]),
                Ascii("a"),
                Ascii("1"),
                Ascii("b"),
                Ascii("2"),
                Array(vec![6]),
                Ascii("3"),
            ],
            0,
        );
        assert_eq!(
            body(&list),
            Ok("<plist><dict><key>a</key><array><string>3</string></array>\
                <key>b</key><string>2</string></dict></plist>"
                .to_owned())
        );
        let xml = "<?xml version=\"1.0\"?>\n<plist><dict><key>a</key><string>1</string>\
                   <key>b</key><dict><key>x</key><string>2</string><key>x</key><string>3</string></dict>\
                   <key>a</key><array><string>4</string><dict><key>y</key><true/><key>y</key><false/></dict></array>\
                   <key>a</key><array><string>5</string><dict><key>z</key><true/><key>z</key><integer>6</integer></dict></array>\
                   <key>c</key><real>7.5</real></dict></plist>";
        assert_eq!(
            body(xml.as_bytes()),
            Ok(
                "<plist><dict><key>a</key><array><string>5</string><dict><key>z</key>\
                <number>6</number></dict></array><key>b</key><dict><key>x</key>\
                <string>3</string></dict><key>c</key><number>7.5</number></dict></plist>"
                    .to_owned()
            )
        );
    }

    /// The text of each string of the lists the large-file tests write, by its index.
    fn line(index: u64) -> String {
        format!("Linje {index:010}\n")
    }

    /// Writes to `out` a binary list of a dictionary that puts `title` twice, around `items`,
    /// an array of `count` strings, each the [`line`] of its index, with four-byte offsets and
    /// references.
    fn large_bplist(out: &mut dyn std::io::Write, count: u32) -> std::io::Result<()> {
        let ascii = |text: &str| [vec![0x50 | text.len() as u8], text.as_bytes().to_vec()].concat();
        let dict: Vec<u8> = std::iter::once(0xD3)
            .chain(
                [1u32, 3, 1, 2, 4, 5]
                    .iter()
                    .flat_map(|reference| reference.to_be_bytes()),
            )
            .collect();
        out.write_all(b"bplist00")?;
        let mut at = 8u64;
        let mut offsets = Vec::new();
        for object in [dict, ascii("title"), ascii("Begynd\n"), ascii("items")] {
            offsets.push(at);
            out.write_all(&object)?;
            at += object.len() as u64;
        }
        offsets.push(at);
        out.write_all(&[0xAF, 0x12])?;
        out.write_all(&count.to_be_bytes())?;
        for index in 0..count {
            out.write_all(&(6 + index).to_be_bytes())?;
        }
        at += 6 + 4 * u64::from(count);
        offsets.push(at);
        let end = ascii("Slut\n");
        out.write_all(&end)?;
        at += end.len() as u64;
        let lines = at;
        for index in 0..count {
            let text = line(u64::from(index));
            out.write_all(&[0x5F, 0x10, text.len() as u8])?;
            out.write_all(text.as_bytes())?;
        }
        at += 20 * u64::from(count);
        for offset in offsets {
            out.write_all(&(offset as u32).to_be_bytes())?;
        }
        for index in 0..u64::from(count) {
            out.write_all(&((lines + 20 * index) as u32).to_be_bytes())?;
        }
        out.write_all(&[0, 0, 0, 0, 0, 0, 4, 4])?;
        out.write_all(&(6 + u64::from(count)).to_be_bytes())?;
        out.write_all(&0u64.to_be_bytes())?;
        out.write_all(&at.to_be_bytes())
    }

    /// Asserts that `file`, a list the large-file tests write of `count` items, is written an
    /// object at a time under the default budget, its title put again written in its first
    /// place with its last value.
    fn assert_items(file: &dijo_core::large::LargeFile, count: u64) {
        let mut items = 0u64;
        let mut titled = false;
        file.extract_lines(PListParser.name(), |line| {
            assert!(!line.contains("Begynd"), "{line}");
            if line.contains("Slut") {
                assert_eq!(items, 0, "{line}");
                titled = true;
            }
            if let Some(at) = line.find("Linje ") {
                assert_eq!(&line[at..], line_of(items), "{line}");
                items += 1;
            }
        });
        assert!(titled);
        assert_eq!(items, count);
    }

    fn line_of(index: u64) -> String {
        line(index).trim_end().to_owned()
    }

    #[test]
    fn writes_binary_lists_too_large_to_read_whole_an_object_at_a_time() {
        use dijo_core::large::{bodies_past_limit, LargeFile};

        let count = bodies_past_limit(4 + 20 + 4);
        let file = LargeFile::written("large.bplist", |out| {
            large_bplist(out, u32::try_from(count).unwrap())
        });
        assert_items(&file, count);
    }

    #[test]
    fn writes_xml_lists_too_large_to_read_whole_an_element_at_a_time() {
        use dijo_core::large::{bodies_past_limit, LargeFile};

        let item = |index: u64| format!("<string>{}</string>\n", line(index)).into_bytes();
        let count = bodies_past_limit(item(0).len());
        let file = LargeFile::new(
            "large.plist",
            b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \
              \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
              <plist version=\"1.0\">\n<dict><key>title</key><string>Begynd\n</string>\
              <key>items</key><array>\n",
            count,
            item,
            b"</array><key>title</key><string>Slut\n</string></dict>\n</plist>\n",
        );
        assert_items(&file, count);
    }

    #[test]
    fn fails_on_keys_that_are_not_strings_before_writing() {
        // dd-plist takes any object's `toString` as a key (#104).
        use Item::*;
        for key in [Int(7), Uid(7), Array(vec![])] {
            let list = bplist(&[Dict(vec![(1, 2)]), key, Ascii("seven")], 0);
            let failure = body(&list).unwrap_err();
            assert!(failure.contains("is not a string"), "{failure}");
        }
    }
}
