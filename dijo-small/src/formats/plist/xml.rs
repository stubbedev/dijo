//! XML property lists as dd-plist 1.30's `XMLPropertyListParser` reads them: the document parsed
//! by the JDK's namespace-unaware DOM builder, coalescing CDATA into text and leaving comments
//! out, then each element read as the object its name stands for, its text the first run of
//! character data among its children.
//!
//! The DOM is not built: the document is walked as it is parsed, twice, each object read as its
//! element comes. The first walk ([`survey`]) fails where dd-plist fails and plans the second
//! ([`write`]), which writes the objects as they come. A dictionary that puts a key again keeps
//! the key in its first place with the value it was last put with, which comes later: the first
//! walk builds each such later value, and the second writes the last in the first's place and
//! leaves the others out, so that only those values are held.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::io::Write as _;

use super::{embed, text_element, write_data, write_date, write_null, write_number};
use super::{Base64, Dates, Problem};
use crate::bplist::{KindKeys, ListKind, Number};
use crate::errors::Error;
use crate::formats::xhtml::Xhtml;
use crate::formats::Failure;
use crate::input::{Input, Pages, Paging, Spool, MAX_READ_WHOLE};
use crate::java;
use crate::java::time::{CalendarFields, TimeZone};
use crate::xml::{self, Event, Flow, XmlError};

/// `XMLPropertyListParser.MAX_NESTING_DEPTH`.
const MAX_NESTING_DEPTH: usize = 512;
/// Milliseconds from the Java epoch to dd-plist's, 2001-01-01.
const EPOCH: i64 = 978_307_200_000;
/// The bytes of a data object's text decoded at a time.
const DECODED_PIECE: usize = 1 << 16;

/// dd-plist's failure to read an element as the object its name stands for.
fn unparsed(name: &str) -> Problem {
    Problem::Failed(format!("The {name} node could not be parsed."))
}

/// An object written as one element that holds nothing but its text.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Leaf {
    /// What dd-plist reads an element of no object's name as.
    Null,
    Number(Number),
    /// Seconds since 2001-01-01.
    Date(f64),
}

/// What the walk reads of the list's objects, in the order of their elements: each object's
/// start and end, the key of each dictionary entry before its value, and the text of strings
/// and the bytes of data objects as they come.
#[derive(Debug)]
enum Value<'t> {
    StartDict,
    Key(String),
    EndDict,
    StartArray,
    EndArray,
    Leaf(Leaf),
    StartString,
    StringText(&'t str),
    EndString,
    StartData,
    DataBytes(&'t [u8]),
    EndData,
}

impl Value<'_> {
    /// Whether it starts an object.
    fn starts_object(&self) -> bool {
        self.opens() || matches!(self, Value::Leaf(_))
    }

    /// Whether it starts an object that ends with a value of its own.
    fn opens(&self) -> bool {
        matches!(
            self,
            Value::StartDict | Value::StartArray | Value::StartString | Value::StartData
        )
    }

    /// Whether it ends an object [`Value::opens`] started.
    fn closes(&self) -> bool {
        matches!(
            self,
            Value::EndDict | Value::EndArray | Value::EndString | Value::EndData
        )
    }
}

/// What takes the objects the walk reads.
trait Values {
    type Error;

    fn value(&mut self, value: Value<'_>) -> Result<(), Self::Error>;
}

/// How far the text of an element has come: the first run of character data among its
/// children, which a child element or processing instruction ends, is its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Run {
    Before,
    In,
    After,
}

/// What the walk takes of an element's text.
enum Collected {
    /// A dictionary's key, any element at an even place among the dictionary's.
    Key(String),
    /// An `integer` or `real`, by its element's name.
    Number(&'static str, String),
    Date(String),
    /// A string, its text handed on as it comes.
    String,
    /// A data object, its text decoded as it comes.
    Data(Base64Decoder),
}

struct Collecting {
    run: Run,
    collected: Collected,
}

impl Collecting {
    fn new(collected: Collected) -> Self {
        Self {
            run: Run::Before,
            collected,
        }
    }

    /// Ends the run of text, as a child element or processing instruction does.
    fn interrupt(&mut self) {
        if self.run == Run::In {
            self.run = Run::After;
        }
    }
}

/// What an open element is to the walk.
enum Frame {
    /// An element inside which the walk reads nothing.
    Ignored,
    /// The root `plist` element, and whether its first element, the list's object, has come.
    Plist {
        read: bool,
    },
    Dict {
        depth: usize,
        children: usize,
    },
    Array {
        depth: usize,
    },
    Text(Collecting),
}

/// Why the walk stops reading objects: dd-plist would fail, or the taker of the objects did.
enum Halt<E> {
    Problem(Problem),
    Failure(E),
}

impl<E> From<Problem> for Halt<E> {
    fn from(problem: Problem) -> Self {
        Self::Problem(problem)
    }
}

/// The walk of a document's elements, handing the objects they stand for to `values`.
struct Walk<'v, V: Values> {
    values: &'v mut V,
    frames: Vec<Frame>,
    /// How many elements are open.
    open: usize,
    /// Whether the root element is `plist`, once it has come.
    root_is_plist: Option<bool>,
    /// How many elements the root element holds.
    root_children: usize,
    document_type: bool,
    halted: Option<Halt<V::Error>>,
}

impl<V: Values> Walk<'_, V> {
    fn event(&mut self, event: Event<'_>) -> Flow {
        let read = match event {
            Event::DocumentType => {
                self.document_type = true;
                Ok(())
            }
            Event::Start(name, _) => {
                let name = name.qualified_name();
                match self.open {
                    0 => self.root_is_plist = Some(name == "plist"),
                    1 => self.root_children += 1,
                    _ => {}
                }
                self.open += 1;
                self.reading(|walk| walk.start(name))
            }
            Event::End(_) => {
                self.open -= 1;
                self.reading(Self::end)
            }
            Event::Text(text, _) => self.reading(|walk| walk.text(text)),
            Event::ProcessingInstruction(..) => self.reading(|walk| {
                if let Some(Frame::Text(collecting)) = walk.frames.last_mut() {
                    collecting.interrupt();
                }
                Ok(())
            }),
            Event::Comment(_) => Ok(()),
        };
        if let Err(halt) = read {
            self.halted = Some(halt);
        }
        match self.halted {
            Some(Halt::Failure(_)) => Flow::Stop,
            _ => Flow::Continue,
        }
    }

    /// `read`, unless the walk has stopped reading objects.
    fn reading(
        &mut self,
        read: impl FnOnce(&mut Self) -> Result<(), Halt<V::Error>>,
    ) -> Result<(), Halt<V::Error>> {
        if self.halted.is_some() {
            return Ok(());
        }
        read(self)
    }

    fn hand_on(&mut self, value: Value<'_>) -> Result<(), Halt<V::Error>> {
        self.values.value(value).map_err(Halt::Failure)
    }

    fn start(&mut self, name: &str) -> Result<(), Halt<V::Error>> {
        let object_depth = match self.frames.last_mut() {
            None if name == "plist" => {
                self.frames.push(Frame::Plist { read: false });
                return Ok(());
            }
            None => 1,
            Some(Frame::Plist { read }) if !*read => {
                *read = true;
                1
            }
            Some(Frame::Plist { .. } | Frame::Ignored) => {
                self.frames.push(Frame::Ignored);
                return Ok(());
            }
            Some(Frame::Array { depth }) => *depth + 1,
            Some(Frame::Dict { depth, children }) => {
                *children += 1;
                if *children % 2 == 1 {
                    let key = Collecting::new(Collected::Key(String::new()));
                    self.frames.push(Frame::Text(key));
                    return Ok(());
                }
                *depth + 1
            }
            Some(Frame::Text(collecting)) => {
                collecting.interrupt();
                self.frames.push(Frame::Ignored);
                return Ok(());
            }
        };
        let frame = self.object(name, object_depth)?;
        self.frames.push(frame);
        Ok(())
    }

    /// `parseObject` as the element `name` starts, at `depth`: what the walk reads inside it.
    fn object(&mut self, name: &str, depth: usize) -> Result<Frame, Halt<V::Error>> {
        if depth > MAX_NESTING_DEPTH {
            return Err(Problem::Failed(format!(
                "The nesting depth of the property list exceeds the maximum supported depth of \
                 {MAX_NESTING_DEPTH}."
            ))
            .into());
        }
        let text = |collected| Frame::Text(Collecting::new(collected));
        let (value, frame) = match name {
            "dict" => (Some(Value::StartDict), Frame::Dict { depth, children: 0 }),
            "array" => (Some(Value::StartArray), Frame::Array { depth }),
            "true" => (
                Some(Value::Leaf(Leaf::Number(Number::Boolean(true)))),
                Frame::Ignored,
            ),
            "false" => (
                Some(Value::Leaf(Leaf::Number(Number::Boolean(false)))),
                Frame::Ignored,
            ),
            "integer" => (None, text(Collected::Number("integer", String::new()))),
            "real" => (None, text(Collected::Number("real", String::new()))),
            "date" => (None, text(Collected::Date(String::new()))),
            "string" => (Some(Value::StartString), text(Collected::String)),
            "data" => (
                Some(Value::StartData),
                text(Collected::Data(Base64Decoder::default())),
            ),
            _ => (Some(Value::Leaf(Leaf::Null)), Frame::Ignored),
        };
        if let Some(value) = value {
            self.hand_on(value)?;
        }
        Ok(frame)
    }

    fn end(&mut self) -> Result<(), Halt<V::Error>> {
        let frame = self
            .frames
            .pop()
            .expect("an end tag closes an element the walk opened");
        let value = match frame {
            Frame::Ignored | Frame::Plist { .. } => return Ok(()),
            Frame::Dict { children, .. } if children % 2 == 1 => {
                return Err(unparsed("dict").into())
            }
            Frame::Dict { .. } => Value::EndDict,
            Frame::Array { .. } => Value::EndArray,
            Frame::Text(Collecting { collected, .. }) => match collected {
                Collected::Key(name) => Value::Key(name),
                Collected::Number(element, text) => Value::Leaf(Leaf::Number(
                    number(&text).ok_or_else(|| unparsed(element))?,
                )),
                Collected::Date(text) => Value::Leaf(Leaf::Date(date(&text)?)),
                Collected::String => Value::EndString,
                Collected::Data(decoder) if decoder.complete() => Value::EndData,
                Collected::Data(_) => return Err(unparsed("data").into()),
            },
        };
        self.hand_on(value)
    }

    fn text(&mut self, text: &str) -> Result<(), Halt<V::Error>> {
        let Some(Frame::Text(collecting)) = self.frames.last_mut() else {
            return Ok(());
        };
        if text.is_empty() || collecting.run == Run::After {
            return Ok(());
        }
        collecting.run = Run::In;
        match &mut collecting.collected {
            Collected::Key(collected)
            | Collected::Number(_, collected)
            | Collected::Date(collected) => collected.push_str(text),
            Collected::String => {
                self.values
                    .value(Value::StringText(text))
                    .map_err(Halt::Failure)?;
            }
            Collected::Data(decoder) => {
                let mut decoded = Vec::new();
                for piece in pieces(text, DECODED_PIECE) {
                    decoded.clear();
                    if decoder.feed(piece, &mut decoded).is_none() {
                        return Err(unparsed("data").into());
                    }
                    if !decoded.is_empty() {
                        self.values
                            .value(Value::DataBytes(&decoded))
                            .map_err(Halt::Failure)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// `text` in pieces of about `size` bytes, cut between characters.
fn pieces(text: &str, size: usize) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = size.min(rest.len());
        while !rest.is_char_boundary(end) {
            end += 1;
        }
        let (piece, tail) = rest.split_at(end);
        rest = tail;
        Some(piece)
    })
}

/// `XMLPropertyListParser.parse(InputStream)` of `input`, whose pages are `pages`, its objects
/// handed to `values` as they come: what stops it, dd-plist's failure or that of `values`.
fn walk<V: Values>(input: &[u8], pages: Pages<'_>, values: &mut V) -> Result<(), Halt<V::Error>> {
    let mut walk = Walk {
        values,
        frames: Vec::new(),
        open: 0,
        root_is_plist: None,
        root_children: 0,
        document_type: false,
        halted: None,
    };
    let parsed = xml::parse_pages(input, pages, &mut |event| walk.event(event));
    let paging = Paging::new(pages);
    let halted = walk.halted.take();
    if let Some(Halt::Failure(failure)) = halted {
        return Err(Halt::Failure(failure));
    }
    if let Err(error) = parsed {
        if paging.find(input, b":").is_some() || paging.find(input, b"xmlns").is_some() {
            return Err(Problem::Unread(
                "a malformed XML property list with names a namespace-unaware parser may accept",
            )
            .into());
        }
        return Err(Problem::Failed(match error {
            XmlError::CharConversion => "a byte sequence its encoding does not allow".to_owned(),
            XmlError::Malformed(reason) => reason,
            XmlError::EntityExpansionLimit => "more entity expansions than allowed".to_owned(),
        })
        .into());
    }
    let root_is_plist = walk
        .root_is_plist
        .expect("a well-formed document has a root element");
    let named_plist = if walk.document_type {
        document_type_name(input, &paging)? == "plist"
    } else {
        root_is_plist
    };
    if !named_plist {
        return Err(
            Problem::Failed("The given XML document is not a property list.".to_owned()).into(),
        );
    }
    if root_is_plist {
        match walk.root_children {
            0 => {
                return Err(Problem::Failed(
                    "The given XML property list has no root element!".to_owned(),
                )
                .into())
            }
            1 => {}
            _ => {
                return Err(Problem::Failed(
                    "The given XML property list has more than one root element!".to_owned(),
                )
                .into())
            }
        }
    }
    halted.map_or(Ok(()), Err)
}

/// Refuses what the port does not read as the JDK's DOM builder does: a list in an encoding
/// that does not write ASCII as ASCII, which the checks below cannot scan, and entities beyond
/// the predefined ones, which the builder keeps as entity reference nodes.
fn check_supported(input: &[u8], paging: &Paging) -> Result<(), Problem> {
    let ascii_compatible = !input.starts_with(&[0xFE, 0xFF])
        && !input.starts_with(&[0xFF, 0xFE])
        && !input.starts_with(&[0x4C, 0x6F, 0xA7, 0x94])
        && paging.find(input, &[0]).is_none();
    if !ascii_compatible {
        return Err(Problem::Unread(
            "an XML property list in an encoding that does not write ASCII as ASCII",
        ));
    }
    if paging.find(input, b"<!ENTITY").is_some() {
        return Err(Problem::Unread(
            "an XML property list that declares entities",
        ));
    }
    let predefined: [&[u8]; 6] = [b"#", b"lt;", b"gt;", b"amp;", b"apos;", b"quot;"];
    let mut rest = input;
    while let Some(at) = paging.find(rest, b"&") {
        rest = &rest[at + 1..];
        if !predefined.iter().any(|name| rest.starts_with(name)) {
            return Err(Problem::Unread(
                "an XML property list that refers to an entity beyond the predefined ones",
            ));
        }
    }
    Ok(())
}

/// The name a document's one `<!DOCTYPE` declares, read from its ASCII-compatible bytes.
fn document_type_name(input: &[u8], paging: &Paging) -> Result<String, Problem> {
    const DOCTYPE: &[u8] = b"<!DOCTYPE";
    let first = paging.find(input, DOCTYPE);
    let second = first.and_then(|at| paging.find(&input[at + DOCTYPE.len()..], DOCTYPE));
    let (Some(at), None) = (first, second) else {
        return Err(Problem::Unread(
            "an XML property list that spells <!DOCTYPE more than once",
        ));
    };
    let rest = &input[at + DOCTYPE.len()..];
    let rest = &rest[rest
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count()..];
    let end = rest
        .iter()
        .position(|&byte| byte.is_ascii_whitespace() || byte == b'>' || byte == b'[')
        .unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end])
        .map(str::to_owned)
        .map_err(|_| Problem::Unread("a document type name beyond ASCII"))
}

/// What the first walk found the second needs: the kind of a root dictionary, how each
/// dictionary that puts a key again is written, and the lengths of data objects too large to
/// hold.
pub(super) struct Survey {
    root_kind: Option<ListKind>,
    plans: HashMap<u64, DictPlan>,
    large_data: HashMap<u64, u64>,
    checked: Result<(), Problem>,
}

impl Survey {
    /// What `BPListDetector.detectXMLOnKeys` makes of the root object's keys, if it is a
    /// dictionary.
    pub(super) fn root_kind(&self) -> Option<ListKind> {
        self.root_kind
    }

    /// Why the port does not write a date of the list, if it does not.
    pub(super) fn checked(&self) -> Result<(), Problem> {
        self.checked.clone()
    }
}

/// How a dictionary that puts a key again is written, by the places of its entries: in a key's
/// first place, the value it was last put with; in its later places, nothing.
#[derive(Default)]
struct DictPlan {
    displaced: HashMap<u32, Owned>,
    dropped: HashSet<u32>,
}

/// An object built whole: a value put again with a key a dictionary already holds.
enum Owned {
    Dict(OwnedDict),
    Array(Vec<Owned>),
    String(String),
    Data(Vec<u8>),
    Leaf(Leaf),
}

/// An `NSDictionary` built whole: a `LinkedHashMap`, a key put again keeping its place and
/// taking the new value.
#[derive(Default)]
struct OwnedDict {
    entries: Vec<(String, Owned)>,
    positions: HashMap<String, usize>,
}

impl OwnedDict {
    fn put(&mut self, key: String, value: Owned) {
        match self.positions.entry(key) {
            Entry::Occupied(position) => self.entries[*position.get()].1 = value,
            Entry::Vacant(position) => {
                let key = position.key().clone();
                position.insert(self.entries.len());
                self.entries.push((key, value));
            }
        }
    }
}

/// An object being built.
enum Partial {
    /// A dictionary, and the key whose value comes next.
    Dict(OwnedDict, Option<String>),
    Array(Vec<Owned>),
    String(String),
    Data(Vec<u8>),
}

/// Builds one object whole from its values.
#[derive(Default)]
struct Builder {
    open: Vec<Partial>,
    built: Option<Owned>,
}

impl Builder {
    fn add(&mut self, value: Value<'_>) {
        match value {
            Value::StartDict => self.open.push(Partial::Dict(OwnedDict::default(), None)),
            Value::StartArray => self.open.push(Partial::Array(Vec::new())),
            Value::StartString => self.open.push(Partial::String(String::new())),
            Value::StartData => self.open.push(Partial::Data(Vec::new())),
            Value::Key(name) => match self.open.last_mut() {
                Some(Partial::Dict(_, key)) => *key = Some(name),
                _ => unreachable!("a key is read in a dictionary"),
            },
            Value::StringText(text) => match self.open.last_mut() {
                Some(Partial::String(string)) => string.push_str(text),
                _ => unreachable!("a string's text is read in the string"),
            },
            Value::DataBytes(bytes) => match self.open.last_mut() {
                Some(Partial::Data(data)) => data.extend_from_slice(bytes),
                _ => unreachable!("a data object's bytes are read in the data object"),
            },
            Value::Leaf(leaf) => self.complete(Owned::Leaf(leaf)),
            Value::EndDict | Value::EndArray | Value::EndString | Value::EndData => {
                let built = match self.open.pop() {
                    Some(Partial::Dict(dict, _)) => Owned::Dict(dict),
                    Some(Partial::Array(items)) => Owned::Array(items),
                    Some(Partial::String(text)) => Owned::String(text),
                    Some(Partial::Data(bytes)) => Owned::Data(bytes),
                    None => unreachable!("an end closes an object the builder started"),
                };
                self.complete(built);
            }
        }
    }

    fn complete(&mut self, built: Owned) {
        match self.open.last_mut() {
            None => self.built = Some(built),
            Some(Partial::Dict(dict, key)) => {
                dict.put(
                    key.take().expect("a dictionary's value follows its key"),
                    built,
                );
            }
            Some(Partial::Array(items)) => items.push(built),
            Some(Partial::String(_) | Partial::Data(_)) => {
                unreachable!("a string or data object holds no object")
            }
        }
    }
}

/// A dictionary the first walk has open, numbered in the order dictionaries start, with the
/// places its keys were first put in.
struct PlannedDict {
    ordinal: u64,
    entries: u32,
    keys: HashMap<String, u32>,
    plan: DictPlan,
}

enum Planned {
    Dict(PlannedDict),
    Array,
}

/// The value of a key put again being built, with the places of the key's first entry and of
/// this one.
struct Building {
    first: u32,
    entry: u32,
    builder: Builder,
}

/// The first walk's taker of objects.
struct Planner<'d> {
    dates: &'d Dates,
    checked: Result<(), Problem>,
    dicts: u64,
    data: u64,
    data_ordinal: u64,
    data_length: u64,
    open: Vec<Planned>,
    plans: HashMap<u64, DictPlan>,
    large_data: HashMap<u64, u64>,
    started: bool,
    root_kind: Option<KindKeys>,
    /// The key of the root dictionary whose value comes next.
    root_key: Option<String>,
    /// The places of a key put again whose value comes next.
    repeated: Option<(u32, u32)>,
    building: Option<Building>,
}

impl<'d> Planner<'d> {
    fn new(dates: &'d Dates) -> Self {
        Self {
            dates,
            checked: Ok(()),
            dicts: 0,
            data: 0,
            data_ordinal: 0,
            data_length: 0,
            open: Vec::new(),
            plans: HashMap::new(),
            large_data: HashMap::new(),
            started: false,
            root_kind: None,
            root_key: None,
            repeated: None,
            building: None,
        }
    }

    /// Notes the value starting at the root of the list, or of its root dictionary.
    fn note_root(&mut self, value: &Value<'_>) {
        if !self.started {
            self.started = true;
            if matches!(value, Value::StartDict) {
                self.root_kind = Some(KindKeys::default());
            }
            return;
        }
        if self.open.len() != 1 {
            return;
        }
        if let (Some(name), Some(kind)) = (self.root_key.take(), &mut self.root_kind) {
            if !matches!(value, Value::Leaf(Leaf::Null)) {
                kind.put(&name);
            }
        }
    }
}

impl Values for Planner<'_> {
    type Error = Infallible;

    fn value(&mut self, value: Value<'_>) -> Result<(), Infallible> {
        let ordinal = self.dicts;
        match &value {
            Value::StartDict => self.dicts += 1,
            Value::StartData => {
                self.data_ordinal = self.data;
                self.data += 1;
                self.data_length = 0;
            }
            Value::DataBytes(bytes) => self.data_length += bytes.len() as u64,
            Value::EndData if self.data_length > MAX_READ_WHOLE => {
                self.large_data.insert(self.data_ordinal, self.data_length);
            }
            Value::Leaf(Leaf::Date(seconds)) if self.checked.is_ok() => {
                self.checked = self.dates.text(*seconds).map(|_| ());
            }
            _ => {}
        }
        if value.starts_object() {
            self.note_root(&value);
            if let Some((first, entry)) = self.repeated.take() {
                self.building = Some(Building {
                    first,
                    entry,
                    builder: Builder::default(),
                });
            }
        }
        if let Some(building) = &mut self.building {
            building.builder.add(value);
            if let Some(built) = building.builder.built.take() {
                let (first, entry) = (building.first, building.entry);
                self.building = None;
                let Some(Planned::Dict(dict)) = self.open.last_mut() else {
                    unreachable!("a key put again is in an open dictionary");
                };
                dict.plan.displaced.insert(first, built);
                dict.plan.dropped.insert(entry);
            }
            return Ok(());
        }
        match value {
            Value::StartDict => self.open.push(Planned::Dict(PlannedDict {
                ordinal,
                entries: 0,
                keys: HashMap::new(),
                plan: DictPlan::default(),
            })),
            Value::Key(name) => {
                let at_root = self.open.len() == 1 && self.root_kind.is_some();
                let Some(Planned::Dict(dict)) = self.open.last_mut() else {
                    unreachable!("a key is read in a dictionary");
                };
                let entry = dict.entries;
                dict.entries += 1;
                if at_root {
                    self.root_key = Some(name.clone());
                }
                match dict.keys.entry(name) {
                    Entry::Vacant(first) => {
                        first.insert(entry);
                    }
                    Entry::Occupied(first) => self.repeated = Some((*first.get(), entry)),
                }
            }
            Value::EndDict => {
                let Some(Planned::Dict(dict)) = self.open.pop() else {
                    unreachable!("a dictionary's end closes the dictionary");
                };
                if !dict.plan.dropped.is_empty() {
                    self.plans.insert(dict.ordinal, dict.plan);
                }
            }
            Value::StartArray => self.open.push(Planned::Array),
            Value::EndArray => {
                self.open.pop();
            }
            _ => {}
        }
        Ok(())
    }
}

/// The first walk: fails where dd-plist fails on `input`, whose pages are `pages`, or the port
/// does not read it as Tika does, and plans the second, checking dates as `dates` writes them.
pub(super) fn survey(input: &[u8], pages: Pages<'_>, dates: &Dates) -> Result<Survey, Problem> {
    check_supported(input, &Paging::new(pages))?;
    let mut planner = Planner::new(dates);
    walk(input, pages, &mut planner).map_err(|halt| match halt {
        Halt::Problem(problem) => problem,
        Halt::Failure(never) => match never {},
    })?;
    Ok(Survey {
        root_kind: planner.root_kind.map(KindKeys::kind),
        plans: planner.plans,
        large_data: planner.large_data,
        checked: planner.checked,
    })
}

/// A data object written as its bytes come: its base64 so far, and its bytes spooled.
struct StreamedData {
    base64: Base64,
    spool: Spool,
}

/// An open object of the second walk.
enum Written<'s> {
    Dict {
        plan: Option<&'s DictPlan>,
        entries: u32,
    },
    Array,
}

/// The second walk's taker of objects, which writes them.
struct Writer<'s, 'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    dates: &'s Dates,
    survey: &'s Survey,
    dicts: u64,
    data: u64,
    open: Vec<Written<'s>>,
    /// Whether the next object is left out.
    skip_next: bool,
    /// How many objects are open in the object being left out.
    skipping: usize,
    streamed: Option<StreamedData>,
}

fn spool_failed(error: std::io::Error) -> Failure {
    Failure::Extraction(Error::IoError(format!(
        "Could not spool a data object of a property list: {error}"
    )))
}

impl Values for Writer<'_, '_, '_> {
    type Error = Failure;

    fn value(&mut self, value: Value<'_>) -> Result<(), Failure> {
        let dict = self.dicts;
        let data = self.data;
        match value {
            Value::StartDict => self.dicts += 1,
            Value::StartData => self.data += 1,
            _ => {}
        }
        if self.skipping > 0 {
            if value.opens() {
                self.skipping += 1;
            } else if value.closes() {
                self.skipping -= 1;
            }
            return Ok(());
        }
        if self.skip_next && value.starts_object() {
            self.skip_next = false;
            self.skipping = usize::from(value.opens());
            return Ok(());
        }
        let xhtml = &mut *self.xhtml;
        match value {
            Value::StartDict => {
                xhtml.start_element("dict")?;
                let plan = self.survey.plans.get(&dict);
                self.open.push(Written::Dict { plan, entries: 0 });
            }
            Value::Key(name) => {
                let Some(Written::Dict { plan, entries }) = self.open.last_mut() else {
                    unreachable!("a key is read in a dictionary");
                };
                let (plan, entry) = (*plan, *entries);
                *entries += 1;
                if plan.is_some_and(|plan| plan.dropped.contains(&entry)) {
                    self.skip_next = true;
                    return Ok(());
                }
                xhtml.element("key", &name)?;
                if let Some(last) = plan.and_then(|plan| plan.displaced.get(&entry)) {
                    write_owned(xhtml, self.dates, last)?;
                    self.skip_next = true;
                }
            }
            Value::EndDict => {
                self.open.pop();
                xhtml.end_element("dict")?;
            }
            Value::StartArray => {
                xhtml.start_element("array")?;
                self.open.push(Written::Array);
            }
            Value::EndArray => {
                self.open.pop();
                xhtml.end_element("array")?;
            }
            Value::Leaf(leaf) => write_leaf(xhtml, self.dates, leaf)?,
            Value::StartString => xhtml.start_element("string")?,
            Value::StringText(text) => xhtml.characters(text)?,
            Value::EndString => xhtml.end_element("string")?,
            Value::StartData => {
                xhtml.start_element("data")?;
                let expected = self.survey.large_data.get(&data).copied();
                self.streamed = Some(StreamedData {
                    base64: Base64::default(),
                    spool: Spool::new(expected),
                });
            }
            Value::DataBytes(bytes) => {
                let streamed = self
                    .streamed
                    .as_mut()
                    .expect("a data object's bytes are read in the data object");
                let mut text = String::new();
                streamed.base64.encode(bytes, &mut text);
                xhtml.characters(&text)?;
                streamed.spool.write_all(bytes).map_err(spool_failed)?;
            }
            Value::EndData => {
                let streamed = self
                    .streamed
                    .take()
                    .expect("a data object's end closes the data object");
                let mut text = String::new();
                streamed.base64.finish(&mut text);
                xhtml.characters(&text)?;
                let bytes = streamed.spool.finish().map_err(spool_failed)?;
                embed(xhtml, &bytes)?;
                xhtml.end_element("data")?;
            }
        }
        Ok(())
    }
}

fn write_leaf(xhtml: &mut Xhtml, dates: &Dates, leaf: Leaf) -> Result<(), Failure> {
    match leaf {
        Leaf::Null => write_null(xhtml)?,
        Leaf::Number(number) => write_number(xhtml, number)?,
        Leaf::Date(seconds) => write_date(xhtml, dates, seconds)?,
    }
    Ok(())
}

/// Writes an object built whole.
fn write_owned(xhtml: &mut Xhtml, dates: &Dates, owned: &Owned) -> Result<(), Failure> {
    match owned {
        Owned::Dict(dict) => {
            xhtml.start_element("dict")?;
            for (key, value) in &dict.entries {
                xhtml.element("key", key)?;
                write_owned(xhtml, dates, value)?;
            }
            xhtml.end_element("dict")?;
        }
        Owned::Array(items) => {
            xhtml.start_element("array")?;
            for item in items {
                write_owned(xhtml, dates, item)?;
            }
            xhtml.end_element("array")?;
        }
        Owned::String(text) => text_element(xhtml, "string", text)?,
        Owned::Data(bytes) => write_data(xhtml, &Input::Borrowed(bytes))?,
        Owned::Leaf(leaf) => write_leaf(xhtml, dates, *leaf)?,
    }
    Ok(())
}

/// The second walk: writes the object of `input`, whose pages are `pages`, as `survey` planned,
/// its dates as `dates` writes them.
pub(super) fn write(
    input: &[u8],
    pages: Pages<'_>,
    survey: &Survey,
    dates: &Dates,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let mut writer = Writer {
        xhtml,
        dates,
        survey,
        dicts: 0,
        data: 0,
        open: Vec::new(),
        skip_next: false,
        skipping: 0,
        streamed: None,
    };
    walk(input, pages, &mut writer).map_err(|halt| match halt {
        Halt::Failure(failure) => failure,
        Halt::Problem(problem) => Failure::Malformed(format!(
            "the property list read otherwise the second time: {problem:?}"
        )),
    })
}

/// `new NSNumber(text)`: a boolean by name, a special real, a decimal or `0x` hexadecimal
/// `long`, else a `double`; `None` where it throws.
fn number(text: &str) -> Option<Number> {
    let named = |name: &str| java::equals_ignore_case(text, name);
    if named("true") || named("YES") {
        return Some(Number::Boolean(true));
    }
    if named("false") || named("NO") {
        return Some(Number::Boolean(false));
    }
    if named("nan") {
        return Some(Number::Real(f64::NAN));
    }
    if named("+infinity") {
        return Some(Number::Real(f64::INFINITY));
    }
    if named("-infinity") {
        return Some(Number::Real(f64::NEG_INFINITY));
    }
    let integer = match text.strip_prefix("0x") {
        Some(hexadecimal) => java::parse_hex_long(hexadecimal),
        None => java::parse_long(text),
    };
    integer
        .map(Number::Integer)
        .or_else(|| java::parse_double(text).map(Number::Real))
}

/// `new NSData(base64)` of text that comes in pieces: the text without Java's `\s` characters,
/// decoded by dd-plist's `Base64.decode` from its US-ASCII bytes.
#[derive(Default)]
struct Base64Decoder {
    quartet: [u8; 4],
    filled: usize,
    /// How many characters there are other than white space.
    count: usize,
    /// Whether a quartet that ends with `=` ended the decoding.
    ended: bool,
}

/// The six bits a base64 character stands for, `0xFF` for the padding `=`.
fn sextet(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        b'=' => Some(0xFF),
        _ => None,
    }
}

impl Base64Decoder {
    /// Decodes `text` onto `decoded`; `None` where `Base64.decode` throws on it.
    fn feed(&mut self, text: &str, decoded: &mut Vec<u8>) -> Option<()> {
        for character in text.chars() {
            if java::is_regex_space(character) {
                continue;
            }
            self.count += 1;
            if self.ended {
                continue;
            }
            let byte = u8::try_from(character)
                .ok()
                .filter(u8::is_ascii)
                .unwrap_or(b'?');
            sextet(byte)?;
            self.quartet[self.filled] = byte;
            self.filled += 1;
            if self.filled < 4 {
                continue;
            }
            self.filled = 0;
            let bits = self.quartet.iter().fold(0u32, |bits, &byte| {
                bits << 6 | sextet(byte).unwrap_or_default()
            });
            let [_, high, middle, low] = bits.to_be_bytes();
            let width = if self.quartet[2] == b'=' {
                1
            } else if self.quartet[3] == b'=' {
                2
            } else {
                3
            };
            decoded.extend_from_slice(&[high, middle, low][..width]);
            if byte == b'=' {
                self.ended = true;
            }
        }
        Some(())
    }

    /// Whether `Base64.decode` decodes the whole text: none of it, or at least a quartet's
    /// worth of characters.
    fn complete(&self) -> bool {
        self.count == 0 || self.count >= 4
    }
}

/// `new NSDate(text)`'s seconds since 2001 for a date in the canonical form
/// `yyyy-MM-dd'T'HH:mm:ss'Z'` of a year from 1 to 9999, each field in range. The lenient
/// `SimpleDateFormat` parse dd-plist makes of any other text is not ported.
fn date(text: &str) -> Result<f64, Problem> {
    let unread = || Problem::Unread("a date the port does not parse as SimpleDateFormat does");
    let bytes = text.as_bytes();
    let shape = b"dddd-dd-ddTdd:dd:ddZ";
    if bytes.len() != shape.len()
        || !bytes.iter().zip(shape).all(|(&byte, &expected)| {
            if expected == b'd' {
                byte.is_ascii_digit()
            } else {
                byte == expected
            }
        })
    {
        return Err(unread());
    }
    let field = |range: std::ops::Range<usize>| -> i32 {
        text[range].parse().expect("the field is ASCII digits")
    };
    let (year, month, day) = (field(0..4), field(5..7), field(8..10));
    let (hour, minute, second) = (field(11..13), field(14..16), field(17..19));
    let days_in_month = match month {
        2 if java::time::civil::is_leap_year(i64::from(year)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if year == 0
        || !(1..=12).contains(&month)
        || !(1..=days_in_month).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(unread());
    }
    let millis = CalendarFields {
        year,
        month: month - 1,
        day,
        hour,
        minute,
        second,
        millisecond: 0,
    }
    .epoch_millis(&TimeZone::fixed(0))
    .ok_or_else(unread)?;
    Ok((millis - EPOCH) as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The objects a walk reads, written compactly: `{` and `}` around a dictionary, `key=`
    /// before each value, `[` and `]` around an array, a string's text in quotes, a data
    /// object's bytes in hexadecimal between `<` and `>`, a date's seconds after `@`.
    #[derive(Default)]
    struct Recorder(String);

    impl Values for Recorder {
        type Error = Infallible;

        fn value(&mut self, value: Value<'_>) -> Result<(), Infallible> {
            let recorded = &mut self.0;
            match value {
                Value::StartDict => recorded.push('{'),
                Value::Key(name) => recorded.push_str(&format!(" {name}=")),
                Value::EndDict => recorded.push('}'),
                Value::StartArray => recorded.push('['),
                Value::EndArray => recorded.push(']'),
                Value::Leaf(Leaf::Null) => recorded.push_str("null"),
                Value::Leaf(Leaf::Number(number)) => recorded.push_str(&number.text()),
                Value::Leaf(Leaf::Date(seconds)) => recorded.push_str(&format!("@{seconds}")),
                Value::StartString | Value::EndString => recorded.push('"'),
                Value::StringText(text) => recorded.push_str(text),
                Value::StartData => recorded.push('<'),
                Value::DataBytes(bytes) => {
                    for byte in bytes {
                        recorded.push_str(&format!("{byte:02X}"));
                    }
                }
                Value::EndData => recorded.push('>'),
            }
            Ok(())
        }
    }

    /// The objects of the list `xml` as [`Recorder`] writes them, or why the port does not read
    /// it as dd-plist does.
    fn read(xml: &str) -> Result<String, Problem> {
        let input = xml.as_bytes();
        check_supported(input, &Paging::new(Pages::IN_MEMORY))?;
        let mut recorder = Recorder::default();
        walk(input, Pages::IN_MEMORY, &mut recorder).map_err(|halt| match halt {
            Halt::Problem(problem) => problem,
            Halt::Failure(never) => match never {},
        })?;
        Ok(recorder.0)
    }

    #[test]
    fn reads_objects_as_dd_plist_does() {
        let objects = read(
            "<?xml version='1.0'?><!DOCTYPE plist PUBLIC '-//Apple//DTD PLIST 1.0//EN' 'x.dtd'>\
             <plist><dict><key>a</key><string>x<!--c-->y<![CDATA[z]]><?p?>w</string>\
             <key>b</key><integer>0x1F</integer><key>c</key><real> 2</real><key>d</key><true/>\
             <key>a</key><unknown><string>n</string></unknown><key>e</key><data>QU\nJD</data>\
             <key>f</key><date>2001-01-02T00:00:00Z</date><key>g</key><array><b/>t<false/></array>\
             </dict></plist>",
        );
        assert_eq!(
            objects,
            Ok(
                "{ a=\"xyz\" b=31 c=2.0 d=true a=null e=<414243> f=@86400 g=[nullfalse]}"
                    .to_owned()
            )
        );
    }

    #[test]
    fn decodes_data_in_pieces_as_whole() {
        let mut decoder = Base64Decoder::default();
        let mut decoded = Vec::new();
        for piece in ["Q", "U\nJ", "DQ", "Q==", "*"] {
            decoder.feed(piece, &mut decoded).unwrap();
        }
        assert!(decoder.complete());
        assert_eq!(decoded, b"ABCA");
        assert_eq!(pieces("a\u{e9}b", 2).collect::<Vec<_>>(), ["a\u{e9}", "b"]);
    }

    #[test]
    fn fails_where_dd_plist_fails() {
        for xml in [
            "<plist/>",
            "<plist><a/><b/></plist>",
            "<root><string/></root>",
            "<!DOCTYPE other><plist><string/></plist>",
            "<plist><dict><key>a</key></dict></plist>",
            "<plist><integer>x</integer></plist>",
            "<plist><data>QQ</data></plist>",
            "<plist><data>QU*D</data></plist>",
        ] {
            assert!(matches!(read(xml), Err(Problem::Failed(_))), "{xml}");
        }
        assert_eq!(read("<!DOCTYPE plist><dict/>"), Ok("{}".to_owned()));
    }

    #[test]
    fn leaves_what_it_does_not_read_to_tika() {
        for xml in [
            "<!DOCTYPE plist [<!ENTITY e 'v'>]><plist><string>&e;</string></plist>",
            "<plist><string>&e;</string></plist>",
            "<plist><date>2001-1-02T00:00:00Z</date></plist>",
            "<p:plist><string/></plist>",
        ] {
            assert!(matches!(read(xml), Err(Problem::Unread(_))), "{xml}");
        }
    }
}
