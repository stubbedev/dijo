//! Works 2000 to 9 word processor documents, as libwps' `WPS8Parser` and `WPS8Text` read their
//! `CONTENTS` stream: an index of named zones; the text, in UTF-16LE, in the `TEXT` zone; its
//! division into stories (body, footnotes, endnotes, header, footer, text boxes and tables) in
//! the `STRS` PLC; the character properties in the `FDPC` pages the `BTEC` PLC lists, which
//! tell a `#` that stands for a footnote mark or a field from one typed; and the ends of a
//! table's cells in the `TCD ` PLCs.

use std::collections::BTreeSet;
use std::ops::Range;

use super::{Bytes, Malformed, Paragraphs, Stories, Story};

/// What the stream starts with: Works 7 to 9 (`CHNKWKS`) and Works 2000 (`CHNKINK`).
pub(super) const MAGICS: [&[u8]; 2] = [b"CHNKWKS", b"CHNKINK"];

/// Where the index starts, its count of entries, and the most entries a block of it holds.
const INDEX: usize = 0x18;
const INDEX_COUNT: usize = 0x0C;
const MAX_BLOCK_ENTRIES: u16 = 0x20;
/// How long an index entry is, at least, and the length below which it is no entry at all.
const ENTRY: u16 = 0x18;
const MIN_ENTRY: u16 = 10;
/// The next block's offset of the last block of the index.
const LAST_BLOCK: u32 = u32::MAX;
/// How long a PLC's header is before its text positions, and with its last position.
const PLC_HEADER: usize = 12;
const PLC_MINIMUM: usize = 16;
/// How long an `FDPC` page's header is, and each of its runs' position and property offset.
const FDP_HEADER: usize = 8;
const FDP_RUN: usize = 6;

/// The names of the zones read, as the index names them, and the types they must have.
const TEXT: Tag = *b"TEXT";
const STRS: Tag = *b"STRS";
const BTEC: Tag = *b"BTEC";
const FDPC: Tag = *b"FDPC";
const TCD: Tag = *b"TCD ";
const PLC: Tag = *b"PLC ";

/// The structured data item of a story's kind in the `STRS` PLC, and of a character's special
/// kind in a character property: their ids and types.
const STORY_ITEM: (u8, u8) = (0x00, 0x22);
const SPECIAL_ITEM: (u8, u8) = (0x00, 0x12);

/// The kinds of story the `STRS` PLC names.
const BODY: i64 = 1;
const FOOTNOTES: i64 = 2;
const ENDNOTES: i64 = 3;
const HEADER: i64 = 6;
const FOOTER: i64 = 7;

/// The characters of the text that mean more than themselves.
const TAB: u16 = 0x09;
const LINE_BREAK: u16 = 0x0A;
const PAGE_BREAK: u16 = 0x0C;
const PARAGRAPH_END: u16 = 0x0D;
const COLUMN_BREAK: u16 = 0x0E;
const NON_BREAKING_HYPHEN: u16 = 0x1E;
const NON_BREAKING_SPACE: u16 = 0x1F;
/// The character that stands for a footnote mark, a field or a comment where its properties
/// make it special, and is itself otherwise.
const SPECIAL: u16 = 0x23;
/// The character that holds the place of a picture, a table or another object.
const OBJECT: u16 = 0xFFFC;

/// The four bytes that name a zone and its type.
type Tag = [u8; 4];

/// A zone the index lists: its name, its type, its id and where it lies in the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: Tag,
    kind: Tag,
    id: u16,
    range: Range<usize>,
}

/// A story of the text, and where its characters lie in the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Zone {
    story: Story,
    range: Range<usize>,
}

/// The stories of the `CONTENTS` stream `contents`.
pub(super) fn read(contents: &[u8]) -> Result<Stories, Malformed> {
    let data = Bytes(contents);
    let entries = index(data).map_err(|malformed| malformed.within("index"))?;
    let text = named(&entries, TEXT)
        .next()
        .ok_or_else(|| Malformed("the index lists no TEXT zone".to_owned()))?
        .range
        .clone();
    if !text.len().is_multiple_of(2) {
        return Err(Malformed(format!(
            "the TEXT zone at {} is {} bytes long, no whole number of UTF-16 characters",
            text.start,
            text.len()
        )));
    }
    let zones = zones(data, &entries, &text).map_err(|malformed| malformed.within("STRS"))?;
    let specials = specials(data, &entries, &text).map_err(|malformed| malformed.within("FDPC"))?;
    let cells = cells(data, &entries, &zones).map_err(|malformed| malformed.within("TCD"))?;
    let mut stories = Stories::default();
    for zone in &zones {
        let mut paragraphs = Paragraphs::default();
        decode(data, zone.range.clone(), &specials, &cells, &mut paragraphs)?;
        stories.add(zone.story, paragraphs);
    }
    Ok(stories)
}

/// `WPS8Parser::parseHeaderIndex`: the zones the index lists, those that lie in the stream, in
/// its order.
fn index(data: Bytes<'_>) -> Result<Vec<Entry>, Malformed> {
    let mut remaining = data.u16(INDEX_COUNT)?;
    let mut entries = Vec::new();
    let mut block = INDEX;
    while remaining > 0 {
        let count = data.u16(block + 2)?;
        if count > MAX_BLOCK_ENTRIES {
            return Err(Malformed(format!(
                "the block at {block} lists {count} entries, more than {MAX_BLOCK_ENTRIES}"
            )));
        }
        let next = data.u32(block + 4)?;
        let mut at = block + 8;
        for _ in 0..count.min(remaining) {
            let length = data.u16(at)?;
            if length < MIN_ENTRY {
                return Err(Malformed(format!(
                    "the entry at {at} is {length} bytes long, shorter than {MIN_ENTRY}"
                )));
            }
            if length >= ENTRY {
                entries.extend(entry(data, at)?);
            }
            at += usize::from(length);
            remaining -= 1;
        }
        if next == LAST_BLOCK {
            break;
        }
        let next = next as usize;
        if next <= block {
            return Err(Malformed(format!(
                "the block at {block} is followed by one before it, at {next}"
            )));
        }
        block = next;
    }
    Ok(entries)
}

/// `WPS8Parser::parseHeaderIndexEntry`: the zone the entry at `at` lists, unless its name is
/// none or it lies past the end of the stream, which libwps passes over.
fn entry(data: Bytes<'_>, at: usize) -> Result<Option<Entry>, Malformed> {
    let name: Tag = data.array(at + 2)?;
    if !name
        .iter()
        .all(|&byte| byte == 0 || byte == b' ' || (b')'..=b'Z').contains(&byte))
    {
        return Ok(None);
    }
    let id = data.u16(at + 6)?;
    let kind: Tag = data.array(at + 12)?;
    let start = data.u32(at + 16)? as usize;
    let length = data.u32(at + 20)? as usize;
    let Some(end) = start.checked_add(length).filter(|&end| end <= data.len()) else {
        return Ok(None);
    };
    Ok(Some(Entry {
        name,
        kind,
        id,
        range: start..end,
    }))
}

/// The zones of the index named `name`, in its order.
fn named(entries: &[Entry], name: Tag) -> impl Iterator<Item = &Entry> {
    entries.iter().filter(move |entry| entry.name == name)
}

/// A PLC: the text positions it lists, one more than its values, and where its values start.
struct Plc {
    positions: Vec<u32>,
    values: usize,
    count: usize,
    value_size: usize,
}

/// `WPS8Text::readPLC`'s reading of the PLC `entry`, its values `constant` in size where the
/// PLC says so and its header agrees, else structured.
fn plc(data: Bytes<'_>, entry: &Entry, constant: bool) -> Result<Plc, Malformed> {
    let range = &entry.range;
    if entry.kind != PLC {
        return Err(Malformed(format!(
            "the zone at {} is of type {:?}, no PLC",
            range.start,
            String::from_utf8_lossy(&entry.kind)
        )));
    }
    let length = range.len();
    if length < PLC_MINIMUM {
        return Err(Malformed(format!(
            "the PLC at {} is {length} bytes long, shorter than {PLC_MINIMUM}",
            range.start
        )));
    }
    let count = data.u32(range.start)? as usize;
    let declared = data.u32(range.start + 4)? as usize;
    let value_size = match constant {
        true if count
            .checked_mul(4 + declared)
            .is_some_and(|values| values + PLC_MINIMUM == length) =>
        {
            declared
        }
        _ => 0,
    };
    if count > (length - PLC_MINIMUM) / (4 + value_size) {
        return Err(Malformed(format!(
            "the PLC at {} of {length} bytes lists {count} positions",
            range.start
        )));
    }
    let at = range.start + PLC_HEADER;
    let positions = (0..=count)
        .map(|index| data.u32(at + 4 * index))
        .collect::<Result<_, _>>()?;
    Ok(Plc {
        positions,
        values: at + 4 * (count + 1),
        count,
        value_size,
    })
}

/// `WPS8Text::readStructures`' division of the text into stories by the `STRS` PLC, its
/// positions the lengths of consecutive stories; the whole text the body where there is none.
fn zones(data: Bytes<'_>, entries: &[Entry], text: &Range<usize>) -> Result<Vec<Zone>, Malformed> {
    let Some(entry) = named(entries, STRS).next() else {
        return Ok(vec![Zone {
            story: Story::Body,
            range: text.clone(),
        }]);
    };
    let plc = plc(data, entry, false)?;
    let mut zones = Vec::with_capacity(plc.count);
    let mut start = text.start;
    let mut at = plc.values;
    let mut previous = None;
    for &length in &plc.positions[..plc.count] {
        let end = (length as usize)
            .checked_mul(2)
            .and_then(|length| start.checked_add(length))
            .filter(|&end| end <= text.end)
            .ok_or_else(|| {
                Malformed(format!(
                    "a story of {length} characters from {start} runs past the text's end at {}",
                    text.end
                ))
            })?;
        let size = data.i16(at)?;
        let block_end = usize::try_from(size)
            .ok()
            .filter(|&size| size >= 2)
            .map(|size| at + size)
            .filter(|&end| end <= entry.range.end)
            .ok_or_else(|| Malformed(format!("the story's data at {at} is {size} bytes long")))?;
        let items = items(data.slice(at + 2, block_end - at - 2)?)
            .map_err(|malformed| malformed.within(&format!("the story's data at {at}")))?;
        let kind = items
            .iter()
            .find(|item| (item.id, item.kind) == STORY_ITEM)
            .map(|item| item.value)
            .or(previous);
        previous = kind;
        zones.push(Zone {
            story: story(kind),
            range: start..end,
        });
        start = end;
        at = block_end;
    }
    Ok(zones)
}

/// The story of the kind the `STRS` PLC names: the text boxes, table cells and other frames
/// (kind 5) for any but the body, notes, header and footer.
fn story(kind: Option<i64>) -> Story {
    match kind {
        Some(BODY) => Story::Body,
        Some(FOOTNOTES) => Story::Footnote,
        Some(ENDNOTES) => Story::Endnote,
        Some(HEADER) => Story::Header,
        Some(FOOTER) => Story::Footer,
        _ => Story::Frame,
    }
}

/// A structured data item, `WPS8Struct::FileData` as `readData` reads it: its id, its type and
/// its value, if it has a number for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Item {
    id: u8,
    kind: u8,
    value: i64,
}

/// `WPS8Struct::readBlockData`: the items of a block of structured data, after its first two
/// bytes, their ids in order.
fn items(block: &[u8]) -> Result<Vec<Item>, Malformed> {
    let data = Bytes(block);
    let mut items = Vec::new();
    let mut at = 2;
    while at < block.len() {
        let header = data.u16(at)?;
        let [id, kind] = header.to_le_bytes();
        let (value, length) = match kind >> 4 {
            _ if kind & 5 != 0 => {
                return Err(Malformed(format!("an item of type {kind:#x} at {at}")));
            }
            0 => (0, 2),
            1 if kind == 0x12 => (i64::from(data.u8(at + 2)?), 4),
            1 => (i64::from(data.u16(at + 2)?), 4),
            2 if kind == 0x2A => (i64::from(data.i32(at + 6)?), 10),
            2 => (i64::from(data.i32(at + 2)?), 6),
            8 => {
                let size = usize::from(data.u16(at + 2)?);
                if !size.is_multiple_of(2) || size < 2 {
                    return Err(Malformed(format!("an item of {size} bytes at {at}")));
                }
                (0, 2 + size)
            }
            _ => return Err(Malformed(format!("an item of type {kind:#x} at {at}"))),
        };
        if at + length > block.len() {
            return Err(Malformed(format!(
                "the item at {at} runs past the end of its block, at {}",
                block.len()
            )));
        }
        if items.last().is_some_and(|last: &Item| last.id > id) {
            return Err(Malformed(format!("the item at {at} is out of order")));
        }
        items.push(Item { id, kind, value });
        at += length;
    }
    Ok(items)
}

/// The runs of text whose characters' properties make them special, as their starts and ends,
/// in order.
#[derive(Debug, Default)]
struct Specials(Vec<Range<usize>>);

impl Specials {
    /// Whether the character at `at` is special: a `#` there stands for something else.
    fn contains(&self, at: usize) -> bool {
        let after = self.0.partition_point(|run| run.start <= at);
        after > 0 && self.0[after - 1].contains(&at)
    }
}

/// `WPS8TextStyle::readStructures`' reading of the character properties: the `FDPC` pages the
/// `BTEC` PLCs list, in the order of their ids, or every `FDPC` page where there is no `BTEC`.
fn specials(
    data: Bytes<'_>,
    entries: &[Entry],
    text: &Range<usize>,
) -> Result<Specials, Malformed> {
    let mut indexes: Vec<&Entry> = named(entries, BTEC).collect();
    indexes.sort_by_key(|entry| entry.id);
    let pages: Vec<Range<usize>> = if indexes.is_empty() {
        named(entries, FDPC)
            .filter(|entry| entry.kind == FDPC)
            .map(|entry| entry.range.clone())
            .collect()
    } else {
        let mut pages = Vec::new();
        for index in indexes {
            let plc = plc(data, index, true)?;
            for value in 0..plc.count {
                let at = plc.values + value * plc.value_size;
                let offset = match plc.value_size {
                    1 => u32::from(data.u8(at)?),
                    2 => u32::from(data.u16(at)?),
                    4 => data.u32(at)?,
                    size => {
                        return Err(Malformed(format!(
                            "the BTEC PLC at {} has values of {size} bytes",
                            index.range.start
                        )))
                    }
                } as usize;
                let page = named(entries, FDPC)
                    .find(|entry| entry.range.start == offset)
                    .ok_or_else(|| {
                        Malformed(format!("the BTEC PLC lists no FDPC page at {offset}"))
                    })?;
                pages.push(page.range.clone());
            }
        }
        pages
    };
    let mut runs = Vec::new();
    for page in pages {
        fdp(data, page.clone(), text, &mut runs)
            .map_err(|malformed| malformed.within(&format!("the page at {}", page.start)))?;
    }
    runs.sort_by_key(|run: &Range<usize>| run.start);
    Ok(Specials(runs))
}

/// `WPSTextParser::readFDP` of a page of Works 2000 and later: adds to `runs` those of its
/// runs whose characters are special.
fn fdp(
    data: Bytes<'_>,
    page: Range<usize>,
    text: &Range<usize>,
    runs: &mut Vec<Range<usize>>,
) -> Result<(), Malformed> {
    let count = usize::from(data.u16(page.start)?);
    if FDP_HEADER + FDP_RUN * count > page.len() {
        return Err(Malformed(format!(
            "{count} runs do not fit in its {} bytes",
            page.len()
        )));
    }
    let mut positions = Vec::with_capacity(count + 1);
    for index in 0..=count {
        let position = match data.u32(page.start + 4 + 4 * index)? as usize {
            0 => text.start,
            position => position,
        };
        if position > text.end || positions.last().is_some_and(|&last| last > position) {
            return Err(Malformed(format!(
                "its run boundary {index}, at {position}, lies out of order or past the text"
            )));
        }
        positions.push(position);
    }
    let offsets = page.start + 4 + 4 * (count + 1);
    for (index, run) in positions.windows(2).enumerate() {
        let offset = usize::from(data.u16(offsets + 2 * index)?);
        if offset == 0 {
            continue;
        }
        if offset < FDP_HEADER + FDP_RUN * count || page.start + offset > page.end {
            return Err(Malformed(format!(
                "its run {index} has its properties at {offset}"
            )));
        }
        let at = page.start + offset;
        let size = usize::from(data.u16(at)?);
        if size < 2 || at + size > page.end || !size.is_multiple_of(2) {
            return Err(Malformed(format!(
                "the properties at {at} are {size} bytes long"
            )));
        }
        if size == 2 {
            continue;
        }
        let items = items(data.slice(at + 2, size - 2)?)
            .map_err(|malformed| malformed.within(&format!("the properties at {at}")))?;
        let special = items
            .iter()
            .any(|item| (item.id, item.kind) == SPECIAL_ITEM && item.value != 0);
        if special {
            runs.push(run[0]..run[1]);
        }
    }
    Ok(())
}

/// The positions of the characters that end a table's cells, which the `TCD ` PLCs list,
/// relative to the story each PLC's id names.
fn cells(data: Bytes<'_>, entries: &[Entry], zones: &[Zone]) -> Result<BTreeSet<usize>, Malformed> {
    let mut ends = BTreeSet::new();
    for entry in named(entries, TCD) {
        let zone = zones.get(usize::from(entry.id)).ok_or_else(|| {
            Malformed(format!(
                "the PLC at {} names story {} of {}",
                entry.range.start,
                entry.id,
                zones.len()
            ))
        })?;
        let plc = plc(data, entry, true)?;
        for &position in &plc.positions {
            let end = (position as usize)
                .checked_mul(2)
                .and_then(|offset| zone.range.start.checked_add(offset))
                .filter(|&end| end <= zone.range.end)
                .ok_or_else(|| {
                    Malformed(format!(
                        "a cell ends at character {position}, past its story's {} bytes",
                        zone.range.len()
                    ))
                })?;
            ends.insert(end);
        }
    }
    Ok(ends)
}

/// `WPS8Text::readText`'s reading of the characters of `range` into paragraphs.
fn decode(
    data: Bytes<'_>,
    range: Range<usize>,
    specials: &Specials,
    cells: &BTreeSet<usize>,
    paragraphs: &mut Paragraphs,
) -> Result<(), Malformed> {
    let mut at = range.start;
    while at < range.end {
        let unit = data.u16(at)?;
        let here = at;
        at += 2;
        if cells.contains(&here) {
            paragraphs.end();
            continue;
        }
        match unit {
            TAB => paragraphs.push('\t'),
            LINE_BREAK => paragraphs.push('\n'),
            PAGE_BREAK | PARAGRAPH_END | COLUMN_BREAK => paragraphs.end(),
            NON_BREAKING_HYPHEN => paragraphs.push('\u{2011}'),
            NON_BREAKING_SPACE => paragraphs.push('\u{A0}'),
            SPECIAL if specials.contains(here) => {}
            OBJECT => {}
            unit if unit < 0x20 => {}
            unit => {
                let low = (at < range.end).then(|| data.u16(at)).transpose()?;
                let pair = [unit, low.unwrap_or(0)];
                let character = char::decode_utf16(pair)
                    .next()
                    .expect("a unit to decode")
                    .unwrap_or(char::REPLACEMENT_CHARACTER);
                if character.len_utf16() == 2 {
                    at += 2;
                }
                paragraphs.push(character);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A `CONTENTS` stream of Works 7: an index of `zones`, each a name, a type, an id and its
    /// data, laid out after the index in their order.
    pub(crate) fn contents(zones: &[(Tag, Tag, u16, Vec<u8>)]) -> Vec<u8> {
        let mut stream = b"CHNKWKS ".to_vec();
        stream.extend([4, 0, 8, 0]);
        stream.extend((zones.len() as u16).to_le_bytes());
        stream.resize(INDEX, 0);
        let block_entries = zones.len().min(usize::from(MAX_BLOCK_ENTRIES));
        assert_eq!(block_entries, zones.len(), "one index block");
        stream.extend(0x1F8u16.to_le_bytes());
        stream.extend((zones.len() as u16).to_le_bytes());
        stream.extend(LAST_BLOCK.to_le_bytes());
        let mut at = INDEX + 8 + zones.len() * usize::from(ENTRY);
        for (name, kind, id, data) in zones {
            stream.extend(ENTRY.to_le_bytes());
            stream.extend(name);
            stream.extend(id.to_le_bytes());
            stream.extend([0; 4]);
            stream.extend(kind);
            stream.extend((at as u32).to_le_bytes());
            stream.extend((data.len() as u32).to_le_bytes());
            at += data.len();
        }
        for (_, _, _, data) in zones {
            stream.extend(data);
        }
        stream
    }

    pub(crate) fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// The `STRS` PLC of stories of these lengths in characters and kinds.
    fn strs(stories: &[(u32, i32)]) -> Vec<u8> {
        let mut plc = (stories.len() as u32).to_le_bytes().to_vec();
        plc.extend(8u32.to_le_bytes());
        plc.extend([0xFF, 0, 0, 0]);
        for (length, _) in stories {
            plc.extend(length.to_le_bytes());
        }
        plc.extend(0u32.to_le_bytes());
        for (_, kind) in stories {
            plc.extend(10i16.to_le_bytes());
            plc.extend([0, 0, 0x00, 0x22]);
            plc.extend(kind.to_le_bytes());
        }
        plc
    }

    /// The offset in the stream of the data of zone `index` of `zones`.
    fn offset(zones: &[(Tag, Tag, u16, Vec<u8>)], index: usize) -> usize {
        INDEX
            + 8
            + zones.len() * usize::from(ENTRY)
            + zones[..index]
                .iter()
                .map(|zone| zone.3.len())
                .sum::<usize>()
    }

    /// An `FDPC` page of runs from the text's start, each its length in characters and whether
    /// it is special.
    fn fdpc(text_start: usize, runs: &[(usize, bool)]) -> Vec<u8> {
        let mut page = vec![0u8; 512];
        page[..2].copy_from_slice(&(runs.len() as u16).to_le_bytes());
        let mut position = text_start;
        let mut at = 4;
        page[at..at + 4].copy_from_slice(&(position as u32).to_le_bytes());
        for (length, _) in runs {
            position += 2 * length;
            at += 4;
            page[at..at + 4].copy_from_slice(&(position as u32).to_le_bytes());
        }
        let property = 0x1F0u16;
        let at = usize::from(property);
        page[at..at + 14].copy_from_slice(&[14, 0, 0, 0, 0x00, 0x12, 3, 0, 0x12, 0x22, 9, 4, 0, 0]);
        let offsets = 4 + 4 * (runs.len() + 1);
        for (index, (_, special)) in runs.iter().enumerate() {
            let value = if *special { property } else { 0 };
            page[offsets + 2 * index..offsets + 2 * index + 2]
                .copy_from_slice(&value.to_le_bytes());
        }
        page
    }

    fn paragraphs(stories: &Stories, story: Story) -> Vec<&str> {
        stories
            .0
            .get(&story)
            .map(|paragraphs| paragraphs.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    #[test]
    fn reads_the_stories_of_the_text_and_leaves_out_special_characters() {
        let body = "Havnen i \u{c5}lborg#\r\tog fjorden # \u{1F30A}\u{FFFC}\x1e\x1f\r\r";
        let footnote = "#Note\r";
        let header = "Top\r";
        let footer = "\r";
        let text = format!("{body}{footnote}{header}{footer}");
        let length = |text: &str| text.encode_utf16().count();
        let stories = strs(&[
            (length(body) as u32, 1),
            (length(footnote) as u32, 2),
            (length(header) as u32, 6),
            (length(footer) as u32, 7),
        ]);
        let mut zones = vec![
            (TEXT, TEXT, 0, utf16(&text)),
            (STRS, PLC, 0, stories),
            (FDPC, FDPC, 0, Vec::new()),
        ];
        let text_start = offset(&zones, 0);
        let mark = body
            .encode_utf16()
            .position(|unit| unit == SPECIAL)
            .unwrap();
        let note_mark = length(body);
        zones[2].3 = fdpc(
            text_start,
            &[
                (mark, false),
                (1, true),
                (note_mark - mark - 1, false),
                (1, true),
                (length(&text) - note_mark - 1, false),
            ],
        );
        let stories = read(&contents(&zones)).unwrap();
        assert_eq!(
            paragraphs(&stories, Story::Body),
            [
                "Havnen i \u{c5}lborg",
                "\tog fjorden # \u{1F30A}\u{2011}\u{A0}"
            ]
        );
        assert_eq!(paragraphs(&stories, Story::Footnote), ["Note"]);
        assert_eq!(paragraphs(&stories, Story::Header), ["Top"]);
        assert!(paragraphs(&stories, Story::Footer).is_empty());
    }

    #[test]
    fn reads_the_whole_text_as_the_body_without_stories_and_splits_table_cells() {
        let text = "one\u{7}two\u{7}three\r";
        let zones = [(TEXT, TEXT, 0, utf16(text))];
        let stories = read(&contents(&zones)).unwrap();
        assert_eq!(paragraphs(&stories, Story::Body), ["onetwothree"]);

        let mut tcd = 2u32.to_le_bytes().to_vec();
        tcd.extend(4u32.to_le_bytes());
        tcd.extend([0; 4]);
        for end in [3u32, 7, 13] {
            tcd.extend(end.to_le_bytes());
        }
        tcd.extend([0; 8]);
        let length = text.encode_utf16().count() as u32;
        let zones = [
            (TEXT, TEXT, 0, utf16(text)),
            (STRS, PLC, 0, strs(&[(length, 5)])),
            (TCD, PLC, 0, tcd),
        ];
        let stories = read(&contents(&zones)).unwrap();
        assert_eq!(paragraphs(&stories, Story::Frame), ["one", "two", "three"]);
    }

    #[test]
    fn fails_on_a_malformed_index_or_text() {
        let mut odd = contents(&[(TEXT, TEXT, 0, vec![b'a', 0, b'b'])]);
        assert!(read(&odd).unwrap_err().0.contains("no whole number"));
        odd[INDEX + 8] = 2;
        assert!(read(&odd).unwrap_err().0.contains("shorter than 10"));
        let none = contents(&[(STRS, PLC, 0, strs(&[(1, 1)]))]);
        assert!(read(&none).unwrap_err().0.contains("no TEXT zone"));
        let past = contents(&[
            (TEXT, TEXT, 0, utf16("ab")),
            (STRS, PLC, 0, strs(&[(3, 1)])),
        ]);
        assert!(read(&past).unwrap_err().0.contains("runs past the text"));
        let mut cyclic = contents(&[(TEXT, TEXT, 0, utf16("ab"))]);
        cyclic[INDEX + 4..INDEX + 8].copy_from_slice(&(INDEX as u32).to_le_bytes());
        cyclic[INDEX_COUNT..INDEX_COUNT + 2].copy_from_slice(&5u16.to_le_bytes());
        assert!(read(&cyclic)
            .unwrap_err()
            .0
            .contains("followed by one before it"));
    }
}
