//! POI's `CellFormat`: Excel number formats of several sections, or with conditions, which
//! `DataFormatter` hands to it rather than to Java's own formats. Each section is a
//! `CellFormatPart`, read by POI's regular expressions, whose formatter is one of
//! `CellNumberFormatter`, `CellDateFormatter`, `CellElapsedFormatter`, `CellTextFormatter` and
//! `CellGeneralFormatter`, each ported with its quirks. Where Java throws, the error carries
//! why, and `DataFormatter` falls back as Java's catch does.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::date_format::{is_valid_excel_date, SimpleDateFormat};
use super::decimal::{BigDecimal, Rounding};
use super::decimal_format::DecimalFormat;
use super::dtoa::java_double_to_string;
use super::fraction::max_denominator;
use super::{printf, FormatError, DOT, SPACE};
use crate::formats::java::{parse_double, round as java_round, trim as java_trim};
use crate::formats::java_time::{self, CalendarFields, TimeZone};

type Outcome<T> = Result<T, FormatError>;

fn thrown<T>(reason: impl Into<String>) -> Outcome<T> {
    Err(FormatError::Thrown(reason.into()))
}

/// What `toString` of a `Date` writes: a time zone's name, which this port does not have.
const DATE_TO_STRING: &str = "a date as Date.toString writes it, with its time zone's name";

/// `HSSFColor.HSSFColorPredefined`'s names, which a section's colour may be.
const PREDEFINED_COLORS: [&str; 48] = [
    "BLACK",
    "BROWN",
    "OLIVE_GREEN",
    "DARK_GREEN",
    "DARK_TEAL",
    "DARK_BLUE",
    "INDIGO",
    "GREY_80_PERCENT",
    "ORANGE",
    "DARK_YELLOW",
    "GREEN",
    "TEAL",
    "BLUE",
    "BLUE_GREY",
    "GREY_50_PERCENT",
    "RED",
    "LIGHT_ORANGE",
    "LIME",
    "SEA_GREEN",
    "AQUA",
    "LIGHT_BLUE",
    "VIOLET",
    "GREY_40_PERCENT",
    "PINK",
    "GOLD",
    "YELLOW",
    "BRIGHT_GREEN",
    "TURQUOISE",
    "DARK_RED",
    "SKY_BLUE",
    "PLUM",
    "GREY_25_PERCENT",
    "ROSE",
    "LIGHT_YELLOW",
    "LIGHT_GREEN",
    "LIGHT_TURQUOISE",
    "PALE_BLUE",
    "LAVENDER",
    "WHITE",
    "CORNFLOWER_BLUE",
    "LEMON_CHIFFON",
    "MAROON",
    "ORCHID",
    "CORAL",
    "ROYAL_BLUE",
    "LIGHT_CORNFLOWER_BLUE",
    "TAN",
    "AUTOMATIC",
];
/// The colour names `CellFormatPart` adds to the predefined ones.
const EXTRA_COLORS: [&str; 2] = ["magenta", "cyan"];
const INVALID_VALUE_LENGTH: usize = 255;

/// `word` as a pattern matching it in any ASCII case, as Java's `CASE_INSENSITIVE` does.
fn ascii_case_insensitive(word: &str) -> String {
    word.chars()
        .map(|c| {
            if c.is_ascii_alphabetic() {
                format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
            } else {
                regex::escape(&c.to_string())
            }
        })
        .collect()
}

fn color_names() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in PREDEFINED_COLORS {
        names.push(name.to_owned());
        if name.find('_').is_some_and(|at| at > 0) {
            names.push(name.replace('_', " "));
        }
        if name.find("_PERCENT").is_some_and(|at| at > 0) {
            names.push(name.replace("_PERCENT", "%").replace('_', " "));
        }
    }
    names.extend(EXTRA_COLORS.iter().map(|name| (*name).to_owned()));
    names
}

/// `CellFormatPart`'s pattern of one piece of a section.
fn part_pattern() -> String {
    [
        format!(r"\\{DOT}"),
        format!(r#""(?:[^\\"]|\\{DOT})*""#),
        format!(r"\[\${DOT}{{0,3}}(?:-[0-9a-fA-F]{{3,4}})?\]"),
        format!("_{DOT}"),
        format!(r"\*{DOT}"),
        "@".to_owned(),
        "[0?#][0?#,]*".to_owned(),
        "[eE][-+]".to_owned(),
        "[mM]{1,5}".to_owned(),
        "[dD]{1,4}".to_owned(),
        "[yY]{2,4}".to_owned(),
        "[hH]{1,2}".to_owned(),
        "[sS]{1,2}".to_owned(),
        "[aA][mM]?/[pP][mM]?".to_owned(),
        r"\[[hH]{1,2}\]".to_owned(),
        r"\[[mM]{1,2}\]".to_owned(),
        r"\[[sS]{1,2}\]".to_owned(),
        "[^;]".to_owned(),
    ]
    .join("|")
}

/// `CellFormatPart.FORMAT_PAT`: a colour, a condition and a locale, each optional, then the
/// pieces of the section.
fn format_pattern() -> String {
    let colors: Vec<String> = color_names()
        .iter()
        .map(|name| ascii_case_insensitive(name))
        .collect();
    let color = format!(
        r"\[(?P<color>{}|{}{SPACE}*[0-9]+)\]",
        colors.join("|"),
        ascii_case_insensitive("color")
    );
    let condition = format!(
        r"\[(?P<op>[<>=]=?|!=|<>){SPACE}*(?P<value>-?(?:[0-9]+(?:\.[0-9]*)?)|(?:\.[0-9]*)){SPACE}*\]"
    );
    format!(
        r"(?:{color})?(?:{condition})?(?:\[\$-[0-9a-fA-F]+\])?(?P<spec>(?:{})+)",
        part_pattern()
    )
}

static ONE_PART: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"{}(?:;|\z)", format_pattern())).expect("a valid pattern")
});
static WHOLE_PART: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"\A(?:{})\z", format_pattern())).expect("a valid pattern")
});
static SPECIFICATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&part_pattern()).expect("a valid pattern"));
static CURRENCY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"\[\${DOT}{{0,3}}(?:-[0-9a-fA-F]{{3,4}})?\]")).expect("a valid pattern")
});

/// What a format is applied to: a number, or the instant of a `Date`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CellValue {
    Number(f64),
    Date(i64),
}

/// The kind of a section, `CellFormatType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatType {
    General,
    Number,
    Date,
    Elapsed,
    Text,
}

impl FormatType {
    fn is_special(self, c: char) -> bool {
        self == FormatType::Date && (c == '\'' || c.is_ascii_alphabetic())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Comparison {
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Equal,
    NotEqual,
}

/// A section's condition, `CellFormatCondition`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Condition {
    comparison: Comparison,
    constant: f64,
}

impl Condition {
    fn new(operator: &str, constant: &str) -> Result<Self, String> {
        let comparison = match operator {
            "<" => Comparison::Less,
            "<=" => Comparison::LessOrEqual,
            ">" => Comparison::Greater,
            ">=" => Comparison::GreaterOrEqual,
            "=" | "==" => Comparison::Equal,
            "!=" | "<>" => Comparison::NotEqual,
            other => return Err(format!("Unknown test: {other}")),
        };
        let constant = parse_double(constant)
            .ok_or_else(|| format!("The condition's constant {constant:?} is not a double"))?;
        Ok(Self {
            comparison,
            constant,
        })
    }

    fn pass(&self, value: f64) -> bool {
        match self.comparison {
            Comparison::Less => value < self.constant,
            Comparison::LessOrEqual => value <= self.constant,
            Comparison::Greater => value > self.constant,
            Comparison::GreaterOrEqual => value >= self.constant,
            Comparison::Equal => value == self.constant,
            Comparison::NotEqual => value != self.constant,
        }
    }
}

/// A section of a format, `CellFormatPart`.
#[derive(Debug, Clone)]
struct Part {
    condition: Option<Condition>,
    formatter: Formatter,
}

#[derive(Debug, Clone)]
enum Formatter {
    General,
    Number(Box<NumberFormatter>),
    Date(DateFormatter),
    Elapsed(ElapsedFormatter),
    Text(TextFormatter),
}

impl Part {
    /// `new CellFormatPart(desc)`; the exception's message where it throws.
    fn new(desc: &str) -> Result<Self, String> {
        let captures = WHOLE_PART
            .captures(desc)
            .ok_or_else(|| format!("Unrecognized format: \"{desc}\""))?;
        let condition = match (captures.name("op"), captures.name("value")) {
            (Some(operator), Some(constant)) if !operator.as_str().is_empty() => {
                Some(Condition::new(operator.as_str(), constant.as_str())?)
            }
            _ => None,
        };
        let spec = captures.name("spec").map_or("", |m| m.as_str());
        let kind = format_type(spec)?;
        let mut fdesc = spec.to_owned();
        if let Some(currency) = CURRENCY.find(spec) {
            let part = currency.as_str();
            let replacement = if part.starts_with("[$-") {
                "$"
            } else if !part.contains('-') {
                &part[2..part.find(']').unwrap_or(part.len())]
            } else {
                &part[2..part.rfind('-').unwrap_or(2)]
            };
            fdesc = fdesc.replace(part, replacement);
        }
        let formatter = match kind {
            FormatType::General => Formatter::General,
            FormatType::Number => Formatter::Number(Box::new(NumberFormatter::new(&fdesc)?)),
            FormatType::Date => Formatter::Date(DateFormatter::new(&fdesc)?),
            FormatType::Elapsed => Formatter::Elapsed(ElapsedFormatter::new(&fdesc)?),
            FormatType::Text => Formatter::Text(TextFormatter::new(&fdesc)?),
        };
        Ok(Self {
            condition,
            formatter,
        })
    }

    fn has_condition(&self) -> bool {
        self.condition.is_some()
    }

    fn applies(&self, value: f64) -> bool {
        self.condition.is_none_or(|condition| condition.pass(value))
    }

    /// `apply`: the formatted text, or the simple form where the condition does not hold.
    fn apply(&self, value: CellValue, zone: &TimeZone) -> Outcome<String> {
        let applies = match value {
            CellValue::Number(number) => self.applies(number),
            CellValue::Date(_) => true,
        };
        if applies {
            self.formatter.format(value, zone)
        } else {
            self.formatter.simple_format(value, zone)
        }
    }
}

/// `CellFormatPart.formatType`.
fn format_type(fdesc: &str) -> Result<FormatType, String> {
    let fdesc = java_trim(fdesc);
    if fdesc.is_empty() || fdesc.eq_ignore_ascii_case("General") {
        return Ok(FormatType::General);
    }
    let mut could_be_date = false;
    let mut seen_zero = false;
    for found in SPECIFICATION.find_iter(fdesc) {
        let mut chars = found.as_str().chars();
        match chars.next() {
            Some('@') => return Ok(FormatType::Text),
            Some('d' | 'D' | 'y' | 'Y') => return Ok(FormatType::Date),
            Some('h' | 'H' | 'm' | 'M' | 's' | 'S') => could_be_date = true,
            Some('0') => seen_zero = true,
            Some('[') => {
                let second: Option<String> = chars.next().map(|c| c.to_lowercase().collect());
                return match second.as_deref() {
                    Some("h" | "m" | "s") => Ok(FormatType::Elapsed),
                    Some("$") => Ok(FormatType::Number),
                    _ => Err(format!(
                        "Unsupported [] format block '{}' in '{fdesc}'",
                        found.as_str()
                    )),
                };
            }
            Some('#' | '?') => return Ok(FormatType::Number),
            _ => {}
        }
    }
    Ok(if could_be_date {
        FormatType::Date
    } else if seen_zero {
        FormatType::Number
    } else {
        FormatType::Text
    })
}

/// `CellFormatPart.PartHandler`: what a formatter makes of each piece of its format.
trait PartHandler {
    /// The replacement of `part`, or `None` for the default one; `desc` is the description so
    /// far, which the handler may change.
    fn handle(&mut self, part: &str, desc: &mut Vec<u16>) -> Result<Option<String>, String>;

    /// `DatePartHandler.updatePositions`: the description changed length by `offset` at `pos`.
    fn update_positions(&mut self, _pos: usize, _offset: isize) {}
}

/// `quoteSpecial`.
fn quote_special(text: &str, kind: FormatType) -> String {
    let mut quoted = String::new();
    for c in text.chars() {
        if c == '\'' && kind.is_special('\'') {
            quoted.push('\0');
            continue;
        }
        let special = kind.is_special(c);
        if special {
            quoted.push('\'');
        }
        quoted.push(c);
        if special {
            quoted.push('\'');
        }
    }
    quoted
}

/// `expandChar`: the character after the first, three times.
fn expand_char(part: &str) -> Result<String, String> {
    let mut chars = part.chars();
    match (chars.next(), chars.next()) {
        (Some(_), Some(c)) => Ok(c.to_string().repeat(3)),
        _ => Err("Expected part string to have at least 2 chars".to_owned()),
    }
}

/// What `parseFormat` puts in place of a piece no handler took.
fn default_replacement(part: &str, kind: FormatType) -> Result<String, String> {
    Ok(match part.as_bytes().first() {
        Some(b'"') => {
            if part.len() < 2 {
                return Err(format!("The quoted text {part:?} has no closing quote"));
            }
            quote_special(&part[1..part.len() - 1], kind)
        }
        Some(b'\\') => quote_special(&part[1..], kind),
        Some(b'_') => " ".to_owned(),
        Some(b'*') => expand_char(part)?,
        _ => part.to_owned(),
    })
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn find_units(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&at| haystack[at..at + needle.len()] == *needle)
}

/// `CellFormatPart.parseFormat`: each piece of the format replaced as the handler says, then,
/// for dates, `''` pairs dropped and `\0` made `''`.
fn parse_format(
    fdesc: &str,
    kind: FormatType,
    handler: &mut impl PartHandler,
) -> Result<Vec<u16>, String> {
    let mut desc: Vec<u16> = Vec::new();
    let mut last = 0;
    for found in SPECIFICATION.find_iter(fdesc) {
        let part = found.as_str();
        let replacement = match handler.handle(part, &mut desc)? {
            Some(replacement) => replacement,
            None => default_replacement(part, kind)?,
        };
        desc.extend(fdesc[last..found.start()].encode_utf16());
        desc.extend(replacement.encode_utf16());
        last = found.end();
    }
    desc.extend(fdesc[last..].encode_utf16());
    if kind.is_special('\'') {
        let pair = utf16("''");
        let mut pos = 0;
        while let Some(at) = find_units(&desc, &pair, pos) {
            desc.drain(at..at + 2);
            handler.update_positions(at, -2);
            pos = at;
        }
        let mut pos = 0;
        while let Some(at) = find_units(&desc, &[0], pos) {
            desc.splice(at..at + 1, pair.iter().copied());
            handler.update_positions(at, 1);
            pos = at;
        }
    }
    Ok(desc)
}

impl Formatter {
    fn format(&self, value: CellValue, zone: &TimeZone) -> Outcome<String> {
        match self {
            Formatter::General => general_format(value),
            Formatter::Number(number) => number.format(value),
            Formatter::Date(date) => date.format(value, zone),
            Formatter::Elapsed(elapsed) => elapsed.format(value),
            Formatter::Text(text) => text.format(value),
        }
    }

    /// `simpleValue`.
    fn simple_format(&self, value: CellValue, zone: &TimeZone) -> Outcome<String> {
        match self {
            Formatter::General | Formatter::Elapsed(_) => self.format(value, zone),
            Formatter::Number(_) => {
                let CellValue::Number(number) = value else {
                    return Err(FormatError::Unported(DATE_TO_STRING));
                };
                let pattern = if number % 1.0 == 0.0 { "#" } else { "#.#" };
                NumberFormatter::new(pattern)
                    .map_err(FormatError::Thrown)?
                    .format(value)
            }
            Formatter::Date(_) => DateFormatter::new("mm/d/y")
                .map_err(FormatError::Thrown)?
                .format(value, zone),
            Formatter::Text(_) => TextFormatter::new("@")
                .map_err(FormatError::Thrown)?
                .format(value),
        }
    }
}

fn number_of(value: CellValue) -> Outcome<f64> {
    match value {
        CellValue::Number(number) => Ok(number),
        CellValue::Date(_) => thrown("A Date cannot be cast to a Number"),
    }
}

/// `CellGeneralFormatter.formatValue`.
fn general_format(value: CellValue) -> Outcome<String> {
    let CellValue::Number(value) = value else {
        return Err(FormatError::Unported(DATE_TO_STRING));
    };
    if value == 0.0 {
        return Ok("0".to_owned());
    }
    let exponent = value.abs().log10();
    let (mut text, scientific) = if exponent > 10.0 || exponent < -9.0 {
        (printf::scientific(value, 5), true)
    } else if (value as i64) as f64 != value {
        (printf::fixed(value, 1, 9, false), false)
    } else {
        return Ok(printf::fixed(value, 1, 0, false));
    };
    let mut remove_from = if scientific {
        match text.rfind('E') {
            Some(at) => at.checked_sub(1),
            None => return thrown(format!("{text:?} has no exponent to strip zeros before")),
        }
    } else {
        text.len().checked_sub(1)
    };
    while let Some(at) = remove_from {
        if text.as_bytes()[at] != b'0' {
            break;
        }
        text.remove(at);
        remove_from = at.checked_sub(1);
    }
    match remove_from {
        Some(at) if text.as_bytes()[at] == b'.' => {
            text.remove(at);
        }
        Some(_) => {}
        None => return thrown(format!("{text:?} has no digit before its zeros")),
    }
    Ok(text)
}

/// `CellTextFormatter`: the format with each `@` replaced by the value's text.
#[derive(Debug, Clone)]
struct TextFormatter {
    desc: Vec<u16>,
    positions: Vec<Option<usize>>,
}

/// Counts the `@` pieces, each made a `\0`.
struct TextPartHandler {
    places: usize,
}

impl PartHandler for TextPartHandler {
    fn handle(&mut self, part: &str, _: &mut Vec<u16>) -> Result<Option<String>, String> {
        if part == "@" {
            self.places += 1;
            return Ok(Some("\0".to_owned()));
        }
        Ok(None)
    }
}

impl TextFormatter {
    fn new(format: &str) -> Result<Self, String> {
        let mut handler = TextPartHandler { places: 0 };
        let desc = parse_format(format, FormatType::Text, &mut handler)?;
        let mut positions = Vec::with_capacity(handler.places);
        let mut end = desc.len();
        for _ in 0..handler.places {
            let found = desc[..end].iter().rposition(|&unit| unit == 0);
            positions.push(found);
            end = found.unwrap_or(0);
        }
        Ok(Self { desc, positions })
    }

    fn format(&self, value: CellValue) -> Outcome<String> {
        let CellValue::Number(number) = value else {
            return Err(FormatError::Unported(DATE_TO_STRING));
        };
        let text = utf16(&java_double_to_string(number));
        let mut output = self.desc.clone();
        for position in &self.positions {
            let Some(position) = *position else {
                return thrown("A text placeholder is out of the format");
            };
            output.splice(position..position + 1, text.iter().copied());
        }
        Ok(String::from_utf16_lossy(&output))
    }
}

/// A placeholder of a number format: its character and its position in the description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Special {
    ch: char,
    pos: usize,
}

fn is_digit_format(special: &Special) -> bool {
    matches!(special.ch, '0' | '?' | '#')
}

/// What a number format does to its output once the digits are in, `CellNumberStringMod`,
/// its placeholders by their index among all placeholders made.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Modification {
    Before {
        special: usize,
        text: Vec<u16>,
    },
    After {
        special: usize,
        text: Vec<u16>,
    },
    Replace {
        start: usize,
        start_inclusive: bool,
        end: usize,
        end_inclusive: bool,
        fill: Option<u16>,
    },
}

impl Modification {
    fn special(&self) -> usize {
        match self {
            Modification::Before { special, .. } | Modification::After { special, .. } => *special,
            Modification::Replace { start, .. } => *start,
        }
    }

    fn operation(&self) -> u8 {
        match self {
            Modification::Before { .. } => 1,
            Modification::After { .. } => 2,
            Modification::Replace { .. } => 3,
        }
    }
}

/// The modifications in a `TreeSet`: by position and operation, the first of equal ones kept.
#[derive(Default)]
struct Modifications(BTreeMap<(usize, u8), Modification>);

impl Modifications {
    fn add(&mut self, all: &[Special], modification: Modification) {
        let key = (all[modification.special()].pos, modification.operation());
        self.0.entry(key).or_insert(modification);
    }
}

/// `CellNumberPartHandler`.
struct NumberPartHandler {
    insert_sign: Option<char>,
    scale: f64,
    decimal_point: Option<usize>,
    slash: Option<usize>,
    exponent: Option<usize>,
    numerator: Option<usize>,
    all: Vec<Special>,
    specials: Vec<usize>,
    improper_fraction: bool,
}

impl NumberPartHandler {
    fn add(&mut self, special: Special) -> usize {
        self.all.push(special);
        let id = self.all.len() - 1;
        self.specials.push(id);
        id
    }

    fn previous_number(&self) -> Option<usize> {
        let mut iter = self.specials.iter().rev();
        while let Some(&id) = iter.next() {
            if is_digit_format(&self.all[id]) {
                let mut last = id;
                for &previous in iter.by_ref() {
                    let special = &self.all[previous];
                    if self.all[last].pos.abs_diff(special.pos) > 1 || !is_digit_format(special) {
                        break;
                    }
                    last = previous;
                }
                return Some(last);
            }
        }
        None
    }

    fn first_digit(&self) -> Option<usize> {
        self.specials
            .iter()
            .copied()
            .find(|&id| is_digit_format(&self.all[id]))
    }
}

impl PartHandler for NumberPartHandler {
    fn handle(&mut self, part: &str, desc: &mut Vec<u16>) -> Result<Option<String>, String> {
        let mut pos = desc.len();
        match part.chars().next() {
            Some('e' | 'E') => {
                if self.exponent.is_none() && !self.specials.is_empty() {
                    self.exponent = Some(self.add(Special { ch: '.', pos }));
                    let sign = part
                        .chars()
                        .nth(1)
                        .ok_or_else(|| format!("The exponent {part:?} has no sign"))?;
                    self.insert_sign = Some(sign);
                    return Ok(Some(part[..1].to_owned()));
                }
            }
            Some('0' | '?' | '#') => {
                if let Some(sign) = self.insert_sign.take() {
                    self.add(Special { ch: sign, pos });
                    desc.extend(sign.encode_utf16(&mut [0; 2]).iter());
                    pos += 1;
                }
                for (i, ch) in part.chars().enumerate() {
                    self.add(Special { ch, pos: pos + i });
                }
            }
            Some('.') => {
                if self.decimal_point.is_none() && !self.specials.is_empty() {
                    self.decimal_point = Some(self.add(Special { ch: '.', pos }));
                }
            }
            Some('/') => {
                if self.slash.is_none() && !self.specials.is_empty() {
                    self.numerator = self.previous_number();
                    self.improper_fraction |= self.numerator == self.first_digit();
                    self.slash = Some(self.add(Special { ch: '.', pos }));
                }
            }
            Some('%') => self.scale *= 100.0,
            _ => return Ok(None),
        }
        Ok(Some(part.to_owned()))
    }
}

/// `CellNumberFormatter`.
#[derive(Debug, Clone)]
struct NumberFormatter {
    desc: Vec<u16>,
    /// The width and precision of `printfFmt`, `None` for the empty one.
    printf: Option<(usize, usize)>,
    scale: f64,
    /// Every placeholder made, by identity.
    all: Vec<Special>,
    /// The placeholders of the description, in order.
    specials: Vec<usize>,
    slash: Option<usize>,
    exponent: Option<usize>,
    numerator: Option<usize>,
    integer_specials: Vec<usize>,
    fractional_specials: Vec<usize>,
    numerator_specials: Vec<usize>,
    denominator_specials: Vec<usize>,
    exponent_specials: Vec<usize>,
    exponent_digit_specials: Vec<usize>,
    show_grouping: bool,
    max_denominator: i32,
    /// The widths of `numeratorFmt` and `denominatorFmt`, `None` where they are null.
    fraction_widths: Option<(usize, usize)>,
    improper_fraction: bool,
    decimal_format: Option<DecimalFormat>,
}

/// `List.subList(from, to)`, which throws where the bounds are out of order or of the list.
fn sub_list(list: &[usize], from: usize, to: usize) -> Result<Vec<usize>, String> {
    list.get(from..to)
        .map(<[usize]>::to_vec)
        .ok_or_else(|| format!("subList({from}, {to}) of a list of {}", list.len()))
}

fn index_of(list: &[usize], id: Option<usize>) -> Option<usize> {
    id.and_then(|id| list.iter().position(|&x| x == id))
}

/// `specialsFor(pos, takeFirst)`: the placeholders from `pos` through the run of adjacent
/// digit placeholders that starts `take_first` after it.
fn specials_for(
    specials: &[usize],
    all: &[Special],
    pos: usize,
    take_first: usize,
) -> Result<Vec<usize>, String> {
    if pos >= specials.len() {
        return Ok(Vec::new());
    }
    let mut end = pos + take_first;
    let mut last = *specials
        .get(end)
        .ok_or_else(|| format!("No placeholder at {end} of {}", specials.len()))?;
    for &id in &specials[end + 1..] {
        if !is_digit_format(&all[id]) || all[id].pos.abs_diff(all[last].pos) > 1 {
            break;
        }
        end += 1;
        last = id;
    }
    sub_list(specials, pos, end + 1)
}

/// The index of `id` in `specials`, or the end for none, as `integerEnd` and `fractionalEnd`
/// give it; an error where `indexOf` gives -1, which `listIterator` throws for.
fn end_of(specials: &[usize], id: Option<usize>) -> Result<usize, String> {
    match id {
        None => Ok(specials.len()),
        Some(id) => specials
            .iter()
            .position(|&x| x == id)
            .ok_or_else(|| "A part's placeholder is not in the format".to_owned()),
    }
}

impl NumberFormatter {
    fn new(format: &str) -> Result<Self, String> {
        let mut handler = NumberPartHandler {
            insert_sign: None,
            scale: 1.0,
            decimal_point: None,
            slash: None,
            exponent: None,
            numerator: None,
            all: Vec::new(),
            specials: Vec::new(),
            improper_fraction: false,
        };
        let mut desc = parse_format(format, FormatType::Number, &mut handler)?;
        let NumberPartHandler {
            scale: handler_scale,
            decimal_point: handler_decimal_point,
            slash: handler_slash,
            exponent,
            numerator: handler_numerator,
            mut all,
            mut specials,
            improper_fraction,
            ..
        } = handler;
        let (slash, numerator) =
            if (handler_decimal_point.is_some() || exponent.is_some()) && handler_slash.is_some() {
                (None, None)
            } else {
                (handler_slash, handler_numerator)
            };
        let precision = match index_of(&specials, handler_decimal_point) {
            Some(index) => specials[index + 1..]
                .iter()
                .take_while(|&&id| is_digit_format(&all[id]))
                .count(),
            None => 0,
        };
        let mut fraction_width = 0;
        let decimal_point = match handler_decimal_point {
            Some(point) => {
                fraction_width = 1 + precision;
                if precision == 0 {
                    specials.retain(|&id| id != point);
                    None
                } else {
                    Some(point)
                }
            }
            None => None,
        };
        let after_integer = decimal_point.or(exponent).or(numerator);
        let after_fractional = exponent.or(numerator);
        let mut scale = handler_scale;
        let integer_end = end_of(&specials, after_integer)?;
        let fractional_end = end_of(&specials, after_fractional)?;
        let mut still_scaling = true;
        let mut show_grouping = false;
        for &id in specials[..integer_end].iter().rev() {
            if all[id].ch != ',' {
                still_scaling = false;
            } else if still_scaling {
                scale /= 1000.0;
            } else {
                show_grouping = true;
            }
        }
        if decimal_point.is_some() {
            for &id in specials[..fractional_end].iter().rev() {
                if all[id].ch != ',' {
                    break;
                }
                scale /= 1000.0;
            }
        }
        let mut removed = 0;
        let mut kept = Vec::with_capacity(specials.len());
        for &id in &specials {
            all[id].pos -= removed;
            if all[id].ch == ',' {
                removed += 1;
                if all[id].pos >= desc.len() {
                    return Err(format!(
                        "deleteCharAt({}) of {} characters",
                        all[id].pos,
                        desc.len()
                    ));
                }
                desc.remove(all[id].pos);
            } else {
                kept.push(id);
            }
        }
        let specials = kept;
        let scale = if exponent.is_none() { scale } else { 1.0 };
        let fractional_specials = if precision != 0 {
            let from = index_of(&specials, decimal_point).map_or(0, |index| index + 1);
            sub_list(&specials, from, end_of(&specials, after_fractional)?)?
        } else {
            Vec::new()
        };
        let (exponent_specials, exponent_digit_specials) = match index_of(&specials, exponent) {
            Some(position) => (
                specials_for(&specials, &all, position, 2)?,
                specials_for(&specials, &all, position + 2, 0)?,
            ),
            None => (Vec::new(), Vec::new()),
        };
        let mut numerator_specials = Vec::new();
        let mut denominator_specials = Vec::new();
        let mut max_denominator = 1;
        let mut fraction_widths = None;
        if let Some(slash) = slash {
            if let Some(position) = index_of(&specials, numerator) {
                numerator_specials = specials_for(&specials, &all, position, 0)?;
            }
            let after_slash = index_of(&specials, Some(slash)).map_or(0, |index| index + 1);
            denominator_specials = specials_for(&specials, &all, after_slash, 0)?;
            if denominator_specials.is_empty() {
                numerator_specials.clear();
            } else {
                let maximum = java_round(10f64.powi(denominator_specials.len() as i32) - 1.0);
                max_denominator = i32::try_from(maximum)
                    .map_err(|_| format!("The largest denominator {maximum} overflows an int"))?;
                fraction_widths = Some((numerator_specials.len(), denominator_specials.len()));
            }
        }
        let integer_specials = sub_list(&specials, 0, end_of(&specials, after_integer)?)?;
        let (printf, decimal_format) = if exponent.is_none() {
            let integer_width = specials
                .iter()
                .take_while(|&&id| Some(id) != after_integer)
                .filter(|&&id| is_digit_format(&all[id]))
                .count();
            let total = integer_width + fraction_width;
            ((total != 0).then_some((total, precision)), None)
        } else {
            let mut pattern = String::new();
            let mut first = true;
            if integer_specials.len() == 1 {
                pattern.push('0');
                first = false;
            } else {
                for &id in &integer_specials {
                    if is_digit_format(&all[id]) {
                        pattern.push(if first { '#' } else { '0' });
                        first = false;
                    }
                }
            }
            if !fractional_specials.is_empty() {
                pattern.push('.');
                for &id in &fractional_specials {
                    if is_digit_format(&all[id]) {
                        if !first {
                            pattern.push('0');
                        }
                        first = false;
                    }
                }
            }
            pattern.push('E');
            let exponent_digits = exponent_specials
                .get(2..)
                .ok_or_else(|| "The exponent has no digits".to_owned())?;
            for &id in exponent_digits {
                if is_digit_format(&all[id]) {
                    pattern.push('0');
                }
            }
            (
                None,
                Some(DecimalFormat::new(&pattern, Rounding::HalfEven)?),
            )
        };
        Ok(Self {
            desc,
            printf,
            scale,
            all,
            specials,
            slash,
            exponent,
            numerator,
            integer_specials,
            fractional_specials,
            numerator_specials,
            denominator_specials,
            exponent_specials,
            exponent_digit_specials,
            show_grouping,
            max_denominator,
            fraction_widths,
            improper_fraction,
            decimal_format,
        })
    }

    fn format(&self, value: CellValue) -> Outcome<String> {
        let number = number_of(value)?;
        let unscaled = BigDecimal::value_of(number)
            .ok_or_else(|| FormatError::Thrown(format!("BigDecimal.valueOf({number}) throws")))?;
        let factor = BigDecimal::value_of(self.scale).ok_or_else(|| {
            FormatError::Thrown(format!("BigDecimal.valueOf({}) throws", self.scale))
        })?;
        let mut value = unscaled.multiply(&factor).to_f64();
        let negative = value < 0.0;
        if negative {
            value = -value;
        }
        let mut fractional = 0.0;
        if self.slash.is_some() {
            if self.improper_fraction {
                fractional = value;
                value = 0.0;
            } else {
                fractional = value % 1.0;
                value = (value as i64) as f64;
            }
        }
        let mut mods = Modifications::default();
        let mut output = self.desc.clone();
        if self.exponent.is_some() {
            self.write_scientific(value, &mut output, &mut mods)?;
        } else if self.improper_fraction {
            self.write_fraction(value, &[], fractional, &mut output, &mut mods)?;
        } else {
            let result = match self.printf {
                Some((width, precision)) => utf16(&printf::fixed(value, width, precision, true)),
                None => Vec::new(),
            };
            if self.numerator.is_none() {
                self.write_fractional(&result, &mut output)?;
                self.write_integer(
                    &result,
                    &mut output,
                    Digits::Integer,
                    &mut mods,
                    self.show_grouping,
                )?;
            } else {
                self.write_fraction(value, &result, fractional, &mut output, &mut mods)?;
            }
        }
        self.apply_modifications(mods, &mut output)?;
        let mut text = String::new();
        if negative {
            text.push('-');
        }
        text.push_str(&String::from_utf16_lossy(&output));
        Ok(text)
    }

    /// The end of `formatValue`: placeholders left as `#` dropped and the modifications made.
    fn apply_modifications(&self, mods: Modifications, output: &mut Vec<u16>) -> Outcome<()> {
        let out_of_range = |what: &str, at: isize, length: usize| {
            FormatError::Thrown(format!("{what} at {at} of {length} characters"))
        };
        let comma = u16::from(b',');
        let mut deleted: HashSet<usize> = HashSet::new();
        let mut adjust: isize = 0;
        let mut changes = mods.0.into_values().peekable();
        for &id in &self.specials {
            let special = self.all[id];
            let adjusted = special.pos as isize + adjust;
            if !deleted.contains(&special.pos) {
                let unit = usize::try_from(adjusted)
                    .ok()
                    .and_then(|at| output.get(at))
                    .ok_or_else(|| out_of_range("charAt", adjusted, output.len()))?;
                if *unit == u16::from(b'#') {
                    output.remove(adjusted as usize);
                    adjust -= 1;
                    deleted.insert(special.pos);
                }
            }
            while let Some(change) = changes.next_if(|change| change.special() == id) {
                let length_before = output.len() as isize;
                self.modify(change, special, adjust, &mut deleted, output, comma)
                    .map_err(|()| {
                        FormatError::Thrown("A modification is out of the output".to_owned())
                    })?;
                adjust += output.len() as isize - length_before;
            }
        }
        Ok(())
    }

    /// Makes one modification at `special`, `adjust` the change in length so far.
    fn modify(
        &self,
        change: Modification,
        special: Special,
        adjust: isize,
        deleted: &mut HashSet<usize>,
        output: &mut Vec<u16>,
        comma: u16,
    ) -> Result<(), ()> {
        let mut position = special.pos as isize + adjust;
        let insert = |output: &mut Vec<u16>, at: isize, text: Vec<u16>| -> Result<(), ()> {
            let at = usize::try_from(at).map_err(|_| ())?;
            if at > output.len() {
                return Err(());
            }
            output.splice(at..at, text);
            Ok(())
        };
        match change {
            Modification::After { text, .. } => {
                if text == [comma] && deleted.contains(&special.pos) {
                    return Ok(());
                }
                insert(output, position + 1, text)
            }
            Modification::Before { text, .. } => insert(output, position, text),
            Modification::Replace {
                start_inclusive,
                end,
                end_inclusive,
                fill,
                ..
            } => {
                let mut delete_from = special.pos;
                if !start_inclusive {
                    delete_from += 1;
                    position += 1;
                }
                while deleted.contains(&delete_from) {
                    delete_from += 1;
                    position += 1;
                }
                let mut delete_end = self.all[end].pos;
                if end_inclusive {
                    delete_end += 1;
                }
                let modified_end = delete_end as isize + adjust;
                if position < modified_end {
                    let from = usize::try_from(position).map_err(|_| ())?;
                    match fill {
                        None => {
                            if from > output.len() {
                                return Err(());
                            }
                            let to = (modified_end as usize).min(output.len());
                            output.drain(from..to);
                        }
                        Some(fill) => {
                            let to = modified_end as usize;
                            output.get_mut(from..to).ok_or(())?.fill(fill);
                        }
                    }
                    deleted.extend(delete_from..delete_end);
                }
                Ok(())
            }
        }
    }

    fn write_scientific(
        &self,
        value: f64,
        output: &mut [u16],
        mods: &mut Modifications,
    ) -> Outcome<()> {
        let format = self.decimal_format.as_ref().ok_or_else(|| {
            FormatError::Thrown("A scientific format without its DecimalFormat".to_owned())
        })?;
        let mut result = utf16(&format.format_f64(value));
        self.write_integer(&result, output, Digits::Integer, mods, self.show_grouping)?;
        self.write_fractional(&result, output)?;
        let exponent_at = result
            .iter()
            .position(|&unit| unit == u16::from(b'E'))
            .ok_or_else(|| {
                FormatError::Thrown("The scientific result has no exponent".to_owned())
            })?;
        let sign_at = exponent_at + 1;
        let mut sign = *result.get(sign_at).ok_or_else(|| {
            FormatError::Thrown("The scientific result has no exponent digits".to_owned())
        })?;
        if sign != u16::from(b'-') {
            sign = u16::from(b'+');
            result.insert(sign_at, sign);
        }
        let sign_special = *self
            .exponent_specials
            .get(1)
            .ok_or_else(|| FormatError::Thrown("The exponent has no sign".to_owned()))?;
        let fill = (sign == u16::from(b'-') || self.all[sign_special].ch == '+').then_some(sign);
        mods.add(
            &self.all,
            Modification::Replace {
                start: sign_special,
                start_inclusive: true,
                end: sign_special,
                end_inclusive: true,
                fill,
            },
        );
        let exponent_number = &result[sign_at + 1..];
        self.write_integer(exponent_number, output, Digits::Exponent, mods, false)
    }

    fn has_char(&self, c: char, lists: &[&[usize]]) -> bool {
        lists
            .iter()
            .any(|list| list.iter().any(|&id| self.all[id].ch == c))
    }

    fn write_fraction(
        &self,
        value: f64,
        result: &[u16],
        fractional: f64,
        output: &mut [u16],
        mods: &mut Modifications,
    ) -> Outcome<()> {
        let last = |list: &[usize]| -> Outcome<usize> {
            list.last()
                .copied()
                .ok_or_else(|| FormatError::Thrown("get(-1) of an empty list".to_owned()))
        };
        if !self.improper_fraction {
            if fractional == 0.0 && !self.has_char('0', &[&self.numerator_specials]) {
                self.write_integer(result, output, Digits::Integer, mods, false)?;
                let start = last(&self.integer_specials)?;
                let end = last(&self.denominator_specials)?;
                let spaces = self.has_char(
                    '?',
                    &[
                        &self.integer_specials,
                        &self.numerator_specials,
                        &self.denominator_specials,
                    ],
                );
                mods.add(
                    &self.all,
                    Modification::Replace {
                        start,
                        start_inclusive: false,
                        end,
                        end_inclusive: true,
                        fill: spaces.then_some(u16::from(b' ')),
                    },
                );
                return Ok(());
            }
            let numerator_no_zero = !self.has_char('0', &[&self.numerator_specials]);
            let integer_no_zero = !self.has_char('0', &[&self.integer_specials]);
            let integer_only_hash = self.integer_specials.is_empty()
                || (self.integer_specials.len() == 1
                    && self.has_char('#', &[&self.integer_specials]));
            let remove_because_zero = fractional == 0.0 && (integer_only_hash || numerator_no_zero);
            let remove_because_fraction = fractional != 0.0 && integer_no_zero;
            if value == 0.0 && (remove_because_zero || remove_because_fraction) {
                let start = last(&self.integer_specials)?;
                let end = self.numerator.ok_or_else(|| {
                    FormatError::Thrown("A fraction without a numerator".to_owned())
                })?;
                let spaces =
                    self.has_char('?', &[&self.integer_specials, &self.numerator_specials]);
                mods.add(
                    &self.all,
                    Modification::Replace {
                        start,
                        start_inclusive: true,
                        end,
                        end_inclusive: false,
                        fill: spaces.then_some(u16::from(b' ')),
                    },
                );
            } else {
                self.write_integer(result, output, Digits::Integer, mods, false)?;
            }
        }
        match self.write_fraction_digits(value, fractional, output, mods) {
            Err(FormatError::Thrown(_)) | Ok(()) => Ok(()),
            Err(unported) => Err(unported),
        }
    }

    /// The numerator and denominator, inside the `try` whose exceptions `writeFraction` logs.
    fn write_fraction_digits(
        &self,
        value: f64,
        fractional: f64,
        output: &mut [u16],
        mods: &mut Modifications,
    ) -> Outcome<()> {
        let (numerator, denominator) =
            if fractional == 0.0 || (self.improper_fraction && fractional % 1.0 == 0.0) {
                (java_round(fractional) as i32, 1)
            } else {
                let fraction = max_denominator(fractional, self.max_denominator)?;
                (fraction.numerator, fraction.denominator)
            };
        let numerator = if self.improper_fraction {
            let sum = i64::from(numerator) + java_round(value * f64::from(denominator));
            i32::try_from(sum)
                .map_err(|_| FormatError::Thrown(format!("integer overflow: {sum}")))?
        } else {
            numerator
        };
        let (numerator_width, denominator_width) = self
            .fraction_widths
            .ok_or_else(|| FormatError::Thrown("A fraction without its formats".to_owned()))?;
        self.write_single_integer(numerator_width, numerator, output, Digits::Numerator, mods)?;
        self.write_single_integer(
            denominator_width,
            denominator,
            output,
            Digits::Denominator,
            mods,
        )
    }

    /// `writeSingleInteger`: `%0Nd` of `number` written into the placeholders.
    fn write_single_integer(
        &self,
        width: usize,
        number: i32,
        output: &mut [u16],
        digits: Digits,
        mods: &mut Modifications,
    ) -> Outcome<()> {
        if width == 0 {
            return thrown("%00d has a zero flag without a width");
        }
        let text = utf16(&printf::integer(i64::from(number), width));
        self.write_integer(&text, output, digits, mods, false)
    }

    fn digits(&self, digits: Digits) -> &[usize] {
        match digits {
            Digits::Integer => &self.integer_specials,
            Digits::Numerator => &self.numerator_specials,
            Digits::Denominator => &self.denominator_specials,
            Digits::Exponent => &self.exponent_digit_specials,
        }
    }

    fn write_integer(
        &self,
        result: &[u16],
        output: &mut [u16],
        digits: Digits,
        mods: &mut Modifications,
        show_grouping: bool,
    ) -> Outcome<()> {
        let index_of_unit = |unit: u8| {
            result
                .iter()
                .position(|&u| u == u16::from(unit))
                .map_or(-1, |at| at as isize)
        };
        let mut pos = index_of_unit(b'.') - 1;
        if pos < 0 {
            pos = if self.exponent.is_some() && digits == Digits::Integer {
                index_of_unit(b'E') - 1
            } else {
                result.len() as isize - 1
            };
        }
        let char_at = |at: isize| -> Outcome<u16> {
            usize::try_from(at)
                .ok()
                .and_then(|at| result.get(at).copied())
                .ok_or_else(|| {
                    FormatError::Thrown(format!("charAt({at}) of {} characters", result.len()))
                })
        };
        let zero = u16::from(b'0');
        let comma = u16::from(b',');
        let mut strip: isize = 0;
        while strip < pos {
            let unit = char_at(strip)?;
            if unit != zero && unit != comma {
                break;
            }
            strip += 1;
        }
        let mut last_output = None;
        let mut digit = 0;
        for &id in self.digits(digits).iter().rev() {
            let result_unit = if pos >= 0 { char_at(pos)? } else { zero };
            let special = self.all[id];
            let follow = show_grouping && digit > 0 && digit % 3 == 0;
            let mut zero_strip = false;
            if result_unit != zero || special.ch == '0' || special.ch == '?' || pos >= strip {
                zero_strip = special.ch == '?' && pos < strip;
                let length = output.len();
                let slot = output.get_mut(special.pos).ok_or_else(|| {
                    FormatError::Thrown(format!(
                        "setCharAt({}) of {length} characters",
                        special.pos
                    ))
                })?;
                *slot = if zero_strip {
                    u16::from(b' ')
                } else {
                    result_unit
                };
                last_output = Some(id);
            }
            if follow {
                mods.add(
                    &self.all,
                    Modification::After {
                        special: id,
                        text: vec![if zero_strip { u16::from(b' ') } else { comma }],
                    },
                );
            }
            digit += 1;
            pos -= 1;
        }
        if pos >= 0 {
            pos += 1;
            let mut extra: Vec<u16> = result
                .get(..pos as usize)
                .ok_or_else(|| FormatError::Thrown("substring out of the result".to_owned()))?
                .to_vec();
            if show_grouping {
                while pos > 0 {
                    if digit > 0 && digit % 3 == 0 {
                        extra.insert(pos as usize, comma);
                    }
                    digit += 1;
                    pos -= 1;
                }
            }
            let special = last_output.ok_or_else(|| {
                FormatError::Thrown(
                    "No digit was written to put the leading digits before".to_owned(),
                )
            })?;
            mods.add(
                &self.all,
                Modification::Before {
                    special,
                    text: extra,
                },
            );
        }
        Ok(())
    }

    fn write_fractional(&self, result: &[u16], output: &mut [u16]) -> Outcome<()> {
        if self.fractional_specials.is_empty() {
            return Ok(());
        }
        let index_of_unit = |unit: u8| {
            result
                .iter()
                .position(|&u| u == u16::from(unit))
                .map_or(-1, |at| at as isize)
        };
        let zero = u16::from(b'0');
        let mut digit = index_of_unit(b'.') + 1;
        let mut strip = if self.exponent.is_some() {
            index_of_unit(b'e') - 1
        } else {
            result.len() as isize - 1
        };
        while strip > digit && result.get(strip as usize) == Some(&zero) {
            strip -= 1;
        }
        for &id in &self.fractional_specials {
            let special = self.all[id];
            let result_unit = usize::try_from(digit)
                .ok()
                .and_then(|at| result.get(at).copied())
                .ok_or_else(|| {
                    FormatError::Thrown(format!("charAt({digit}) of {} characters", result.len()))
                })?;
            let length = output.len();
            let mut set = |unit: u16| -> Outcome<()> {
                *output.get_mut(special.pos).ok_or_else(|| {
                    FormatError::Thrown(format!(
                        "setCharAt({}) of {length} characters",
                        special.pos
                    ))
                })? = unit;
                Ok(())
            };
            if result_unit != zero || special.ch == '0' || digit < strip {
                set(result_unit)?;
            } else if special.ch == '?' {
                set(u16::from(b' '))?;
            }
            digit += 1;
        }
        Ok(())
    }
}

/// Which placeholders `writeInteger` writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Digits {
    Integer,
    Numerator,
    Denominator,
    Exponent,
}

/// `CellDateFormatter`.
#[derive(Debug, Clone)]
struct DateFormatter {
    format: SimpleDateFormat,
    am_pm_upper: bool,
    show_m: bool,
    show_am_pm: bool,
    /// The width and precision of `sFmt`, for fractions of a second.
    seconds_format: Option<(usize, usize)>,
}

/// `DatePartHandler`.
struct DatePartHandler {
    m_start: isize,
    m_len: usize,
    h_start: isize,
    h_len: usize,
    show_am_pm: bool,
    show_m: bool,
    am_pm_upper: bool,
    seconds_format: Option<(usize, usize)>,
}

impl PartHandler for DatePartHandler {
    fn handle(&mut self, part: &str, desc: &mut Vec<u16>) -> Result<Option<String>, String> {
        let pos = desc.len() as isize;
        let length = part.encode_utf16().count();
        let first = part.chars().next().unwrap_or('\0');
        Ok(match first {
            's' | 'S' => {
                if self.m_start >= 0 {
                    for i in 0..self.m_len {
                        let at = self.m_start as usize + i;
                        let length = desc.len();
                        *desc
                            .get_mut(at)
                            .ok_or_else(|| format!("setCharAt({at}) of {length} characters"))? =
                            u16::from(b'm');
                    }
                    self.m_start = -1;
                }
                Some(part.to_ascii_lowercase())
            }
            'h' | 'H' => {
                self.m_start = -1;
                self.h_start = pos;
                self.h_len = length;
                Some(part.to_ascii_lowercase())
            }
            'd' | 'D' => {
                self.m_start = -1;
                if length <= 2 {
                    Some(part.to_ascii_lowercase())
                } else {
                    Some(part.to_ascii_lowercase().replace('d', "E"))
                }
            }
            'm' | 'M' => {
                self.m_start = pos;
                self.m_len = length;
                if self.h_start >= 0 {
                    Some(part.to_ascii_lowercase())
                } else {
                    Some(part.to_ascii_uppercase())
                }
            }
            'y' | 'Y' => {
                self.m_start = -1;
                let part = match length {
                    1 => "yy",
                    3 => "yyyy",
                    _ => part,
                };
                Some(part.to_ascii_lowercase())
            }
            '0' => {
                self.m_start = -1;
                self.seconds_format = Some((length + 2, length));
                Some(part.replace('0', "S"))
            }
            'a' | 'A' | 'p' | 'P' if length > 1 => {
                self.m_start = -1;
                self.show_am_pm = true;
                self.show_m = part
                    .chars()
                    .nth(1)
                    .is_some_and(|c| c.to_lowercase().eq(['m']));
                self.am_pm_upper = self.show_m || first.is_uppercase();
                Some("a".to_owned())
            }
            _ => None,
        })
    }

    fn update_positions(&mut self, pos: usize, offset: isize) {
        let pos = pos as isize;
        if pos < self.h_start {
            self.h_start += offset;
        }
        if pos < self.m_start {
            self.m_start += offset;
        }
    }
}

impl DateFormatter {
    fn new(format: &str) -> Result<Self, String> {
        let mut handler = DatePartHandler {
            m_start: -1,
            m_len: 0,
            h_start: -1,
            h_len: 0,
            show_am_pm: false,
            show_m: false,
            am_pm_upper: false,
            seconds_format: None,
        };
        let mut desc = parse_format(format, FormatType::Date, &mut handler)?;
        if handler.h_start >= 0 && !handler.show_am_pm {
            for i in 0..handler.h_len {
                let at = handler.h_start as usize + i;
                let length = desc.len();
                *desc
                    .get_mut(at)
                    .ok_or_else(|| format!("setCharAt({at}) of {length} characters"))? =
                    u16::from(b'H');
            }
        }
        let pattern = String::from_utf16(&desc)
            .map_err(|error| format!("The date pattern is not text: {error}"))?;
        Ok(Self {
            format: SimpleDateFormat::new(&pattern)?,
            am_pm_upper: handler.am_pm_upper,
            show_m: handler.show_m,
            show_am_pm: handler.show_am_pm,
            seconds_format: handler.seconds_format,
        })
    }

    /// The instant of a number of days since the start of 1904, as `formatValue` takes it.
    fn instant_of_days(&self, days: f64, zone: &TimeZone) -> i64 {
        let epoch = CalendarFields {
            year: 1904,
            month: 0,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            millisecond: 0,
        }
        .epoch_millis(zone);
        let milliseconds = java_round(days * 86_400_000.0) as f64;
        if milliseconds == 0.0 {
            return epoch;
        }
        let seconds = if self.seconds_format.is_none() {
            java_round(milliseconds / 1000.0) as f64
        } else {
            milliseconds / 1000.0
        } as i32;
        epoch
            .wrapping_add(i64::from(seconds) * 1000)
            .wrapping_add(i64::from((milliseconds % 1000.0) as i32))
    }

    fn format(&self, value: CellValue, zone: &TimeZone) -> Outcome<String> {
        let millis = match value {
            CellValue::Date(millis) => millis,
            CellValue::Number(days) => self.instant_of_days(days, zone),
        };
        let pieces = self.format.pieces(millis, zone)?;
        let mut text = String::new();
        let mut done_am = false;
        let mut done_millis = false;
        for (piece, letter) in pieces {
            match letter {
                Some('S') => {
                    if done_millis {
                        continue;
                    }
                    let (width, precision) = self.seconds_format.ok_or_else(|| {
                        FormatError::Thrown("Milliseconds without their format".to_owned())
                    })?;
                    let fraction = millis.rem_euclid(1000) as f64 / 1000.0;
                    let formatted = printf::fixed(fraction, width, precision, true);
                    text.push_str(formatted.get(2..).unwrap_or(""));
                    done_millis = true;
                }
                Some('a') => {
                    if done_am {
                        continue;
                    }
                    if self.show_am_pm {
                        let first = piece.chars().next().unwrap_or('A');
                        if self.am_pm_upper {
                            text.push(first.to_ascii_uppercase());
                            if self.show_m {
                                text.push('M');
                            }
                        } else {
                            text.push(first.to_ascii_lowercase());
                            if self.show_m {
                                text.push('m');
                            }
                        }
                    }
                    done_am = true;
                }
                _ => text.push_str(&piece),
            }
        }
        Ok(text)
    }
}

/// A time unit of an elapsed time format.
#[derive(Debug, Clone, Copy)]
struct TimeSpec {
    kind: char,
    pos: usize,
    len: usize,
    factor: f64,
    modulo: f64,
}

impl TimeSpec {
    fn value_for(&self, elapsed: f64) -> i64 {
        let value = if self.modulo == 0.0 {
            elapsed / self.factor
        } else {
            elapsed / self.factor % self.modulo
        };
        if self.kind == '0' || self.kind == 's' {
            java_round(value)
        } else {
            value as i64
        }
    }
}

const HOUR_FACTOR: f64 = 1.0 / 24.0;
const MINUTE_FACTOR: f64 = HOUR_FACTOR / 60.0;
const SECOND_FACTOR: f64 = MINUTE_FACTOR / 60.0;

fn factor_for(kind: char, len: usize) -> Result<f64, String> {
    Ok(match kind {
        'h' => HOUR_FACTOR,
        'm' => MINUTE_FACTOR,
        's' => SECOND_FACTOR,
        '0' => SECOND_FACTOR / 10f64.powi(len as i32),
        other => return Err(format!("Unknown elapsed time spec: {other}")),
    })
}

fn modulo_for(kind: char, len: usize) -> Result<f64, String> {
    Ok(match kind {
        'h' => 24.0,
        'm' | 's' => 60.0,
        '0' => 10f64.powi(len as i32),
        other => return Err(format!("Unknown elapsed time spec: {other}")),
    })
}

/// `ElapsedPartHandler`.
struct ElapsedPartHandler {
    specs: Vec<TimeSpec>,
    topmost: Option<usize>,
}

impl ElapsedPartHandler {
    fn assign(&mut self, kind: char, pos: usize, len: usize) -> Result<usize, String> {
        self.specs.push(TimeSpec {
            kind,
            pos,
            len,
            factor: factor_for(kind, len)?,
            modulo: 0.0,
        });
        Ok(self.specs.len() - 1)
    }
}

impl PartHandler for ElapsedPartHandler {
    fn handle(&mut self, part: &str, desc: &mut Vec<u16>) -> Result<Option<String>, String> {
        let pos = desc.len();
        let length = part.encode_utf16().count();
        let mut literal = part.to_owned();
        match part.chars().next() {
            Some('[') if length >= 3 => {
                if self.topmost.is_some() {
                    return Err("Duplicate '[' times in format".to_owned());
                }
                let lower = part.to_ascii_lowercase();
                let kind = lower.chars().nth(1).unwrap_or('\0');
                let spec_len = length - 2;
                self.topmost = Some(self.assign(kind, pos, spec_len)?);
                let inner: String = lower.chars().skip(1).take(spec_len).collect();
                return Ok(Some(inner));
            }
            Some('h' | 'm' | 's' | '0') => {
                let lower = part.to_ascii_lowercase();
                let kind = lower.chars().next().unwrap_or('\0');
                self.assign(kind, pos, length)?;
                return Ok(Some(lower));
            }
            Some('\n') => return Ok(Some("%n".to_owned())),
            Some('"') => {
                if part.len() < 2 {
                    return Err(format!("The quoted text {part:?} has no closing quote"));
                }
                literal = part[1..part.len() - 1].to_owned();
            }
            Some('\\') => literal = part[1..].to_owned(),
            Some('*') => {
                if length > 1 {
                    literal = expand_char(part)?;
                }
            }
            Some('_') => return Ok(None),
            _ => {}
        }
        Ok(Some(literal.replace('%', "%%")))
    }
}

/// A piece of an elapsed time format's `printf` format.
#[derive(Debug, Clone)]
enum ElapsedPiece {
    Text(String),
    Value { spec: usize, width: usize },
}

/// `CellElapsedFormatter`.
#[derive(Debug, Clone)]
struct ElapsedFormatter {
    specs: Vec<TimeSpec>,
    pieces: Vec<ElapsedPiece>,
}

impl ElapsedFormatter {
    fn new(pattern: &str) -> Result<Self, String> {
        let mut handler = ElapsedPartHandler {
            specs: Vec::new(),
            topmost: None,
        };
        let desc = parse_format(pattern, FormatType::Elapsed, &mut handler)?;
        let ElapsedPartHandler { mut specs, topmost } = handler;
        if !specs.is_empty() {
            let topmost_kind =
                specs[topmost.ok_or("An elapsed time format without a [] time")?].kind;
            for spec in specs.iter_mut().rev() {
                if spec.kind != topmost_kind {
                    spec.modulo = modulo_for(spec.kind, spec.len)?;
                }
            }
        }
        let mut pieces = Vec::new();
        let mut cursor = 0;
        for (index, spec) in specs.iter().enumerate() {
            let literal = desc
                .get(cursor..spec.pos)
                .ok_or_else(|| "An elapsed time spec is out of order".to_owned())?;
            pieces.push(ElapsedPiece::Text(printf_literal(literal)?));
            pieces.push(ElapsedPiece::Value {
                spec: index,
                width: spec.len,
            });
            cursor = spec.pos + spec.len;
        }
        let rest = desc
            .get(cursor..)
            .ok_or_else(|| "An elapsed time spec is out of the format".to_owned())?;
        pieces.push(ElapsedPiece::Text(printf_literal(rest)?));
        Ok(Self { specs, pieces })
    }

    fn format(&self, value: CellValue) -> Outcome<String> {
        let mut elapsed = number_of(value)?;
        let mut text = String::new();
        if elapsed < 0.0 {
            text.push('-');
            elapsed = -elapsed;
        }
        for piece in &self.pieces {
            match piece {
                ElapsedPiece::Text(literal) => text.push_str(literal),
                ElapsedPiece::Value { spec, width } => {
                    text.push_str(&printf::integer(
                        self.specs[*spec].value_for(elapsed),
                        *width,
                    ));
                }
            }
        }
        Ok(text)
    }
}

/// What `java.util.Formatter` writes for the literal text of a format: `%%` as `%`, `%n` as a
/// line break; an error for any other conversion, which the handler never leaves.
fn printf_literal(units: &[u16]) -> Result<String, String> {
    let text =
        String::from_utf16(units).map_err(|error| format!("The format is not text: {error}"))?;
    let mut literal = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            literal.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => literal.push('%'),
            Some('n') => literal.push('\n'),
            other => {
                return Err(format!(
                    "An unexpected conversion %{other:?} in an elapsed time format"
                ))
            }
        }
    }
    Ok(literal)
}

/// `CellFormat`: up to four sections, for positive, negative and zero numbers and for text.
#[derive(Debug, Clone)]
pub(crate) struct CellFormat {
    parts: Vec<Option<Part>>,
}

impl CellFormat {
    /// `CellFormat.getInstance(locale, format)`; the exception's message where it throws.
    pub(crate) fn new(format: &str) -> Result<Self, String> {
        let parts: Vec<Option<Part>> = ONE_PART
            .find_iter(format)
            .map(|found| {
                let whole = found.as_str();
                Part::new(whole.strip_suffix(';').unwrap_or(whole)).ok()
            })
            .collect();
        if parts.is_empty() {
            return Err(format!("The format {format:?} has no sections"));
        }
        Ok(Self { parts })
    }

    fn part(&self, index: usize) -> Outcome<&Part> {
        self.parts
            .get(index)
            .and_then(Option::as_ref)
            .ok_or_else(|| FormatError::Thrown(format!("The format's section {index} is invalid")))
    }

    /// `apply(value)`, the value `None` for the null `getJavaDate` gives an invalid date.
    pub(crate) fn apply(&self, value: Option<CellValue>, zone: &TimeZone) -> Outcome<String> {
        match value {
            None => thrown("valueObject"),
            Some(CellValue::Number(number)) => {
                if number < 0.0 {
                    let negative_section = match self.parts.len() {
                        2 => !self.part(0)?.has_condition() && !self.part(1)?.has_condition(),
                        3 | 4 => !self.part(1)?.has_condition(),
                        _ => false,
                    };
                    if negative_section {
                        return self.part(1)?.apply(CellValue::Number(-number), zone);
                    }
                }
                self.applicable(number)?
                    .apply(CellValue::Number(number), zone)
            }
            Some(CellValue::Date(millis)) => {
                let numeric = excel_date(millis, zone);
                if !is_valid_excel_date(numeric) {
                    return thrown(format!("value {numeric} is not a valid Excel date"));
                }
                self.applicable(numeric)?
                    .apply(CellValue::Date(millis), zone)
            }
        }
    }

    /// `getApplicableFormatPart`.
    fn applicable(&self, value: f64) -> Outcome<std::borrow::Cow<'_, Part>> {
        use std::borrow::Cow;
        match self.parts.len() {
            1 => {
                let positive = self.part(0)?;
                if !positive.has_condition() || positive.applies(value) {
                    return Ok(Cow::Borrowed(positive));
                }
                Ok(Cow::Owned(
                    Part::new("General").map_err(FormatError::Thrown)?,
                ))
            }
            2 => {
                let positive = self.part(0)?;
                if (!positive.has_condition() && value >= 0.0)
                    || (positive.has_condition() && positive.applies(value))
                {
                    return Ok(Cow::Borrowed(positive));
                }
                let negative = self.part(1)?;
                if !negative.has_condition() || negative.applies(value) {
                    return Ok(Cow::Borrowed(negative));
                }
                let invalid = format!("\"{}\"", "#".repeat(INVALID_VALUE_LENGTH));
                Ok(Cow::Owned(
                    Part::new(&invalid).map_err(FormatError::Thrown)?,
                ))
            }
            _ => {
                let positive = self.part(0)?;
                if (!positive.has_condition() && value > 0.0)
                    || (positive.has_condition() && positive.applies(value))
                {
                    return Ok(Cow::Borrowed(positive));
                }
                let negative = self.part(1)?;
                if (!negative.has_condition() && value < 0.0)
                    || (negative.has_condition() && negative.applies(value))
                {
                    return Ok(Cow::Borrowed(negative));
                }
                Ok(Cow::Borrowed(self.part(2)?))
            }
        }
    }
}

/// `DateUtil.getExcelDate(Date)`: the cell value of an instant in `zone`, the 1900 system.
fn excel_date(millis: i64, zone: &TimeZone) -> f64 {
    let local = java_time::local_fields(millis, zone);
    let year = local.year;
    let last_day_of_1899 = year == 1899 && local.day_of_year == 365;
    if year < 1900 && !last_day_of_1899 {
        return -1.0;
    }
    let fraction =
        (((local.hour as f64 * 60.0 + local.minute as f64) * 60.0 + local.second as f64) * 1000.0
            + local.millisecond as f64)
            / 86_400_000.0;
    let prior = year - 1;
    let leap_days = prior / 4 - prior / 100 + prior / 400 - 460;
    let absolute_day = local.day_of_year + 365 * (year - 1900) + leap_days;
    let mut value = fraction + absolute_day as f64;
    if value >= 60.0 {
        value += 1.0;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(format: &str, value: f64) -> Outcome<String> {
        CellFormat::new(format)
            .map_err(FormatError::Thrown)?
            .apply(Some(CellValue::Number(value)), &TimeZone::fixed(0))
    }

    #[test]
    fn formats_numbers_as_cell_format_does() {
        assert_eq!(
            apply("#,##0.00;(#,##0.00);\"-\"", 1234.5).unwrap(),
            "1,234.50"
        );
        assert_eq!(
            apply("#,##0.00;(#,##0.00);\"-\"", -1234.5).unwrap(),
            "(1,234.50)"
        );
        assert_eq!(apply("#,##0.00;(#,##0.00);\"-\"", 0.0).unwrap(), "-");
        assert_eq!(apply("0%;-0%;0%", 0.256).unwrap(), "26%");
        assert_eq!(apply("[h]:mm;;", 1.5).unwrap(), "36:00");
        assert_eq!(apply("# ?/?;;", 1.25).unwrap(), "1 1/4");
        assert_eq!(apply("0.00E+00;;", 12345.0).unwrap(), "1.23E+04");
        assert_eq!(
            apply("[>100]\"big\";[<=100]\"small\"", 5.0).unwrap(),
            "small"
        );
        assert!(apply("[Red", 1.0).is_err());
    }
}
