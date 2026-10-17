//! `java.text.SimpleDateFormat` for formatting, with the `en_US` `DateFormatSymbols`: a pattern
//! compiled as `SimpleDateFormat.compile` compiles it, failing where it throws, and the fields of
//! an instant in a time zone written as its `format` writes them.

use super::time::{self, LocalFields, TimeZone};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const SHORT_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const SHORT_WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const PATTERN_LETTERS: &str = "GyMdkHmsSEDFwWahKzZYuXL";

/// What the port does not write that Java does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unported(pub(crate) &'static str);

/// A field of a compiled `SimpleDateFormat` pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Field {
    Literal(String),
    Pattern(char, usize),
}

/// `java.text.SimpleDateFormat` for formatting, with the `en_US` `DateFormatSymbols`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleDateFormat {
    fields: Vec<Field>,
}

impl SimpleDateFormat {
    /// `SimpleDateFormat.compile`: an error where the pattern throws.
    pub fn new(pattern: &str) -> Result<Self, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut fields: Vec<Field> = Vec::new();
        let mut literal = String::new();
        let mut in_quote = false;
        let mut last: Option<(char, usize)> = None;
        let flush_pattern = |fields: &mut Vec<Field>, last: &mut Option<(char, usize)>| {
            if let Some((letter, count)) = last.take() {
                fields.push(Field::Pattern(letter, count));
            }
        };
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    i += 1;
                    flush_pattern(&mut fields, &mut last);
                    literal.push('\'');
                } else {
                    flush_pattern(&mut fields, &mut last);
                    in_quote = !in_quote;
                }
                i += 1;
                continue;
            }
            if in_quote || !c.is_ascii_alphabetic() {
                flush_pattern(&mut fields, &mut last);
                literal.push(c);
                i += 1;
                continue;
            }
            if !PATTERN_LETTERS.contains(c) {
                return Err(format!("Illegal pattern character '{c}'"));
            }
            if !literal.is_empty() {
                fields.push(Field::Literal(std::mem::take(&mut literal)));
            }
            match &mut last {
                Some((letter, count)) if *letter == c => *count += 1,
                _ => {
                    flush_pattern(&mut fields, &mut last);
                    last = Some((c, 1));
                }
            }
            i += 1;
        }
        if in_quote {
            return Err("Unterminated quote".to_owned());
        }
        flush_pattern(&mut fields, &mut last);
        if !literal.is_empty() {
            fields.push(Field::Literal(literal));
        }
        Ok(Self { fields })
    }

    /// The instant `epoch_millis` as the pattern writes it in `zone`; an error for a time
    /// zone name, which this port does not write.
    pub fn format(&self, epoch_millis: i64, zone: &TimeZone) -> Result<String, Unported> {
        Ok(self
            .pieces(epoch_millis, zone)?
            .into_iter()
            .map(|(piece, _)| piece)
            .collect())
    }

    /// `formatToCharacterIterator`: each piece of the text with the pattern letter that wrote
    /// it, or `None` for a literal.
    pub(crate) fn pieces(
        &self,
        epoch_millis: i64,
        zone: &TimeZone,
    ) -> Result<Vec<(String, Option<char>)>, Unported> {
        let local = time::local_fields(epoch_millis, zone);
        self.fields
            .iter()
            .map(|field| match field {
                Field::Literal(literal) => Ok((literal.clone(), None)),
                Field::Pattern(letter, count) => {
                    format_field(*letter, *count, &local).map(|piece| (piece, Some(*letter)))
                }
            })
            .collect()
    }
}

/// `zeroPaddingNumber`: at least `min` digits, at most the last `max`.
fn zero_padding(value: i64, min: usize, max: usize) -> String {
    let digits = value.unsigned_abs().to_string();
    let digits = if digits.len() > max {
        &digits[digits.len() - max..]
    } else {
        &digits[..]
    };
    let mut text = String::new();
    if value < 0 {
        text.push('-');
    }
    for _ in digits.len()..min {
        text.push('0');
    }
    text.push_str(digits);
    text
}

fn format_field(letter: char, count: usize, local: &LocalFields) -> Result<String, Unported> {
    let number = |value: i64| zero_padding(value, count, usize::MAX);
    let piece = match letter {
        'G' => (if local.before_christ { "BC" } else { "AD" }).to_owned(),
        'y' | 'Y' => {
            let value = if letter == 'y' {
                local.year
            } else {
                local.week_year
            };
            if count == 2 {
                zero_padding(value, 2, 2)
            } else {
                number(value)
            }
        }
        'M' | 'L' => match count {
            4.. => MONTHS[local.month as usize].to_owned(),
            3 => SHORT_MONTHS[local.month as usize].to_owned(),
            _ => number(local.month + 1),
        },
        'd' => number(local.day),
        'k' => number(if local.hour == 0 { 24 } else { local.hour }),
        'H' => number(local.hour),
        'm' => number(local.minute),
        's' => number(local.second),
        'S' => number(local.millisecond),
        'E' => {
            let index = (local.day_of_week - 1) as usize;
            if count >= 4 {
                WEEKDAYS[index].to_owned()
            } else {
                SHORT_WEEKDAYS[index].to_owned()
            }
        }
        'u' => number(if local.day_of_week == 1 {
            7
        } else {
            local.day_of_week - 1
        }),
        'D' => number(local.day_of_year),
        'F' => number(local.day_of_week_in_month),
        'w' => number(local.week_of_year),
        'W' => number(local.week_of_month),
        'a' => (if local.hour >= 12 { "PM" } else { "AM" }).to_owned(),
        'h' => number(if local.hour % 12 == 0 {
            12
        } else {
            local.hour % 12
        }),
        'K' => number(local.hour % 12),
        'Z' => {
            let minutes = local.offset / 60_000;
            let sign = if minutes < 0 { '-' } else { '+' };
            let minutes = minutes.abs();
            format!("{sign}{:02}{:02}", minutes / 60, minutes % 60)
        }
        'X' => {
            let minutes = local.offset / 60_000;
            if minutes == 0 {
                "Z".to_owned()
            } else {
                let sign = if minutes < 0 { '-' } else { '+' };
                let minutes = minutes.abs();
                match count {
                    1 => format!("{sign}{:02}", minutes / 60),
                    2 => format!("{sign}{:02}{:02}", minutes / 60, minutes % 60),
                    _ => format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60),
                }
            }
        }
        _ => return Err(Unported("a time zone's name, pattern letter z")),
    };
    Ok(piece)
}
