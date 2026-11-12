//! The text of a page, laid out from where its strings are shown: a string shown on the
//! baseline of the one before goes on its line, after a space where the gap between them is a
//! word's; one shown on another baseline starts a line, and one far from it a paragraph. The
//! gap is known where the program moves on from a string by a relative move, or where the
//! string's width is known from its font's metrics; a string the program places anew on the same
//! baseline without either is taken for a new word.

/// The font size, in the units of the user space, taken for a font whose size is unknown.
pub(super) const DEFAULT_EM: f64 = 10.0;
/// The share of an em a gap on a baseline must exceed to separate words.
const WORD_GAP: f64 = 0.15;
/// The share of an em by which a baseline must move to start a line.
const LINE_SHIFT: f64 = 0.4;
/// The share of an em by which a baseline must move to start a paragraph.
const PARAGRAPH_SHIFT: f64 = 1.8;
/// The advance taken for a glyph of unknown width, as a share of an em.
const ESTIMATED_ADVANCE: f64 = 0.5;

/// How well the horizontal position of the current point is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Accuracy {
    Exact,
    Estimated,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    x: f64,
    y: f64,
    accuracy: Accuracy,
}

/// A move whose operands were not known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lost {
    /// To a place anew: what is shown next starts a line.
    Absolute,
    /// By an offset: what is shown next goes on as before.
    Relative,
}

/// Where the next string is shown, as far as the program's moves tell.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct Position {
    point: Option<Point>,
    /// The horizontal distance moved since the last string was shown, while every move since was
    /// relative.
    gap: Option<f64>,
    lost: Option<Lost>,
}

impl Position {
    /// The current point, if it is known.
    pub(super) fn current(&self) -> Option<(f64, f64)> {
        self.point.map(|point| (point.x, point.y))
    }
}

/// How far a string's width is known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Advance {
    /// From the font's metrics or the program's own displacements.
    Measured(f64),
    /// Not at all: it is estimated from its glyphs.
    Unknown,
}

/// A string shown, as the page lays it out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Shown<'a> {
    pub(super) text: &'a str,
    pub(super) glyphs: u32,
    pub(super) unmapped: u32,
    pub(super) advance: Advance,
    /// The font size in the units of the user space.
    pub(super) em: f64,
}

/// What separates a string from the one shown before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Separator {
    None,
    Space,
    Line,
    Paragraph,
}

/// The last string shown: its baseline, where it ended if that is known, and its font size.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Last {
    y: f64,
    end: Option<f64>,
    em: f64,
}

/// What a page shows, as it is shown.
#[derive(Debug, Default)]
pub(super) struct Layout {
    position: Position,
    last: Option<Last>,
    paragraphs: Vec<String>,
    paragraph: String,
    glyphs: u32,
    unmapped: u32,
}

/// A page's text, its paragraphs in the order they were shown.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct PageText {
    pub(super) paragraphs: Vec<String>,
    pub(super) glyphs: u32,
    pub(super) unmapped: u32,
}

impl Layout {
    pub(super) fn position(&self) -> Position {
        self.position
    }

    /// Returns to a position saved before, as `grestore` does.
    pub(super) fn restore(&mut self, position: Position) {
        self.position = position;
    }

    /// Moves to `point`, `None` where its coordinates are not known.
    pub(super) fn move_to(&mut self, point: Option<(f64, f64)>) {
        self.position = match point {
            Some((x, y)) => Position {
                point: Some(Point {
                    x,
                    y,
                    accuracy: Accuracy::Exact,
                }),
                gap: None,
                lost: None,
            },
            None => Position {
                point: None,
                gap: None,
                lost: Some(Lost::Absolute),
            },
        };
    }

    /// Moves by `offset`, `None` where it is not known.
    pub(super) fn move_by(&mut self, offset: Option<(f64, f64)>) {
        let position = &mut self.position;
        match offset {
            Some((dx, dy)) => {
                if let Some(point) = &mut position.point {
                    point.x += dx;
                    point.y += dy;
                }
                position.gap = position.gap.map(|gap| gap + dx);
            }
            None => {
                position.point = None;
                position.gap = None;
                position.lost = position.lost.or(Some(Lost::Relative));
            }
        }
    }

    /// Leaves no current point, as `newpath` does.
    pub(super) fn clear_point(&mut self) {
        self.position = Position::default();
    }

    /// Lays out a string shown at the current position, which moves to its end.
    pub(super) fn show(&mut self, shown: Shown<'_>) {
        self.glyphs = self.glyphs.saturating_add(shown.glyphs);
        self.unmapped = self.unmapped.saturating_add(shown.unmapped);
        match self.separator() {
            Separator::None => {}
            Separator::Space => {
                let spaced = self.paragraph.is_empty()
                    || self.paragraph.ends_with(char::is_whitespace)
                    || shown.text.starts_with(char::is_whitespace);
                if !spaced {
                    self.paragraph.push(' ');
                }
            }
            Separator::Line => {
                if !self.paragraph.is_empty() && !self.paragraph.ends_with('\n') {
                    self.paragraph.push('\n');
                }
            }
            Separator::Paragraph => self.end_paragraph(),
        }
        self.paragraph.push_str(shown.text);
        let width = match shown.advance {
            Advance::Measured(width) => width,
            Advance::Unknown => f64::from(shown.glyphs) * ESTIMATED_ADVANCE * shown.em,
        };
        let position = &mut self.position;
        if let Some(point) = &mut position.point {
            let measured =
                point.accuracy == Accuracy::Exact && matches!(shown.advance, Advance::Measured(_));
            self.last = Some(Last {
                y: point.y,
                end: measured.then_some(point.x + width),
                em: shown.em,
            });
            point.x += width;
            if !measured {
                point.accuracy = Accuracy::Estimated;
            }
        }
        position.gap = Some(0.0);
        position.lost = None;
    }

    fn separator(&self) -> Separator {
        match self.position.lost {
            Some(Lost::Absolute) => return Separator::Line,
            Some(Lost::Relative) => return Separator::None,
            None => {}
        }
        let (Some(point), Some(last)) = (self.position.point, self.last) else {
            return Separator::None;
        };
        let shift = (point.y - last.y).abs();
        if shift > PARAGRAPH_SHIFT * last.em {
            return Separator::Paragraph;
        }
        if shift > LINE_SHIFT * last.em {
            return Separator::Line;
        }
        let gap = match (self.position.gap, last.end, point.accuracy) {
            (Some(gap), _, _) => gap,
            (None, Some(end), Accuracy::Exact) => point.x - end,
            _ => return Separator::Space,
        };
        if gap > WORD_GAP * last.em {
            Separator::Space
        } else {
            Separator::None
        }
    }

    fn end_paragraph(&mut self) {
        let paragraph = std::mem::take(&mut self.paragraph);
        if !paragraph.trim().is_empty() {
            self.paragraphs.push(paragraph);
        }
    }

    /// The page's text, the layout left for the next page, where the current point is lost.
    pub(super) fn end_page(&mut self) -> PageText {
        self.end_paragraph();
        let page = PageText {
            paragraphs: std::mem::take(&mut self.paragraphs),
            glyphs: self.glyphs,
            unmapped: self.unmapped,
        };
        *self = Layout::default();
        page
    }

    /// Whether anything was shown on the page so far.
    pub(super) fn is_blank(&self) -> bool {
        self.glyphs == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(text: &str, advance: Advance) -> Shown<'_> {
        Shown {
            text,
            glyphs: text.chars().count() as u32,
            unmapped: 0,
            advance,
            em: 10.0,
        }
    }

    #[test]
    fn separates_words_by_measured_gaps_and_relative_moves() {
        let mut layout = Layout::default();
        layout.move_to(Some((72.0, 700.0)));
        layout.show(shown("Ke", Advance::Measured(12.0)));
        layout.move_to(Some((84.5, 700.0)));
        layout.show(shown("eper", Advance::Unknown));
        layout.move_by(Some((4.0, 0.0)));
        layout.show(shown("climbed", Advance::Unknown));
        layout.move_by(Some((0.5, 0.0)));
        layout.show(shown("!", Advance::Unknown));
        layout.move_to(Some((300.0, 700.0)));
        layout.show(shown("far", Advance::Unknown));
        assert_eq!(layout.end_page().paragraphs, ["Keeper climbed! far"]);
    }

    #[test]
    fn breaks_lines_and_paragraphs_by_baseline() {
        let mut layout = Layout::default();
        layout.move_to(Some((72.0, 700.0)));
        layout.show(shown("one", Advance::Unknown));
        layout.move_to(Some((72.0, 688.0)));
        layout.show(shown("two", Advance::Unknown));
        layout.move_to(Some((72.0, 640.0)));
        layout.show(shown("three", Advance::Unknown));
        layout.move_to(None);
        layout.show(shown("four", Advance::Unknown));
        layout.move_by(None);
        layout.show(shown("five", Advance::Unknown));
        let page = layout.end_page();
        assert_eq!(page.paragraphs, ["one\ntwo", "three\nfourfive"]);
        assert_eq!(page.glyphs, 19);
        assert!(layout.is_blank());
    }
}
