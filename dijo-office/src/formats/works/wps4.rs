//! Works 3 and 4 for Windows word processor documents, as libwps' `WPS4Parser` and `WPS4Text`
//! read their `MN0` stream: a header that bounds the text of the header, the footer and the
//! body, and lists the zones of the document; the text, a byte a character in the code page of
//! its font, which the `FDPC` pages the `BTEC` PLC lists give, with the fonts' names in the
//! `FONT` zone; and the footnotes, kept at the end of the body, which the `FTNd` and `FTNp` PLCs
//! delimit. A header or footer the header does not bound is the string of the `SHdr` or `SFtr`
//! zone, in which `&` and a letter stand for a field.

use std::collections::BTreeMap;
use std::ops::Range;

use super::{Bytes, Malformed, Paragraphs, Stories, Story};
use crate::text::charset::Charset;

/// How long the header is, where the text cannot start; its version byte, and the byte that
/// follows it.
const HEADER: usize = 0x100;
const MAX_VERSION: u8 = 7;
const SIGNATURE: u8 = 0xFE;
/// The first version byte of Works 3 for Windows; those before are of Works for DOS.
const WINDOWS: u8 = 4;
/// Where the header bounds the text, where it gives the stream's length, and where it lists the
/// zones `ZONES` names, an offset and a length each.
const TEXT_LIMITS: usize = 0x1A;
const STREAM_LENGTH: usize = 0x2A;
const ZONE_LIST: usize = 0x2E;
/// The zones the header lists, in its order.
const ZONES: [&str; 9] = [
    "BTEC", "BTEP", "SHdr", "SFtr", "DLINK", "FTNp", "FTNd", "BKMK", "FONT",
];
/// A text limit that bounds nothing.
const NO_LIMIT: i64 = -1;
/// How long an `FDPC` page is, how long its header is, and each of its runs' position and
/// property offset; and how many bytes the `BTEC` PLC's values count.
const FDP_PAGE: usize = 0x80;
const FDP_HEADER: usize = 4;
const FDP_RUN: usize = 5;
const PAGE_UNIT: usize = 0x80;
/// The longest value a PLC may have.
const MAX_PLC_VALUE: usize = 100;

/// The characters of the text that mean more than themselves.
const TAB: u8 = 0x09;
const PARAGRAPH_END: u8 = 0x0A;
const LINE_BREAK: u8 = 0x0B;
const PAGE_BREAK: u8 = 0x0C;
const NON_BREAKING_HYPHEN: u8 = 0x11;
const NON_BREAKING_SPACE: u8 = 0x12;
/// The non-breaking space of Works for DOS.
const DOS_NON_BREAKING_SPACE: u8 = 0xCA;
/// What starts a field in the string of a header or footer, and the letters of the fields.
const FIELD: u8 = b'&';
const FIELDS: &[u8] = b"pPdDtTfF";

/// The code page of a run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodePage {
    Ibm850,
    Windows1250,
    Windows1251,
    Windows1252,
    Windows1253,
    Windows1254,
    Windows1255,
    Windows1256,
    Windows1257,
    Windows1258,
}

impl CodePage {
    fn charset(self) -> &'static str {
        match self {
            CodePage::Ibm850 => "IBM850",
            CodePage::Windows1250 => "windows-1250",
            CodePage::Windows1251 => "windows-1251",
            CodePage::Windows1252 => "windows-1252",
            CodePage::Windows1253 => "windows-1253",
            CodePage::Windows1254 => "windows-1254",
            CodePage::Windows1255 => "windows-1255",
            CodePage::Windows1256 => "windows-1256",
            CodePage::Windows1257 => "windows-1257",
            CodePage::Windows1258 => "windows-1258",
        }
    }

    fn decode(self, bytes: &[u8]) -> String {
        Charset::named(self.charset())
            .decode(bytes)
            .unwrap_or_else(|| panic!("the JDK decodes {}", self.charset()))
    }

    /// `libwps_tools_win::Font::getFontType`: the code page a font's name tells by its suffix,
    /// if any.
    fn of_font(name: &str) -> Option<CodePage> {
        const SUFFIXES: [(&str, CodePage); 5] = [
            (" cyr", CodePage::Windows1251),
            (" ce", CodePage::Windows1250),
            (" greek", CodePage::Windows1253),
            (" tur", CodePage::Windows1254),
            (" baltic", CodePage::Windows1257),
        ];
        const SCRIPTS: [(&str, CodePage); 3] = [
            (" (hebrew)", CodePage::Windows1255),
            (" (arabic)", CodePage::Windows1256),
            (" (vietnamese)", CodePage::Windows1258),
        ];
        let ends_with = |suffix: &str| {
            name.len() > suffix.len()
                && name.as_bytes()[name.len() - suffix.len()..]
                    .eq_ignore_ascii_case(suffix.as_bytes())
        };
        if name == "Baltica" || name == "Pragmatica" {
            return Some(CodePage::Windows1251);
        }
        let candidates: &[(&str, CodePage)] = if name.ends_with(')') {
            &SCRIPTS
        } else {
            &SUFFIXES
        };
        candidates
            .iter()
            .find(|(suffix, _)| ends_with(suffix))
            .map(|&(_, code_page)| code_page)
    }
}

/// The stories of the `MN0` stream `mn0`.
pub(super) fn read(mn0: &[u8]) -> Result<Stories, Malformed> {
    let data = Bytes(mn0);
    if data.len() < HEADER {
        return Err(Malformed(format!(
            "the stream is {} bytes long, shorter than its header",
            data.len()
        )));
    }
    let version = data.u8(0)?;
    if version > MAX_VERSION || data.u8(1)? != SIGNATURE {
        return Err(Malformed(format!(
            "the stream starts {:02x} {:02x}, no Works 3 or 4 header",
            version,
            data.u8(1)?
        )));
    }
    let default = if version < WINDOWS {
        CodePage::Ibm850
    } else {
        CodePage::Windows1252
    };
    let layout = Layout::read(data)?;
    let zones = Zones::read(data, layout.size)?;
    let fonts = match zones.get("FONT") {
        Some(range) => fonts(data, range, default).map_err(|malformed| malformed.within("FONT"))?,
        None => BTreeMap::new(),
    };
    let runs = match zones.get("BTEC") {
        Some(range) => {
            runs(data, range, &layout.text, &fonts).map_err(|malformed| malformed.within("BTEC"))?
        }
        None => Vec::new(),
    };
    let decoder = Decoder {
        data,
        runs: &runs,
        default,
        dos: version < WINDOWS,
    };
    let mut stories = Stories::default();
    let strings = [
        (Story::Header, layout.header.clone(), "SHdr"),
        (Story::Footer, layout.footer.clone(), "SFtr"),
    ];
    for (story, range, string) in strings {
        let mut paragraphs = Paragraphs::default();
        match (range, zones.get(string)) {
            (Some(range), _) => decoder.decode(range, Text::Formatted, &mut paragraphs)?,
            (None, Some(range)) => decoder.decode(range, Text::String, &mut paragraphs)?,
            (None, None) => {}
        }
        stories.add(story, paragraphs);
    }
    let notes = footnotes(data, &zones, &layout.text, &layout.main)?;
    let mut body = Paragraphs::default();
    let mut start = layout.main.start;
    for note in &notes {
        decoder.decode(start..note.start, Text::Formatted, &mut body)?;
        start = note.end;
    }
    decoder.decode(start..layout.main.end, Text::Formatted, &mut body)?;
    stories.add(Story::Body, body);
    for note in notes {
        let mut paragraphs = Paragraphs::default();
        decoder.decode(note, Text::Formatted, &mut paragraphs)?;
        stories.add(Story::Footnote, paragraphs);
    }
    Ok(stories)
}

/// Where the header puts the text of the stories, and how long it says the stream is.
struct Layout {
    header: Option<Range<usize>>,
    footer: Option<Range<usize>>,
    main: Range<usize>,
    /// From the start of the first story to the end of the last.
    text: Range<usize>,
    size: usize,
}

impl Layout {
    /// `WPS4Text::readEntries`' reading of the text limits: the header, footer and body each
    /// run from the end of the one before to their limit; where a limit is out of place, the
    /// whole text is the body.
    fn read(data: Bytes<'_>) -> Result<Self, Malformed> {
        let limits = (0..4)
            .map(|index| data.i32(TEXT_LIMITS + 4 * index).map(i64::from))
            .collect::<Result<Vec<_>, _>>()?;
        let mut last = limits[0].max(HEADER as i64);
        let mut stories: [Option<Range<usize>>; 3] = [None, None, None];
        let mut text: Option<Range<usize>> = None;
        let mut in_place = true;
        for (index, story) in stories.iter_mut().enumerate() {
            let limit = limits[index + 1];
            let start = last;
            if limit >= last {
                last = limit;
            }
            if limit <= start {
                if limit != HEADER as i64 && limit != NO_LIMIT {
                    in_place = false;
                }
                continue;
            }
            let range = start as usize..limit as usize;
            text = Some(text.map_or(range.clone(), |text| text.start..range.end));
            *story = Some(range);
        }
        let text = text.ok_or_else(|| Malformed("the header bounds no text".to_owned()))?;
        let size = (data.u32(STREAM_LENGTH)? as usize).min(data.len());
        if text.end > size {
            return Err(Malformed(format!(
                "the text ends at {}, past the stream's end at {size}",
                text.end
            )));
        }
        let [header, footer, main] = stories;
        let (header, footer, main) = if in_place {
            let main = main.ok_or_else(|| Malformed("the header bounds no body".to_owned()))?;
            (header, footer, main)
        } else {
            (None, None, text.clone())
        };
        Ok(Self {
            header,
            footer,
            main,
            text,
            size,
        })
    }
}

/// The zones the header lists that lie in the stream, by their names.
struct Zones(BTreeMap<&'static str, Range<usize>>);

impl Zones {
    /// `WPS4Parser::parseEntry` of each zone the header lists.
    fn read(data: Bytes<'_>, size: usize) -> Result<Self, Malformed> {
        let mut zones = BTreeMap::new();
        for (index, name) in ZONES.into_iter().enumerate() {
            let at = ZONE_LIST + 6 * index;
            let start = data.u32(at)? as usize;
            let length = usize::from(data.u16(at + 4)?);
            if length > 0 && start + length <= size {
                zones.insert(name, start..start + length);
            }
        }
        Ok(Self(zones))
    }

    fn get(&self, name: &str) -> Option<Range<usize>> {
        self.0.get(name).cloned()
    }
}

/// `WPS4Text::readFontNames`: the code page of each font, by its number, from its name.
fn fonts(
    data: Bytes<'_>,
    range: Range<usize>,
    default: CodePage,
) -> Result<BTreeMap<u8, CodePage>, Malformed> {
    let mut fonts = BTreeMap::new();
    let mut at = range.start;
    while at < range.end {
        let number = data.u8(at)?;
        let length = usize::from(data.u8(at + 2)?);
        let name: String = data
            .slice(at + 3, length)?
            .iter()
            .filter(|byte| (b' '..=b'z').contains(byte))
            .map(|&byte| char::from(byte))
            .collect();
        if fonts
            .insert(number, CodePage::of_font(&name).unwrap_or(default))
            .is_some()
        {
            return Err(Malformed(format!("font {number} is named twice")));
        }
        at += 3 + length;
    }
    Ok(fonts)
}

/// A run of text, and the code page of its font, if its properties name one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Run {
    range: Range<usize>,
    code_page: Option<CodePage>,
}

/// How a PLC's positions count: from the start of the stream, or of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Positions {
    Absolute,
    Relative,
}

/// A PLC: the text positions it lists, from the first to the end of the text, and its values,
/// one fewer, each of `value_size` bytes from `values`.
struct Plc {
    positions: Vec<usize>,
    values: usize,
    value_size: usize,
}

/// `WPS4Text::readPLC`: the PLC in `range`, whose positions run to the end of `text`.
fn plc(
    data: Bytes<'_>,
    range: &Range<usize>,
    text: &Range<usize>,
    counted: Positions,
) -> Result<Plc, Malformed> {
    let size = range.len();
    let mut positions = Vec::new();
    let mut last = 0;
    while positions.len() * 4 + 4 <= size {
        let mut position = data.u32(range.start + 4 * positions.len())? as usize;
        if counted == Positions::Relative {
            position += text.start;
        }
        if counted == Positions::Relative && position == text.end + text.start {
            position = text.end;
        }
        if position < last || position > text.end {
            return Err(Malformed(format!(
                "the PLC at {} lists the position {position}, out of order or past the text's \
                 end at {}",
                range.start, text.end
            )));
        }
        positions.push(position);
        if position == text.end {
            break;
        }
        last = position;
    }
    let count = positions.len().saturating_sub(1);
    if positions.last() != Some(&text.end) || count == 0 {
        return Err(Malformed(format!(
            "the PLC at {} does not list positions to the text's end",
            range.start
        )));
    }
    let value_size = (size - 4 * count - 4) / count;
    if value_size > MAX_PLC_VALUE || size != count * (4 + value_size) + 4 {
        return Err(Malformed(format!(
            "the PLC at {} of {size} bytes does not hold {count} values",
            range.start
        )));
    }
    Ok(Plc {
        positions,
        values: range.start + 4 * (count + 1),
        value_size,
    })
}

/// The runs of text of each code page, as their starts and ends, in order: `WPS4Text`'s
/// reading of the `FDPC` pages the `BTEC` PLC lists, of the fonts of their runs.
fn runs(
    data: Bytes<'_>,
    btec: Range<usize>,
    text: &Range<usize>,
    fonts: &BTreeMap<u8, CodePage>,
) -> Result<Vec<Run>, Malformed> {
    let plc = plc(data, &btec, text, Positions::Absolute)?;
    let mut runs = Vec::new();
    for index in 0..plc.positions.len() - 1 {
        let at = plc.values + index * plc.value_size;
        let page = match plc.value_size {
            1 => usize::from(data.u8(at)?),
            2 => usize::from(data.u16(at)?),
            4 => data.u32(at)? as usize,
            size => {
                return Err(Malformed(format!("its values are {size} bytes long")));
            }
        } * PAGE_UNIT;
        if page == 0 || page + FDP_PAGE > data.len() {
            return Err(Malformed(format!("it lists an FDPC page at {page}")));
        }
        fdp(data, page, text, fonts, &mut runs)
            .map_err(|malformed| malformed.within(&format!("the FDPC page at {page}")))?;
    }
    runs.sort_by_key(|run| run.range.start);
    Ok(runs)
}

/// `WPSTextParser::readFDP` of a page of Works 3 and 4: adds its runs to `runs`, with the code
/// page of each one's font, if its properties name one.
fn fdp(
    data: Bytes<'_>,
    page: usize,
    text: &Range<usize>,
    fonts: &BTreeMap<u8, CodePage>,
    runs: &mut Vec<Run>,
) -> Result<(), Malformed> {
    let end = page + FDP_PAGE - 1;
    let count = usize::from(data.u8(end)?);
    if FDP_HEADER + FDP_RUN * count > FDP_PAGE {
        return Err(Malformed(format!("{count} runs do not fit in it")));
    }
    let mut positions = Vec::with_capacity(count + 1);
    for index in 0..=count {
        let position = match data.u32(page + 4 * index)? as usize {
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
    let offsets = page + 4 * (count + 1);
    for (index, run) in positions.windows(2).enumerate() {
        let offset = usize::from(data.u8(offsets + index)?);
        if offset == 0 {
            runs.push(Run {
                range: run[0]..run[1],
                code_page: None,
            });
            continue;
        }
        let at = page + offset;
        if offset < FDP_HEADER + FDP_RUN * count || at > end {
            return Err(Malformed(format!(
                "its run {index} has its properties at {offset}"
            )));
        }
        let properties = at + 1..at + 1 + usize::from(data.u8(at)?);
        if properties.end > end {
            return Err(Malformed(format!(
                "the properties at {at} run past the page's end"
            )));
        }
        let font = (properties.len() >= 3)
            .then(|| data.u8(properties.start + 2))
            .transpose()?;
        runs.push(Run {
            range: run[0]..run[1],
            code_page: font.and_then(|font| fonts.get(&font).copied()),
        });
    }
    Ok(())
}

/// `WPS4Text::readFootNotes`: where the footnotes' text lies, in order, at the end of the body.
/// Where the `FTNd` and `FTNp` PLCs disagree, or their notes overlap, libwps reads no notes:
/// their text is then read as the body's.
fn footnotes(
    data: Bytes<'_>,
    zones: &Zones,
    text: &Range<usize>,
    main: &Range<usize>,
) -> Result<Vec<Range<usize>>, Malformed> {
    let (Some(definitions), Some(references)) = (zones.get("FTNd"), zones.get("FTNp")) else {
        return Ok(Vec::new());
    };
    let definitions = plc(data, &definitions, text, Positions::Relative)
        .map_err(|malformed| malformed.within("FTNd"))?
        .positions;
    let references = plc(data, &references, text, Positions::Relative)
        .map_err(|malformed| malformed.within("FTNp"))?
        .positions;
    if definitions.len() != references.len() {
        return Ok(Vec::new());
    }
    let mut notes: Vec<Range<usize>> = definitions
        .windows(2)
        .map(|bounds| bounds[0]..bounds[1])
        .collect();
    notes.sort_by_key(|note| note.start);
    if notes.windows(2).any(|pair| pair[1].start < pair[0].end) {
        return Ok(Vec::new());
    }
    if let Some(note) = notes
        .iter()
        .find(|note| note.start < main.start || note.end > main.end)
    {
        return Err(Malformed(format!(
            "a footnote from {} to {} lies outside the body, from {} to {}",
            note.start, note.end, main.start, main.end
        )));
    }
    Ok(notes)
}

/// How a run of text is read: formatted, as a story's text is, or as the string of a header or
/// footer, in which `&` and a letter stand for a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Text {
    Formatted,
    String,
}

/// What reads the characters of the text in the code pages of their runs.
struct Decoder<'d, 'a> {
    data: Bytes<'a>,
    runs: &'d [Run],
    default: CodePage,
    dos: bool,
}

impl Decoder<'_, '_> {
    /// The code page of the character at `at`.
    fn code_page(&self, at: usize, text: Text) -> CodePage {
        if text == Text::String {
            return self.default;
        }
        let after = self.runs.partition_point(|run| run.range.start <= at);
        match after.checked_sub(1).map(|index| &self.runs[index]) {
            Some(Run {
                range,
                code_page: Some(code_page),
            }) if range.contains(&at) => *code_page,
            _ => self.default,
        }
    }

    /// `WPS4Text::readText`'s reading of the characters of `range` into paragraphs: fields,
    /// footnote marks and the places of objects and links are left out.
    fn decode(
        &self,
        range: Range<usize>,
        text: Text,
        paragraphs: &mut Paragraphs,
    ) -> Result<(), Malformed> {
        let bytes = self.data.slice(range.start, range.len())?;
        let mut pending = Vec::new();
        let mut code_page = self.default;
        let flush = |pending: &mut Vec<u8>, code_page: CodePage, paragraphs: &mut Paragraphs| {
            if !pending.is_empty() {
                paragraphs.push_str(&code_page.decode(pending));
                pending.clear();
            }
        };
        let mut index = 0;
        while index < bytes.len() {
            let byte = bytes[index];
            let at = range.start + index;
            index += 1;
            let character = match byte {
                TAB => Some('\t'),
                LINE_BREAK => Some('\n'),
                NON_BREAKING_HYPHEN => Some('\u{2011}'),
                NON_BREAKING_SPACE => Some('\u{A0}'),
                DOS_NON_BREAKING_SPACE if self.dos => Some('\u{A0}'),
                PARAGRAPH_END | PAGE_BREAK => {
                    flush(&mut pending, code_page, paragraphs);
                    paragraphs.end();
                    continue;
                }
                byte if byte < 0x20 => {
                    flush(&mut pending, code_page, paragraphs);
                    continue;
                }
                FIELD
                    if text == Text::String
                        && bytes.get(index).is_some_and(|next| FIELDS.contains(next)) =>
                {
                    flush(&mut pending, code_page, paragraphs);
                    index += 1;
                    continue;
                }
                _ => None,
            };
            if let Some(character) = character {
                flush(&mut pending, code_page, paragraphs);
                paragraphs.push(character);
                continue;
            }
            let here = self.code_page(at, text);
            if here != code_page {
                flush(&mut pending, code_page, paragraphs);
                code_page = here;
            }
            pending.push(byte);
        }
        flush(&mut pending, code_page, paragraphs);
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// An `MN0` stream of Works 4: its header bounds a header, a footer and a body; the body's
    /// text is followed by the notes', each a note's text; the fonts are named in a `FONT` zone,
    /// and an `FDPC` page gives each run, its length and its font, if any.
    pub(crate) struct Mn0<'t> {
        pub(crate) header: &'t [u8],
        pub(crate) footer: &'t [u8],
        pub(crate) body: &'t [u8],
        pub(crate) notes: &'t [&'t [u8]],
        pub(crate) fonts: &'t [(u8, &'t str)],
        pub(crate) runs: &'t [(usize, Option<u8>)],
    }

    impl Mn0<'_> {
        pub(crate) fn bytes(&self) -> Vec<u8> {
            let mut stream = vec![0u8; HEADER];
            stream[0] = 6;
            stream[1] = SIGNATURE;
            let mut text = self.header.to_vec();
            text.extend(self.footer);
            text.extend(self.body);
            assert!(
                self.notes.is_empty() || !self.header.is_empty(),
                "notes count from the header's start"
            );
            let mut definitions = vec![text.len() as u32];
            for note in self.notes {
                text.extend(*note);
                definitions.push(text.len() as u32);
            }
            let start = HEADER;
            let header_end = start + self.header.len();
            let footer_end = header_end + self.footer.len();
            let end = start + text.len();
            for (index, limit) in [start, header_end, footer_end, end].into_iter().enumerate() {
                stream[TEXT_LIMITS + 4 * index..TEXT_LIMITS + 4 * index + 4]
                    .copy_from_slice(&(limit as u32).to_le_bytes());
            }
            stream.extend(&text);
            stream.resize(end.div_ceil(PAGE_UNIT) * PAGE_UNIT, 0);
            let page = stream.len();
            let mut fdpc = vec![0u8; FDP_PAGE];
            fdpc[FDP_PAGE - 1] = self.runs.len() as u8;
            let mut position = start;
            fdpc[..4].copy_from_slice(&(position as u32).to_le_bytes());
            let offsets = 4 * (self.runs.len() + 1);
            let mut property = 0x60;
            for (index, (length, font)) in self.runs.iter().enumerate() {
                position += length;
                fdpc[4 * (index + 1)..4 * (index + 2)]
                    .copy_from_slice(&(position as u32).to_le_bytes());
                if let Some(font) = font {
                    fdpc[offsets + index] = property as u8;
                    fdpc[property..property + 4].copy_from_slice(&[3, 0, 0x08, *font]);
                    property += 4;
                }
            }
            stream.extend(&fdpc);
            let zone = |stream: &mut Vec<u8>, name: &str, data: Vec<u8>| {
                let index = ZONES.iter().position(|zone| *zone == name).unwrap();
                let at = ZONE_LIST + 6 * index;
                let offset = stream.len() as u32;
                stream[at..at + 4].copy_from_slice(&offset.to_le_bytes());
                stream[at + 4..at + 6].copy_from_slice(&(data.len() as u16).to_le_bytes());
                stream.extend(data);
            };
            let mut btec = (start as u32).to_le_bytes().to_vec();
            btec.extend((end as u32).to_le_bytes());
            btec.extend(((page / PAGE_UNIT) as u16).to_le_bytes());
            zone(&mut stream, "BTEC", btec);
            let mut fonts = Vec::new();
            for (number, name) in self.fonts {
                fonts.extend([*number, 0, name.len() as u8]);
                fonts.extend(name.as_bytes());
            }
            zone(&mut stream, "FONT", fonts);
            if !self.notes.is_empty() {
                let plc = |values: usize| {
                    let mut plc: Vec<u8> = definitions
                        .iter()
                        .flat_map(|position| position.to_le_bytes())
                        .collect();
                    plc.extend(vec![0; values * self.notes.len()]);
                    plc
                };
                let mut references = plc(0);
                references[..4].copy_from_slice(&[0, 0, 0, 0]);
                zone(&mut stream, "FTNd", plc(0));
                zone(&mut stream, "FTNp", references);
            }
            let length = stream.len() as u32;
            stream[STREAM_LENGTH..STREAM_LENGTH + 4].copy_from_slice(&length.to_le_bytes());
            stream
        }
    }

    fn paragraphs(stories: &Stories, story: Story) -> Vec<&str> {
        stories
            .0
            .get(&story)
            .map(|paragraphs| paragraphs.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    #[test]
    fn reads_the_stories_in_the_code_pages_of_their_fonts() {
        let header = b"Side &p\r\n";
        let footer = b"\r\n";
        let body = b"Havnen i \xc5lborg\x06\r\n\t\xcf\xf0\xe8\xe2\xe5\xf2\x02\x11\x12\r\n";
        let note = b"\x06 En note\r\n";
        let latin = header.len() + footer.len() + body.iter().position(|&b| b == 0xcf).unwrap();
        let total = header.len() + footer.len() + body.len() + note.len();
        let stories = read(
            &Mn0 {
                header,
                footer,
                body,
                notes: &[note],
                fonts: &[(0, "Times New Roman"), (1, "Arial Cyr")],
                runs: &[(latin, Some(0)), (6, Some(1)), (total - latin - 6, None)],
            }
            .bytes(),
        )
        .unwrap();
        assert_eq!(paragraphs(&stories, Story::Header), ["Side &p"]);
        assert_eq!(
            paragraphs(&stories, Story::Body),
            [
                "Havnen i \u{c5}lborg",
                "\t\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}\u{2011}\u{a0}"
            ]
        );
        assert_eq!(paragraphs(&stories, Story::Footnote), [" En note"]);
        assert!(paragraphs(&stories, Story::Footer).is_empty());
    }

    #[test]
    fn reads_the_header_string_without_its_fields_and_tells_code_pages_by_font_names() {
        let mut stream = Mn0 {
            header: b"",
            footer: b"",
            body: b"Body\r\n",
            notes: &[],
            fonts: &[],
            runs: &[(6, None)],
        }
        .bytes();
        let string = b"Page &p of &f && more";
        let at = stream.len();
        stream.extend(string);
        let index = ZONES.iter().position(|zone| *zone == "SHdr").unwrap();
        stream[ZONE_LIST + 6 * index..ZONE_LIST + 6 * index + 4]
            .copy_from_slice(&(at as u32).to_le_bytes());
        stream[ZONE_LIST + 6 * index + 4..ZONE_LIST + 6 * index + 6]
            .copy_from_slice(&(string.len() as u16).to_le_bytes());
        let length = stream.len() as u32;
        stream[STREAM_LENGTH..STREAM_LENGTH + 4].copy_from_slice(&length.to_le_bytes());
        let stories = read(&stream).unwrap();
        assert_eq!(paragraphs(&stories, Story::Header), ["Page  of  && more"]);
        assert_eq!(paragraphs(&stories, Story::Body), ["Body"]);

        assert_eq!(
            CodePage::of_font("Times New Roman CE"),
            Some(CodePage::Windows1250)
        );
        assert_eq!(
            CodePage::of_font("Arial (Hebrew)"),
            Some(CodePage::Windows1255)
        );
        assert_eq!(
            CodePage::of_font("Arial Greek"),
            Some(CodePage::Windows1253)
        );
        assert_eq!(CodePage::of_font("Pragmatica"), Some(CodePage::Windows1251));
        assert_eq!(CodePage::of_font("Times New Roman"), None);
    }

    #[test]
    fn fails_on_a_malformed_header() {
        let valid = Mn0 {
            header: b"",
            footer: b"",
            body: b"Body\r\n",
            notes: &[],
            fonts: &[],
            runs: &[(6, None)],
        }
        .bytes();
        assert!(read(&valid[..0x80])
            .unwrap_err()
            .0
            .contains("shorter than its header"));
        let mut signature = valid.clone();
        signature[1] = 0;
        assert!(read(&signature)
            .unwrap_err()
            .0
            .contains("no Works 3 or 4 header"));
        let mut past = valid.clone();
        past[TEXT_LIMITS + 12..TEXT_LIMITS + 16].copy_from_slice(&0x10_0000u32.to_le_bytes());
        assert!(read(&past).unwrap_err().0.contains("past the stream's end"));
        let mut duplicate = valid;
        let index = ZONES.iter().position(|zone| *zone == "FONT").unwrap();
        let at = ZONE_LIST + 6 * index;
        let start = u32::from_le_bytes(duplicate[at..at + 4].try_into().unwrap()) as usize;
        duplicate.truncate(start);
        duplicate.extend([0, 0, 1, b'A', 0, 0, 1, b'B']);
        duplicate[at + 4..at + 6].copy_from_slice(&8u16.to_le_bytes());
        let length = duplicate.len() as u32;
        duplicate[STREAM_LENGTH..STREAM_LENGTH + 4].copy_from_slice(&length.to_le_bytes());
        assert!(read(&duplicate).unwrap_err().0.contains("named twice"));
    }
}
