//! The numbers of list paragraphs: the definitions Tika's `XWPFNumberingShim` reads from the
//! numbering part, and the counting and formatting of `XWPFListManager` and the
//! `AbstractListManager` it extends.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use super::W_NAMESPACE;
use crate::formats::list_numbering::{LevelCounter, LevelTuple};
use crate::formats::xml::{attribute, Attribute, SaxHandler};
use crate::formats::Failure;
use crate::java::parse_int;

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

fn int_attribute(attributes: &[Attribute], local: &str) -> i32 {
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
        attributes: &[Attribute],
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
pub(crate) struct ListManager {
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
            _ => numbering.override_levels(num_id, counter.level_count()),
        };
        let formatted = counter.increment(level, overrides.as_deref());
        self.override_tuples.insert(num_id, overrides);
        formatted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::xml::parse_sax;

    fn numbering(xml: &str) -> Numbering {
        let mut numbering = Numbering::default();
        parse_sax(xml.as_bytes(), "numbering", &mut numbering).unwrap();
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
}
