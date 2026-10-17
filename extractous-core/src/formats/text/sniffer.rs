//! A port of Tika's `CSVSniffer` with TextAndCSVParser's default configuration: whether the
//! start of a text reads as a table, and with which delimiter.
//!
//! The sniffer reads the first 20,000 UTF-16 units of the decoded text, after the byte order
//! mark. For each delimiter, in the order Tika's delimiter map (a `HashMap`) yields them, a pass
//! over that window counts the cells of each row and the cells enclosed in double quotes; a
//! quote that opens inside a cell, or a closing quote followed by anything but spaces and then a
//! delimiter or line break, makes that delimiter's confidence -1. The confidence grows with the
//! share of rows holding the most common number of cells (above one) and with the number of rows,
//! and is 0 with fewer than three rows or a first row of one cell; enclosed cells add a bonus.
//! The most confident delimiter wins, the first in map order on a tie, and the text is a table
//! when that confidence reaches 0.5. Tika's rule that a colon yields to a tied runner-up applies
//! only to a configured colon delimiter, which the default map does not hold.

use std::collections::BTreeMap;

/// The characters (UTF-16 units) the sniffer reads: `TextAndCSVConfig`'s default mark limit.
pub(super) const MARK_LIMIT: usize = 20_000;
/// The confidence from which the text is a table.
const MIN_CONFIDENCE: f64 = 0.5;
const QUOTE: u16 = b'"' as u16;
const NEW_LINE: u16 = b'\n' as u16;
const CARRIAGE_RETURN: u16 = b'\r' as u16;
const SPACE: u16 = b' ' as u16;

/// A delimiter of TextAndCSVParser's default delimiter map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delimiter {
    Tab,
    Semicolon,
    Comma,
    Pipe,
}

impl Delimiter {
    /// The delimiters in the order Tika's sniffer tries them, which decides ties: the iteration
    /// order of `TextAndCSVConfig`'s `HashMap<Character, String>` of four entries.
    const ALL: [Delimiter; 4] = [
        Delimiter::Tab,
        Delimiter::Semicolon,
        Delimiter::Comma,
        Delimiter::Pipe,
    ];

    pub(crate) fn char(self) -> char {
        match self {
            Delimiter::Tab => '\t',
            Delimiter::Semicolon => ';',
            Delimiter::Comma => ',',
            Delimiter::Pipe => '|',
        }
    }

    /// The name Tika gives the delimiter in the content type and `csv:delimiter`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Delimiter::Tab => "tab",
            Delimiter::Semicolon => "semicolon",
            Delimiter::Comma => "comma",
            Delimiter::Pipe => "pipe",
        }
    }

    fn unit(self) -> u16 {
        self.char() as u16
    }
}

/// The delimiter the text's start reads as a table with, or `None` when it reads as plain text:
/// `CSVSniffer.getBest`.
pub(crate) fn sniff(text: &str) -> Option<Delimiter> {
    let window: Vec<u16> = text.encode_utf16().take(MARK_LIMIT).collect();
    let has_quote = window.contains(&QUOTE);
    let mut results: Vec<(Delimiter, f64)> = Delimiter::ALL
        .iter()
        .map(|&delimiter| {
            let confidence = if !has_quote && !window.contains(&delimiter.unit()) {
                0.0
            } else {
                Snifflet::new(delimiter, &window).sniff()
            };
            (delimiter, confidence)
        })
        .collect();
    results.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (delimiter, confidence) = results[0];
    (confidence >= MIN_CONFIDENCE).then_some(delimiter)
}

/// Why a pass stopped reading before the window's end: the exceptions Tika's snifflet throws.
enum Stop {
    /// It reached the last unit the mark limit allows (`HitMarkLimitException`).
    MarkLimit,
    /// The text ended right after a closing quote or a delimiter (`UnsurprisingEOF`).
    Unsurprising,
    /// The text ended inside an enclosed cell or in line breaks (`EOFException`).
    EndOfFile,
}

/// One delimiter's pass over the window (`CSVSniffer.Snifflet`).
struct Snifflet<'a> {
    delimiter: u16,
    window: &'a [u16],
    position: usize,
    row_length_counts: BTreeMap<usize, usize>,
    columns: usize,
    row_zero: bool,
    row_zero_empty: bool,
    enclosed: u32,
    parse_exception: bool,
    unquoted_length: usize,
}

impl<'a> Snifflet<'a> {
    fn new(delimiter: Delimiter, window: &'a [u16]) -> Self {
        Snifflet {
            delimiter: delimiter.unit(),
            window,
            position: 0,
            row_length_counts: BTreeMap::new(),
            columns: 0,
            row_zero: true,
            row_zero_empty: false,
            enclosed: 0,
            parse_exception: false,
            unquoted_length: 0,
        }
    }

    /// The next unit, `None` at the text's end, or the mark limit's stop at its last unit.
    fn read(&mut self) -> Result<Option<u16>, Stop> {
        if self.position >= MARK_LIMIT - 1 {
            return Err(Stop::MarkLimit);
        }
        let unit = self.window.get(self.position).copied();
        if unit.is_some() {
            self.position += 1;
        }
        Ok(unit)
    }

    fn unread(&mut self, unit: Option<u16>) {
        if unit.is_some() {
            self.position -= 1;
        }
    }

    fn sniff(mut self) -> f64 {
        let mut last = None;
        let stop = self.scan(&mut last).err();
        if self.parse_exception {
            return self.confidence();
        }
        let ended_cleanly = matches!(stop, None | Some(Stop::Unsurprising));
        if ended_cleanly && !matches!(last, Some(NEW_LINE | CARRIAGE_RETURN)) {
            self.unquoted_length = 0;
            self.end_column();
            self.end_row();
        }
        self.confidence()
    }

    /// Reads the window until it ends, a stop, or a parse exception; `last` is the last unit the
    /// main loop finished with.
    fn scan(&mut self, last: &mut Option<u16>) -> Result<(), Stop> {
        let mut unit = self.read()?;
        while let Some(c) = unit {
            if c == QUOTE {
                self.unquoted_length = 0;
                if last
                    .is_some_and(|l| l != self.delimiter && l != NEW_LINE && l != CARRIAGE_RETURN)
                {
                    self.parse_exception = true;
                    return Ok(());
                }
                if !self.consume_quoted()? {
                    self.parse_exception = true;
                    return Ok(());
                }
            } else if c == self.delimiter {
                self.unquoted_length = 0;
                self.end_column();
                self.consume_spaces()?;
            } else if c == NEW_LINE || c == CARRIAGE_RETURN {
                if self.unquoted_length > 0 {
                    self.end_column();
                }
                self.unquoted_length = 0;
                self.end_row();
                self.consume_new_lines()?;
            } else {
                self.unquoted_length += 1;
            }
            *last = Some(c);
            unit = self.read()?;
        }
        Ok(())
    }

    fn consume_spaces(&mut self) -> Result<(), Stop> {
        let mut unit = self.read()?;
        while unit == Some(SPACE) {
            unit = self.read()?;
        }
        if unit.is_none() {
            return Err(Stop::Unsurprising);
        }
        self.unread(unit);
        Ok(())
    }

    /// Reads an enclosed cell after its opening quote, with `""` as an escaped quote: whether it
    /// closes properly, followed by spaces and then a delimiter or line break.
    fn consume_quoted(&mut self) -> Result<bool, Stop> {
        let mut unit = self.read()?;
        while let Some(c) = unit {
            if c == QUOTE {
                let next = self.read()?;
                match next {
                    None => {
                        self.enclosed += 1;
                        self.end_column();
                        return Err(Stop::Unsurprising);
                    }
                    Some(QUOTE) => {}
                    Some(_) => {
                        self.enclosed += 1;
                        self.end_column();
                        self.unread(next);
                        self.consume_spaces()?;
                        let after = self.read()?;
                        let Some(after) = after else {
                            return Err(Stop::Unsurprising);
                        };
                        self.unread(Some(after));
                        return Ok(after == NEW_LINE
                            || after == CARRIAGE_RETURN
                            || after == self.delimiter);
                    }
                }
            }
            unit = self.read()?;
        }
        Err(Stop::EndOfFile)
    }

    fn consume_new_lines(&mut self) -> Result<(), Stop> {
        let mut unit = self.read()?;
        while matches!(unit, Some(NEW_LINE | CARRIAGE_RETURN)) {
            unit = self.read()?;
        }
        if unit.is_none() {
            return Err(Stop::EndOfFile);
        }
        self.unread(unit);
        Ok(())
    }

    fn end_column(&mut self) {
        self.columns += 1;
    }

    fn end_row(&mut self) {
        *self.row_length_counts.entry(self.columns).or_insert(0) += 1;
        if self.row_zero && self.columns <= 1 {
            self.row_zero_empty = true;
        }
        self.columns = 0;
        self.row_zero = false;
    }

    fn confidence(&self) -> f64 {
        if self.parse_exception {
            return -1.0;
        }
        let consistency = self.column_count_consistency();
        let base = if consistency > -1.0 { consistency } else { 0.0 };
        let bonus = if self.enclosed > 0 {
            1.0 - 1.0 / f64::from(self.enclosed).powf(0.2)
        } else {
            0.0
        };
        (base + bonus).min(1.0)
    }

    fn column_count_consistency(&self) -> f64 {
        let total_rows: usize = self.row_length_counts.values().sum();
        let most = self
            .row_length_counts
            .iter()
            .filter(|&(&columns, _)| columns > 1)
            .map(|(_, &count)| count)
            .max();
        let Some(most) = most else {
            return 0.0;
        };
        if total_rows < 3 || self.row_zero_empty {
            return 0.0;
        }
        let total = total_rows as f64;
        (1.0 - 1.0 / total.powf(0.3)) * (most as f64 / total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_a_credible_table() {
        let rows = "a,b,c\n1,2,3\n".repeat(6);
        assert_eq!(sniff(&rows), Some(Delimiter::Comma));
        assert_eq!(sniff("a,b,c\n1,2,3\n4,5,6\n"), None);
        assert_eq!(sniff(&format!("title\n{rows}")), None);
        assert_eq!(sniff(&rows.replace(',', "\t")), Some(Delimiter::Tab));
        assert_eq!(sniff(&format!("a,x\"y\n{rows}")), None);
    }

    #[test]
    fn breaks_ties_in_map_order() {
        let tied = "a,b;c|d\n1,2;3|4\n".repeat(7);
        assert_eq!(sniff(&tied), Some(Delimiter::Semicolon));
        let tied = "a,b|c\n1,2|3\n".repeat(7);
        assert_eq!(sniff(&tied), Some(Delimiter::Comma));
    }

    #[test]
    fn counts_enclosed_cells() {
        let quoted = "\"a\",\"b\"\n".repeat(4);
        assert_eq!(sniff(&quoted), Some(Delimiter::Comma));
    }
}
