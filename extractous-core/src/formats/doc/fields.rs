//! POI's `FieldsTables` and `FieldsImpl`: the field marks of each part of the document, paired
//! into fields of a begin, an optional separator and an end mark as POI pairs them, by the
//! offset of their begin mark.

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
    end: i32,
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
                for (start, end, descriptor) in Plex::read(table, offset, size, FLD_SIZE)?.entries {
                    let [holder, kind, ..] = descriptor[..] else {
                        return Err(Thrown(format!(
                            "Index {} out of bounds for length {}",
                            descriptor.len(),
                            descriptor.len()
                        )));
                    };
                    marks.push(Mark {
                        position: start,
                        end,
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

/// `parseFieldStructure`: the marks sorted by position, paired into fields.
fn structure(mut marks: Vec<Mark>) -> HashMap<i32, Field> {
    marks.sort_by_key(|mark| mark.position);
    let mut fields = Vec::new();
    structure_between(&marks, 0, marks.len(), &mut fields);
    let mut by_offset = HashMap::with_capacity(fields.len());
    for (offset, field) in fields {
        by_offset.insert(offset, field);
    }
    by_offset
}

/// `parseFieldStructureImpl`: the fields of the marks from `start` to before `end`, nested
/// fields after the field they are in.
fn structure_between(marks: &[Mark], start: usize, end: usize, fields: &mut Vec<(i32, Field)>) {
    let mut next = start;
    while next < end {
        let begin = marks[next];
        if begin.boundary != BEGIN_MARK {
            next += 1;
            continue;
        }
        let Some(following) = search(marks, next + 1, end, begin.end) else {
            next += 1;
            continue;
        };
        let after = marks[following];
        match after.boundary {
            SEPARATOR_MARK => {
                let Some(closing) = search(marks, following, end, after.end) else {
                    next += 1;
                    continue;
                };
                let finish = marks[closing];
                if finish.boundary != END_MARK {
                    next += 1;
                    continue;
                }
                fields.push((
                    begin.position,
                    Field {
                        kind: begin.kind,
                        separator: Some(after.position),
                    },
                ));
                if begin.position + 1 < after.position - 1 {
                    structure_between(marks, next + 1, following, fields);
                }
                if after.position + 1 < finish.position - 1 {
                    structure_between(marks, following + 1, closing, fields);
                }
                next = closing + 1;
            }
            END_MARK => {
                fields.push((
                    begin.position,
                    Field {
                        kind: begin.kind,
                        separator: None,
                    },
                ));
                if begin.position + 1 < after.position - 1 {
                    structure_between(marks, next + 1, following, fields);
                }
                next = following + 1;
            }
            _ => next += 1,
        }
    }
}

/// `FieldsImpl.binarySearch` for a mark at `position` among those from `start` to before
/// `end`: its index, or `None` where POI gives a negative insertion point.
fn search(marks: &[Mark], start: usize, end: usize, position: i32) -> Option<usize> {
    let (mut low, mut high) = (start as i64, end as i64 - 1);
    while low <= high {
        let mid = ((low + high) as u64 >> 1) as usize;
        let at = marks[mid].position;
        match at.cmp(&position) {
            std::cmp::Ordering::Equal => return Some(mid),
            std::cmp::Ordering::Less => low = mid as i64 + 1,
            std::cmp::Ordering::Greater => high = mid as i64 - 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marks of a plex: each runs to the position of the next, the last one on.
    fn marks(entries: &[(i32, u8, i8)]) -> Vec<Mark> {
        entries
            .iter()
            .enumerate()
            .map(|(i, &(position, boundary, kind))| Mark {
                position,
                end: entries.get(i + 1).map_or(position + 1, |next| next.0),
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
    fn pairs_no_field_whose_begin_mark_is_followed_by_a_nested_one() {
        let fields = structure(marks(&[
            (10, BEGIN_MARK, 88),
            (12, BEGIN_MARK, 58),
            (13, SEPARATOR_MARK, 0),
            (14, END_MARK, 0),
            (15, SEPARATOR_MARK, 0),
            (20, END_MARK, 0),
        ]));
        assert_eq!(fields.get(&10), None);
        assert_eq!(
            fields.get(&12),
            Some(&Field {
                kind: 58,
                separator: Some(13)
            })
        );
    }
}
