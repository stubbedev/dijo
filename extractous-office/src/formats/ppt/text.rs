//! The text of slides as POI's `HSLFTextParagraph` finds it: the text atoms after each text
//! header split into paragraphs at carriage returns, the runs of their character styles and the
//! paragraph styles and indents of their style atoms, the hyperlinks of their interactive
//! records, and whether a paragraph is a bullet by its own flags or its master's styles.

use crate::formats::poi::{i16_at, i32_at, u16_at, Read, Thrown, Unread};
use crate::formats::Unported;

use super::records::{kind, Record};

/// `HSLFTextParagraph.MAX_NUMBER_OF_STYLES`.
const MAX_NUMBER_OF_STYLES: i32 = 20_000;
const CARRIAGE_RETURN: u16 = 0x0D;
const LINE_FEED: u16 = 0x0A;

/// Text placeholder types, as `TextShape.TextPlaceholder` numbers them.
pub(super) mod placeholder {
    pub(crate) const TITLE: i32 = 0;
    pub(crate) const BODY: i32 = 1;
    pub(crate) const CENTER_BODY: i32 = 5;
    pub(crate) const CENTER_TITLE: i32 = 6;
    pub(crate) const NOTES: i32 = 2;
    pub(crate) const OTHER: i32 = 4;
}

/// Whether a property collection is a paragraph's or a character run's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PropType {
    Paragraph,
    Character,
}

/// A text property `TextPropCollection` may hold: its mask in the header and the bytes it
/// takes, where it is a fixed size.
#[derive(Debug, Clone, Copy)]
enum Size {
    Fixed(usize),
    /// `HSLFTabStopPropCollection`: a count, then that many tab stops of four bytes.
    TabStops,
}

const PARAGRAPH_PROPS: [(i32, Size); 19] = [
    (0xF, Size::Fixed(2)),
    (0x80, Size::Fixed(2)),
    (0x10, Size::Fixed(2)),
    (0x40, Size::Fixed(2)),
    (0x20, Size::Fixed(4)),
    (0x800, Size::Fixed(2)),
    (0x1000, Size::Fixed(2)),
    (0x2000, Size::Fixed(2)),
    (0x4000, Size::Fixed(2)),
    (0x100, Size::Fixed(2)),
    (0x400, Size::Fixed(2)),
    (0x8000, Size::Fixed(2)),
    (0x10_0000, Size::TabStops),
    (0x1_0000, Size::Fixed(2)),
    (0xE_0000, Size::Fixed(2)),
    (0x20_0000, Size::Fixed(2)),
    (0x80_0000, Size::Fixed(0)),
    (0x100_0000, Size::Fixed(0)),
    (0x200_0000, Size::Fixed(0)),
];

const CHARACTER_PROPS: [(i32, Size); 12] = [
    (0x10_0000, Size::Fixed(0)),
    (0x100_0000, Size::Fixed(0)),
    (0x200_0000, Size::Fixed(0)),
    (0x400_0000, Size::Fixed(0)),
    (0xFFFF, Size::Fixed(2)),
    (0x1_0000, Size::Fixed(2)),
    (0x20_0000, Size::Fixed(2)),
    (0x40_0000, Size::Fixed(2)),
    (0x80_0000, Size::Fixed(2)),
    (0x2_0000, Size::Fixed(2)),
    (0x4_0000, Size::Fixed(4)),
    (0x8_0000, Size::Fixed(2)),
];

/// The mask of `ParagraphFlagsTextProp`, the first paragraph property.
const PARAGRAPH_FLAGS_MASK: i32 = 0xF;

/// A paragraph's flags (`ParagraphFlagsTextProp`): the value its record gives, masked to the
/// bits the record sets, and which of the four bits it sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Flags {
    value: i32,
    set: [bool; 4],
}

impl Flags {
    /// `setValueWithMask(value, containsField)`.
    fn read(value: i32, contains: i32) -> Flags {
        let set = [0, 1, 2, 3].map(|bit| contains & (1 << bit) != 0);
        let mut masked = value;
        for (bit, &is_set) in set.iter().enumerate() {
            if !is_set {
                masked &= !(1 << bit);
            }
        }
        Flags { value: masked, set }
    }

    /// `getSubValue(BULLET_IDX)`.
    pub(super) fn bullet(self) -> bool {
        self.set[0] && self.value & 1 != 0
    }
}

/// A collection of text properties (`TextPropCollection`), as far as Tika's extractor reads it:
/// the characters it covers, its indent level and a paragraph's flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Props {
    pub(super) covered: i32,
    pub(super) indent: i16,
    pub(super) flags: Option<Flags>,
}

impl Props {
    fn new(covered: i32) -> Props {
        Props {
            covered,
            indent: 0,
            flags: None,
        }
    }

    /// `buildTextPropList`: the properties `contains` names, read from `data` at `offset`, and
    /// the bytes they take.
    fn build(&mut self, kind: PropType, contains: i32, data: &[u8], offset: usize) -> Read<usize> {
        let props: &[(i32, Size)] = match kind {
            PropType::Paragraph => &PARAGRAPH_PROPS,
            PropType::Character => &CHARACTER_PROPS,
        };
        let mut passed = 0usize;
        for &(mask, size) in props {
            if contains & mask == 0 {
                continue;
            }
            let at = offset + passed;
            if at >= data.len() {
                return Ok(passed);
            }
            let taken = match size {
                Size::Fixed(0) => continue,
                Size::Fixed(2) => {
                    let value = i32::from(i16_at(data, at)?);
                    if kind == PropType::Paragraph && mask == PARAGRAPH_FLAGS_MASK {
                        self.flags = Some(Flags::read(value, contains));
                    }
                    2
                }
                Size::Fixed(4) => {
                    i32_at(data, at)?;
                    4
                }
                Size::Fixed(_) => unreachable!("text properties take 0, 2 or 4 bytes"),
                Size::TabStops => {
                    if at + 2 > data.len() {
                        return Err(Thrown::new("Buffer overrun reading tab stops"));
                    }
                    let count = usize::from(u16_at(data, at)?);
                    if at + 2 + count * 4 > data.len() {
                        return Err(Thrown::new("Buffer overrun reading tab stops"));
                    }
                    2 + count * 4
                }
            };
            passed += taken;
        }
        Ok(passed)
    }
}

/// `StyleTextPropAtom.setParentTextSize`: the paragraph and character styles of a style atom
/// for text of `size` characters.
fn styles(raw: &[u8], size: i32) -> Read<(Vec<Props>, Vec<Props>)> {
    let check = |read: i32, handled: i32| {
        if read.wrapping_add(handled) > size.wrapping_add(1) {
            size.wrapping_add(1).wrapping_sub(handled)
        } else {
            read
        }
    };
    let mut position = 0usize;
    let mut paragraphs = Vec::new();
    let mut handled = 0i32;
    let mut limit = size;
    while position < raw.len() && handled < limit {
        let length = check(i32_at(raw, position)?, handled);
        handled = handled.wrapping_add(length);
        position += 4;
        let indent = i16_at(raw, position)?;
        position += 2;
        let flags = i32_at(raw, position)?;
        position += 4;
        let mut props = Props::new(length);
        props.indent = indent;
        position += props.build(PropType::Paragraph, flags, raw, position)?;
        paragraphs.push(props);
        if position < raw.len() && handled == size {
            limit = limit.wrapping_add(1);
        }
    }
    let mut characters = Vec::new();
    handled = 0;
    limit = size;
    while position < raw.len() && handled < limit {
        let length = check(i32_at(raw, position)?, handled);
        handled = handled.wrapping_add(length);
        position += 4;
        let flags = i32_at(raw, position)?;
        position += 4;
        let mut props = Props::new(length);
        position += props.build(PropType::Character, flags, raw, position)?;
        characters.push(props);
        if position < raw.len() && handled == size {
            limit = limit.wrapping_add(1);
        }
    }
    Ok((paragraphs, characters))
}

/// The styles of a master's text type (`TxMasterStyleAtom`): its paragraph and character styles
/// by indent level, as far as `init` read them before it threw, or `None` where it set none.
#[derive(Debug, Clone)]
pub(super) struct MasterStyles {
    pub(super) text_type: i32,
    pub(super) paragraphs: Option<Vec<Props>>,
    pub(super) characters: Option<Vec<Props>>,
}

impl MasterStyles {
    /// `new TxMasterStyleAtom(...)`, whose `init` swallows what it throws.
    pub(super) fn read(atom: &Record) -> MasterStyles {
        let text_type = atom.instance();
        let data = atom.data();
        let mut styles = MasterStyles {
            text_type,
            paragraphs: None,
            characters: None,
        };
        let Ok(levels) = i16_at(data, 0) else {
            return styles;
        };
        if levels < 0 {
            return styles;
        }
        let mut paragraphs = Vec::new();
        let mut characters = Vec::new();
        let _ = Self::init(text_type, levels, data, &mut paragraphs, &mut characters);
        styles.paragraphs = Some(paragraphs);
        styles.characters = Some(characters);
        styles
    }

    fn init(
        text_type: i32,
        levels: i16,
        data: &[u8],
        paragraphs: &mut Vec<Props>,
        characters: &mut Vec<Props>,
    ) -> Read<()> {
        let mut position = 2usize;
        for _ in 0..levels {
            let mut paragraph = Props::new(0);
            if text_type >= placeholder::CENTER_BODY {
                paragraph.indent = i16_at(data, position)?;
                position += 2;
            } else {
                paragraph.indent = -1;
            }
            let head = i32_at(data, position)?;
            position += 4;
            position += paragraph.build(PropType::Paragraph, head, data, position)?;
            paragraphs.push(paragraph);
            let head = i32_at(data, position)?;
            position += 4;
            let mut character = Props::new(0);
            position += character.build(PropType::Character, head, data, position)?;
            characters.push(character);
        }
        Ok(())
    }
}

/// The hyperlink of a run: the URL of its `ExHyperlink`, if it has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Link {
    pub(super) address: Option<String>,
}

/// A text run (`HSLFTextRun`): its text in Java's units, as POI keeps it internally, and its
/// hyperlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Run {
    pub(super) text: Vec<u16>,
    pub(super) link: Option<Link>,
}

impl Run {
    fn new(text: &[u16]) -> Run {
        Run {
            text: internal(text),
            link: None,
        }
    }
}

/// `toInternalString`: each line feed, and each carriage return with a line feed, a carriage
/// return.
pub(super) fn internal(text: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == CARRIAGE_RETURN && text.get(i + 1) == Some(&LINE_FEED) {
            out.push(CARRIAGE_RETURN);
            i += 2;
        } else if text[i] == LINE_FEED {
            out.push(CARRIAGE_RETURN);
            i += 1;
        } else {
            out.push(text[i]);
            i += 1;
        }
    }
    out
}

/// A paragraph (`HSLFTextParagraph`).
#[derive(Debug, Clone)]
pub(super) struct Paragraph<'a> {
    /// The children of its text header's parent, a slide list or a text box.
    pub(super) parent: &'a [Record<'a>],
    /// Where its text header is among them.
    pub(super) header: usize,
    /// `getIndex`: the text's index in its slide list, -1 for a drawing's text.
    pub(super) index: i32,
    /// `getRunType`: the text header's type.
    pub(super) run_type: i32,
    pub(super) style: Props,
    pub(super) runs: Vec<Run>,
}

impl Paragraph<'_> {
    pub(super) fn indent_level(&self) -> i32 {
        i32::from(self.style.indent)
    }

    /// `getRecords`: the records from its text header up to the next text header or slide.
    fn records(&self) -> &[Record<'_>] {
        let rest = &self.parent[self.header..];
        let end = rest
            .iter()
            .skip(1)
            .position(|record| {
                record.kind == kind::TEXT_HEADER_ATOM || record.kind == kind::SLIDE_PERSIST_ATOM
            })
            .map_or(rest.len(), |at| at + 1);
        &rest[..end]
    }

    /// The paragraph's own flags, where its style sets them.
    pub(super) fn own_bullet(&self) -> Option<bool> {
        self.style.flags.map(Flags::bullet)
    }
}

/// `getRecords(records, startIdx, null)`: from `start`, the records from the next text header
/// up to the one after it or a slide, and where the search continues.
fn next_group(records: &[Record], start: usize) -> (std::ops::Range<usize>, usize) {
    let Some(header) = records[start..]
        .iter()
        .position(|record| record.kind == kind::TEXT_HEADER_ATOM)
        .map(|at| start + at)
    else {
        return (records.len()..records.len(), records.len());
    };
    let end = records[header + 1..]
        .iter()
        .position(|record| {
            record.kind == kind::TEXT_HEADER_ATOM || record.kind == kind::SLIDE_PERSIST_ATOM
        })
        .map_or(records.len(), |at| header + 1 + at);
    (header..end, end)
}

/// `findTextParagraphs(Record[])` of `parent[range]`: the paragraphs of each text header, styled.
/// A text header of a slide list is numbered by its place among them.
pub(super) fn find_paragraphs<'a>(
    parent: &'a [Record],
    range: std::ops::Range<usize>,
    in_slide_list: bool,
) -> Result<Vec<Vec<Paragraph<'a>>>, Unread> {
    let records = &parent[range.clone()];
    let mut collection = Vec::new();
    let mut position = 0usize;
    let mut list_index = 0i32;
    while position < records.len() {
        let (group, next) = next_group(records, position);
        position = next;
        let group_records = &records[group.clone()];
        let mut header = None;
        let mut bytes = None;
        let mut chars = None;
        let mut indents = None;
        for (at, record) in group_records.iter().enumerate() {
            match record.kind {
                kind::TEXT_HEADER_ATOM => header = Some(range.start + group.start + at),
                kind::TEXT_BYTES_ATOM => bytes = Some(record),
                kind::TEXT_CHARS_ATOM => chars = Some(record),
                kind::MASTER_TEXT_PROP_ATOM => indents = Some(record),
                _ => {}
            }
        }
        let Some(header) = header else {
            break;
        };
        let index = if in_slide_list { list_index } else { -1 };
        list_index += 1;
        let text = match (chars, bytes) {
            (Some(chars), _) => chars.utf16(),
            (None, Some(bytes)) => bytes.latin1(),
            (None, None) => Vec::new(),
        };
        let size = text.len() as i32;
        let (paragraph_styles, character_styles) = style_of(parent, header, size)?;
        let run_type = parent[header].int(0).map_err(thrown)?;
        let mut paragraphs: Vec<Paragraph<'a>> = split_paragraphs(&text)
            .map(|segment| {
                let mut style = Props::new(1);
                style.covered = segment.len() as i32;
                Paragraph {
                    parent,
                    header,
                    index,
                    run_type,
                    style,
                    runs: vec![Run::new(segment)],
                }
            })
            .collect();
        apply_character_styles(&mut paragraphs, &character_styles)?;
        apply_paragraph_styles(&mut paragraphs, &paragraph_styles);
        if let Some(indents) = indents {
            apply_indents(&mut paragraphs, &master_indents(indents.data()));
        }
        collection.push(paragraphs);
    }
    Ok(collection)
}

fn thrown(thrown: Thrown) -> Unread {
    thrown.within("reading the presentation's text").into()
}

/// `findStyleAtomPresent`: the styles of the last style atom after the header before the next
/// one, or a style atom's defaults for text of `size` characters.
fn style_of(
    parent: &[Record],
    header: usize,
    size: i32,
) -> Result<(Vec<Props>, Vec<Props>), Unread> {
    let style = parent[header + 1..]
        .iter()
        .take_while(|record| record.kind != kind::TEXT_HEADER_ATOM)
        .filter(|record| record.kind == kind::STYLE_TEXT_PROP_ATOM)
        .last();
    match style {
        Some(style) => styles(style.data(), size).map_err(thrown),
        None => {
            let covered = if size < 0 { 1 } else { size };
            Ok((vec![Props::new(covered)], vec![Props::new(covered)]))
        }
    }
}

/// `rawText.split("(?<=\r)")`: the text cut after each carriage return, without a trailing
/// empty part, one empty part for empty text.
fn split_paragraphs(text: &[u16]) -> impl Iterator<Item = &[u16]> {
    let mut parts: Vec<&[u16]> = text
        .split_inclusive(|&unit| unit == CARRIAGE_RETURN)
        .collect();
    if parts.is_empty() {
        parts.push(&[]);
    }
    parts.into_iter()
}

/// `MasterTextPropAtom`'s indents: the characters each covers and its level.
fn master_indents(data: &[u8]) -> Vec<(i32, i16)> {
    data.as_chunks::<6>()
        .0
        .iter()
        .map(|chunk| {
            let count = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let level = i16::from_le_bytes([chunk[4], chunk[5]]);
            (count, level)
        })
        .collect()
}

fn run_length(run: &Run) -> i32 {
    run.text.len() as i32
}

fn missing() -> Unread {
    Thrown::new("character styles that run past the paragraphs").into()
}

/// `applyCharacterStyles`: the runs split where the character styles change.
fn apply_character_styles(paragraphs: &mut [Paragraph], styles: &[Props]) -> Result<(), Unread> {
    let mut paragraph_index = 0usize;
    let mut run_index = 0usize;
    for (style_index, style) in styles.iter().enumerate() {
        let covered = style.covered;
        if covered > MAX_NUMBER_OF_STYLES {
            return Err(Unported(format!(
                "a paragraph covered by {covered} styles, more than POI processes"
            ))
            .into());
        }
        let mut run_covered = 0i32;
        while run_covered < covered {
            let paragraph_count = paragraphs.len();
            let paragraph = paragraphs.get_mut(paragraph_index).ok_or_else(missing)?;
            let runs = &mut paragraph.runs;
            let length = run_length(runs.get(run_index).ok_or_else(missing)?);
            if run_covered + length <= covered {
                run_covered += length;
            } else {
                let cut = (covered - run_covered) as usize;
                let rest = runs[run_index].text.split_off(cut);
                runs.insert(run_index + 1, Run::new(&rest));
                run_covered += covered - run_covered;
            }
            if paragraph_index == paragraph_count - 1 && run_index == runs.len() - 1 {
                if style_index < styles.len() - 1 {
                    runs.push(Run::new(&[]));
                }
                run_covered += 1;
            }
            run_index += 1;
            if run_index == runs.len() {
                paragraph_index += 1;
                run_index = 0;
            }
        }
    }
    Ok(())
}

/// `applyParagraphStyles`: each paragraph's style from the style covering it.
fn apply_paragraph_styles(paragraphs: &mut [Paragraph], styles: &[Props]) {
    let mut paragraph_index = 0usize;
    for style in styles {
        let mut covered = 0i32;
        while covered < style.covered {
            if paragraph_index >= paragraphs.len() {
                return;
            }
            let last = paragraph_index == paragraphs.len() - 1;
            let paragraph = &mut paragraphs[paragraph_index];
            let mut length: i32 = paragraph.runs.iter().map(run_length).sum();
            if last {
                length += 1;
            }
            let mut copy = style.clone();
            copy.covered = length;
            paragraph.style = copy;
            covered += length;
            paragraph_index += 1;
        }
    }
}

/// `applyParagraphIndents`.
fn apply_indents(paragraphs: &mut [Paragraph], indents: &[(i32, i16)]) {
    let mut paragraph_index = 0usize;
    for &(count, level) in indents {
        let mut covered = 0i32;
        while covered < count {
            if paragraph_index >= paragraphs.len() || covered >= count - 1 {
                return;
            }
            let paragraph = &mut paragraphs[paragraph_index];
            let length: i32 = paragraph.runs.iter().map(run_length).sum();
            paragraph.style.indent = level;
            covered += length + 1;
            paragraph_index += 1;
        }
    }
}

/// A hyperlink of text (`HSLFHyperlink`): its address, and the characters its text run info
/// covers, if it has any.
#[derive(Debug, Clone)]
struct TextLink {
    link: Link,
    range: Option<(i32, i32)>,
}

/// The links `HSLFHyperlink.find(records, exobj)` finds: each interactive record whose
/// hyperlink the document lists, with the text run info that follows it.
fn find_links(records: &[Record], objects: Option<&Record>) -> Result<Vec<TextLink>, Unread> {
    let Some(objects) = objects else {
        return Ok(Vec::new());
    };
    let mut links = Vec::new();
    let mut i = 0;
    while i < records.len() {
        let record = &records[i];
        i += 1;
        if record.kind != kind::INTERACTIVE_INFO {
            continue;
        }
        let Some(atom) = record
            .children()
            .first()
            .filter(|child| child.kind == kind::INTERACTIVE_INFO_ATOM)
        else {
            continue;
        };
        let id = atom.int(4).map_err(thrown)?;
        let Some(hyperlink) = find_hyperlink(objects, id).map_err(thrown)? else {
            continue;
        };
        let link = Link {
            address: link_url(hyperlink),
        };
        let mut range = None;
        if let Some(next) = records.get(i) {
            if next.kind == kind::TX_INTERACTIVE_INFO_ATOM {
                i += 1;
                range = Some((next.int(0).map_err(thrown)?, next.int(4).map_err(thrown)?));
            }
        }
        links.push(TextLink { link, range });
    }
    Ok(links)
}

/// `ExObjList.get(id)`: the first hyperlink of the document's objects numbered `id`.
pub(super) fn find_hyperlink<'r, 'd>(
    objects: &'r Record<'d>,
    id: i32,
) -> Read<Option<&'r Record<'d>>> {
    for child in objects.children() {
        if child.kind != kind::EX_HYPERLINK {
            continue;
        }
        let Some(atom) = child
            .children()
            .first()
            .filter(|atom| atom.kind == kind::EX_HYPERLINK_ATOM)
        else {
            continue;
        };
        if atom.int(0)? == id {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

/// `ExHyperlink.getLinkURL`: the text of the last string after the first, among its children
/// after the first.
fn link_url(hyperlink: &Record) -> Option<String> {
    hyperlink
        .children()
        .iter()
        .skip(1)
        .filter(|child| child.kind == kind::C_STRING)
        .skip(1)
        .last()
        .map(|string| String::from_utf16_lossy(&string.utf16()))
}

/// `applyHyperlinks`: the runs a hyperlink's text run info covers given its link, split where
/// it starts and ends.
pub(super) fn apply_hyperlinks(
    arena: &mut [Paragraph],
    list: &[usize],
    objects: Option<&Record>,
) -> Result<(), Unread> {
    let Some(&first) = list.first() else {
        return Ok(());
    };
    let links = find_links(arena[first].records(), objects)?;
    for TextLink { link, range } in links {
        let (start, end) = range.unwrap_or((-1, -1));
        let mut covered = 0i32;
        for &id in list {
            let paragraph = &mut arena[id];
            if covered > end {
                break;
            }
            let mut run_index = 0;
            while run_index < paragraph.runs.len() {
                let mut length = run_length(&paragraph.runs[run_index]);
                if covered < end && start < covered + length {
                    let start_in_run = start - covered;
                    if start_in_run > 0 {
                        let run = &mut paragraph.runs[run_index];
                        let rest = run.text.split_off(start_in_run as usize);
                        paragraph.runs.insert(run_index + 1, Run::new(&rest));
                        covered += start_in_run;
                        run_index += 1;
                        continue;
                    }
                    let end_in_run = length.min(end - start);
                    if end_in_run < length {
                        let run = &mut paragraph.runs[run_index];
                        let rest = run.text.split_off(end_in_run as usize);
                        let head = std::mem::replace(&mut run.text, rest);
                        paragraph.runs.insert(run_index, Run::new(&head));
                        length = end_in_run;
                    }
                    paragraph.runs[run_index].link = Some(link.clone());
                }
                covered += length;
                run_index += 1;
            }
        }
    }
    Ok(())
}

/// `toExternalString`: carriage returns as line feeds, vertical tabs as line feeds in titles
/// and drawings' text, as spaces otherwise.
pub(super) fn external(raw: &[u16], run_type: i32) -> Vec<u16> {
    let tab = if run_type == -1
        || run_type == placeholder::TITLE
        || run_type == placeholder::CENTER_TITLE
    {
        LINE_FEED
    } else {
        u16::from(b' ')
    };
    raw.iter()
        .map(|&unit| match unit {
            CARRIAGE_RETURN => LINE_FEED,
            0x0B => tab,
            other => other,
        })
        .collect()
}

/// `getRawText` of paragraphs: the text of their runs.
pub(super) fn raw_text(paragraphs: &[&Paragraph]) -> Vec<u16> {
    paragraphs
        .iter()
        .flat_map(|paragraph| paragraph.runs.iter())
        .flat_map(|run| run.text.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::ppt::records::tests::record;
    use crate::formats::ppt::records::{build_at, Body, Depth};

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn atoms(records: &[Vec<u8>]) -> Vec<Record<'static>> {
        let data = records.concat();
        let container = record(0x0F, kind::LIST, &data);
        match build_at(&container, 0, Depth::TOP)
            .unwrap()
            .unwrap()
            .into_owned()
            .body
        {
            Body::Container(children) => children,
            _ => unreachable!(),
        }
    }

    #[test]
    fn splits_text_into_paragraphs_and_styles_runs() {
        let mut style = Vec::new();
        style.extend(6i32.to_le_bytes());
        style.extend(0i16.to_le_bytes());
        style.extend(0xFi32.to_le_bytes());
        style.extend(1i16.to_le_bytes());
        style.extend(3i32.to_le_bytes());
        style.extend(0i16.to_le_bytes());
        style.extend(0i32.to_le_bytes());
        style.extend(2i32.to_le_bytes());
        style.extend(0i32.to_le_bytes());
        style.extend(7i32.to_le_bytes());
        style.extend(0i32.to_le_bytes());
        let records = atoms(&[
            record(0, kind::TEXT_HEADER_ATOM, &1i32.to_le_bytes()),
            record(0, kind::TEXT_BYTES_ATOM, b"abcde\rfg"),
            record(0, kind::STYLE_TEXT_PROP_ATOM, &style),
        ]);
        let lists = find_paragraphs(&records, 0..records.len(), false).unwrap();
        assert_eq!(lists.len(), 1);
        let paragraphs = &lists[0];
        assert_eq!(paragraphs.len(), 2);
        let texts: Vec<Vec<Vec<u16>>> = paragraphs
            .iter()
            .map(|paragraph| paragraph.runs.iter().map(|run| run.text.clone()).collect())
            .collect();
        assert_eq!(
            texts,
            [vec![units("ab"), units("cde\r")], vec![units("fg")]]
        );
        assert_eq!(paragraphs[0].own_bullet(), Some(true));
        assert_eq!(paragraphs[1].own_bullet(), None);
    }

    #[test]
    fn converts_line_feeds_as_poi_does() {
        assert_eq!(internal(&units("a\r\nb\nc")), units("a\rb\rc"));
        assert_eq!(external(&units("a\rb\u{b}c"), 1), units("a\nb c"));
        assert_eq!(external(&units("a\u{b}c"), -1), units("a\nc"));
    }

    #[test]
    fn splits_at_carriage_returns_as_java_does() {
        let text = units("a\r\rb\r");
        let parts: Vec<&[u16]> = split_paragraphs(&text).collect();
        assert_eq!(parts, [&units("a\r")[..], &units("\r"), &units("b\r")]);
        assert_eq!(split_paragraphs(&[]).count(), 1);
    }
}
