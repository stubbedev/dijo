//! The interpreter: as much of the PostScript language as it takes to follow a program to the
//! strings it shows and the fonts it shows them in. It keeps the operand and dictionary stacks,
//! names, strings, arrays and dictionaries, procedures and their control flow, `save` and
//! `restore` of definitions, fonts as dictionaries (found, scaled, re-encoded and defined),
//! the current point, and the data a program reads from its own file. It does not paint or
//! transform: an operator that paints only takes its operands, and a value it cannot know
//! (a transformed coordinate, a time) is unknown, which a string shown or a move made with it is
//! too. A program that errs goes on with its next token, as far as the interpreter can tell
//! errors at all; one that runs past its steps or the memory its size allows is stopped.

mod operate;

use super::fonts::{self, Resident, Vector};
use super::layout::{Advance, Layout, PageText, Position, Shown, DEFAULT_EM};
use super::operators::Op;
use super::scanner::{Object, Scanner, Token};
use crate::formats::pdf::afm::Standard14;
use crate::formats::xhtml::{within_budget, Stop};
use crate::hash::FastMap;

/// The steps a program of `n` bytes may take: `BASE_STEPS + STEPS_PER_BYTE * n`.
const BASE_STEPS: u64 = 1 << 22;
const STEPS_PER_BYTE: u64 = 16;
/// The elements (bytes of strings, values of arrays, entries of dictionaries and bytes of names)
/// a program of `n` bytes may create: `BASE_ELEMENTS + ELEMENTS_PER_BYTE * n`.
const BASE_ELEMENTS: u64 = 1 << 20;
const ELEMENTS_PER_BYTE: u64 = 4;
/// How deep procedures may call each other.
const MAX_DEPTH: usize = 128;
const MAX_OPERANDS: usize = 1 << 16;
const MAX_DICTIONARIES: usize = 1 << 10;
const MAX_SAVED_STATES: usize = 1 << 10;
/// The longest string or array a program may create at once.
const MAX_OBJECT_LENGTH: usize = 1 << 20;
/// The units of a Type 1 font's glyph space to its em.
const GLYPH_SPACE: f64 = 1000.0;
const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
const TYPE_1_FONT_MATRIX: [f64; 6] = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
/// The font type of a composite font, whose strings select glyphs through a CMap.
const COMPOSITE_FONT_TYPE: f64 = 0.0;
const USER_DEFINED_FONT_TYPE: f64 = 3.0;
const NO_STRING_VALUE: &[u8] = b"--nostringval--";

/// Why the text of a page is not all there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fault {
    /// A string shown was computed in a way the interpreter does not follow.
    ComputedString,
    /// A string was shown in no font, or one the interpreter does not know.
    UnknownFont,
    /// A string was shown in a font without an encoding vector.
    NoEncoding,
    /// A string was shown in a composite font, which the interpreter does not decode.
    CompositeFont,
    /// The program went on to the next page its structuring comments mark without showing this
    /// one, as far as the interpreter follows it.
    UnfollowedPage,
    /// The program ran past the steps or the memory its size allows, and was stopped.
    ExecutionLimit,
}

impl Fault {
    pub(super) fn name(self) -> &'static str {
        match self {
            Fault::ComputedString => "computed-string",
            Fault::UnknownFont => "unknown-font",
            Fault::NoEncoding => "no-encoding",
            Fault::CompositeFont => "composite-font",
            Fault::UnfollowedPage => "unfollowed-page",
            Fault::ExecutionLimit => "execution-limit",
        }
    }
}

/// A page the program showed: its text, and why it is not all there.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Page {
    pub(super) text: PageText,
    pub(super) faults: Vec<Fault>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct NameId(usize);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StringId(usize);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArrayId(usize);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DictId(usize);

/// Whether a name or array is executed when the interpreter meets it, or pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Literal,
    Executable,
}

/// Where data a filter reads from the program's own file ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminator {
    Base85,
    Hex,
}

impl Terminator {
    fn marker(self) -> &'static [u8] {
        match self {
            Terminator::Base85 => b"~>",
            Terminator::Hex => b">",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Value {
    Number(f64),
    Bool(bool),
    Name(NameId, Access),
    String(StringId),
    Array(ArrayId, Access),
    Dict(DictId),
    Operator(Op),
    Mark,
    Null,
    /// The program's own file, `currentfile`.
    File,
    /// A filter over the program's own file, ending where its terminator, if known, says.
    Filter(Option<Terminator>),
    Save(usize),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Name(NameId),
    Integer(i64),
}

/// A dictionary, its entries in the order they were made; a font's glyph procedures carry the
/// metrics of the resident font they stand for.
#[derive(Debug, Clone, Default)]
struct Dict {
    slots: Vec<Option<(Key, Value)>>,
    index: FastMap<Key, usize>,
    metrics: Option<Standard14>,
}

impl Dict {
    fn get(&self, key: Key) -> Option<Value> {
        self.index
            .get(&key)
            .and_then(|&slot| self.slots[slot])
            .map(|(_, value)| value)
    }

    fn put(&mut self, key: Key, value: Value) -> Option<Value> {
        match self.index.get(&key) {
            Some(&slot) => self.slots[slot].replace((key, value)).map(|(_, old)| old),
            None => {
                self.index.insert(key, self.slots.len());
                self.slots.push(Some((key, value)));
                None
            }
        }
    }

    fn remove(&mut self, key: Key) -> Option<Value> {
        let slot = self.index.remove(&key)?;
        self.slots[slot].take().map(|(_, value)| value)
    }

    fn entries(&self) -> impl Iterator<Item = (Key, Value)> + '_ {
        self.slots.iter().flatten().copied()
    }

    fn len(&self) -> usize {
        self.index.len()
    }
}

/// The names the interpreter meets, each held once.
#[derive(Debug, Default)]
struct Names {
    ids: FastMap<Box<[u8]>, NameId>,
    texts: Vec<Box<[u8]>>,
    operators: Vec<Option<Op>>,
}

impl Names {
    fn intern(&mut self, text: &[u8]) -> NameId {
        if let Some(&id) = self.ids.get(text) {
            return id;
        }
        let id = NameId(self.texts.len());
        self.ids.insert(text.into(), id);
        self.texts.push(text.into());
        self.operators.push(Op::named(text));
        id
    }

    fn text(&self, name: NameId) -> &[u8] {
        &self.texts[name.0]
    }

    fn operator(&self, name: NameId) -> Option<Op> {
        self.operators[name.0]
    }
}

/// The names the interpreter reads fonts and images by.
#[derive(Debug, Clone, Copy)]
struct Known {
    font_name: NameId,
    font_type: NameId,
    font_matrix: NameId,
    font_bbox: NameId,
    encoding: NameId,
    char_strings: NameId,
    fid: NameId,
    font: NameId,
    width: NameId,
    height: NameId,
    bits_per_component: NameId,
    decode: NameId,
    data_source: NameId,
    image_mask: NameId,
}

impl Known {
    fn interned(names: &mut Names) -> Known {
        Known {
            font_name: names.intern(b"FontName"),
            font_type: names.intern(b"FontType"),
            font_matrix: names.intern(b"FontMatrix"),
            font_bbox: names.intern(b"FontBBox"),
            encoding: names.intern(b"Encoding"),
            char_strings: names.intern(b"CharStrings"),
            fid: names.intern(b"FID"),
            font: names.intern(b"Font"),
            width: names.intern(b"Width"),
            height: names.intern(b"Height"),
            bits_per_component: names.intern(b"BitsPerComponent"),
            decode: names.intern(b"Decode"),
            data_source: names.intern(b"DataSource"),
            image_mask: names.intern(b"ImageMask"),
        }
    }
}

/// What `gsave` keeps: the font and the current point.
#[derive(Debug, Clone, Copy)]
struct Graphics {
    font: Option<DictId>,
    position: Position,
}

/// What `save` keeps: how far the journal of definitions went, and the graphics state.
#[derive(Debug, Clone, Copy)]
struct Saved {
    journal: usize,
    graphics: Graphics,
    stacked: usize,
}

/// A definition made while a `save` holds, with the value it replaced.
#[derive(Debug, Clone, Copy)]
struct Change {
    dict: DictId,
    key: Key,
    old: Option<Value>,
}

/// Why execution stops short of the end of what it executes.
#[derive(Debug)]
enum Interrupt {
    Exit,
    Stop,
    Quit,
    Exhausted,
    Output(Stop),
}

impl From<Stop> for Interrupt {
    fn from(stop: Stop) -> Self {
        Interrupt::Output(stop)
    }
}

type Run<T = ()> = Result<T, Interrupt>;

/// What a string shown in the current font reads as.
#[derive(Debug)]
struct Decoded {
    text: String,
    glyphs: u32,
    unmapped: u32,
    /// The string's width from its font's metrics, where the font has them.
    width: Option<f64>,
    em: f64,
}

/// What a show operator adds to each glyph's advance.
#[derive(Debug, Clone, Copy)]
enum Spacing {
    None,
    /// `ashow`: to every glyph.
    Letters(f64),
    /// `widthshow`: to each glyph of one code.
    Words {
        code: u8,
        extra: f64,
    },
    /// `awidthshow`.
    Both {
        code: u8,
        word: f64,
        letter: f64,
    },
    /// `xshow`, `xyshow` and `yshow`: the string's width from the program's displacements.
    Displaced(Option<f64>),
}

/// The kinds of the image operators' operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sampled {
    Image,
    Mask,
    Color,
}

/// The interpreter, running one program.
pub(super) struct Machine<'a, 's> {
    scanner: Scanner<'a>,
    names: Names,
    known: Known,
    strings: Vec<Vec<u8>>,
    arrays: Vec<Vec<Value>>,
    dicts: Vec<Dict>,
    elements: u64,
    element_limit: u64,
    steps: u64,
    step_limit: u64,
    operands: Vec<Value>,
    /// The dictionaries `begin` put on the dictionary stack, above `userdict` and `systemdict`.
    begun: Vec<DictId>,
    user: DictId,
    system: DictId,
    font_directory: DictId,
    vectors: FastMap<Vector, ArrayId>,
    resources: FastMap<(NameId, Key), Value>,
    font: Option<DictId>,
    layout: Layout,
    graphics: Vec<Graphics>,
    saves: Vec<Saved>,
    journal: Vec<Change>,
    faults: Vec<Fault>,
    marks: PageMarks,
    sink: &'s mut dyn FnMut(Page) -> Result<(), Stop>,
}

/// The pages the structuring comments mark, outside the documents the program includes.
#[derive(Debug, Clone, Copy, Default)]
struct PageMarks {
    /// How deep in included documents (`%%BeginDocument`) the program is.
    included: usize,
    /// The page last marked, if any.
    marked: Option<MarkedPage>,
    /// The program ran past what its size allows, and is no longer followed.
    exhausted: bool,
}

/// Whether the program has shown a page since the last page mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkedPage {
    Shown,
    Unshown,
}

/// Runs `program`, handing `sink` each page as it is shown, and the page the program leaves
/// unshown at its end, if it shows anything.
pub(super) fn run(
    program: &[u8],
    sink: &mut dyn FnMut(Page) -> Result<(), Stop>,
) -> Result<(), Stop> {
    let mut machine = Machine::new(program, sink);
    machine.interpret()?;
    machine.end_marked_page()?;
    machine.end_page()
}

fn number(value: Value) -> Option<f64> {
    match value {
        Value::Number(number) => Some(number),
        _ => None,
    }
}

fn integer(value: Value) -> Option<i64> {
    number(value)
        .filter(|number| number.fract() == 0.0 && number.abs() < 9.0e15)
        .map(|number| number as i64)
}

fn length(value: Value) -> Option<usize> {
    integer(value).and_then(|length| usize::try_from(length).ok())
}

fn multiply(m: [f64; 6], n: [f64; 6]) -> [f64; 6] {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

/// How `cvs` writes a number: an integer without a fraction.
fn number_text(value: f64) -> Vec<u8> {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        (value as i64).to_string().into_bytes()
    } else {
        value.to_string().into_bytes()
    }
}

impl<'a, 's> Machine<'a, 's> {
    fn new(program: &'a [u8], sink: &'s mut dyn FnMut(Page) -> Result<(), Stop>) -> Self {
        let size = program.len() as u64;
        let mut names = Names::default();
        let known = Known::interned(&mut names);
        let (system, user, font_directory) = (DictId(0), DictId(1), DictId(2));
        let mut machine = Machine {
            scanner: Scanner::new(program),
            names,
            known,
            strings: Vec::new(),
            arrays: Vec::new(),
            dicts: vec![Dict::default(), Dict::default(), Dict::default()],
            elements: 0,
            element_limit: BASE_ELEMENTS.saturating_add(ELEMENTS_PER_BYTE.saturating_mul(size)),
            steps: 0,
            step_limit: BASE_STEPS.saturating_add(STEPS_PER_BYTE.saturating_mul(size)),
            operands: Vec::new(),
            begun: Vec::new(),
            user,
            system,
            font_directory,
            vectors: FastMap::default(),
            resources: FastMap::default(),
            font: None,
            layout: Layout::default(),
            graphics: Vec::new(),
            saves: Vec::new(),
            journal: Vec::new(),
            faults: Vec::new(),
            marks: PageMarks::default(),
            sink,
        };
        let standard = machine.vector(Vector::Standard);
        let iso_latin_1 = machine.vector(Vector::IsoLatin1);
        let mut entries = vec![
            (&b"systemdict"[..], Value::Dict(system)),
            (b"userdict", Value::Dict(user)),
            (b"globaldict", Value::Dict(user)),
            (b"FontDirectory", Value::Dict(font_directory)),
            (b"GlobalFontDirectory", Value::Dict(font_directory)),
            (b"SharedFontDirectory", Value::Dict(font_directory)),
            (b"StandardEncoding", Value::Array(standard, Access::Literal)),
            (
                b"ISOLatin1Encoding",
                Value::Array(iso_latin_1, Access::Literal),
            ),
        ];
        for name in [&b"statusdict"[..], b"errordict", b"$error", b"serverdict"] {
            let dict = machine.new_dict(Dict::default());
            entries.push((name, Value::Dict(dict)));
        }
        for (name, value) in entries {
            let key = Key::Name(machine.names.intern(name));
            machine.dicts[system.0].put(key, value);
        }
        machine
    }

    /// Interns a name the program makes, within its memory.
    fn name(&mut self, text: &[u8]) -> Run<NameId> {
        if !self.names.ids.contains_key(text) {
            self.charge(text.len())?;
        }
        Ok(self.names.intern(text))
    }

    /// Counts what the program creates against what its size allows, and the extraction's
    /// memory against its budget.
    fn charge(&mut self, elements: usize) -> Run {
        self.elements = self.elements.saturating_add(elements.max(1) as u64);
        if self.elements > self.element_limit {
            return Err(Interrupt::Exhausted);
        }
        Ok(within_budget()?)
    }

    fn tick(&mut self, steps: u64) -> Run {
        self.steps = self.steps.saturating_add(steps);
        if self.steps > self.step_limit {
            return Err(Interrupt::Exhausted);
        }
        Ok(())
    }

    fn new_dict(&mut self, dict: Dict) -> DictId {
        self.dicts.push(dict);
        DictId(self.dicts.len() - 1)
    }

    fn alloc_string(&mut self, bytes: Vec<u8>) -> Run<StringId> {
        self.charge(bytes.len())?;
        self.strings.push(bytes);
        Ok(StringId(self.strings.len() - 1))
    }

    fn alloc_array(&mut self, values: Vec<Value>) -> Run<ArrayId> {
        self.charge(values.len())?;
        self.arrays.push(values);
        Ok(ArrayId(self.arrays.len() - 1))
    }

    fn alloc_dict(&mut self, dict: Dict) -> Run<DictId> {
        self.charge(dict.len())?;
        Ok(self.new_dict(dict))
    }

    fn string(&self, id: StringId) -> &Vec<u8> {
        &self.strings[id.0]
    }

    fn array(&self, id: ArrayId) -> &Vec<Value> {
        &self.arrays[id.0]
    }

    fn dict(&self, id: DictId) -> &Dict {
        &self.dicts[id.0]
    }

    /// The array of an encoding vector, made once.
    fn vector(&mut self, vector: Vector) -> ArrayId {
        if let Some(&id) = self.vectors.get(&vector) {
            return id;
        }
        let names: Vec<Value> = vector
            .names()
            .iter()
            .map(|name| Value::Name(self.names.intern(name.as_bytes()), Access::Literal))
            .collect();
        self.arrays.push(names);
        let id = ArrayId(self.arrays.len() - 1);
        self.vectors.insert(vector, id);
        id
    }

    /// Runs the program to its end, or to `quit`. A program that runs past what its size allows
    /// is stopped, and its structuring comments alone read on, so that each page they mark is
    /// listed as one the interpreter did not follow.
    fn interpret(&mut self) -> Result<(), Stop> {
        while let Some(token) = self.scanner.next_token() {
            if self.marks.exhausted {
                if let Token::Structuring(comment) = token {
                    self.structuring(comment)?;
                }
                continue;
            }
            let ran = match token {
                Token::Structuring(comment) => Ok(self.structuring(comment)?),
                Token::ProcStart => self.read_procedure().and_then(|proc| self.push(proc)),
                Token::ProcEnd => Ok(()),
                Token::Object(object) => self
                    .value_of(object)
                    .and_then(|value| self.execute(value, 0)),
            };
            match ran {
                Ok(()) | Err(Interrupt::Exit | Interrupt::Stop) => {}
                Err(Interrupt::Quit) => break,
                Err(Interrupt::Exhausted) => {
                    self.fault(Fault::ExecutionLimit);
                    self.marks.exhausted = true;
                }
                Err(Interrupt::Output(stop)) => return Err(stop),
            }
        }
        Ok(())
    }

    /// Follows the structuring comments that bear on reading the program: the pages they mark,
    /// a page the program did not show ending at the next mark, and the data a `%%BeginBinary`
    /// or `%%BeginData` comment announces, which is skipped.
    fn structuring(&mut self, comment: &[u8]) -> Result<(), Stop> {
        let marks = &mut self.marks;
        if comment.starts_with(b"BeginDocument") {
            marks.included += 1;
        } else if comment.starts_with(b"EndDocument") {
            marks.included = marks.included.saturating_sub(1);
        } else if marks.included == 0 {
            if comment.starts_with(b"Page:") {
                self.end_marked_page()?;
                self.marks.marked = Some(MarkedPage::Unshown);
            } else if comment.starts_with(b"Trailer") || comment.starts_with(b"EOF") {
                self.end_marked_page()?;
            }
        }
        self.skip_announced(comment);
        Ok(())
    }

    /// Ends the page last marked, if the program did not show it.
    fn end_marked_page(&mut self) -> Result<(), Stop> {
        if self.marks.marked.take() == Some(MarkedPage::Unshown) {
            self.fault(match self.marks.exhausted {
                true => Fault::ExecutionLimit,
                false => Fault::UnfollowedPage,
            });
            self.show_page()?;
        }
        Ok(())
    }

    /// Skips the data a `%%BeginBinary` or `%%BeginData` comment announces.
    fn skip_announced(&mut self, comment: &[u8]) {
        let (announced, lines) = if let Some(rest) = comment.strip_prefix(b"BeginBinary:") {
            (rest, false)
        } else if let Some(rest) = comment.strip_prefix(b"BeginData:") {
            let lines = rest
                .split(u8::is_ascii_whitespace)
                .filter(|field| !field.is_empty())
                .nth(2)
                .is_some_and(|unit| unit == b"Lines");
            (rest, lines)
        } else {
            return;
        };
        let Some(count) = announced
            .split(u8::is_ascii_whitespace)
            .find(|field| !field.is_empty())
            .and_then(|field| std::str::from_utf8(field).ok()?.parse::<usize>().ok())
        else {
            return;
        };
        if lines {
            self.scanner.skip_lines(count);
        } else {
            self.scanner.skip_bytes(count);
        }
    }

    fn value_of(&mut self, object: Object<'_>) -> Run<Value> {
        Ok(match object {
            Object::Number(value) => Value::Number(value),
            Object::Literal(name) => Value::Name(self.name(name)?, Access::Literal),
            Object::Immediate(name) => {
                let name = self.name(name)?;
                self.lookup(name)
                    .or_else(|| self.names.operator(name).map(Value::Operator))
                    .unwrap_or(Value::Unknown)
            }
            Object::Executable(name) => Value::Name(self.name(name)?, Access::Executable),
            Object::String(bytes) => Value::String(self.alloc_string(bytes)?),
        })
    }

    /// A procedure after its `{`, up to its `}`, or else up to the end of the program or the next
    /// structuring comment that marks a page or the trailer, which no procedure spans.
    fn read_procedure(&mut self) -> Run<Value> {
        let mut levels: Vec<Vec<Value>> = vec![Vec::new()];
        while let Some(token) = self.scanner.next_token() {
            match token {
                Token::Structuring(comment) if ends_procedures(comment) => {
                    self.scanner.push_back(token);
                    break;
                }
                Token::Structuring(_) => {}
                Token::ProcStart => levels.push(Vec::new()),
                Token::ProcEnd => {
                    let level = levels.pop().unwrap_or_default();
                    let proc = Value::Array(self.alloc_array(level)?, Access::Executable);
                    match levels.last_mut() {
                        Some(outer) => outer.push(proc),
                        None => return Ok(proc),
                    }
                }
                Token::Object(object) => {
                    let value = self.value_of(object)?;
                    if let Some(level) = levels.last_mut() {
                        level.push(value);
                    }
                }
            }
        }
        let mut closed = levels.pop().unwrap_or_default();
        while let Some(mut outer) = levels.pop() {
            outer.push(Value::Array(self.alloc_array(closed)?, Access::Executable));
            closed = outer;
        }
        Ok(Value::Array(self.alloc_array(closed)?, Access::Executable))
    }

    fn push(&mut self, value: Value) -> Run {
        if self.operands.len() >= MAX_OPERANDS {
            self.operands.clear();
            return Err(Interrupt::Stop);
        }
        self.operands.push(value);
        Ok(())
    }

    fn push_number(&mut self, value: Option<f64>) -> Run {
        self.push(
            value
                .filter(|value| value.is_finite())
                .map_or(Value::Unknown, Value::Number),
        )
    }

    fn pop(&mut self) -> Value {
        self.operands.pop().unwrap_or(Value::Unknown)
    }

    fn pop_number(&mut self) -> Option<f64> {
        number(self.pop())
    }

    fn pop_integer(&mut self) -> Option<i64> {
        integer(self.pop())
    }

    fn pop_point(&mut self) -> Option<(f64, f64)> {
        let y = self.pop_number();
        let x = self.pop_number();
        x.zip(y)
    }

    fn top(&self) -> Value {
        self.operands.last().copied().unwrap_or(Value::Unknown)
    }

    fn fault(&mut self, fault: Fault) {
        if !self.faults.contains(&fault) {
            self.faults.push(fault);
        }
    }

    /// The dictionary stack, from the top.
    fn dict_stack(&self) -> impl Iterator<Item = DictId> + '_ {
        self.begun
            .iter()
            .rev()
            .copied()
            .chain([self.user, self.system])
    }

    /// The dictionary nearest the top of the stack that defines `key`.
    fn defining(&self, key: Key) -> Option<DictId> {
        self.dict_stack()
            .find(|&dict| self.dict(dict).get(key).is_some())
    }

    fn lookup(&self, name: NameId) -> Option<Value> {
        self.dict_stack()
            .find_map(|dict| self.dict(dict).get(Key::Name(name)))
    }

    fn key_of(&mut self, value: Value) -> Run<Option<Key>> {
        Ok(match value {
            Value::Name(name, _) => Some(Key::Name(name)),
            Value::String(id) => {
                let text = self.string(id).clone();
                Some(Key::Name(self.name(&text)?))
            }
            other => integer(other).map(Key::Integer),
        })
    }

    fn key_value(&self, key: Key) -> Value {
        match key {
            Key::Name(name) => Value::Name(name, Access::Literal),
            Key::Integer(integer) => Value::Number(integer as f64),
        }
    }

    /// Defines `key` in `dict`, kept in the journal while a `save` holds.
    fn define(&mut self, dict: DictId, key: Key, value: Value) -> Run {
        let old = self.dicts[dict.0].put(key, value);
        if old.is_none() {
            self.charge(1)?;
        }
        if !self.saves.is_empty() {
            self.journal.push(Change { dict, key, old });
        }
        Ok(())
    }

    fn undefine(&mut self, dict: DictId, key: Key) {
        let old = self.dicts[dict.0].remove(key);
        if !self.saves.is_empty() && old.is_some() {
            self.journal.push(Change { dict, key, old });
        }
    }

    fn current_dict(&self) -> DictId {
        self.begun.last().copied().unwrap_or(self.user)
    }

    fn execute(&mut self, value: Value, depth: usize) -> Run {
        self.tick(1)?;
        match value {
            Value::Name(name, Access::Executable) => match self.lookup(name) {
                Some(found) => self.execute_found(found, depth),
                None => match self.names.operator(name) {
                    Some(op) => self.operate(op, depth),
                    None => Ok(()),
                },
            },
            Value::Operator(op) => self.operate(op, depth),
            other => self.push(other),
        }
    }

    /// Executes a value a name stands for, or one `exec` is given.
    fn execute_found(&mut self, value: Value, depth: usize) -> Run {
        match value {
            Value::Array(proc, Access::Executable) => self.call(proc, depth + 1),
            Value::Name(_, Access::Executable) | Value::Operator(_) => {
                if depth >= MAX_DEPTH {
                    return Err(Interrupt::Stop);
                }
                self.execute(value, depth + 1)
            }
            Value::Filter(terminator) => {
                self.skip_filtered(terminator);
                Ok(())
            }
            other => self.push(other),
        }
    }

    fn call(&mut self, proc: ArrayId, depth: usize) -> Run {
        if depth > MAX_DEPTH {
            return Err(Interrupt::Stop);
        }
        self.tick(1)?;
        let mut index = 0;
        while let Some(&item) = self.array(proc).get(index) {
            index += 1;
            match item {
                Value::Name(_, Access::Executable) | Value::Operator(_) => {
                    self.execute(item, depth)?
                }
                other => {
                    self.tick(1)?;
                    self.push(other)?
                }
            }
        }
        Ok(())
    }

    /// Runs `body` for each value `each` pushes, as long as it gives one; `exit` ends the loop.
    fn repeat(
        &mut self,
        body: Value,
        depth: usize,
        mut each: impl FnMut(&mut Self) -> Run<bool>,
    ) -> Run {
        loop {
            self.tick(1)?;
            if !each(self)? {
                return Ok(());
            }
            match self.execute_found(body, depth) {
                Err(Interrupt::Exit) => return Ok(()),
                other => other?,
            }
        }
    }

    fn graphics_state(&self) -> Graphics {
        Graphics {
            font: self.font,
            position: self.layout.position(),
        }
    }

    fn set_graphics_state(&mut self, graphics: Graphics) {
        self.font = graphics.font;
        self.layout.restore(graphics.position);
    }

    /// Undoes the definitions made since the `save` at `level`, and returns to its graphics state.
    fn restore(&mut self, level: usize) {
        let Some(&saved) = self.saves.get(level) else {
            return;
        };
        for change in self.journal.drain(saved.journal..).rev() {
            let dict = &mut self.dicts[change.dict.0];
            match change.old {
                Some(old) => {
                    dict.put(change.key, old);
                }
                None => {
                    dict.remove(change.key);
                }
            }
        }
        self.saves.truncate(level);
        self.graphics.truncate(saved.stacked);
        self.set_graphics_state(saved.graphics);
    }

    /// `currentfile eexec`: the encrypted part of a Type 1 font program, which defines the font
    /// whose dictionary its clear part leaves on the stack, is skipped up to the `cleartomark`
    /// that ends it, and the font defined as it would define it.
    fn eexec(&mut self) -> Run {
        if !matches!(self.pop(), Value::File | Value::Filter(_)) {
            return Ok(());
        }
        self.scanner.skip_past(b"cleartomark");
        if let Value::Dict(font) = self.top() {
            self.pop();
            let name = self
                .dict(font)
                .get(Key::Name(self.known.font_name))
                .unwrap_or(Value::Unknown);
            self.define_font(name, Value::Dict(font))?;
        }
        Ok(())
    }

    /// `readstring`, `readhexstring` and `readline` of the program's own file.
    fn read(&mut self, op: Op) -> Run {
        let buffer = self.pop();
        let file = self.pop();
        let (Value::File, Value::String(buffer)) = (file, buffer) else {
            self.push(Value::Unknown)?;
            return self.push(Value::Unknown);
        };
        let capacity = self.string(buffer).len();
        let (data, complete) = match op {
            Op::ReadString => {
                let data = self.scanner.read_bytes(capacity).to_vec();
                let complete = data.len() == capacity;
                (data, complete)
            }
            Op::ReadHexString => {
                let data = self.scanner.read_hex(capacity);
                let complete = data.len() == capacity;
                (data, complete)
            }
            _ => match self.scanner.read_line() {
                Some(line) => (line[..line.len().min(capacity)].to_vec(), true),
                None => (Vec::new(), false),
            },
        };
        self.strings[buffer.0][..data.len()].copy_from_slice(&data);
        let read = self.alloc_string(data)?;
        self.push(Value::String(read))?;
        self.push(Value::Bool(complete))
    }

    fn skip_filtered(&mut self, terminator: Option<Terminator>) {
        if let Some(terminator) = terminator {
            self.scanner.skip_past(terminator.marker());
        }
    }

    /// A matrix operand: an array of six numbers.
    fn matrix(&self, value: Value) -> Option<[f64; 6]> {
        let Value::Array(id, _) = value else {
            return None;
        };
        let values = self.array(id);
        if values.len() != 6 {
            return None;
        }
        let mut matrix = [0.0; 6];
        for (slot, &value) in matrix.iter_mut().zip(values) {
            *slot = number(value)?;
        }
        Some(matrix)
    }

    /// `findfont`: the font defined under `key`, else the resident font of that name, which is
    /// defined then.
    fn find_font(&mut self, key: Value) -> Run<Value> {
        let Some(key) = self.key_of(key)? else {
            return Ok(Value::Unknown);
        };
        if let Some(font) = self.dict(self.font_directory).get(key) {
            return Ok(font);
        }
        let Key::Name(name) = key else {
            return Ok(Value::Unknown);
        };
        let resident = Resident::named(&String::from_utf8_lossy(self.names.text(name)));
        let encoding = self.vector(resident.vector);
        let glyphs = self.alloc_dict(Dict {
            metrics: resident.metrics,
            ..Dict::default()
        })?;
        let matrix = self.alloc_array(TYPE_1_FONT_MATRIX.map(Value::Number).to_vec())?;
        let mut font = Dict::default();
        for (entry, value) in [
            (self.known.font_name, Value::Name(name, Access::Literal)),
            (self.known.font_type, Value::Number(1.0)),
            (self.known.encoding, Value::Array(encoding, Access::Literal)),
            (
                self.known.font_matrix,
                Value::Array(matrix, Access::Literal),
            ),
            (self.known.char_strings, Value::Dict(glyphs)),
            (self.known.fid, Value::Null),
        ] {
            font.put(Key::Name(entry), value);
        }
        let font = Value::Dict(self.alloc_dict(font)?);
        self.dicts[self.font_directory.0].put(key, font);
        Ok(font)
    }

    /// `definefont`: `font` defined under `key`.
    fn define_font(&mut self, key: Value, font: Value) -> Run<Value> {
        let (Some(key), Value::Dict(id)) = (self.key_of(key)?, font) else {
            return Ok(Value::Unknown);
        };
        self.dicts[id.0].put(Key::Name(self.known.fid), Value::Null);
        self.define(self.font_directory, key, font)?;
        Ok(font)
    }

    /// `makefont`: a copy of `font` whose font matrix is followed by `matrix`.
    fn transform_font(&mut self, font: Value, matrix: [f64; 6]) -> Run<Value> {
        let Value::Dict(id) = font else {
            return Ok(Value::Unknown);
        };
        let mut copy = self.dict(id).clone();
        let key = Key::Name(self.known.font_matrix);
        let current = copy
            .get(key)
            .and_then(|value| self.matrix(value))
            .unwrap_or(IDENTITY);
        let transformed =
            self.alloc_array(multiply(current, matrix).map(Value::Number).to_vec())?;
        copy.put(key, Value::Array(transformed, Access::Literal));
        Ok(Value::Dict(self.alloc_dict(copy)?))
    }

    /// What `bytes` read as in the current font.
    fn decode(&self, bytes: &[u8]) -> Result<Decoded, Fault> {
        let font = self.dict(self.font.ok_or(Fault::UnknownFont)?);
        let entry = |name: NameId| font.get(Key::Name(name));
        let font_type = entry(self.known.font_type).and_then(number);
        if font_type == Some(COMPOSITE_FONT_TYPE) {
            return Err(Fault::CompositeFont);
        }
        let Some(Value::Array(encoding, _)) = entry(self.known.encoding) else {
            return Err(Fault::NoEncoding);
        };
        let names = self.array(encoding);
        let matrix = entry(self.known.font_matrix).and_then(|value| self.matrix(value));
        let scale = matrix.map(|m| m[0].hypot(m[1]));
        let em = self.em(entry(self.known.font_bbox), font_type, matrix);
        let metrics = match entry(self.known.char_strings) {
            Some(Value::Dict(glyphs)) => self.dict(glyphs).metrics,
            _ => None,
        };
        let dingbats = metrics == Some(Standard14::ZapfDingbats);
        let metrics = metrics.map(Standard14::metrics);
        let mut text = String::with_capacity(bytes.len());
        let mut unmapped = 0u32;
        let mut width = 0.0f64;
        for &code in bytes {
            let name = match names.get(usize::from(code)) {
                Some(Value::Name(name, _)) => Some(self.names.text(*name)),
                _ => None,
            };
            if !name.is_some_and(|name| fonts::glyph_text(name, dingbats, &mut text)) {
                unmapped += 1;
            }
            if let (Some(metrics), Some(name)) = (metrics, name) {
                let name = std::str::from_utf8(name).unwrap_or_default();
                width += f64::from(metrics.character_width(name));
            }
        }
        Ok(Decoded {
            text,
            glyphs: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            unmapped,
            width: metrics.and(scale).map(|scale| width * scale),
            em,
        })
    }

    /// The font size in the units of the user space: from the glyph box's height for a font that
    /// defines its glyphs itself, else from the em of a Type 1 font's glyph space.
    fn em(&self, bbox: Option<Value>, font_type: Option<f64>, matrix: Option<[f64; 6]>) -> f64 {
        let Some(matrix) = matrix else {
            return DEFAULT_EM;
        };
        let em = if font_type == Some(USER_DEFINED_FONT_TYPE) {
            let height = match bbox {
                Some(Value::Array(id, _)) => match self.array(id).as_slice() {
                    [_, y0, _, y1] => number(*y1).zip(number(*y0)).map(|(y1, y0)| y1 - y0),
                    _ => None,
                },
                _ => None,
            };
            height.map_or(0.0, |height| height.abs() * matrix[2].hypot(matrix[3]))
        } else {
            GLYPH_SPACE * matrix[0].hypot(matrix[1])
        };
        if em.is_finite() && em > 0.0 {
            em
        } else {
            DEFAULT_EM
        }
    }

    fn show(&mut self, string: Value, spacing: Spacing) -> Run {
        let Value::String(id) = string else {
            self.fault(Fault::ComputedString);
            return Ok(());
        };
        let bytes = self.string(id).clone();
        let decoded = match self.decode(&bytes) {
            Ok(decoded) => decoded,
            Err(fault) => {
                self.fault(fault);
                return Ok(());
            }
        };
        self.tick(u64::from(decoded.glyphs))?;
        let count = |code: u8| bytes.iter().filter(|&&byte| byte == code).count() as f64;
        let glyphs = f64::from(decoded.glyphs);
        let advance = match (spacing, decoded.width) {
            (Spacing::Displaced(Some(width)), _) => Advance::Measured(width),
            (Spacing::Displaced(None), _) | (_, None) => Advance::Unknown,
            (Spacing::None, Some(width)) => Advance::Measured(width),
            (Spacing::Letters(letter), Some(width)) => Advance::Measured(width + glyphs * letter),
            (Spacing::Words { code, extra }, Some(width)) => {
                Advance::Measured(width + count(code) * extra)
            }
            (Spacing::Both { code, word, letter }, Some(width)) => {
                Advance::Measured(width + count(code) * word + glyphs * letter)
            }
        };
        self.layout.show(Shown {
            text: &decoded.text,
            glyphs: decoded.glyphs,
            unmapped: decoded.unmapped,
            advance,
            em: decoded.em,
        });
        Ok(())
    }

    /// `glyphshow`: one glyph, by name, in the current font.
    fn glyph_show(&mut self, glyph: Value) -> Run {
        let Value::Name(name, _) = glyph else {
            self.fault(Fault::ComputedString);
            return Ok(());
        };
        let Some(font) = self.font else {
            self.fault(Fault::UnknownFont);
            return Ok(());
        };
        let font = self.dict(font);
        let matrix = font
            .get(Key::Name(self.known.font_matrix))
            .and_then(|value| self.matrix(value));
        let font_type = font.get(Key::Name(self.known.font_type)).and_then(number);
        let em = self.em(font.get(Key::Name(self.known.font_bbox)), font_type, matrix);
        let metrics = match font.get(Key::Name(self.known.char_strings)) {
            Some(Value::Dict(glyphs)) => self.dict(glyphs).metrics,
            _ => None,
        };
        let text_name = self.names.text(name);
        let mut text = String::new();
        let mapped = fonts::glyph_text(
            text_name,
            metrics == Some(Standard14::ZapfDingbats),
            &mut text,
        );
        let width = metrics.zip(matrix).map(|(metrics, matrix)| {
            let name = std::str::from_utf8(text_name).unwrap_or_default();
            f64::from(metrics.metrics().character_width(name)) * matrix[0].hypot(matrix[1])
        });
        self.tick(1)?;
        self.layout.show(Shown {
            text: &text,
            glyphs: 1,
            unmapped: u32::from(!mapped),
            advance: width.map_or(Advance::Unknown, Advance::Measured),
            em,
        });
        Ok(())
    }

    /// `showpage`: the page shown is handed on, and the next one starts.
    fn show_page(&mut self) -> Result<(), Stop> {
        if let Some(marked) = &mut self.marks.marked {
            *marked = MarkedPage::Shown;
        }
        let page = Page {
            text: self.layout.end_page(),
            faults: std::mem::take(&mut self.faults),
        };
        (self.sink)(page)
    }

    /// The page the program leaves unshown at its end, if it shows anything.
    fn end_page(&mut self) -> Result<(), Stop> {
        if self.layout.is_blank() && self.faults.is_empty() {
            return Ok(());
        }
        self.show_page()
    }

    /// The image operators: their operands taken, and the data they read from the program's own
    /// file passed, by running the procedures that read it for as many bytes as the image holds.
    fn image(&mut self, sampled: Sampled, depth: usize) -> Run {
        if let Value::Dict(image) = self.top() {
            self.pop();
            return self.image_dictionary(image, depth);
        }
        let (sources, components) = match sampled {
            Sampled::Color => {
                let components = self.pop_integer().unwrap_or(1).clamp(1, 4) as usize;
                let multiple = self.pop() == Value::Bool(true);
                let count = if multiple { components } else { 1 };
                let mut sources: Vec<Value> = (0..count).map(|_| self.pop()).collect();
                sources.reverse();
                (sources, components)
            }
            Sampled::Image | Sampled::Mask => (vec![self.pop()], 1),
        };
        self.pop();
        let bits = match sampled {
            Sampled::Mask => {
                self.pop();
                Some(1)
            }
            Sampled::Image | Sampled::Color => self.pop_integer(),
        };
        let height = self.pop_integer();
        let width = self.pop_integer();
        let bytes = sample_bytes(width, height, bits, components);
        self.pass_image_data(&sources, bytes, depth)
    }

    fn image_dictionary(&mut self, image: DictId, depth: usize) -> Run {
        let dict = self.dict(image);
        let entry = |name: NameId| dict.get(Key::Name(name));
        let mask = entry(self.known.image_mask) == Some(Value::Bool(true));
        let components = match entry(self.known.decode) {
            Some(Value::Array(id, _)) => (self.array(id).len() / 2).max(1),
            _ => 1,
        };
        let bits = if mask {
            Some(1)
        } else {
            entry(self.known.bits_per_component).and_then(integer)
        };
        let bytes = sample_bytes(
            entry(self.known.width).and_then(integer),
            entry(self.known.height).and_then(integer),
            bits,
            components,
        );
        let source = entry(self.known.data_source).unwrap_or(Value::Unknown);
        self.pass_image_data(&[source], bytes, depth)
    }

    fn pass_image_data(&mut self, sources: &[Value], bytes: Option<u64>, depth: usize) -> Run {
        match sources {
            [Value::File] => {
                if let Some(bytes) = bytes {
                    self.scanner
                        .skip_bytes(usize::try_from(bytes).unwrap_or(usize::MAX));
                }
                return Ok(());
            }
            [Value::Filter(terminator)] => {
                self.skip_filtered(*terminator);
                return Ok(());
            }
            _ => {}
        }
        let Some(mut left) = bytes else {
            return Ok(());
        };
        let procedures = sources
            .iter()
            .all(|source| matches!(source, Value::Array(_, Access::Executable)));
        if !procedures || sources.is_empty() {
            return Ok(());
        }
        for &source in sources.iter().cycle() {
            if left == 0 {
                break;
            }
            self.execute_found(source, depth)?;
            match self.pop() {
                Value::String(id) if !self.string(id).is_empty() => {
                    left = left.saturating_sub(self.string(id).len() as u64);
                }
                _ => break,
            }
        }
        Ok(())
    }
}

/// Whether a structuring comment marks a page or the trailer, which no procedure spans.
fn ends_procedures(comment: &[u8]) -> bool {
    [&b"Page:"[..], b"Trailer", b"EOF"]
        .iter()
        .any(|mark| comment.starts_with(mark))
}

/// The bytes of an image's samples, each row padded to a byte.
fn sample_bytes(
    width: Option<i64>,
    height: Option<i64>,
    bits: Option<i64>,
    components: usize,
) -> Option<u64> {
    let width = u64::try_from(width?).ok()?;
    let height = u64::try_from(height?).ok()?;
    let bits = u64::try_from(bits?).ok()?;
    let row = width
        .checked_mul(bits)?
        .checked_mul(components as u64)?
        .div_ceil(8);
    row.checked_mul(height)
}

#[cfg(test)]
mod tests;
