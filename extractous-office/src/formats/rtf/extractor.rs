//! Tika's `TextExtractor`: a shallow parse of an RTF document, just deep enough to decode its
//! text, keep its paragraphs, lists, bold and italic runs and hyperlinks, and read the metadata
//! of its `\info` group. Tika runs it over the document's bytes once, and so does this port,
//! call for call, down to how it recovers from documents whose groups or paragraphs do not
//! nest.

use std::collections::HashMap;

use super::charsets;
use super::embedded::{hex_value, Deferral, Embedded};
use super::input::Input;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::Failure;
use crate::java;
use crate::java::time::{civil, format_instant};
use crate::text::charset::Charset;

/// The UTF-16 units `TextExtractor` decodes into at a time, each batch written as one
/// `characters` call.
const DECODE_BUFFER: usize = 128;
/// The paragraph tags `TextExtractor` keeps track of at most, against documents that open
/// paragraphs without end.
const MAX_PARAGRAPH_STACK: usize = 1000;
/// The levels a list's numbering is recorded for.
const LIST_LEVELS: usize = 9;
/// `\levelnfcN` for a bulleted list level.
const BULLET: i32 = 23;
const HYPERLINK: &str = "HYPERLINK";

/// Why an extraction ended before the end of the document.
#[derive(Debug)]
pub(super) enum Interrupt {
    /// It failed as Tika's does with a checked exception, after which `RTFParser` ends the
    /// elements still open.
    Failed(Failure),
    /// It failed as Tika's does with a runtime exception, which leaves the elements open.
    Unchecked(Failure),
    /// It met something only Tika's path extracts.
    Deferred(Deferral),
}

impl From<Failure> for Interrupt {
    fn from(failure: Failure) -> Self {
        Self::Failed(failure)
    }
}

impl From<Stop> for Interrupt {
    fn from(stop: Stop) -> Self {
        Self::Failed(Failure::Stopped(stop))
    }
}

impl From<Deferral> for Interrupt {
    fn from(deferral: Deferral) -> Self {
        Self::Deferred(deferral)
    }
}

type Step = Result<(), Interrupt>;

/// Extracts `input` into `xhtml`. On a failure, the elements still open are ended, as
/// `RTFParser`'s `XHTMLBalancingHandler` drains them, unless that fails in turn.
pub(super) fn extract(input: &[u8], xhtml: &mut Xhtml) -> Step {
    let mut extractor = Extractor::new(input, xhtml, None);
    let result = extractor.extract();
    if let Err(Interrupt::Failed(_)) = result {
        extractor.drain()?;
    }
    result
}

/// `RTFParser.parseInline`: extracts `input` into the elements of another document's `xhtml`,
/// the metadata of its header into `metadata`, ending no element on a failure.
pub(super) fn extract_inline(input: &[u8], xhtml: &mut Xhtml, metadata: TikaMetadata) -> Step {
    Extractor::new(input, xhtml, Some(metadata)).extract()
}

/// The state of one group, `{...}`, which its nested groups start from.
#[derive(Debug, Clone, Copy)]
struct GroupState {
    depth: i32,
    bold: bool,
    italic: bool,
    /// Whether the text of the group is skipped, as in a destination marked `\*`.
    ignore: bool,
    /// How many bytes stand in for a character written as `\uN`.
    uc_skip: i32,
    list: i32,
    list_level: i32,
    font_charset: Option<Charset>,
    objdata: bool,
    /// How deep in a picture the group is: 1 for the `\pict` group itself.
    pict_depth: i32,
    sp: bool,
    sn: bool,
    sv: bool,
    annotation: bool,
}

impl GroupState {
    fn root() -> Self {
        Self {
            depth: 0,
            bold: false,
            italic: false,
            ignore: false,
            uc_skip: 1,
            list: 0,
            list_level: 0,
            font_charset: None,
            objdata: false,
            pict_depth: 0,
            sp: false,
            sn: false,
            sv: false,
            annotation: false,
        }
    }

    /// The state a group nested in this one starts with.
    fn nested(&self) -> Self {
        Self {
            depth: self.depth + 1,
            pict_depth: if self.pict_depth > 0 {
                self.pict_depth + 1
            } else {
                0
            },
            objdata: false,
            sp: false,
            sn: false,
            sv: false,
            annotation: false,
            ..*self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    P,
    Li,
    Ol,
    Ul,
}

impl Tag {
    fn name(self) -> &'static str {
        match self {
            Tag::P => "p",
            Tag::Li => "li",
            Tag::Ol => "ol",
            Tag::Ul => "ul",
        }
    }
}

/// The paragraph and list tags written and not yet ended, which `TextExtractor` keeps to avoid
/// ending them out of order.
#[derive(Default)]
struct ParagraphStack {
    tags: Vec<Tag>,
    paragraphs: usize,
}

impl ParagraphStack {
    fn push(&mut self, tag: Tag) {
        if self.tags.len() < MAX_PARAGRAPH_STACK {
            self.tags.push(tag);
            self.paragraphs += usize::from(tag == Tag::P);
        }
    }

    fn pop(&mut self) -> Option<Tag> {
        let tag = self.tags.pop()?;
        self.paragraphs -= usize::from(tag == Tag::P);
        Some(tag)
    }

    fn has_paragraph(&self) -> bool {
        self.paragraphs > 0
    }

    fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }
}

/// Where the reader is with the font table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontTable {
    NotSeen,
    Inside,
    Done,
}

/// Where the reader is in a field, `{\field{\*\fldinst ...}{\fldrslt ...}}`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Field {
    None,
    /// In the instruction, collecting its text.
    Instruction,
    /// After a `HYPERLINK` instruction to this target, before its result.
    PendingLink(String),
    /// In the result of a hyperlink, inside its `<a>`.
    Link,
}

/// Where the reader is in a `\upr` group, whose second group holds the Unicode text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Upr {
    Outside,
    Started,
    InGroups,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListTable {
    Lists,
    Overrides,
}

/// The `\info` property the text of the current group is the value of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InfoProperty {
    Creator,
    Title,
    Subject,
    Keywords,
    Category,
    Comments,
    Company,
    Manager,
    Template,
    Created,
}

impl InfoProperty {
    fn for_word(word: &[u8]) -> Option<Self> {
        Some(match word {
            b"author" => Self::Creator,
            b"title" => Self::Title,
            b"subject" => Self::Subject,
            b"keywords" => Self::Keywords,
            b"category" => Self::Category,
            b"comment" => Self::Comments,
            b"company" => Self::Company,
            b"manager" => Self::Manager,
            b"template" => Self::Template,
            b"creatim" => Self::Created,
            _ => return None,
        })
    }

    /// Whether the text is the value: everything but the date, which is read from numbers.
    fn is_text(self) -> bool {
        self != Self::Created
    }
}

/// A list of the list table, its number formats by level.
#[derive(Debug, Default)]
struct ListDescriptor {
    number_type: [i32; LIST_LEVELS],
}

/// The list table and list override table, as far as they are read.
#[derive(Default)]
struct Lists {
    descriptors: Vec<ListDescriptor>,
    by_id: HashMap<i32, usize>,
    table: Option<ListTable>,
    current: Option<usize>,
    level: i32,
}

/// The parts of the `\creatim` group, each unset until the group names it.
#[derive(Debug, Default)]
struct DateParts {
    year: Option<i32>,
    month: Option<i32>,
    day: Option<i32>,
    hour: Option<i32>,
    minute: Option<i32>,
}

impl DateParts {
    /// The date as an instant, its fields read as a wall-clock time in UTC; none when the year,
    /// month or day is missing or any field is out of range.
    ///
    /// Tika reads the fields with a lenient `Calendar` in the JVM's default time zone, so the
    /// same document gives different instants on machines in different zones, an empty group
    /// gives year 0 (-0001-11-27 in Copenhagen) and out-of-range fields roll over (#65).
    fn instant(&self) -> Option<String> {
        let (year, month, day) = (self.year?, self.month?, self.day?);
        let (hour, minute) = (self.hour.unwrap_or(0), self.minute.unwrap_or(0));
        let days_in_month = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if civil::is_leap_year(i64::from(year)) => 29,
            2 => 28,
            _ => return None,
        };
        if !(1..=9999).contains(&year)
            || !(1..=days_in_month).contains(&day)
            || !(0..24).contains(&hour)
            || !(0..60).contains(&minute)
        {
            return None;
        }
        let days = civil::days_from_civil(i64::from(year), i64::from(month), i64::from(day));
        let seconds = days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60;
        Some(format_instant(seconds * 1_000))
    }
}

struct Extractor<'a, 'x, 'h> {
    input: Input<'a>,
    out: &'x mut Xhtml<'h>,
    embedded: Embedded,
    font_to_charset: HashMap<i32, Charset>,
    groups: Vec<GroupState>,
    group: GroupState,
    /// Text of the header or of a field instruction, collected rather than written.
    pending_buffer: Vec<u16>,
    /// How many bytes after a `\uN` still stand in for it.
    ansi_skip: i32,
    pending_bytes: Vec<u8>,
    pending_chars: Vec<u16>,
    control: Vec<u8>,
    global_charset: Charset,
    global_default_font: i32,
    current_font: i32,
    in_header: bool,
    font_table: FontTable,
    font_table_depth: i32,
    info: Option<InfoProperty>,
    in_paragraph: bool,
    field: Field,
    pending_list_end: i32,
    lists: Lists,
    /// The depth of the group whose `<a>` is open, or -1.
    hyperlink_depth: i32,
    upr: Upr,
    date: DateParts,
    paragraphs: ParagraphStack,
    /// The elements started and not yet ended, as `XHTMLBalancingHandler` tracks them.
    open: Vec<&'static str>,
    /// The metadata of a document extracted into another's elements, else the document's own.
    inline_metadata: Option<TikaMetadata>,
}

impl<'a, 'x, 'h> Extractor<'a, 'x, 'h> {
    fn new(input: &'a [u8], out: &'x mut Xhtml<'h>, inline_metadata: Option<TikaMetadata>) -> Self {
        Self {
            input: Input::new(input),
            out,
            embedded: Embedded::new(),
            font_to_charset: HashMap::new(),
            groups: Vec::new(),
            group: GroupState::root(),
            pending_buffer: Vec::new(),
            ansi_skip: 0,
            pending_bytes: Vec::new(),
            pending_chars: Vec::new(),
            control: Vec::new(),
            global_charset: charsets::windows_1252(),
            global_default_font: -1,
            current_font: -1,
            in_header: true,
            font_table: FontTable::NotSeen,
            font_table_depth: 0,
            info: None,
            in_paragraph: false,
            field: Field::None,
            pending_list_end: 0,
            lists: Lists::default(),
            hyperlink_depth: -1,
            upr: Upr::Outside,
            date: DateParts::default(),
            paragraphs: ParagraphStack::default(),
            open: Vec::new(),
            inline_metadata,
        }
    }

    fn metadata(&mut self) -> &mut TikaMetadata {
        match &mut self.inline_metadata {
            Some(metadata) => metadata,
            None => self.out.metadata_mut(),
        }
    }

    fn start(&mut self, tag: &'static str) -> Step {
        self.open.push(tag);
        Ok(self.out.start_element(tag)?)
    }

    fn end(&mut self, tag: &str) -> Step {
        self.open.pop();
        Ok(self.out.end_element(tag)?)
    }

    /// Ends the open elements, innermost first.
    fn drain(&mut self) -> Step {
        while let Some(tag) = self.open.pop() {
            self.out.end_element(tag)?;
        }
        Ok(())
    }

    fn word(&self, word: &[u8]) -> bool {
        self.control == word
    }

    fn extract(&mut self) -> Step {
        while let Some(byte) = self.input.read() {
            match byte {
                b'\\' => self.control_token()?,
                b'{' => {
                    self.push_text()?;
                    self.group_start()?;
                }
                b'}' => {
                    self.push_text()?;
                    self.group_end()?;
                    if self.groups.is_empty() {
                        break;
                    }
                }
                _ if self.group.objdata || self.group.pict_depth == 1 => {
                    self.embedded.write_hex_char(byte);
                }
                b'\r' | b'\n' => {}
                _ if !self.group.ignore
                    || self.info.is_some()
                    || self.group.sn
                    || self.group.sv =>
                {
                    if self.ansi_skip != 0 {
                        self.ansi_skip = self.ansi_skip.wrapping_sub(1);
                    } else {
                        self.add_output_byte(byte)?;
                    }
                }
                _ => {}
            }
        }
        self.end_paragraph(false)?;
        while let Some(tag) = self.paragraphs.pop() {
            self.end(tag.name())?;
        }
        Ok(())
    }

    fn push_text(&mut self) -> Step {
        if self.pending_bytes.is_empty() {
            self.push_chars()
        } else {
            self.push_bytes()
        }
    }

    fn add_output_byte(&mut self, byte: u8) -> Step {
        if !self.pending_chars.is_empty() {
            self.push_chars()?;
        }
        if self.group.pict_depth == 0 {
            self.pending_bytes.push(byte);
        }
        Ok(())
    }

    fn add_output_char(&mut self, unit: u16) -> Step {
        if !self.pending_bytes.is_empty() {
            self.push_bytes()?;
        }
        if self.in_header || self.field == Field::Instruction {
            self.pending_buffer.push(unit);
        } else if !self.group.sn && !self.group.sv {
            self.pending_chars.push(unit);
        }
        Ok(())
    }

    fn control_token(&mut self) -> Step {
        match self.input.read() {
            Some(b'\'') => self.hex_char(),
            Some(byte) if byte.is_ascii_alphabetic() => self.control_word(byte),
            Some(byte @ (b'{' | b'}' | b'\\' | b'\r' | b'\n')) => self.add_output_byte(byte),
            Some(b'~') => self.add_output_char(0x00A0),
            Some(b'-') => self.add_output_char(0x00AD),
            Some(b'_') => self.add_output_char(0x2011),
            Some(_) | None => Ok(()),
        }
    }

    fn hex_char(&mut self) -> Step {
        let high = self.input.read();
        let Some(high) = high.and_then(hex_value) else {
            return Ok(self.input.unread(high)?);
        };
        let low = self.input.read();
        let Some(low) = low.and_then(hex_value) else {
            return Ok(self.input.unread(low)?);
        };
        if self.ansi_skip != 0 {
            self.ansi_skip = self.ansi_skip.wrapping_sub(1);
            Ok(())
        } else {
            self.add_output_byte(16 * high + low)
        }
    }

    fn control_word(&mut self, first: u8) -> Step {
        self.control.clear();
        self.control.push(first);
        let mut byte = self.input.read();
        while let Some(letter) = byte.filter(u8::is_ascii_alphabetic) {
            self.control.push(letter);
            byte = self.input.read();
        }
        let mut has_param = false;
        let mut negative = false;
        if byte == Some(b'-') {
            negative = true;
            has_param = true;
            byte = self.input.read();
        }
        let mut param: i32 = 0;
        while let Some(digit) = byte.filter(u8::is_ascii_digit) {
            param = param.wrapping_mul(10).wrapping_add(i32::from(digit - b'0'));
            has_param = true;
            byte = self.input.read();
        }
        if byte != Some(b' ') {
            self.input.unread(byte)?;
        }
        if has_param {
            if negative {
                param = param.wrapping_neg();
            }
            self.control_word_with(param)?;
        } else {
            self.control_word_without()?;
        }
        self.control.clear();
        Ok(())
    }

    fn lazy_start_paragraph(&mut self) -> Step {
        if self.in_paragraph || self.paragraphs.has_paragraph() {
            return Ok(());
        }
        self.end_styles()?;
        if self.pending_list_end != 0 && self.group.list != self.pending_list_end {
            self.end_list(self.pending_list_end)?;
            self.pending_list_end = 0;
        }
        if self.in_list() && self.pending_list_end != self.group.list {
            self.start_list(self.group.list)?;
        }
        let tag = if self.in_list() { Tag::Li } else { Tag::P };
        self.start(tag.name())?;
        self.paragraphs.push(tag);
        self.start_styles()?;
        self.in_paragraph = true;
        Ok(())
    }

    fn end_paragraph(&mut self, preserve_styles: bool) -> Step {
        self.push_text()?;
        if !self.in_paragraph {
            self.lazy_start_paragraph()?;
        }
        if self.in_paragraph || !self.paragraphs.is_empty() {
            if self.hyperlink_depth >= 0 {
                self.end("a")?;
                self.field = Field::None;
                self.hyperlink_depth = -1;
            }
            if self.group.italic {
                self.end("i")?;
                self.group.italic = preserve_styles;
            }
            if self.group.bold {
                self.end("b")?;
                self.group.bold = preserve_styles;
            }
            let expected = if self.in_list() { Tag::Li } else { Tag::P };
            let mut misaligned = false;
            if let Some(last) = self.paragraphs.pop() {
                if last == expected {
                    self.end(expected.name())?;
                } else {
                    self.paragraphs.push(last);
                    misaligned = true;
                }
            }
            if misaligned {
                while let Some(tag) = self.paragraphs.pop() {
                    self.end(tag.name())?;
                }
            }
            if preserve_styles && (self.group.bold || self.group.italic) {
                self.start(Tag::P.name())?;
                self.paragraphs.push(Tag::P);
                self.start_styles()?;
                self.in_paragraph = true;
            } else {
                self.in_paragraph = false;
            }
        }
        if !preserve_styles && self.pending_list_end != 0 {
            self.end_list(self.pending_list_end)?;
            self.pending_list_end = 0;
        }
        Ok(())
    }

    fn push_chars(&mut self) -> Step {
        if self.pending_chars.is_empty() {
            return Ok(());
        }
        self.lazy_start_paragraph()?;
        self.out.characters_utf16(&self.pending_chars)?;
        self.pending_chars.clear();
        Ok(())
    }

    /// Decodes the pending bytes with the current charset into the header's text, a field
    /// instruction, or the body, `DECODE_BUFFER` units at a time.
    fn push_bytes(&mut self) -> Step {
        if !self.pending_bytes.is_empty() && (!self.group.ignore || self.info.is_some()) {
            let charset = self.charset();
            let text = charset
                .decode(&self.pending_bytes)
                .ok_or(Deferral::Charset)?;
            let mut start = 0;
            let mut units = 0;
            for (index, character) in text.char_indices() {
                if units + character.len_utf16() > DECODE_BUFFER {
                    self.write_decoded(&text[start..index])?;
                    start = index;
                    units = 0;
                }
                units += character.len_utf16();
            }
            if start < text.len() {
                self.write_decoded(&text[start..])?;
            }
        }
        self.pending_bytes.clear();
        Ok(())
    }

    fn write_decoded(&mut self, text: &str) -> Step {
        if self.in_header || self.field == Field::Instruction {
            self.pending_buffer.extend(text.encode_utf16());
            Ok(())
        } else {
            self.lazy_start_paragraph()?;
            Ok(self.out.characters(text)?)
        }
    }

    /// The charset of the current font, else of the default font once the header is over, else
    /// of the document.
    fn charset(&self) -> Charset {
        if let Some(charset) = self.group.font_charset {
            return charset;
        }
        if self.global_default_font != -1 && !self.in_header {
            if let Some(&charset) = self.font_to_charset.get(&self.global_default_font) {
                return charset;
            }
        }
        self.global_charset
    }

    fn control_word_with(&mut self, param: i32) -> Step {
        if self.in_header {
            self.header_word_with(param)?;
        } else if self.word(b"b") {
            if self.field == Field::None && self.group.bold {
                self.push_text()?;
                if self.group.italic {
                    self.end("i")?;
                }
                self.end("b")?;
                if self.group.italic {
                    self.start("i")?;
                }
                self.group.bold = false;
            }
        } else if self.word(b"i") {
            if self.field == Field::None && self.group.italic {
                self.push_text()?;
                self.end("i")?;
                self.group.italic = false;
            }
        } else if self.word(b"f") {
            let charset = self.font_to_charset.get(&param).copied();
            self.push_text()?;
            self.group.font_charset = charset;
        } else if self.word(b"ls") {
            self.group.list = param;
        } else if self.word(b"lslvl") {
            self.group.list_level = param;
        }

        if self.word(b"u") {
            if !self.group.ignore || self.group.sv || self.group.sn {
                self.add_output_char(param as u16)?;
            }
            self.ansi_skip = self.group.uc_skip;
        } else if self.word(b"uc") {
            self.group.uc_skip = param;
        } else if self.word(b"bin") {
            if let Ok(length) = u32::try_from(param) {
                if self.group.pict_depth == 1 {
                    self.embedded.write_bytes(&mut self.input, length)?;
                } else {
                    self.input.skip(length as usize)?;
                }
            }
        }
        Ok(())
    }

    fn header_word_with(&mut self, param: i32) -> Step {
        if self.word(b"ansicpg") {
            if let Some(charset) = charsets::for_ansicpg(param) {
                self.global_charset = charset;
            }
        } else if self.word(b"deff") {
            self.global_default_font = param;
        } else if self.word(b"nofpages") {
            self.add_count("meta:page-count", param)?;
        } else if self.word(b"nofwords") {
            self.add_count("meta:word-count", param)?;
        } else if self.word(b"nofchars") {
            self.add_count("meta:character-count", param)?;
        } else if self.word(b"yr") {
            self.date.year = Some(param);
        } else if self.word(b"mo") {
            self.date.month = Some(param);
        } else if self.word(b"dy") {
            self.date.day = Some(param);
        } else if self.word(b"hr") {
            self.date.hour = Some(param);
        } else if self.word(b"min") {
            self.date.minute = Some(param);
        } else if self.word(b"fromhtml") && param == 1 {
            self.metadata()
                .set("rtf:contains-encapsulated-html", "true");
        }

        if self.font_table == FontTable::Inside {
            if self.group.depth < self.font_table_depth {
                self.font_table = FontTable::Done;
            } else if self.word(b"f") {
                self.current_font = param;
            } else if self.word(b"fcharset") {
                if let Some(charset) = charsets::for_fcharset(param) {
                    self.font_to_charset.insert(self.current_font, charset);
                }
            }
        }
        if self.font_table == FontTable::Done && !self.group.ignore && self.word(b"f") {
            self.in_header = false;
        }

        if let Some(current) = self.lists.current {
            if self.word(b"listid") {
                if self.lists.table == Some(ListTable::Lists) {
                    self.lists.by_id.insert(param, current);
                }
            } else if self.word(b"levelnfc") || self.word(b"levelnfcn") {
                if let Ok(level) = usize::try_from(self.lists.level) {
                    if level < LIST_LEVELS {
                        self.lists.descriptors[current].number_type[level] = param;
                    }
                }
            }
        }
        Ok(())
    }

    /// `Metadata.add` of a count, a single-valued property, which fails when it has a value.
    fn add_count(&mut self, name: &str, count: i32) -> Step {
        if self.metadata().get(name).is_some() {
            return Err(Interrupt::Unchecked(Failure::Malformed(format!(
                "The RTF header gives {name} twice, which Tika's metadata refuses"
            ))));
        }
        self.metadata().set(name, count.to_string());
        Ok(())
    }

    fn in_list(&self) -> bool {
        self.group.list != 0
    }

    fn pending_list_end(&mut self) {
        self.pending_list_end = self.group.list;
        self.group.list = 0;
    }

    fn list_tag(&self, list: i32) -> Result<Tag, Interrupt> {
        let Some(&descriptor) = self.lists.by_id.get(&list) else {
            return Ok(Tag::Ul);
        };
        let number_type = usize::try_from(self.group.list_level)
            .ok()
            .and_then(|level| self.lists.descriptors[descriptor].number_type.get(level))
            .ok_or_else(|| {
                Interrupt::Unchecked(Failure::Malformed(format!(
                    "The RTF list level {} is out of range, which fails Tika's parser",
                    self.group.list_level
                )))
            })?;
        Ok(if *number_type == BULLET {
            Tag::Ul
        } else {
            Tag::Ol
        })
    }

    fn end_list(&mut self, list: i32) -> Step {
        let tag = self.list_tag(list)?;
        if self.paragraphs.pop() == Some(tag) {
            self.end(tag.name())?;
        }
        Ok(())
    }

    fn start_list(&mut self, list: i32) -> Step {
        let tag = self.list_tag(list)?;
        self.start(tag.name())?;
        self.paragraphs.push(tag);
        Ok(())
    }

    fn control_word_without(&mut self) -> Step {
        if self.in_header {
            self.header_word_without();
        } else if self.field == Field::None {
            if self.word(b"b") {
                if !self.group.bold {
                    self.push_text()?;
                    self.lazy_start_paragraph()?;
                    if self.group.italic {
                        self.end("i")?;
                    }
                    self.group.bold = true;
                    self.start_styles()?;
                }
            } else if self.word(b"i") && !self.group.italic {
                self.push_text()?;
                self.lazy_start_paragraph()?;
                self.group.italic = true;
                self.start("i")?;
            }
        }

        let ignored = self.group.ignore;
        match self.control.as_slice() {
            b"pard" => {
                self.push_text()?;
                self.end_styles()?;
                if self.in_list() {
                    self.pending_list_end();
                }
            }
            b"plain" => {
                if self.group.italic || self.group.bold {
                    self.push_text()?;
                    self.end_styles()?;
                }
            }
            b"par" => {
                if !ignored {
                    self.end_paragraph(true)?;
                    if self.in_list() {
                        self.pending_list_end();
                    }
                }
            }
            b"shptxt" => {
                self.push_text()?;
                self.group.ignore = false;
            }
            b"chatn" | b"atnid" | b"atnauthor" => {
                self.add_output_char(u16::from(b' '))?;
                self.push_text()?;
                self.group.ignore = false;
            }
            b"annotation" => {
                self.group.annotation = true;
                self.push_text()?;
                self.group.ignore = false;
            }
            b"listtext" => self.group.ignore = true,
            b"cell" => self.end_paragraph(true)?,
            b"sp" => self.group.sp = true,
            b"sn" => self.group.sn = true,
            b"sv" => self.group.sv = true,
            b"object" => self.push_text()?,
            b"objdata" => {
                self.group.objdata = true;
                self.embedded.start_object();
            }
            b"pict" => {
                self.push_text()?;
                self.group.pict_depth = 1;
                self.embedded.start_picture();
            }
            b"upr" => self.upr = Upr::Started,
            b"ud" if self.upr == Upr::InGroups => {
                self.upr = Upr::Outside;
                self.group.ignore = false;
            }
            b"fldinst" => {
                if self.field == Field::None {
                    self.field = Field::Instruction;
                    self.group.ignore = false;
                } else {
                    self.group.ignore = true;
                }
            }
            b"fldrslt" if matches!(self.field, Field::PendingLink(_)) => self.open_hyperlink()?,
            word => {
                if let Some(unit) = special_character(word) {
                    if !ignored {
                        self.add_output_char(unit)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn header_word_without(&mut self) {
        match self.control.as_slice() {
            b"ansi" => self.global_charset = charsets::windows_1252(),
            b"pca" => self.global_charset = charsets::cp850(),
            b"pc" => self.global_charset = charsets::cp437(),
            b"mac" => self.global_charset = charsets::mac_roman(),
            _ => {}
        }
        match self.control.as_slice() {
            b"colortbl" | b"stylesheet" | b"fonttbl" => self.group.ignore = true,
            b"listtable" => self.lists.table = Some(ListTable::Lists),
            b"listoverridetable" => self.lists.table = Some(ListTable::Overrides),
            _ => {}
        }
        if self.upr == Upr::Outside {
            if let Some(property) = InfoProperty::for_word(&self.control) {
                if property == InfoProperty::Created {
                    self.date = DateParts::default();
                }
                self.info = Some(property);
            }
        }
        match self.font_table {
            FontTable::NotSeen => {
                if self.word(b"fonttbl") {
                    self.font_table = FontTable::Inside;
                    self.font_table_depth = self.group.depth;
                }
            }
            FontTable::Inside => {
                if self.group.depth < self.font_table_depth {
                    self.font_table = FontTable::Done;
                }
            }
            FontTable::Done => {}
        }
        if self.lists.table.is_some() {
            if self.word(b"list") || self.word(b"listoverride") {
                self.lists.descriptors.push(ListDescriptor::default());
                self.lists.current = Some(self.lists.descriptors.len() - 1);
                self.lists.level = -1;
            } else if self.lists.current.is_some() && self.word(b"listlevel") {
                self.lists.level = self.lists.level.wrapping_add(1);
            }
        }
        if !self.group.ignore
            && matches!(
                self.control.as_slice(),
                b"par"
                    | b"pard"
                    | b"sect"
                    | b"sectd"
                    | b"plain"
                    | b"ltrch"
                    | b"rtlch"
                    | b"htmlrtf"
                    | b"line"
            )
        {
            self.in_header = false;
        }
    }

    /// Opens the `<a>` of a hyperlink's result, first ending the styles of the text before it.
    fn open_hyperlink(&mut self) -> Step {
        let Field::PendingLink(url) = std::mem::replace(&mut self.field, Field::None) else {
            return Ok(());
        };
        self.lazy_start_paragraph()?;
        self.open.push("a");
        self.out.start_element_with("a", &[("href", &url)])?;
        self.field = Field::Link;
        self.hyperlink_depth = self.group.depth;
        self.group.ignore = false;
        Ok(())
    }

    fn start_styles(&mut self) -> Step {
        if self.field != Field::None {
            return Ok(());
        }
        if self.group.bold {
            self.start("b")?;
        }
        if self.group.italic {
            self.start("i")?;
        }
        Ok(())
    }

    fn end_styles(&mut self) -> Step {
        if self.field != Field::None {
            return Ok(());
        }
        if self.group.italic {
            self.end("i")?;
            self.group.italic = false;
        }
        if self.group.bold {
            self.end("b")?;
            self.group.bold = false;
        }
        Ok(())
    }

    fn group_start(&mut self) -> Step {
        self.ansi_skip = 0;
        let nested = self.group.nested();
        self.groups.push(std::mem::replace(&mut self.group, nested));
        if self.upr == Upr::Started {
            self.upr = Upr::InGroups;
            self.group.ignore = true;
        }
        let second = self.input.read();
        if second == Some(b'\\') {
            let third = self.input.read();
            if third == Some(b'*') {
                self.group.ignore = true;
            }
            self.input.unread(third)?;
        }
        self.input.unread(second)?;
        Ok(())
    }

    fn group_end(&mut self) -> Step {
        if self.in_header {
            if let Some(property) = self.info.take() {
                self.write_info(property)?;
            }
            self.pending_buffer.clear();
        }
        self.ansi_skip = 0;
        let picture_ends =
            self.group.pict_depth == 1 && !self.group.sn && !self.group.sv && !self.group.sp;
        if self.group.objdata || picture_ends {
            self.embedded.complete()?;
        }
        if self.group.annotation {
            self.add_output_char(u16::from(b' '))?;
        }
        if let Some(outer) = self.groups.pop() {
            if self.field == Field::Link
                && self.hyperlink_depth >= 0
                && outer.depth < self.hyperlink_depth
            {
                self.end("a")?;
                self.field = Field::None;
                self.hyperlink_depth = -1;
            }
            if self.field == Field::None {
                if self.group.italic && (!outer.italic || self.group.bold != outer.bold) {
                    self.end("i")?;
                    self.group.italic = false;
                }
                if self.group.bold && !outer.bold {
                    self.end("b")?;
                }
                if !self.group.bold && outer.bold {
                    self.start("b")?;
                }
                if !self.group.italic && outer.italic {
                    self.start("i")?;
                }
            }
            self.group = outer;
        }
        match self.field {
            Field::Instruction => self.read_instruction(),
            Field::Link => {
                if self.hyperlink_depth >= 0 && self.group.depth < self.hyperlink_depth {
                    self.end("a")?;
                    self.field = Field::None;
                    self.hyperlink_depth = -1;
                }
            }
            Field::None | Field::PendingLink(_) => {}
        }
        Ok(())
    }

    /// Reads a field instruction once its group ends: a `HYPERLINK` makes its result a link to
    /// its quoted target, marked local by a `\l` switch; other fields are written as text.
    fn read_instruction(&mut self) {
        let instruction = String::from_utf16_lossy(&self.pending_buffer);
        self.pending_buffer.clear();
        let instruction = java::trim(&instruction);
        let Some(target) = instruction.strip_prefix(HYPERLINK) else {
            self.field = Field::None;
            return;
        };
        let mut target = java::trim(target);
        let local = target.contains("\\l ");
        if let Some(open) = target.find('"') {
            if let Some(close) = target[open + 1..].find('"') {
                target = &target[open + 1..open + 1 + close];
            }
        }
        self.field = Field::PendingLink(if local {
            format!("#{target}")
        } else {
            target.to_owned()
        });
    }

    /// Ends an `\info` property's group: its text, or for `\creatim` its date, is the value.
    fn write_info(&mut self, property: InfoProperty) -> Step {
        if !property.is_text() {
            if let Some(created) = self.date.instant() {
                self.metadata().set("dcterms:created", created);
            }
            return Ok(());
        }
        let value = String::from_utf16_lossy(&self.pending_buffer);
        let metadata = self.metadata();
        match property {
            InfoProperty::Creator => metadata.add("dc:creator", value),
            InfoProperty::Title => metadata.add("dc:title", value),
            InfoProperty::Subject => metadata.add("dc:subject", value),
            InfoProperty::Keywords => {
                metadata.add("meta:keyword", value.clone());
                metadata.add("dc:subject", value);
            }
            InfoProperty::Category => metadata.set("cp:category", value),
            InfoProperty::Comments => metadata.add("w:Comments", value),
            InfoProperty::Company => metadata.set("extended-properties:Company", value),
            InfoProperty::Manager => metadata.add("extended-properties:Manager", value),
            InfoProperty::Template => metadata.set("extended-properties:Template", value),
            InfoProperty::Created => unreachable!("the creation date is not text"),
        }
        Ok(())
    }
}

/// The character a control word without a parameter stands for, if it is one.
fn special_character(word: &[u8]) -> Option<u16> {
    Some(match word {
        b"line" | b"page" | b"softline" | b"softpage" => u16::from(b'\n'),
        b"column" | b"softcolumn" => u16::from(b' '),
        b"tab" => u16::from(b'\t'),
        b"bullet" => 0x2022,
        b"endash" => 0x2013,
        b"emdash" => 0x2014,
        b"enspace" => 0x2002,
        b"qmspace" => 0x2005,
        b"emspace" => 0x2003,
        b"lquote" => 0x2018,
        b"rquote" => 0x2019,
        b"ldblquote" => 0x201C,
        b"rdblquote" => 0x201D,
        _ => return None,
    })
}
