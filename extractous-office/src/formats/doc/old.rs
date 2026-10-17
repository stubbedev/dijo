//! Word 6 and Word 95 documents as POI's `HWPFOldDocument` reads them for Tika's `parseWord6`:
//! the code page its fonts name, the text pieces in it, the character and paragraph runs and
//! the sections as POI reads them, failing where it fails, and the paragraphs as
//! `Word6Extractor.getParagraphText` gives them through our substitute of POI's
//! `getParagraphText`, or, where reading them throws, the text pieces whole.

use super::bins::{
    bin_pages, character_runs, paragraph_runs, rebuild_character_runs, rebuild_paragraph_runs,
    PageNumber, PropertyRun,
};
use super::bytes::{index, safely_clone};
use super::extractor::{clean, strip_fields};
use super::range::{find_range, subrange};
use super::text::{read_clx, ComplexFileTable, Plex, TextPiece, MAX_PIECE_LENGTH};
use crate::formats::poi::{i16_at, i32_at, u16_at, Read, Thrown};
use crate::formats::Unported;
use crate::text::charset::Charset;

/// `HWPFOldDocument.getMaxRecordLength`.
const MAX_RECORD_LENGTH: i64 = 10_000_000;
/// `fComplex` of the FIB's base flags.
const COMPLEX: u16 = 0x0004;
/// `SectionDescriptor`'s size.
const SED_SIZE: i32 = 12;
/// What POI's text-piece fallback writes a carriage return as.
const LINE_END: [u16; 2] = [0x0D, 0x0A];

fn throws(Thrown(reason): Thrown) -> Unported {
    Unported(format!(
        "POI throws reading the Word 6 or 95 document: {reason}"
    ))
}

/// A Word 6 or 95 document, read.
pub(super) struct OldDocument {
    pieces: ComplexFileTable,
    text: Vec<u16>,
    paragraphs: Vec<PropertyRun>,
    /// `fcMac - fcMin`: the end of the range `getRange` gives.
    length: i32,
}

impl OldDocument {
    /// `new HWPFOldDocument(directory)` past the FIB, from the main stream.
    pub(super) fn read(main: &[u8]) -> Result<OldDocument, Unported> {
        let int = |at: usize| i32_at(main, at).map_err(throws);
        let sed_offset = int(0x88)?;
        let sed_size = int(0x8C)?;
        let chp_offset = int(0xB8)?;
        let chp_size = int(0xBC)?;
        let pap_offset = int(0xC0)?;
        let pap_size = int(0xC4)?;
        let font_offset = int(0xD0)?;
        let font_size = int(0xD4)?;
        let charset = guess_charset(main, font_offset, font_size)?;
        let complex_offset = int(0x160)?;
        let fc_min = int(0x18)?;
        let fc_mac = int(0x1C)?;
        let complex = u16_at(main, 0xA).map_err(throws)? & COMPLEX != 0;
        let pieces = if complex {
            complex_pieces(main, complex_offset, charset)?
        } else {
            whole_text(main, fc_min, fc_mac, charset)?
        };
        let (text, paragraphs) = runs(
            main,
            &pieces,
            (chp_offset, chp_size),
            (pap_offset, pap_size),
            (sed_offset, sed_size),
        )
        .map_err(throws)?;
        Ok(OldDocument {
            pieces,
            text,
            paragraphs,
            length: fc_mac.wrapping_sub(fc_min),
        })
    }

    /// `Word6Extractor.getParagraphText`: the paragraphs as our substitute of
    /// `WordExtractor.getParagraphText` gives them, or, where reading them throws, the text of
    /// each piece, its carriage returns ended as lines.
    pub(super) fn paragraph_text(&self) -> Vec<Vec<u16>> {
        self.paragraphs_of_range()
            .unwrap_or_else(|_| self.pieces.pieces.iter().map(piece_text).collect())
    }

    /// Our `getParagraphText(getRange())`: each paragraph of the range, its fields stripped and
    /// its text cleaned, as `Paragraph.newParagraph` makes it without a style sheet. POI also
    /// reads each paragraph's properties by Word 97's grammar, which the properties of Word 6
    /// and 95 do not follow, and which, where it throws, sends Tika to the text pieces whole
    /// (#119); the properties are not needed for the text.
    fn paragraphs_of_range(&self) -> Read<Vec<Vec<u16>>> {
        let length = self.text.len();
        let range = subrange(0, self.length, None)?;
        let (first, last) = find_range(&self.paragraphs, range)?;
        let mut paragraphs = Vec::with_capacity(last - first);
        for run in &self.paragraphs[first..last] {
            let span = subrange(
                range.start.max(run.start),
                range.end.min(run.end),
                Some(length),
            )?;
            let text = &self.text[span.start as usize..span.end as usize];
            paragraphs.push(clean(&strip_fields(text)?));
        }
        Ok(paragraphs)
    }
}

/// A piece's text, each carriage return and each U+FFFE ended as a line, as `Word6Extractor`
/// replaces the one by the other and that by a line end.
fn piece_text(piece: &TextPiece) -> Vec<u16> {
    let mut text = Vec::with_capacity(piece.text().len());
    for &unit in piece.text() {
        if unit == 0x0D || unit == 0xFFFE {
            text.extend(LINE_END);
        } else {
            text.push(unit);
        }
    }
    text
}

/// The text and the paragraph runs, after the character runs, the paragraph runs and the
/// sections are read and both kinds of runs rebuilt as POI reads and rebuilds them.
fn runs(
    main: &[u8],
    pieces: &ComplexFileTable,
    characters: (i32, i32),
    paragraphs: (i32, i32),
    sections: (i32, i32),
) -> Read<(Vec<u16>, Vec<PropertyRun>)> {
    let text = pieces.text()?;
    let character_pages = bin_pages(main, characters.0, characters.1, PageNumber::Short)?;
    let mut characters = character_runs(main, &character_pages, pieces)?;
    characters.sort_by_key(|run| run.start);
    let paragraph_pages = bin_pages(main, paragraphs.0, paragraphs.1, PageNumber::Short)?;
    let paragraphs = paragraph_runs(main, main, &paragraph_pages, pieces)?;
    read_sections(main, sections.0, sections.1)?;
    rebuild_character_runs(characters, pieces)?;
    let paragraphs = rebuild_paragraph_runs(paragraphs, &text, pieces)?;
    Ok((text, paragraphs))
}

/// `new OldSectionTable(documentStream, offset, size)`: its sections read, a section's
/// properties two bytes longer than they say.
fn read_sections(main: &[u8], offset: i32, size: i32) -> Read<()> {
    let plex = Plex::read(main, offset, size, SED_SIZE)?;
    for (_, _, descriptor) in &plex.entries {
        i16_at(descriptor, 0)?;
        let sepx = i32_at(descriptor, 2)?;
        i16_at(descriptor, 6)?;
        i32_at(descriptor, 8)?;
        if sepx != -1 {
            let size = i32::from(i16_at(main, index(sepx)?)?);
            safely_clone(
                main,
                sepx.wrapping_add(2),
                size.wrapping_add(2),
                i64::from(i16::MAX) + 2,
            )?;
        }
    }
    Ok(())
}

/// The text pieces of a complex document, each in `charset`, at the file positions the piece
/// descriptors give as they are.
fn complex_pieces(
    main: &[u8],
    offset: i32,
    charset: Charset,
) -> Result<ComplexFileTable, Unported> {
    let (grpprls, descriptors) = read_clx(main, offset).map_err(throws)?;
    let mut pieces = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        let bytes = safely_clone(
            main,
            descriptor.fc,
            descriptor.end.wrapping_sub(descriptor.start),
            MAX_PIECE_LENGTH,
        )
        .map_err(throws)?;
        let text = decode(charset, &bytes)?;
        pieces.push(
            TextPiece::new(
                descriptor.start,
                descriptor.end,
                descriptor.fc,
                false,
                descriptor.prm,
                text,
            )
            .map_err(throws)?,
        );
    }
    Ok(ComplexFileTable::new(grpprls, pieces))
}

/// `buildTextPiece`: the text from `fcMin` to `fcMac` as one piece, in `charset`, else, where
/// it does not decode to as many characters as bytes, in code page 1252.
fn whole_text(
    main: &[u8],
    fc_min: i32,
    fc_mac: i32,
    charset: Charset,
) -> Result<ComplexFileTable, Unported> {
    let bytes = safely_clone(main, fc_min, fc_mac.wrapping_sub(fc_min), MAX_RECORD_LENGTH)
        .map_err(throws)?;
    let length = i32::try_from(bytes.len()).expect("a record of at most ten million bytes");
    let mut text = decode(charset, &bytes)?;
    if text.len() != bytes.len() {
        text = decode(Charset::named("windows-1252"), &bytes)?;
    }
    let piece = TextPiece::new(0, length, fc_min, false, 0, text).map_err(throws)?;
    Ok(ComplexFileTable::new(Vec::new(), vec![piece]))
}

/// The bytes as `new String(bytes, charset)` gives them, in UTF-16 units.
fn decode(charset: Charset, bytes: &[u8]) -> Result<Vec<u16>, Unported> {
    charset
        .decode_exactly(bytes)
        .map(|text| text.encode_utf16().collect())
        .ok_or_else(|| {
            Unported(format!(
                "text in {}, which this port does not decode as Java does",
                charset.name()
            ))
        })
}

/// `FontCharset`'s Java charset of a Windows character set, `None` for one it does not know,
/// and an empty name for the symbol set.
fn font_charset(chs: u8) -> Option<&'static str> {
    Some(match chs {
        0x00 | 0x01 | 0xFF => "Cp1252",
        0x02 => "",
        0x4D => "MacRoman",
        0x80 => "Shift_JIS",
        0x81 => "cp949",
        0x82 => "x-Johab",
        0x86 => "GB2312",
        0x88 => "Big5",
        0xA1 => "Cp1253",
        0xA2 => "Cp1254",
        0xA3 => "Cp1258",
        0xB1 => "Cp1255",
        0xB2 => "Cp1256",
        0xBA => "Cp1257",
        0xCC => "Cp1251",
        0xDE => "x-windows-874",
        0xEE => "Cp1250",
        _ => return None,
    })
}

/// `guessCodePage(new OldFontTable(...))`: the charset of the first font whose character set
/// is named and is no ANSI, default or symbol set, else code page 1252.
fn guess_charset(main: &[u8], offset: i32, length: i32) -> Result<Charset, Unported> {
    let character_sets = font_character_sets(main, offset, length).map_err(throws)?;
    let named = character_sets
        .into_iter()
        .filter(|chs| !matches!(chs, 0x00..=0x02))
        .find_map(font_charset);
    let name = named.unwrap_or("Cp1252");
    if name == "Big5" {
        return Err(Unported(
            "a Word 6 document in Big5, which POI reads with its own code page 950 reader"
                .to_owned(),
        ));
    }
    Charset::for_name(name).ok_or_else(|| {
        Unported(format!(
            "a Word 6 document in {name}, a charset this port does not have"
        ))
    })
}

/// `new OldFontTable(buf, offset, length)`: the character set of each font, failing where
/// POI reads past the stream.
fn font_character_sets(main: &[u8], offset: i32, length: i32) -> Read<Vec<u8>> {
    let byte = |at: i32| -> Read<u8> {
        main.get(index(at)?).copied().ok_or_else(|| {
            Thrown(format!(
                "Index {at} out of bounds for length {}",
                main.len()
            ))
        })
    };
    i16_at(main, index(offset)?)?;
    let end = offset.wrapping_add(length);
    let mut start = offset.wrapping_add(2);
    let mut character_sets = Vec::new();
    loop {
        if start.wrapping_add(6) > end {
            break;
        }
        let description = i32::from(byte(start)? as i8);
        let mut at = start + 1;
        if at.wrapping_add(description) > end {
            break;
        }
        at += 3;
        let chs = byte(at)?;
        at += 2;
        let mut name_length = None;
        let mut i = at;
        while i < end {
            if byte(i)? == 0 {
                name_length = Some(i - at);
                break;
            }
            i += 1;
        }
        let Some(name_length) = name_length else {
            break;
        };
        at += name_length + 1;
        let mut alternative = -1;
        if at - start < description {
            let mut i = at;
            while i <= start.wrapping_add(description) {
                if byte(i)? == 0 {
                    alternative = i - at;
                    break;
                }
                i += 1;
            }
        }
        let alternative = if alternative < 0 { 0 } else { alternative + 1 };
        character_sets.push(chs);
        start = start.wrapping_add(6 + name_length + alternative + 1);
    }
    Ok(character_sets)
}

#[cfg(test)]
mod tests;
