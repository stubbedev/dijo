//! PDFBox's `TextPosition`: a glyph as text extraction places it on the page, with the
//! direction-adjusted coordinates PDFBox's line assembly reads, in Java `float` arithmetic.

use std::rc::Rc;

use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_normalization::UnicodeNormalization;

use super::font::Font;
use super::matrix::Matrix;
use super::structure::Label;

/// A glyph placed on a page.
#[derive(Debug, Clone)]
pub(super) struct TextPosition {
    text_matrix: Matrix,
    end_x: f32,
    end_y: f32,
    max_height: f32,
    rotation: i32,
    x: f32,
    y: f32,
    page_height: f32,
    page_width: f32,
    width_of_space: f32,
    pub(super) char_codes: Vec<u32>,
    pub(super) font: Rc<Font>,
    font_size: f32,
    font_size_pt: i32,
    widths: Vec<f32>,
    pub(super) unicode: String,
    direction: f32,
    /// What the glyph was drawn under, in a tagged document.
    pub(super) label: Label,
}

impl TextPosition {
    /// `new TextPosition(...)`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        rotation: i32,
        page_width: f32,
        page_height: f32,
        text_matrix: Matrix,
        end_x: f32,
        end_y: f32,
        max_height: f32,
        individual_width: f32,
        space_width: f32,
        unicode: String,
        char_codes: Vec<u32>,
        font: Rc<Font>,
        font_size: f32,
        font_size_pt: i32,
        label: Label,
    ) -> Self {
        let mut position = Self {
            text_matrix,
            end_x,
            end_y,
            max_height,
            rotation,
            x: 0.0,
            y: 0.0,
            page_height,
            page_width,
            width_of_space: space_width,
            char_codes,
            font,
            font_size,
            font_size_pt,
            widths: vec![individual_width],
            unicode,
            direction: 0.0,
            label,
        };
        position.x = position.x_rot(rotation as f32);
        position.y = if rotation == 0 || rotation == 180 {
            position.page_height - position.y_lower_left_rot(rotation as f32)
        } else {
            position.page_width - position.y_lower_left_rot(rotation as f32)
        };
        position.direction = position.compute_dir();
        position
    }

    /// `getDir`: the direction of the text, 0, 90, 180 or 270.
    fn compute_dir(&self) -> f32 {
        let a = self.text_matrix.scale_y();
        let b = self.text_matrix.shear_y();
        let c = self.text_matrix.shear_x();
        let d = self.text_matrix.scale_x();
        if a > 0.0 && b.abs() < d && c.abs() < a && d > 0.0 {
            0.0
        } else if a < 0.0 && b.abs() < d.abs() && c.abs() < a.abs() && d < 0.0 {
            180.0
        } else if a.abs() < c.abs() && b > 0.0 && c < 0.0 && d.abs() < b {
            90.0
        } else if a.abs() < c && b < 0.0 && c > 0.0 && d.abs() < b.abs() {
            270.0
        } else {
            0.0
        }
    }

    pub(super) fn dir(&self) -> f32 {
        self.direction
    }

    fn x_rot(&self, rotation: f32) -> f32 {
        if rotation == 0.0 {
            self.text_matrix.translate_x()
        } else if rotation == 90.0 {
            self.text_matrix.translate_y()
        } else if rotation == 180.0 {
            self.page_width - self.text_matrix.translate_x()
        } else if rotation == 270.0 {
            self.page_height - self.text_matrix.translate_y()
        } else {
            0.0
        }
    }

    fn y_lower_left_rot(&self, rotation: f32) -> f32 {
        if rotation == 0.0 {
            self.text_matrix.translate_y()
        } else if rotation == 90.0 {
            self.page_width - self.text_matrix.translate_x()
        } else if rotation == 180.0 {
            self.page_height - self.text_matrix.translate_y()
        } else if rotation == 270.0 {
            self.text_matrix.translate_x()
        } else {
            0.0
        }
    }

    fn width_rot(&self, rotation: f32) -> f32 {
        if rotation == 90.0 || rotation == 270.0 {
            (self.end_y - self.text_matrix.translate_y()).abs()
        } else {
            (self.end_x - self.text_matrix.translate_x()).abs()
        }
    }

    pub(super) fn x(&self) -> f32 {
        self.x
    }

    pub(super) fn y(&self) -> f32 {
        self.y
    }

    pub(super) fn x_dir_adj(&self) -> f32 {
        self.x_rot(self.direction)
    }

    pub(super) fn y_dir_adj(&self) -> f32 {
        let dir = self.direction;
        if dir == 0.0 || dir == 180.0 {
            self.page_height - self.y_lower_left_rot(dir)
        } else {
            self.page_width - self.y_lower_left_rot(dir)
        }
    }

    pub(super) fn width(&self) -> f32 {
        self.width_rot(self.rotation as f32)
    }

    pub(super) fn width_dir_adj(&self) -> f32 {
        self.width_rot(self.direction)
    }

    pub(super) fn height(&self) -> f32 {
        self.max_height
    }

    pub(super) fn height_dir(&self) -> f32 {
        self.max_height
    }

    pub(super) fn font_size(&self) -> f32 {
        self.font_size
    }

    pub(super) fn font_size_in_pt(&self) -> f32 {
        self.font_size_pt as f32
    }

    pub(super) fn width_of_space(&self) -> f32 {
        self.width_of_space
    }

    pub(super) fn individual_widths(&self) -> &[f32] {
        &self.widths
    }

    pub(super) fn text_matrix(&self) -> &Matrix {
        &self.text_matrix
    }

    pub(super) fn page_width(&self) -> f32 {
        self.page_width
    }

    pub(super) fn page_height(&self) -> f32 {
        self.page_height
    }

    /// The length of the text in Java `char`s.
    pub(super) fn utf16_len(&self) -> usize {
        self.unicode.encode_utf16().count()
    }

    /// `getVisuallyOrderedUnicode`: the text reversed when it holds right-to-left characters
    /// and is longer than one code point.
    pub(super) fn visually_ordered_unicode(&self) -> String {
        let text = &self.unicode;
        let mut chars = text.chars();
        let first = chars.next();
        let longer = chars.next().is_some();
        let right_to_left = text
            .chars()
            .enumerate()
            .any(|(index, c)| is_right_to_left(c) && (index != 0 || longer));
        if right_to_left && first.is_some() {
            return text.chars().rev().collect();
        }
        text.clone()
    }

    /// `isDiacritic`: a single non-spacing mark, modifier symbol or modifier letter, but not
    /// the prolonged sound mark.
    pub(super) fn is_diacritic(&self) -> bool {
        let mut units = self.unicode.encode_utf16();
        let (Some(unit), None) = (units.next(), units.next()) else {
            return false;
        };
        if self.unicode == "ー" {
            return false;
        }
        let Some(c) = char::from_u32(u32::from(unit)) else {
            return false;
        };
        matches!(
            get_general_category(c),
            GeneralCategory::NonspacingMark
                | GeneralCategory::ModifierSymbol
                | GeneralCategory::ModifierLetter
        )
    }

    /// `contains`: whether this glyph covers enough of `other`.
    pub(super) fn contains(&self, other: &TextPosition) -> bool {
        let this_start = f64::from(self.x_dir_adj());
        let this_width = f64::from(self.width_dir_adj());
        let this_end = this_start + this_width;
        let other_start = f64::from(other.x_dir_adj());
        let other_end = other_start + f64::from(other.width_dir_adj());
        if other_end <= this_start || other_start >= this_end {
            return false;
        }
        let this_y = f64::from(self.y_dir_adj());
        let other_y = f64::from(other.y_dir_adj());
        if other_y + f64::from(other.height_dir()) < this_y
            || other_y > this_y + f64::from(self.height_dir())
        {
            return false;
        }
        if other_start > this_start && other_end > this_end {
            let overlap = this_end - other_start;
            return overlap / this_width > 0.15;
        }
        if other_start < this_start && other_end < this_end {
            let overlap = other_end - this_start;
            return overlap / this_width > 0.15;
        }
        true
    }

    /// `mergeDiacritic`: the diacritic joined to the character it sits on.
    pub(super) fn merge_diacritic(&mut self, diacritic: &TextPosition) {
        if diacritic.utf16_len() > 1 {
            return;
        }
        let diac_start = diacritic.x_dir_adj();
        let diac_end = diac_start + diacritic.widths[0];
        let mut current_start = self.x_dir_adj();
        let units: Vec<u16> = self.unicode.encode_utf16().collect();
        let length = units.len();
        for index in 0..length {
            if index >= self.widths.len() {
                break;
            }
            let current_end = current_start + self.widths[index];
            if diac_start < current_start && diac_end <= current_end {
                if index == 0 {
                    self.insert_diacritic(index, diacritic);
                } else {
                    let first = (diac_end - current_start) / self.widths[index];
                    let second = (current_start - diac_start) / self.widths[index - 1];
                    if first >= second {
                        self.insert_diacritic(index, diacritic);
                    } else {
                        self.insert_diacritic(index - 1, diacritic);
                    }
                }
                return;
            } else if diac_start < current_start || diac_end <= current_end || index == length - 1 {
                self.insert_diacritic(index, diacritic);
                return;
            }
            current_start += self.widths[index];
        }
    }

    /// `insertDiacritic`: the diacritic's combining form after the character at UTF-16 index
    /// `index`, and a zero width for it.
    fn insert_diacritic(&mut self, index: usize, diacritic: &TextPosition) {
        let units: Vec<u16> = self.unicode.encode_utf16().collect();
        let mut widths = Vec::with_capacity(self.widths.len() + 1);
        widths.extend_from_slice(&self.widths[..index]);
        widths.push(self.widths[index]);
        widths.push(0.0);
        widths.extend_from_slice(&self.widths[(index + 1).min(self.widths.len())..]);
        let mut end = index + 1;
        if index + 1 < units.len()
            && (0xD800..0xDC00).contains(&units[index])
            && (0xDC00..0xE000).contains(&units[index + 1])
        {
            end += 1;
        }
        let mut text: Vec<u16> = units[..end].to_vec();
        text.extend(combine_diacritic(&diacritic.unicode).encode_utf16());
        text.extend_from_slice(&units[end..]);
        self.unicode = String::from_utf16_lossy(&text);
        self.widths = widths;
    }

    /// The position with its text replaced, as `setUnicode` does.
    pub(super) fn set_unicode(&mut self, unicode: String) {
        self.unicode = unicode;
    }
}

/// Whether a character's bidirectional class is R or AL, as `Character.getDirectionality`
/// reports `DIRECTIONALITY_RIGHT_TO_LEFT` and `DIRECTIONALITY_RIGHT_TO_LEFT_ARABIC`.
pub(super) fn is_right_to_left(c: char) -> bool {
    use unicode_bidi::BidiClass;
    matches!(unicode_bidi::bidi_class(c), BidiClass::R | BidiClass::AL)
}

/// `TextPosition.combineDiacritic`: the combining form of a spacing diacritic.
fn combine_diacritic(text: &str) -> String {
    let Some(first) = text.chars().next() else {
        return String::new();
    };
    let combining = match u32::from(first) {
        0x0060 | 0x02CB => Some('\u{0300}'),
        0x0027 | 0x02B9 | 0x02CA => Some('\u{0301}'),
        0x005E | 0x02C6 => Some('\u{0302}'),
        0x007E => Some('\u{0303}'),
        0x02C9 => Some('\u{0304}'),
        0x00B0 => Some('\u{030A}'),
        0x02BA => Some('\u{030B}'),
        0x02C7 => Some('\u{030C}'),
        0x02C8 => Some('\u{030D}'),
        0x0022 => Some('\u{030E}'),
        0x02BB => Some('\u{0312}'),
        0x02BC | 0x0486 | 0x055A => Some('\u{0313}'),
        0x02BD | 0x0485 | 0x0559 => Some('\u{0314}'),
        0x02D4 => Some('\u{031D}'),
        0x02D5 => Some('\u{031E}'),
        0x02D6 => Some('\u{031F}'),
        0x02D7 => Some('\u{0320}'),
        0x02B2 => Some('\u{0321}'),
        0x02CC => Some('\u{0329}'),
        0x02B7 => Some('\u{032B}'),
        0x02CD => Some('\u{0331}'),
        0x005F => Some('\u{0332}'),
        0x204E => Some('\u{0359}'),
        _ => None,
    };
    match combining {
        Some(combining) => combining.to_string(),
        None => crate::java::trim(&text.nfkc().collect::<String>()).to_owned(),
    }
}
