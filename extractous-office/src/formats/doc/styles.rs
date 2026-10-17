//! POI's `StyleSheet`: the style descriptions of the table stream with their names, the
//! paragraph and character property exceptions of each, and the properties each style derives
//! from its base styles, failing where POI fails.

use super::bytes::{index, safely_clone, utf16_units};
use super::props::{style_chp, uncompress_pap, Chp, Pap};
use crate::formats::poi::{i16_at, i32_at, Read, Thrown};

/// The index meaning no style.
const NIL_STYLE: i32 = 4095;
const MAX_NESTING: usize = 1000;
const PARAGRAPH_STYLE: u16 = 1;
const CHARACTER_STYLE: u16 = 2;

/// A style description.
#[derive(Debug, Clone)]
pub(super) struct Style {
    /// The name, in UTF-16 units.
    pub(super) name: Vec<u16>,
    kind: u16,
    base: i32,
    upxs: Vec<Vec<u8>>,
    pap: Option<Pap>,
    chp: Option<Chp>,
}

impl Style {
    /// `getCHPX`, failing for a character style without one.
    fn chpx(&self) -> Read<Option<&[u8]>> {
        Ok(match self.kind {
            PARAGRAPH_STYLE => self.upxs.get(1).map(Vec::as_slice),
            CHARACTER_STYLE => Some(first_upx(&self.upxs)?),
            _ => None,
        })
    }

    /// `getPAPX`, failing for a paragraph style without one.
    fn papx(&self) -> Read<Option<&[u8]>> {
        Ok(if self.kind == PARAGRAPH_STYLE {
            Some(first_upx(&self.upxs)?)
        } else {
            None
        })
    }
}

fn first_upx(upxs: &[Vec<u8>]) -> Read<&[u8]> {
    upxs.first()
        .map(Vec::as_slice)
        .ok_or_else(|| Thrown::new("Index 0 out of bounds for length 0"))
}

/// The style sheet.
pub(super) struct StyleSheet {
    styles: Vec<Option<Style>>,
}

impl StyleSheet {
    /// `new StyleSheet(tableStream, fcStshf)`.
    pub(super) fn read(table: &[u8], offset: i32) -> Read<StyleSheet> {
        let start = index(offset)?;
        let header_size = i32::from(i16_at(table, start)?);
        let count = i32::from(i16_at(table, start + 2)?);
        let base_length = i32::from(i16_at(table, start + 4)?);
        if count < 0 {
            return Err(Thrown(format!(
                "Cannot create StyleSheet, invalid Cstd: {count}"
            )));
        }
        let mut position = offset.wrapping_add(2).wrapping_add(header_size);
        let mut styles = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let size = i32::from(i16_at(table, index(position)?)?);
            position = position.wrapping_add(2);
            styles.push(if size > 0 {
                Some(read_style(table, base_length, position)?)
            } else {
                None
            });
            position = position.wrapping_add(size);
        }
        let mut sheet = StyleSheet { styles };
        for istd in 0..sheet.styles.len() {
            if sheet.styles[istd].is_some() {
                sheet.create_pap(istd, 0)?;
                sheet.create_chp(istd, 0)?;
            }
        }
        Ok(sheet)
    }

    fn create_pap(&mut self, istd: usize, nesting: usize) -> Read<()> {
        if nesting > MAX_NESTING {
            return Ok(());
        }
        let style = self.styles[istd]
            .as_ref()
            .ok_or_else(|| Thrown::new("Cannot create Pap, empty styleDescription"))?;
        if style.pap.is_some() {
            return Ok(());
        }
        let Some(papx) = style.papx()?.map(<[u8]>::to_vec) else {
            return Ok(());
        };
        let base = style.base;
        let mut parent = Pap::default();
        if base != NIL_STYLE {
            let base_index = index(base)?;
            let base_style = self
                .styles
                .get(base_index)
                .ok_or_else(|| Thrown(format!("Index {base} out of bounds")))?
                .as_ref()
                .ok_or_else(|| Thrown::new("Cannot create Pap, empty styleDescription"))?;
            parent = match &base_style.pap {
                Some(pap) => pap.clone(),
                None => {
                    if base_index == istd {
                        return Err(Thrown(format!(
                            "Pap style {istd} claimed to have itself as its parent"
                        )));
                    }
                    self.create_pap(base_index, nesting + 1)?;
                    self.styles[base_index]
                        .as_ref()
                        .and_then(|style| style.pap.clone())
                        .unwrap_or_default()
                }
            };
        }
        let pap = uncompress_pap(&parent, &papx, 2)?;
        self.styles[istd].as_mut().expect("a style").pap = Some(pap);
        Ok(())
    }

    fn create_chp(&mut self, istd: usize, nesting: usize) -> Read<()> {
        if nesting > MAX_NESTING {
            return Ok(());
        }
        let style = self.styles[istd]
            .as_ref()
            .ok_or_else(|| Thrown::new("Cannot create Chp, empty styleDescription"))?;
        if style.chp.is_some() {
            return Ok(());
        }
        let Some(chpx) = style.chpx()?.map(<[u8]>::to_vec) else {
            return Ok(());
        };
        let mut base = style.base;
        if base == istd as i32 {
            base = NIL_STYLE;
        }
        let mut parent = Chp::default();
        if base != NIL_STYLE {
            let base_index = index(base)?;
            let base_style = self
                .styles
                .get(base_index)
                .ok_or_else(|| Thrown(format!("Index {base} out of bounds")))?
                .as_ref()
                .ok_or_else(|| Thrown::new("Cannot create Chp, empty styleDescription"))?;
            parent = match &base_style.chp {
                Some(chp) => chp.clone(),
                None => {
                    self.create_chp(base_index, nesting + 1)?;
                    self.styles[base_index]
                        .as_ref()
                        .and_then(|style| style.chp.clone())
                        .unwrap_or_default()
                }
            };
        }
        let chp = style_chp(&parent, &chpx)?;
        self.styles[istd].as_mut().expect("a style").chp = Some(chp);
        Ok(())
    }

    pub(super) fn len(&self) -> usize {
        self.styles.len()
    }

    /// `getStyleDescription`, failing where the index lies outside the sheet.
    pub(super) fn style(&self, istd: i32) -> Read<Option<&Style>> {
        Ok(usize::try_from(istd)
            .ok()
            .and_then(|istd| self.styles.get(istd))
            .ok_or_else(|| Thrown(format!("Index {istd} out of bounds")))?
            .as_ref())
    }

    /// `getCharacterStyle`: `Ok(None)` for Java's null.
    pub(super) fn character_style(&self, istd: i32) -> Read<Option<Chp>> {
        if istd == NIL_STYLE || istd >= self.styles.len() as i32 || istd == -1 {
            return Ok(Some(Chp::default()));
        }
        Ok(match self.style(istd)? {
            Some(style) => style.chp.clone(),
            None => Some(Chp::default()),
        })
    }

    /// `getCHPX`: empty where the sheet has none for the index; a negative index fails, which
    /// the caller passes over.
    pub(super) fn chpx(&self, istd: i32) -> Vec<u8> {
        if istd == NIL_STYLE || istd >= self.styles.len() as i32 || istd < 0 {
            return Vec::new();
        }
        self.styles[istd as usize]
            .as_ref()
            .and_then(|style| style.chpx().ok().flatten())
            .map(<[u8]>::to_vec)
            .unwrap_or_default()
    }

    /// `getPAPX`, failing where the index is negative.
    pub(super) fn papx(&self, istd: i32) -> Read<Vec<u8>> {
        if istd == NIL_STYLE || istd >= self.styles.len() as i32 || istd == -1 {
            return Ok(vec![0, 0]);
        }
        Ok(match self.style(istd)? {
            Some(style) => style.papx()?.map_or_else(|| vec![0, 0], <[u8]>::to_vec),
            None => vec![0, 0],
        })
    }
}

/// `new StyleDescription(table, baseLength, offset, true)`.
fn read_style(table: &[u8], base_length: i32, offset: i32) -> Read<Style> {
    let start = index(offset)?;
    let info2 = i16_at(table, start + 2)? as u16;
    let info3 = i16_at(table, start + 4)? as u16;
    for at in [0, 6, 8] {
        i16_at(table, start + at)?;
    }
    if base_length == 0x12 {
        i32_at(table, start + 10)?;
        i32_at(table, start + 14)?;
    }
    let kind = info2 & 0x000F;
    let base = i32::from((info2 & 0xFFF0) >> 4);
    let upx_count = info3 & 0x000F;
    let name_start = offset.wrapping_add(base_length);
    let name_length = i32::from(i16_at(table, index(name_start)?)?);
    let name_at = name_start.wrapping_add(2);
    if name_length < 0 {
        return Err(Thrown(format!("Illegal length {name_length}")));
    }
    let name = if name_length == 0 {
        Vec::new()
    } else {
        if name_at as i64 >= table.len() as i64 {
            return Err(Thrown(format!("Illegal offset {name_at}")));
        }
        utf16_units(table, index(name_at)?, name_length as usize)
            .map_err(|_| Thrown(format!("Illegal length {name_length}")))?
    };
    let mut position = (name_length + 1).wrapping_mul(2).wrapping_add(name_at);
    let mut upxs = Vec::with_capacity(usize::from(upx_count));
    for _ in 0..upx_count {
        let size = i32::from(i16_at(table, index(position)?)?);
        position = position.wrapping_add(2);
        upxs.push(safely_clone(table, position, size, i64::from(i16::MAX))?);
        position = position.wrapping_add(size);
        if size & 1 == 1 {
            position = position.wrapping_add(1);
        }
    }
    Ok(Style {
        name,
        kind,
        base,
        upxs,
        pap: None,
        chp: None,
    })
}
