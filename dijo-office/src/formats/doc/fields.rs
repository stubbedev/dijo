//! POI's `FieldsTables` and `FieldsImpl`: the field marks of each part of the document, paired
//! into fields of a begin, an optional separator and an end mark, by the offset of their begin
//! mark.

use std::collections::HashMap;

use super::fib::{field, Fib};
use super::text::Plex;
use crate::formats::poi::{Read, Thrown};

const BEGIN_MARK: u8 = 0x13;
const SEPARATOR_MARK: u8 = 0x14;
const END_MARK: u8 = 0x15;
const FLD_SIZE: i32 = 2;

/// The parts of a document with fields of their own, as POI's `FieldsDocumentPart` orders them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Part {
    Annotations,
    Endnotes,
    Footnotes,
    Header,
    HeaderTextbox,
    Main,
    Textbox,
}

impl Part {
    const ALL: [Part; 7] = [
        Part::Annotations,
        Part::Endnotes,
        Part::Footnotes,
        Part::Header,
        Part::HeaderTextbox,
        Part::Main,
        Part::Textbox,
    ];

    /// The FIB field placing the part's field marks.
    fn fib_field(self) -> usize {
        match self {
            Part::Annotations => field::PLCFFLDATN,
            Part::Endnotes => field::PLCFFLDEDN,
            Part::Footnotes => field::PLCFFLDFTN,
            Part::Header => field::PLCFFLDHDR,
            Part::HeaderTextbox => field::PLCFFLDHDRTXBX,
            Part::Main => field::PLCFFLDMOM,
            Part::Textbox => field::PLCFFLDTXBX,
        }
    }
}

/// A field mark, `PlexOfField`: its position and its field descriptor's bytes.
#[derive(Debug, Clone, Copy)]
struct Mark {
    position: i32,
    /// `getBoundaryType`.
    boundary: u8,
    /// `getFlt`.
    kind: i8,
}

/// A field, `FieldImpl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Field {
    /// `getType`.
    pub(super) kind: i8,
    /// `getMarkSeparatorOffset`, where the field has a separator.
    pub(super) separator: Option<i32>,
}

/// The fields of every part, by the offset of their begin marks.
pub(super) struct Fields {
    by_part: Vec<HashMap<i32, Field>>,
}

impl Fields {
    /// `new FieldsImpl(new FieldsTables(tableStream, fib))`.
    pub(super) fn read(table: &[u8], fib: &Fib) -> Read<Fields> {
        let mut by_part = Vec::with_capacity(Part::ALL.len());
        for part in Part::ALL {
            let offset = fib.offset(part.fib_field())?;
            let size = fib.size(part.fib_field())?;
            let mut marks = Vec::new();
            if offset > 0 && size > 0 {
                for (start, _, descriptor) in Plex::read(table, offset, size, FLD_SIZE)?.entries {
                    let [holder, kind, ..] = descriptor[..] else {
                        return Err(Thrown::new(format!(
                            "Index {} out of bounds for length {}",
                            descriptor.len(),
                            descriptor.len()
                        )));
                    };
                    marks.push(Mark {
                        position: start,
                        boundary: holder & 0x1F,
                        kind: kind as i8,
                    });
                }
            }
            by_part.push(structure(marks));
        }
        Ok(Fields { by_part })
    }

    /// `getFieldByStartOffset`.
    pub(super) fn at(&self, part: Part, offset: i32) -> Option<Field> {
        let index = Part::ALL
            .iter()
            .position(|&known| known == part)
            .expect("every part is listed");
        self.by_part[index].get(&offset).copied()
    }
}

/// `parseFieldStructure`: the marks sorted by position, paired into fields, each end mark with
/// the latest begin mark still open and the first separator after it, so that a field whose
/// code holds another field is paired too. POI pairs a begin mark only with the mark right after
/// it, and never pairs a field whose code holds another (#111).
fn structure(mut marks: Vec<Mark>) -> HashMap<i32, Field> {
    marks.sort_by_key(|mark| mark.position);
    let mut open: Vec<(Mark, Option<i32>)> = Vec::new();
    let mut by_offset = HashMap::new();
    for mark in marks {
        match mark.boundary {
            BEGIN_MARK => open.push((mark, None)),
            SEPARATOR_MARK => {
                if let Some((_, separator @ None)) = open.last_mut() {
                    *separator = Some(mark.position);
                }
            }
            END_MARK => {
                if let Some((begin, separator)) = open.pop() {
                    by_offset.insert(
                        begin.position,
                        Field {
                            kind: begin.kind,
                            separator,
                        },
                    );
                }
            }
            _ => {}
        }
    }
    by_offset
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(entries: &[(i32, u8, i8)]) -> Vec<Mark> {
        entries
            .iter()
            .map(|&(position, boundary, kind)| Mark {
                position,
                boundary,
                kind,
            })
            .collect()
    }

    #[test]
    fn pairs_marks_into_fields_as_poi_does() {
        let fields = structure(marks(&[
            (10, BEGIN_MARK, 58),
            (14, SEPARATOR_MARK, 0),
            (16, END_MARK, 0),
            (20, BEGIN_MARK, 3),
            (25, END_MARK, 0),
        ]));
        assert_eq!(
            fields.get(&10),
            Some(&Field {
                kind: 58,
                separator: Some(14)
            })
        );
        assert_eq!(
            fields.get(&20),
            Some(&Field {
                kind: 3,
                separator: None
            })
        );
    }

    #[test]
    fn pairs_a_field_whose_code_holds_another() {
        let fields = structure(marks(&[
            (10, BEGIN_MARK, 88),
            (12, BEGIN_MARK, 58),
            (13, SEPARATOR_MARK, 0),
            (14, END_MARK, 0),
            (15, SEPARATOR_MARK, 0),
            (20, END_MARK, 0),
        ]));
        assert_eq!(
            fields.get(&10),
            Some(&Field {
                kind: 88,
                separator: Some(15)
            })
        );
        assert_eq!(
            fields.get(&12),
            Some(&Field {
                kind: 58,
                separator: Some(13)
            })
        );
    }
}
