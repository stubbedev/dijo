//! FITS files, which Tika without its scientific module leaves empty: that module (#172) reads them
//! only by running GDAL's `gdalinfo` (`GDALParser`). The native parser reads the header of each HDU
//! in place, its 2880-byte blocks of 80-character cards, and skips the data the header sizes. The
//! primary header's keywords go into the metadata under `fits:`, its commentary under
//! `fits:COMMENT` and `fits:HISTORY`; the text lists every HDU with its cards.

use crate::config::ParserConfigs;
use crate::formats::bytes::{malformed, Bytes, Read};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Footprint, Parser, Prepared};

const MEDIA_TYPE: &str = "application/fits";
/// The prefix of the primary header's keywords in the metadata.
const FITS: &str = "fits:";

const BLOCK: u64 = 2880;
const CARD: usize = 80;
/// The columns of a card that hold its keyword.
const KEYWORD: usize = 8;
/// What follows the keyword of a card that holds a value.
const VALUE_INDICATOR: &[u8; 2] = b"= ";
const EXTENSION: &[u8; 8] = b"XTENSION";

/// A FITS parser where Tika's scientific module has `GDALParser`, which runs GDAL's
/// `gdalinfo` on the file.
pub struct FitsParser;

impl Parser for FitsParser {
    fn name(&self) -> &'static str {
        "extractous::FITSParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.gdal.GDALParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let units = units(Bytes::new(input))?;
        Ok(Box::new(move |xhtml| {
            if let Some(primary) = units.first() {
                for card in &primary.cards {
                    if let Some((keyword, value)) = card.metadata() {
                        xhtml.metadata_mut().add_prefixed(FITS, keyword, value);
                    }
                }
            }
            write(xhtml, &units)?;
            Ok(())
        }))
    }
}

/// Each HDU as a heading, then its cards in a list.
fn write(xhtml: &mut Xhtml, units: &[Unit]) -> Result<(), Stop> {
    for (index, unit) in units.iter().enumerate() {
        xhtml.element("h1", &unit.heading(index))?;
        xhtml.start_element("ul")?;
        xhtml.newline()?;
        for card in &unit.cards {
            xhtml.element("li", &card.text)?;
        }
        xhtml.end_element("ul")?;
    }
    Ok(())
}

/// A header card.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Card {
    keyword: String,
    /// The value of a card that holds one: a string without its quotes, else as written.
    value: Option<String>,
    /// The text of a card that holds no value, after its keyword.
    commentary: String,
    /// The card as written, without the spaces that pad it.
    text: String,
}

impl Card {
    fn read(bytes: &[u8]) -> Self {
        let (keyword, rest) = bytes.split_at(KEYWORD.min(bytes.len()));
        let keyword = String::from_utf8_lossy(keyword).trim_end().to_owned();
        let (value, commentary) = match rest.strip_prefix(VALUE_INDICATOR) {
            Some(field) => (Some(value(&String::from_utf8_lossy(field))), String::new()),
            None => (None, String::from_utf8_lossy(rest).trim().to_owned()),
        };
        Self {
            keyword,
            value,
            commentary,
            text: String::from_utf8_lossy(bytes).trim_end().to_owned(),
        }
    }

    /// The metadata the card gives: a keyword with its value, or commentary with its text.
    fn metadata(&self) -> Option<(&str, &str)> {
        if let Some(value) = &self.value {
            return Some((&self.keyword, value));
        }
        (!self.keyword.is_empty() && !self.commentary.is_empty())
            .then_some((self.keyword.as_str(), self.commentary.as_str()))
    }

    fn integer(&self) -> Option<i64> {
        self.value.as_deref()?.parse().ok()
    }
}

/// The value of a card from its eleventh column: a quoted string, a doubled quote in it one
/// quote, the spaces that end it dropped; else what stands before the comment.
fn value(field: &str) -> String {
    let field = field.trim_start();
    let Some(quoted) = field.strip_prefix('\'') else {
        return field
            .split('/')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
    };
    let mut text = String::new();
    let mut characters = quoted.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\'' {
            if characters.peek() == Some(&'\'') {
                characters.next();
            } else {
                break;
            }
        }
        text.push(character);
    }
    text.trim_end().to_owned()
}

/// A header and data unit: its cards, without the END card and the blank ones.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unit {
    cards: Vec<Card>,
}

impl Unit {
    fn find(&self, keyword: &str) -> Option<&Card> {
        self.cards.iter().find(|card| card.keyword == keyword)
    }

    fn required(&self, keyword: &str, offset: u64) -> Read<i64> {
        self.find(keyword).and_then(Card::integer).ok_or_else(|| {
            malformed(format!(
                "the FITS header at offset {offset} has no integer {keyword}"
            ))
        })
    }

    /// The bytes of the data after the header, padded to a whole block.
    fn data_length(&self, offset: u64) -> Read<u64> {
        let too_large = || {
            malformed(format!(
                "the FITS header at offset {offset} sizes more data than a file holds"
            ))
        };
        let bits = self.required("BITPIX", offset)?.unsigned_abs();
        let axes = self.required("NAXIS", offset)?;
        if axes == 0 {
            return Ok(0);
        }
        let groups = self.find("GROUPS").and_then(|card| card.value.as_deref()) == Some("T");
        let mut elements = 1u64;
        for axis in 1..=axes {
            let length = self.required(&format!("NAXIS{axis}"), offset)?;
            let length = u64::try_from(length).map_err(|_| too_large())?;
            if axis == 1 && groups && length == 0 {
                continue;
            }
            elements = elements.checked_mul(length).ok_or_else(too_large)?;
        }
        let parameters = self.find("PCOUNT").and_then(Card::integer).unwrap_or(0);
        let group_count = self.find("GCOUNT").and_then(Card::integer).unwrap_or(1);
        let parameters = u64::try_from(parameters).map_err(|_| too_large())?;
        let group_count = u64::try_from(group_count).map_err(|_| too_large())?;
        let bytes = elements
            .checked_add(parameters)
            .and_then(|count| count.checked_mul(group_count))
            .and_then(|count| count.checked_mul(bits / 8))
            .ok_or_else(too_large)?;
        bytes.checked_next_multiple_of(BLOCK).ok_or_else(too_large)
    }

    fn heading(&self, index: usize) -> String {
        if index == 0 {
            return "Primary HDU".to_owned();
        }
        let kind = self
            .find("XTENSION")
            .and_then(|card| card.value.clone())
            .unwrap_or_default();
        match self.find("EXTNAME").and_then(|card| card.value.as_deref()) {
            Some(name) => format!("Extension {index}: {kind} {name}"),
            None => format!("Extension {index}: {kind}"),
        }
    }
}

/// The HDUs of the file: the primary one, then each extension that follows, up to the end of
/// the file or to what follows the last one that is no extension.
fn units(bytes: Bytes) -> Read<Vec<Unit>> {
    let mut units = Vec::new();
    let mut offset = 0u64;
    loop {
        let (unit, data) = header(bytes, offset)?;
        offset = data
            .checked_add(unit.data_length(offset)?)
            .ok_or_else(|| malformed("a FITS file of more data than a file holds"))?;
        units.push(unit);
        let next = bytes.slice(offset, EXTENSION.len() as u64, "a FITS extension");
        if next.ok() != Some(EXTENSION.as_slice()) {
            return Ok(units);
        }
    }
}

/// The header at `offset`, and where its data starts.
fn header(bytes: Bytes, offset: u64) -> Read<(Unit, u64)> {
    let mut cards = Vec::new();
    let mut block = offset;
    loop {
        let read = bytes
            .slice(block, BLOCK, "a FITS header block")
            .map_err(|_| {
                malformed(format!(
                    "the FITS header at offset {offset} has no END card"
                ))
            })?;
        block += BLOCK;
        for bytes in read.chunks(CARD) {
            let card = Card::read(bytes);
            if card.keyword == "END" {
                return Ok((Unit { cards }, block));
            }
            if !card.text.is_empty() {
                cards.push(card);
            }
        }
    }
}

#[cfg(test)]
mod tests;
