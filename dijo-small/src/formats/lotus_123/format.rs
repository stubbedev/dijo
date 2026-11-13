//! The number formats of 1-2-3's cells, as the Excel formats that show a number the way 1-2-3
//! shows it, for the spreadsheet parsers' formatter.
//!
//! A format byte holds the protection bit, the type in bits 4 to 6 and the decimals in bits 0 to
//! 3: fixed, scientific, currency, percent and comma, or, of the special type, the kind in bits 0
//! to 3: `+/-`, General, the dates D1 to D5 and times D6 to D9, Text, Hidden, Label and the
//! default, which takes the worksheet's default format. Release 3 and later add the parentheses
//! attribute, which shows a value in parentheses, and 1-2-3 97 and later an extended format of
//! the fixed type, read as libwps reads it. `+/-` shows a bar of signs as wide as the
//! value, cut at the column, and Text and Label show a formula's source: a number of them is
//! shown as General, as LibreOffice shows it. The two unused types show a number as General.

use std::borrow::Cow;

/// The type of a format byte that shows currency.
const CURRENCY: u8 = 0x20;
/// The type of a format byte of the special formats, whose kind is in its low bits.
const SPECIAL: u8 = 0x70;
/// The special kinds of a date and of a time of day.
const DATE: u8 = 9;
const TIME: u8 = 7;
/// The attribute bit of Release 3 that shows a value in parentheses.
const PARENTHESES: u8 = 0x02;

/// Excel's serial number of 1 January 10000, the first day past the last date its formats
/// write; 1-2-3 counts days from 1900 as Excel does, its leap day of 1900 included.
const PAST_LAST_DATE: f64 = 2_958_466.0;

/// A cell's format: its format byte, and whether Release 3's parentheses attribute is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Style {
    pub(super) code: u8,
    pub(super) parentheses: bool,
}

impl Style {
    /// A format byte of Release 2, which has no attributes.
    pub(super) fn code(code: u8) -> Self {
        Style {
            code,
            parentheses: false,
        }
    }

    /// A format byte with the attributes of Release 3 and later.
    pub(super) fn with_attributes(code: u8, attributes: u8) -> Self {
        Style {
            code,
            parentheses: attributes & PARENTHESES != 0,
        }
    }

    /// A format of the formats database of 1-2-3 97 and later: the format byte and attributes,
    /// a byte that libwps reads an extended format by, and one whose high bit marks the format
    /// extended. An extended format of the fixed type is a time of day from `0x7A` on, a date
    /// from `0x63` on, and below that currency of the format byte's decimals.
    pub(super) fn of_database([code, attributes, extension, flags]: [u8; 4]) -> Self {
        let code = if flags & 0x80 != 0 && (code >> 4) & 0x07 == 0 {
            match extension {
                0x7A.. => SPECIAL | TIME,
                0x63.. => SPECIAL | DATE,
                _ => CURRENCY | (code & 0x0F),
            }
        } else {
            code
        };
        Style::with_attributes(code, attributes)
    }

    /// Whether the format is the default, which takes the worksheet's.
    pub(super) fn is_default(self) -> bool {
        self.code & 0x7F == 0x7F
    }

    /// How a number of this format is shown.
    pub(super) fn shown(self) -> Shown {
        let decimals = usize::from(self.code & 0x0F);
        let fraction = if decimals == 0 {
            String::new()
        } else {
            format!(".{}", "0".repeat(decimals))
        };
        let kind = match (self.code >> 4) & 0x07 {
            0 => Kind::Number(Cow::Owned(format!("0{fraction}"))),
            1 => Kind::Number(Cow::Owned(format!("0{fraction}E+00"))),
            2 => Kind::Number(Cow::Owned(format!(
                "\"$\"#,##0{fraction};(\"$\"#,##0{fraction})"
            ))),
            3 => Kind::Number(Cow::Owned(format!("0{fraction}%"))),
            4 => Kind::Number(Cow::Owned(format!("#,##0{fraction};(#,##0{fraction})"))),
            5 | 6 => Kind::Number(Cow::Borrowed(GENERAL)),
            _ => match self.code & 0x0F {
                2 => Kind::Date("dd-mmm-yy"),
                3 => Kind::Date("dd-mmm"),
                4 => Kind::Date("mmm-yy"),
                6 => Kind::Hidden,
                7 => Kind::Date("hh:mm:ss AM/PM"),
                8 => Kind::Date("hh:mm AM/PM"),
                9 => Kind::Date("mm/dd/yy"),
                10 => Kind::Date("mm/dd"),
                11 => Kind::Date("hh:mm:ss"),
                12 => Kind::Date("hh:mm"),
                _ => Kind::Number(Cow::Borrowed(GENERAL)),
            },
        };
        Shown {
            kind,
            parentheses: self.parentheses,
        }
    }
}

const GENERAL: &str = "General";

/// How a number of a format is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Shown {
    kind: Kind,
    parentheses: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// In this Excel number format.
    Number(Cow<'static, str>),
    /// In this Excel date or time format, a number of no date as General.
    Date(&'static str),
    /// Not at all.
    Hidden,
}

impl Shown {
    /// The General format, of a cell no format names.
    pub(super) fn general() -> Self {
        Shown {
            kind: Kind::Number(Cow::Borrowed(GENERAL)),
            parentheses: false,
        }
    }

    /// The Excel format `value` is shown in, `None` where it is hidden.
    pub(super) fn excel_format(&self, value: f64) -> Option<&str> {
        match &self.kind {
            Kind::Number(format) => Some(format),
            Kind::Date(format) if (0.0..PAST_LAST_DATE).contains(&value) => Some(format),
            Kind::Date(_) => Some(GENERAL),
            Kind::Hidden => None,
        }
    }

    /// `text`, the shown number, in the parentheses the format's attribute adds.
    pub(super) fn enclose(&self, text: String) -> String {
        if self.parentheses {
            format!("({text})")
        } else {
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format_of(code: u8, value: f64) -> Option<String> {
        Style::code(code)
            .shown()
            .excel_format(value)
            .map(str::to_owned)
    }

    #[test]
    fn maps_each_type_with_its_decimals() {
        assert_eq!(format_of(0x02, 1.0).as_deref(), Some("0.00"));
        assert_eq!(format_of(0x80, 1.0).as_deref(), Some("0"));
        assert_eq!(format_of(0x13, 1.0).as_deref(), Some("0.000E+00"));
        assert_eq!(
            format_of(0x22, 1.0).as_deref(),
            Some("\"$\"#,##0.00;(\"$\"#,##0.00)")
        );
        assert_eq!(format_of(0x31, 1.0).as_deref(), Some("0.0%"));
        assert_eq!(format_of(0x40, 1.0).as_deref(), Some("#,##0;(#,##0)"));
        assert_eq!(format_of(0x71, 1.0).as_deref(), Some("General"));
    }

    #[test]
    fn maps_the_dates_and_times_and_hides() {
        assert_eq!(format_of(0x72, 45000.0).as_deref(), Some("dd-mmm-yy"));
        assert_eq!(format_of(0xF9, 45000.0).as_deref(), Some("mm/dd/yy"));
        assert_eq!(format_of(0x77, 0.5).as_deref(), Some("hh:mm:ss AM/PM"));
        assert_eq!(format_of(0x7C, 0.5).as_deref(), Some("hh:mm"));
        assert_eq!(format_of(0x72, -3.0).as_deref(), Some("General"));
        assert_eq!(format_of(0x76, 1.0), None);
        assert!(Style::code(0xFF).is_default());
        assert!(Style::code(0x7F).is_default());
        assert!(!Style::code(0x71).is_default());
    }

    #[test]
    fn reads_the_extended_formats_of_the_database_as_libwps() {
        let database = |bytes| {
            Style::of_database(bytes)
                .shown()
                .excel_format(45000.0)
                .map(str::to_owned)
        };
        assert_eq!(
            database([0x82, 0, 0x7A, 0x80]).as_deref(),
            Some("hh:mm:ss AM/PM")
        );
        assert_eq!(database([0x02, 0, 0x63, 0x80]).as_deref(), Some("mm/dd/yy"));
        assert_eq!(
            database([0x02, 0, 0x10, 0x80]).as_deref(),
            Some("\"$\"#,##0.00;(\"$\"#,##0.00)")
        );
        assert_eq!(database([0x31, 0, 0x7A, 0x80]).as_deref(), Some("0.0%"));
        assert_eq!(database([0x02, 0, 0x7A, 0]).as_deref(), Some("0.00"));
        assert_eq!(
            Style::of_database([0x02, 0x02, 0, 0]),
            Style {
                code: 0x02,
                parentheses: true
            }
        );
    }
}
