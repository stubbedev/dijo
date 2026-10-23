//! Cell references as Apache POI reads them: `CellReference`, for the column of a cell, and
//! `CellAddress`, which keys comments. Where POI throws on a reference, these fail, and the
//! parse fails as Tika's does.

use std::cmp::Ordering;

use crate::formats::Failure;
use crate::java::{is_digit, parse_int};
use crate::Error;

/// The `IllegalArgumentException` POI throws on an invalid reference, which fails Tika's parse.
fn invalid(what: &str, reference: &str) -> Failure {
    Failure::Extraction(Error::ParseError(format!(
        "The cell reference {reference:?} is invalid: {what}"
    )))
}

/// `CellReference.convertColStringToIndex`: letters as a base-26 number from `A` = 0, a `$` only
/// at the start. A column past what an `int` holds is invalid, where POI's arithmetic wraps
/// around to another column (#186).
fn column_index(text: &str, reference: &str) -> Result<i32, Failure> {
    if text.is_ascii() {
        column_index_of(text.chars().map(|c| c.to_ascii_uppercase()), reference)
    } else {
        column_index_of(text.to_uppercase().chars(), reference)
    }
}

/// [`column_index`] of the upper-cased characters of the column.
fn column_index_of(upper: impl Iterator<Item = char>, reference: &str) -> Result<i32, Failure> {
    let overflow = || invalid("its column is past what an int holds", reference);
    let mut index: i32 = 0;
    for (k, c) in upper.enumerate() {
        if c == '$' {
            if k != 0 {
                return Err(invalid("a $ inside the column", reference));
            }
            continue;
        }
        let digit = i32::try_from(u32::from(c)).unwrap_or(i32::MAX) - i32::from(b'A') + 1;
        index = index
            .checked_mul(26)
            .and_then(|index| index.checked_add(digit))
            .ok_or_else(overflow)?;
    }
    index.checked_sub(1).ok_or_else(overflow)
}

/// The column `new CellReference(text).getCol()` gives, -1 for a reference without one.
pub(crate) fn reference_column(reference: &str) -> Result<i32, Failure> {
    if reference.len() >= 5 && reference[reference.len() - 5..].eq_ignore_ascii_case("#REF!") {
        return Err(invalid("it is #REF!", reference));
    }
    let cell = match reference.rfind('!') {
        Some(pling) => {
            check_sheet_name(reference, pling)?;
            &reference[pling + 1..]
        }
        None => reference,
    };
    let upper;
    let cell = if cell.is_ascii() {
        cell
    } else {
        upper = cell.to_uppercase();
        upper.as_str()
    };
    let (column, rest) = split_part(cell, |c| c.is_ascii_alphabetic());
    let (row, rest) = split_part(rest, |c| c.is_ascii_digit());
    if !rest.is_empty() {
        return Err(invalid("it is not a column and a row", reference));
    }
    if !row.is_empty() {
        let digits = row.strip_prefix('$').unwrap_or(row);
        if parse_int(digits).is_none() {
            return Err(invalid("its row is not an int", reference));
        }
    }
    match column.strip_prefix('$').unwrap_or(column) {
        "" => Ok(-1),
        letters => column_index(letters, reference),
    }
}

/// The longest prefix of `text` matching `\$?[class]+`, or nothing.
fn split_part(text: &str, class: impl Fn(char) -> bool) -> (&str, &str) {
    let dollar = usize::from(text.starts_with('$'));
    let count = text[dollar..].chars().take_while(|&c| class(c)).count();
    if count == 0 {
        return ("", text);
    }
    text.split_at(dollar + count)
}

/// `CellReference.parseSheetName`, for its failures: a sheet name with spaces must be quoted,
/// and quotes inside doubled.
fn check_sheet_name(reference: &str, pling: usize) -> Result<(), Failure> {
    let chars: Vec<char> = reference[..pling].chars().collect();
    if !reference.starts_with('\'') {
        if reference.contains(' ') {
            return Err(invalid("a sheet name with spaces is not quoted", reference));
        }
        return Ok(());
    }
    let Some(last) = chars.len().checked_sub(1) else {
        return Err(invalid("its quotes do not match", reference));
    };
    if chars[last] != '\'' {
        return Err(invalid("its quotes do not match", reference));
    }
    let mut i = 1;
    while i < last {
        if chars[i] == '\'' {
            if i + 1 < last && chars[i + 1] == '\'' {
                i += 2;
                continue;
            }
            return Err(invalid(
                "a quote in its sheet name is not doubled",
                reference,
            ));
        }
        i += 1;
    }
    Ok(())
}

/// `CellAddress`: a row and a column, both from 0, ordered by row then column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CellAddress {
    pub(crate) row: i32,
    pub(crate) column: i32,
}

impl CellAddress {
    /// `new CellAddress(text)`: the letters up to the first digit, then the row.
    pub(crate) fn parse(text: &str) -> Result<Self, Failure> {
        let split = text
            .char_indices()
            .find(|(_, c)| is_digit(*c))
            .map_or(text.len(), |(at, _)| at);
        let row =
            parse_int(&text[split..]).ok_or_else(|| invalid("its row is not an int", text))?;
        Ok(Self {
            row: row
                .checked_sub(1)
                .ok_or_else(|| invalid("its row is before the first", text))?,
            column: column_index(&text[..split], text)?,
        })
    }

    /// By row, then by column. POI's `compareTo` subtracts them as `int`s, which wraps (#95).
    pub(crate) fn compare(&self, other: &Self) -> Ordering {
        (self.row, self.column).cmp(&(other.row, other.column))
    }

    /// `formatAsString`: the column's letters and the row from 1.
    pub(crate) fn format(&self) -> String {
        format!("{}{}", column_letters(self.column), i64::from(self.row) + 1)
    }
}

/// `CellReference.convertNumToColString`.
fn column_letters(column: i32) -> String {
    let mut excel = i64::from(column) + 1;
    let mut letters = Vec::new();
    while excel > 0 {
        let mut digit = excel % 26;
        if digit == 0 {
            digit = 26;
        }
        letters.push(char::from(b'A' + u8::try_from(digit - 1).unwrap_or(0)));
        excel = (excel - digit) / 26;
    }
    letters.iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A column past what an `int` holds is invalid, where POI's arithmetic wraps it around to
    /// a column of the sheet (#186).
    #[test]
    fn refuses_a_column_past_an_int() {
        assert_eq!(reference_column("XFD1").unwrap(), 16383);
        let wrapped = "BAAAAAAAAAAAAAAAAAAAAA1";
        assert!(reference_column(wrapped).is_err());
        assert!(CellAddress::parse(wrapped).is_err());
    }

    #[test]
    fn reads_columns_as_poi_does() {
        assert_eq!(reference_column("A1").unwrap(), 0);
        assert_eq!(reference_column("$AB$12").unwrap(), 27);
        assert_eq!(reference_column("Sheet1!C3").unwrap(), 2);
        assert_eq!(reference_column("'My Sheet'!D4").unwrap(), 3);
        assert_eq!(reference_column("12").unwrap(), -1);
        assert_eq!(reference_column("").unwrap(), -1);
        assert!(reference_column("1A").is_err());
        assert!(reference_column("My Sheet!A1").is_err());
        assert!(reference_column("A99999999999").is_err());
    }

    #[test]
    fn reads_and_writes_addresses_as_poi_does() {
        let address = CellAddress::parse("ab12").unwrap();
        assert_eq!((address.row, address.column), (11, 27));
        assert_eq!(address.format(), "AB12");
        assert!(CellAddress::parse("A").is_err());
        assert!(CellAddress::parse("A1x").is_err());
    }

    /// The column read without upper-casing an ASCII reference is the one read from it upper
    /// cased, as `String.toUpperCase` does, which turns some letters outside ASCII into ASCII.
    #[test]
    fn reads_columns_as_from_the_upper_cased_reference() {
        fn upper_cased_column(reference: &str) -> Result<i32, Failure> {
            if reference.len() >= 5
                && reference[reference.len() - 5..].eq_ignore_ascii_case("#REF!")
            {
                return Err(invalid("it is #REF!", reference));
            }
            let cell = match reference.rfind('!') {
                Some(pling) => {
                    check_sheet_name(reference, pling)?;
                    &reference[pling + 1..]
                }
                None => reference,
            };
            let cell = cell.to_uppercase();
            let (column, rest) = split_part(&cell, |c| c.is_ascii_alphabetic());
            let (row, rest) = split_part(rest, |c| c.is_ascii_digit());
            if !rest.is_empty() {
                return Err(invalid("it is not a column and a row", reference));
            }
            if !row.is_empty() && parse_int(row.strip_prefix('$').unwrap_or(row)).is_none() {
                return Err(invalid("its row is not an int", reference));
            }
            match column.strip_prefix('$').unwrap_or(column) {
                "" => Ok(-1),
                letters => column_index_of(letters.to_uppercase().chars(), reference),
            }
        }
        let references = [
            "A1",
            "a1",
            "xfd1048576",
            "$ab$12",
            "$Ab12",
            "aB$3",
            "Sheet1!c3",
            "'My Sheet'!d4",
            "'It''s'!e5",
            "'Bad'Quote'!A1",
            "My Sheet!A1",
            "sheet!#ref!",
            "12",
            "$12",
            "",
            "$",
            "a",
            "$a$",
            "1a",
            "a1b",
            "a$$1",
            "a99999999999",
            "baaaaaaaaaaaaaaaaaaaaa1",
            "\u{131}1",
            "\u{17f}2",
            "\u{df}3",
            "\u{fb00}4",
            "\u{e9}5",
            "a\u{131}6",
            "\u{212a}7",
            "!a1",
        ];
        for reference in references {
            assert_eq!(
                format!("{:?}", reference_column(reference)),
                format!("{:?}", upper_cased_column(reference)),
                "{reference:?}"
            );
        }
        for text in ["ab12", "AB12", "\u{131}1", "a-1", "-1", "1", "\u{df}2"] {
            assert_eq!(
                format!("{:?}", CellAddress::parse(text)),
                format!("{:?}", {
                    let split = text
                        .find(|c: char| c.is_ascii_digit())
                        .unwrap_or(text.len());
                    column_index_of(text[..split].to_uppercase().chars(), text).map(|column| {
                        CellAddress {
                            row: parse_int(&text[split..]).unwrap() - 1,
                            column,
                        }
                    })
                }),
                "{text:?}"
            );
        }
    }
}
