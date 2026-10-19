//! Tika's `AbstractListManager`: how each level of a list is numbered, and the counting and
//! formatting of the paragraphs of one list in order, which the Word extractors of both formats
//! share.

use crate::java::parse_int;

const BULLET: &str = "\u{b7}";
/// A level not counted yet, as counts of decimal lists start at 0.
const NOT_SEEN_YET: i32 = -1;
const FIRST_SKIPPED: i32 = -2;

/// How one level of a list is numbered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelTuple {
    pub start: i32,
    pub restart: i32,
    pub text: String,
    pub format: String,
    pub legal: bool,
}

impl LevelTuple {
    /// `new LevelTuple(lvlText)`.
    pub(crate) fn with_text(text: String) -> Self {
        Self {
            start: 1,
            restart: -1,
            text,
            format: "decimal".to_owned(),
            legal: false,
        }
    }

    /// The tuple standing for level `level` where none is defined: `%level.`.
    pub fn placeholder(level: usize) -> Self {
        Self::with_text(format!("%{level}."))
    }
}

/// `ParagraphLevelCounter`: the counts of the levels of one abstract list.
#[derive(Debug)]
pub struct LevelCounter {
    levels: Vec<LevelTuple>,
    counts: Vec<i32>,
    last_level: Option<usize>,
}

impl LevelCounter {
    pub fn new(levels: Vec<LevelTuple>) -> Self {
        Self {
            levels,
            counts: Vec::new(),
            last_level: None,
        }
    }

    /// The number of levels the list defines.
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    /// `incrementLevel`: the paragraph at `level` counted, and its number formatted.
    pub fn increment(&mut self, level: usize, overrides: Option<&[LevelTuple]>) -> String {
        let first = self.last_level.map_or(0, |last| last + 1);
        for skipped in first..level {
            if skipped >= self.counts.len() {
                let start = self.start(skipped, overrides);
                self.counts.push(start);
            } else if self.counts[skipped] == NOT_SEEN_YET {
                self.counts[skipped] = self.start(skipped, overrides);
            }
        }
        if level < self.counts.len() {
            self.reset_after(level, overrides);
            let count = self.counts[level];
            self.counts[level] = if count == NOT_SEEN_YET {
                self.start(level, overrides)
            } else {
                count + 1
            };
        } else {
            let start = self.start(level, overrides);
            self.counts.push(start);
        }
        self.last_level = Some(level);
        self.format(level, overrides)
    }

    fn format(&self, level: usize, overrides: Option<&[LevelTuple]>) -> String {
        let Some(tuple) = self.levels.get(level) else {
            return String::new();
        };
        let legal = overrides.map_or(tuple.legal, |overrides| overrides[level].legal);
        if self.number_format(Some(level), legal, overrides) == "bullet" {
            return format!("{BULLET} ");
        }
        let text = overrides.map_or(&tuple.text, |overrides| &overrides[level].text);
        let mut formatted = String::new();
        let mut rest = text.as_str();
        while let Some(index) = find_placeholder(rest) {
            formatted.push_str(&rest[..index]);
            let digits_end = rest[index + 1..]
                .find(|c: char| !c.is_ascii_digit())
                .map_or(rest.len(), |end| index + 1 + end);
            let referenced = parse_int(&rest[index + 1..digits_end]).unwrap_or(-1);
            formatted.push_str(&self.format_number(i64::from(referenced) - 1, legal, overrides));
            rest = &rest[digits_end..];
        }
        formatted.push_str(rest);
        if !formatted.is_empty() {
            formatted.push(' ');
        }
        formatted
    }

    fn format_number(&self, level: i64, legal: bool, overrides: Option<&[LevelTuple]>) -> String {
        let level = usize::try_from(level).ok();
        let format = self.number_format(level, legal, overrides);
        let count = level
            .and_then(|level| self.counts.get(level))
            .copied()
            .unwrap_or(1);
        let count = if count < 0 { 1 } else { count };
        match format {
            "lowerLetter" => letters(count).unwrap_or_default(),
            "lowerRoman" => roman(count).unwrap_or_default(),
            "upperLetter" => letters(count).unwrap_or_default().to_uppercase(),
            "upperRoman" => roman(count).unwrap_or_default().to_uppercase(),
            "bullet" | "none" => String::new(),
            "ordinal" => ordinal(count),
            "decimalZero" => format!("0{count}"),
            _ => count.to_string(),
        }
    }

    fn number_format<'s>(
        &'s self,
        level: Option<usize>,
        legal: bool,
        overrides: Option<&'s [LevelTuple]>,
    ) -> &'s str {
        let Some(level) = level.filter(|&level| level < self.levels.len()) else {
            return "decimal";
        };
        if legal {
            return "decimal";
        }
        overrides.map_or(&self.levels[level].format, |overrides| {
            &overrides[level].format
        })
    }

    fn reset_after(&mut self, level: usize, overrides: Option<&[LevelTuple]>) {
        for later in level + 1..self.counts.len() {
            let count = self.counts[later];
            if count == NOT_SEEN_YET || count == FIRST_SKIPPED {
                continue;
            }
            if later < self.levels.len() {
                let restart = match overrides {
                    Some(overrides) if overrides[later].restart >= 0 => overrides[later].restart,
                    _ => self.levels[later].restart,
                };
                if restart == 0 {
                    return;
                }
                if restart == -1 || usize::try_from(restart).is_ok_and(|restart| level < restart) {
                    self.counts[later] = NOT_SEEN_YET;
                }
            } else {
                self.counts[later] = NOT_SEEN_YET;
            }
        }
    }

    fn start(&self, level: usize, overrides: Option<&[LevelTuple]>) -> i32 {
        if level >= self.levels.len() {
            return 1;
        }
        match overrides {
            Some(overrides) if overrides[level].start >= 0 => overrides[level].start,
            _ => self.levels[level].start,
        }
    }
}

/// The index of the next `%` followed by an ASCII digit.
fn find_placeholder(text: &str) -> Option<usize> {
    text.match_indices('%')
        .map(|(index, _)| index)
        .find(|&index| text[index + 1..].starts_with(|c: char| c.is_ascii_digit()))
}

/// `NumberFormatter`'s letters: 1 is a, 27 is aa.
fn letters(number: i32) -> Option<String> {
    if number <= 0 {
        return None;
    }
    let mut number = number;
    let mut letters = Vec::new();
    while number > 0 {
        number -= 1;
        let remainder = number % 26;
        letters.push(char::from(b'a' + u8::try_from(remainder).ok()?));
        number = (number - remainder) / 26;
    }
    Some(letters.iter().rev().collect())
}

/// `NumberFormatter`'s lowercase Roman numerals.
fn roman(number: i32) -> Option<String> {
    const NUMERALS: [(&str, i32); 13] = [
        ("m", 1000),
        ("cm", 900),
        ("d", 500),
        ("cd", 400),
        ("c", 100),
        ("xc", 90),
        ("l", 50),
        ("xl", 40),
        ("x", 10),
        ("ix", 9),
        ("v", 5),
        ("iv", 4),
        ("i", 1),
    ];
    if number <= 0 {
        return None;
    }
    let mut number = number;
    let mut roman = String::new();
    for (letters, value) in NUMERALS {
        while number >= value {
            number -= value;
            roman.push_str(letters);
        }
    }
    Some(roman)
}

/// `ordinalize`, which only knows English.
fn ordinal(count: i32) -> String {
    let suffix = match count.to_string().chars().last() {
        Some('1') => "st",
        Some('2') => "nd",
        Some('3') => "rd",
        _ => "th",
    };
    format!("{count}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers_as_poi_does() {
        assert_eq!(letters(28).as_deref(), Some("ab"));
        assert_eq!(roman(1994).as_deref(), Some("mcmxciv"));
        assert_eq!(letters(0), None);
        assert_eq!(ordinal(12), "12nd");
    }
}
