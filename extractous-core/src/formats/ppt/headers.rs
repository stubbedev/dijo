//! The headers and footers of a sheet as POI's `HeadersFooters` reads them: from the flags and
//! strings of its headers and footers record, the document's of its type, or a new one; or, in a
//! presentation PowerPoint 2007 saved, from the text of its placeholder shapes.

use crate::formats::Unported;

use super::records::{kind, Record};
use super::shapes::sheet_shapes;
use super::show::{Sheet, Show};

/// The tag a presentation PowerPoint 2007 saved gives its first master.
const PPT_2007_TAG: &str = "___PPT12";
/// `HeadersFootersAtom.fHasHeader` and `fHasFooter`.
const HAS_HEADER: i16 = 16;
const HAS_FOOTER: i16 = 32;
/// The instances of the header's and footer's strings.
const HEADER_STRING: i32 = 1;
const FOOTER_STRING: i32 = 2;
/// The placeholder ids of a header and a footer, alike on slides, notes and masters.
const HEADER_PLACEHOLDER: i32 = 10;
const FOOTER_PLACEHOLDER: i32 = 9;

fn poi_throws(reason: &str) -> Unported {
    Unported(format!(
        "POI throws reading the headers and footers, which fails Tika's parse: {reason}"
    ))
}

/// Which headers and footers a sheet has: a slide's or notes'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    Slide,
    Notes,
}

impl Scope {
    fn options(self) -> i16 {
        match self {
            Scope::Slide => 0x3F,
            Scope::Notes => 0x4F,
        }
    }
}

/// A header or a footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Part {
    Header,
    Footer,
}

/// `HeadersFooters` of a sheet.
pub(super) struct HeadersFooters<'s, 'a> {
    sheet: &'s Sheet<'a>,
    container: Option<&'a Record>,
    ppt_2007: bool,
}

impl<'s, 'a> HeadersFooters<'s, 'a> {
    /// `new HeadersFooters(sheet, type)`.
    pub(super) fn new(
        show: &'s Show<'a>,
        sheet: &'s Sheet<'a>,
        scope: Scope,
    ) -> Result<Self, Unported> {
        let first_master = show
            .masters
            .first()
            .ok_or_else(|| poi_throws("Index 0 out of bounds for length 0"))?;
        let ppt_2007 =
            programmable_tag(first_master.sheet.container).as_deref() == Some(PPT_2007_TAG);
        let container = sheet.container.first(kind::HEADERS_FOOTERS).or_else(|| {
            show.document.children().iter().find(|child| {
                child.kind == kind::HEADERS_FOOTERS && child.options as i16 == scope.options()
            })
        });
        if container.is_none() && show.document.first(kind::LIST).is_none() {
            return Err(poi_throws(
                "Asked to add a new child after another record, but that record wasn't one of our \
                 children!",
            ));
        }
        Ok(HeadersFooters {
            sheet,
            container,
            ppt_2007,
        })
    }

    /// `isHeaderVisible` and `isFooterVisible`.
    pub(super) fn is_visible(&self, part: Part) -> Result<bool, Unported> {
        if self.ppt_2007 {
            return Ok(self.placeholder_text(part)?.is_some());
        }
        let Some(container) = self.container else {
            return Ok(false);
        };
        let atom = container
            .last(kind::HEADERS_FOOTERS_ATOM)
            .ok_or_else(|| poi_throws("a headers and footers record without its atom"))?;
        let mask = atom
            .short(2)
            .map_err(|_| poi_throws("a short headers and footers atom"))?;
        let flag = match part {
            Part::Header => HAS_HEADER,
            Part::Footer => HAS_FOOTER,
        };
        Ok(mask & flag != 0)
    }

    /// `getHeaderText` and `getFooterText`.
    pub(super) fn text(&self, part: Part) -> Result<Option<Vec<u16>>, Unported> {
        if self.ppt_2007 {
            let text = self.placeholder_text(part)?;
            return Ok(text.filter(|text| text != &[u16::from(b'*')]));
        }
        let instance = match part {
            Part::Header => HEADER_STRING,
            Part::Footer => FOOTER_STRING,
        };
        Ok(self.container.and_then(|container| {
            container
                .children()
                .iter()
                .rev()
                .find(|child| child.kind == kind::C_STRING && child.instance() == instance)
                .map(Record::utf16)
        }))
    }

    /// The text of the sheet's first shape of the part's placeholder, where it is a text shape
    /// (`getPlaceholder(ph)` then `getText`).
    fn placeholder_text(&self, part: Part) -> Result<Option<Vec<u16>>, Unported> {
        let wanted = match part {
            Part::Header => HEADER_PLACEHOLDER,
            Part::Footer => FOOTER_PLACEHOLDER,
        };
        for shape in sheet_shapes(self.sheet)? {
            if shape.kind.is_group() {
                continue;
            }
            let client = |record_kind| {
                shape
                    .node
                    .child(crate::formats::escher::kind::CLIENT_DATA)
                    .and_then(|data| data.client.iter().rev().find(|r| r.kind == record_kind))
            };
            let id = if let Some(atom) = client(kind::OE_PLACEHOLDER_ATOM) {
                Some(i32::from(
                    atom.byte(4)
                        .map_err(|_| poi_throws("a short placeholder"))?,
                ))
            } else if let Some(atom) = client(kind::ROUND_TRIP_HF_PLACEHOLDER) {
                Some(i32::from(
                    atom.byte(0)
                        .map_err(|_| poi_throws("a short placeholder"))? as i8,
                ))
            } else {
                None
            };
            if id != Some(wanted) {
                continue;
            }
            if !shape.kind.is_text() {
                return Ok(None);
            }
            return shape.text(self.sheet).map(Some);
        }
        Ok(None)
    }
}

/// `getProgrammableTag`: the text of the first string of the first binary tag of a sheet's tags.
fn programmable_tag(container: &Record) -> Option<String> {
    let tag = container
        .first(kind::PROG_TAGS)?
        .first(kind::PROG_BINARY_TAG)?
        .first(kind::C_STRING)?;
    Some(String::from_utf16_lossy(&tag.utf16()))
}

/// The sheet `getNotesHeadersFooters` reads: the first notes, else the first master.
pub(super) fn notes_sheet<'s, 'a>(show: &'s Show<'a>) -> Result<&'s Sheet<'a>, Unported> {
    if let Some(notes) = show.notes.first() {
        return Ok(notes);
    }
    show.masters
        .first()
        .map(|master| &master.sheet)
        .ok_or_else(|| poi_throws("Index 0 out of bounds for length 0"))
}
