//! The numbers of list paragraphs: the definitions Tika's `XWPFNumberingShim` reads from the
//! numbering part, and the counting and formatting of `XWPFListManager` and the
//! `AbstractListManager` it extends.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use super::W_NAMESPACE;
use crate::formats::java::parse_int;
use crate::formats::xml::{attribute, Attribute, SaxHandler};
use crate::formats::Failure;

const BULLET: &str = "\u{b7}";
/// A level not counted yet, as counts of decimal lists start at 0.
const NOT_SEEN_YET: i32 = -1;
const FIRST_SKIPPED: i32 = -2;

/// How one level of a list is numbered.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LevelTuple {
    start: i32,
    restart: i32,
    text: String,
    format: String,
    legal: bool,
}

impl LevelTuple {
    /// `new LevelTuple(lvlText)`.
    fn with_text(text: String) -> Self {
        Self {
            start: 1,
            restart: -1,
            text,
            format: "decimal".to_owned(),
            legal: false,
        }
    }

    /// The tuple standing for level `level` where none is defined: `%level.`.
    fn placeholder(level: usize) -> Self {
        Self::with_text(format!("%{level}."))
    }
}

/// The list definitions of a document.
#[derive(Debug, Default)]
pub(super) struct Numbering {
    abstract_levels: HashMap<i32, Vec<LevelTuple>>,
    abstract_of: HashMap<i32, i32>,
    overrides: HashMap<i32, HashMap<i32, LevelTuple>>,
    reading: Reading,
}

/// Where the numbering part is being read.
#[derive(Debug)]
struct Reading {
    in_abstract: bool,
    abstract_id: i32,
    in_num: bool,
    num_id: i32,
    in_level: bool,
    in_override: bool,
    level: i32,
    start: i32,
    restart: i32,
    text: Option<String>,
    format: Option<String>,
    legal: bool,
    abstract_levels: HashMap<i32, LevelTuple>,
    overrides: HashMap<i32, LevelTuple>,
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            in_abstract: false,
            abstract_id: -1,
            in_num: false,
            num_id: -1,
            in_level: false,
            in_override: false,
            level: -1,
            start: -1,
            restart: -1,
            text: None,
            format: None,
            legal: false,
            abstract_levels: HashMap::new(),
            overrides: HashMap::new(),
        }
    }
}

fn int_attribute(attributes: &[Attribute<'_>], local: &str) -> i32 {
    attribute(attributes, W_NAMESPACE, local)
        .and_then(parse_int)
        .unwrap_or(-1)
}

impl Reading {
    fn level_tuple(&self) -> LevelTuple {
        let format = self.format.clone().unwrap_or_else(|| "decimal".to_owned());
        let start = if self.start >= 0 {
            self.start
        } else if matches!(format.as_str(), "decimal" | "ordinal" | "decimalZero") {
            0
        } else {
            1
        };
        LevelTuple {
            start,
            restart: self.restart,
            text: self
                .text
                .clone()
                .unwrap_or_else(|| format!("%{}.", self.level)),
            format,
            legal: self.legal,
        }
    }
}

impl SaxHandler for Numbering {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute<'_>],
    ) -> Result<(), Failure> {
        if namespace != W_NAMESPACE {
            return Ok(());
        }
        let reading = &mut self.reading;
        match local_name {
            "abstractNum" => {
                reading.in_abstract = true;
                reading.abstract_id = int_attribute(attributes, "abstractNumId");
                reading.abstract_levels.clear();
            }
            "num" => {
                reading.in_num = true;
                reading.num_id = int_attribute(attributes, "numId");
                reading.overrides.clear();
            }
            "lvlOverride" if reading.in_num => {
                reading.in_override = true;
                reading.level = int_attribute(attributes, "ilvl");
            }
            "lvl" => {
                reading.in_level = true;
                reading.level = int_attribute(attributes, "ilvl");
                reading.start = -1;
                reading.restart = -1;
                reading.text = None;
                reading.format = None;
                reading.legal = false;
            }
            "start" if reading.in_level => reading.start = int_attribute(attributes, "val"),
            "numFmt" if reading.in_level => {
                reading.format = attribute(attributes, W_NAMESPACE, "val").map(str::to_owned);
            }
            "lvlText" if reading.in_level => {
                reading.text = attribute(attributes, W_NAMESPACE, "val").map(str::to_owned);
            }
            "lvlRestart" if reading.in_level => {
                reading.restart = int_attribute(attributes, "val");
            }
            "isLgl" if reading.in_level => reading.legal = true,
            "abstractNumId" if reading.in_num && !reading.in_level => {
                let abstract_id = int_attribute(attributes, "val");
                if reading.num_id >= 0 && abstract_id >= 0 {
                    self.abstract_of.insert(reading.num_id, abstract_id);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
        if namespace != W_NAMESPACE {
            return Ok(());
        }
        let reading = &mut self.reading;
        match local_name {
            "lvl" => {
                if reading.in_level && reading.level >= 0 {
                    let tuple = reading.level_tuple();
                    if reading.in_override && reading.in_num {
                        reading.overrides.insert(reading.level, tuple);
                    } else if reading.in_abstract {
                        reading.abstract_levels.insert(reading.level, tuple);
                    }
                }
                reading.in_level = false;
            }
            "lvlOverride" => reading.in_override = false,
            "abstractNum" => {
                if reading.in_abstract
                    && reading.abstract_id >= 0
                    && !reading.abstract_levels.is_empty()
                {
                    let highest = reading.abstract_levels.keys().copied().max().unwrap_or(-1);
                    let levels = (0..=highest)
                        .map(|level| {
                            reading
                                .abstract_levels
                                .get(&level)
                                .cloned()
                                .unwrap_or_else(|| LevelTuple::placeholder(level as usize))
                        })
                        .collect();
                    self.abstract_levels.insert(reading.abstract_id, levels);
                }
                reading.in_abstract = false;
                reading.abstract_id = -1;
            }
            "num" => {
                if reading.in_num && reading.num_id >= 0 && !reading.overrides.is_empty() {
                    self.overrides
                        .insert(reading.num_id, reading.overrides.clone());
                }
                reading.in_num = false;
                reading.num_id = -1;
            }
            _ => {}
        }
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

impl Numbering {
    /// `getOverrideLevels`: the overridden levels of a list, the others as placeholders.
    fn override_levels(&self, num_id: i32, length: usize) -> Option<Vec<LevelTuple>> {
        let overrides = self.overrides.get(&num_id).filter(|o| !o.is_empty())?;
        Some(
            (0..length)
                .map(|level| {
                    i32::try_from(level)
                        .ok()
                        .and_then(|key| overrides.get(&key))
                        .cloned()
                        .unwrap_or_else(|| LevelTuple::placeholder(level))
                })
                .collect(),
        )
    }
}

/// `XWPFListManager`: counts the numbered paragraphs of a document in order.
#[derive(Debug, Default)]
pub(super) struct ListManager {
    numbering: Option<Numbering>,
    counters: HashMap<i32, LevelCounter>,
    override_tuples: HashMap<i32, Option<Vec<LevelTuple>>>,
}

impl ListManager {
    /// The list manager of a document with these list definitions, or without any.
    pub(super) fn new(numbering: Option<Numbering>) -> Self {
        Self {
            numbering,
            counters: HashMap::new(),
            override_tuples: HashMap::new(),
        }
    }

    /// `getFormattedNumber`: the number of the next paragraph of list `num_id` at `level`, with
    /// the text around it.
    pub(super) fn formatted_number(&mut self, num_id: i32, level: i32) -> String {
        let Some(numbering) = &self.numbering else {
            return String::new();
        };
        let Ok(level) = usize::try_from(level) else {
            return String::new();
        };
        let Some(&abstract_id) = numbering.abstract_of.get(&num_id) else {
            return String::new();
        };
        if abstract_id < 0 {
            return String::new();
        }
        let counter = match self.counters.entry(abstract_id) {
            Entry::Occupied(counter) => counter.into_mut(),
            Entry::Vacant(vacant) => {
                let Some(levels) = numbering.abstract_levels.get(&abstract_id) else {
                    return String::new();
                };
                vacant.insert(LevelCounter::new(levels.clone()))
            }
        };
        let overrides = match self.override_tuples.get(&num_id) {
            Some(Some(tuples)) => Some(tuples.clone()),
            _ => numbering.override_levels(num_id, counter.levels.len()),
        };
        let formatted = counter.increment(level, overrides.as_deref());
        self.override_tuples.insert(num_id, overrides);
        formatted
    }
}

/// `ParagraphLevelCounter`: the counts of the levels of one abstract list.
#[derive(Debug)]
struct LevelCounter {
    levels: Vec<LevelTuple>,
    counts: Vec<i32>,
    last_level: Option<usize>,
}

impl LevelCounter {
    fn new(levels: Vec<LevelTuple>) -> Self {
        Self {
            levels,
            counts: Vec::new(),
            last_level: None,
        }
    }

    fn increment(&mut self, level: usize, overrides: Option<&[LevelTuple]>) -> String {
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
    use crate::formats::xml::parse_sax;

    fn numbering(xml: &str) -> Numbering {
        let mut numbering = Numbering::default();
        parse_sax(xml, "numbering", &mut numbering).unwrap();
        numbering
    }

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

    #[test]
    fn numbers_nested_lists_as_tika_does() {
        let xml = format!(
            "<w:numbering {W}><w:abstractNum w:abstractNumId=\"0\">\
             <w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%1.\"/></w:lvl>\
             <w:lvl w:ilvl=\"1\"><w:start w:val=\"1\"/><w:numFmt w:val=\"lowerLetter\"/><w:lvlText w:val=\"%1.%2)\"/></w:lvl>\
             <w:lvl w:ilvl=\"2\"><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"x\"/></w:lvl>\
             </w:abstractNum><w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num></w:numbering>"
        );
        let mut lists = ListManager::new(Some(numbering(&xml)));
        let numbers: Vec<String> = [(1, 0), (1, 1), (1, 1), (1, 2), (1, 0), (1, 1), (2, 0)]
            .iter()
            .map(|&(num, level)| lists.formatted_number(num, level))
            .collect();
        assert_eq!(
            numbers,
            ["1. ", "1.a) ", "1.b) ", "\u{b7} ", "2. ", "2.a) ", ""]
        );
    }

    #[test]
    fn formats_numbers_as_poi_does() {
        assert_eq!(letters(28).as_deref(), Some("ab"));
        assert_eq!(roman(1994).as_deref(), Some("mcmxciv"));
        assert_eq!(letters(0), None);
        assert_eq!(ordinal(12), "12nd");
    }
}
