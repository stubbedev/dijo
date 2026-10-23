//! The values of a SAS7BDAT file's cells as parso reads them, and as its `DataWriterUtil` writes
//! them: numbers, dates by their SAS formats, times, percentages and text.
//!
//! Dates and percentages are written in one fixed locale, US English: English month and day
//! names, `.` the decimal point and `%` the percent sign, whatever the machine's locale, where
//! parso formats them in the JVM's default locale (#155).

use std::collections::hash_map::{Entry, HashMap};

use crate::formats::thrown::{Exception, Thrown};
use crate::java;
use crate::java::decimal::{BigDecimal, Rounding};
use crate::java::decimal_format::DecimalFormat;
use crate::java::simple_date_format::SimpleDateFormat;
use crate::java::time::TimeZone;

/// `SasTemporalConstants.SAS_VS_EPOCH_DIFF_DAYS`: the days from 1960, SAS's epoch, to 1970.
const SAS_VS_EPOCH_DIFF_DAYS: i64 = 365 * 10 + 3;
const SECONDS_IN_DAY: f64 = 86_400.0;
const SAS_VS_EPOCH_DIFF_SECONDS: f64 = (SAS_VS_EPOCH_DIFF_DAYS * 86_400) as f64;
/// The SAS days of 29 February 4000 and 8000, which SAS does not count as leap days.
const SAS_DAYS_29FEB4000: f64 = 745_154.0;
const SAS_DAYS_29FEB8000: f64 = 2_206_123.0;

/// `EPSILON`: how far a number may lie from a whole one to be read as one.
const EPSILON: f64 = 1e-14;
/// `NAN_EPSILON`: below it a positive number is read as missing.
const NAN_EPSILON: f64 = 1e-300;

/// The digits `DataWriterUtil` keeps of a long double, and the length from which it rounds.
const ACCURACY: i32 = 15;
const ROUNDING_LENGTH: usize = 13;

/// The SAS formats parso reads as dates, `SasDateFormat`'s names.
const DATE_FORMATS: [&str; 51] = [
    "DATE", "DAY", "DDMMYY", "DDMMYYB", "DDMMYYC", "DDMMYYD", "DDMMYYN", "DDMMYYP", "DDMMYYS",
    "MMDDYY", "MMDDYYB", "MMDDYYC", "MMDDYYD", "MMDDYYN", "MMDDYYP", "MMDDYYS", "YYMMDD",
    "YYMMDDB", "YYMMDDC", "YYMMDDD", "YYMMDDN", "YYMMDDP", "YYMMDDS", "MMYY", "MMYYC", "MMYYD",
    "MMYYN", "MMYYP", "MMYYS", "YYMM", "YYMMC", "YYMMD", "YYMMN", "YYMMP", "YYMMS", "JULIAN",
    "JULDAY", "MONTH", "YEAR", "MONYY", "YYMON", "B8601DA", "E8601DA", "MONNAME", "WEEKDATE",
    "WEEKDATX", "WEEKDAY", "DOWNAME", "WORDDATE", "WORDDATX", "QTR",
];

/// The SAS formats parso reads as times, `SasTimeFormat`'s names, which are the formats
/// `DataWriterUtil` writes as times, `DateTimeConstants.TIME_FORMAT_STRINGS`.
const TIME_FORMATS: [&str; 7] = [
    "TIME", "MMSS", "HHMM", "HOUR", "TIMEAMPM", "E8601LZ", "E8601TM",
];

/// The SAS formats parso reads as dates with times, `SasDateTimeFormat`'s names.
const DATE_TIME_FORMATS: [&str; 17] = [
    "DATETIME", "B8601DN", "B8601DT", "B8601DX", "B8601DZ", "B8601LX", "E8601DN", "E8601DT",
    "E8601DX", "E8601DZ", "E8601LX", "DATEAMPM", "DTDATE", "DTMONYY", "DTWKDATX", "DTYEAR", "TOD",
];

/// `DateTimeConstants.DATE_FORMAT_STRINGS` then `DATETIME_FORMAT_STRINGS`: the pattern
/// `DataWriterUtil` writes a date of a format with.
fn date_pattern(format: &str) -> Option<&'static str> {
    Some(match format {
        "B8601DA" => "yyyyMMdd",
        "E8601DA" => "yyyy-MM-dd",
        "DATE" => "ddMMMyyyy",
        "DAY" => "dd",
        "DDMMYY" => "dd/MM/yyyy",
        "DDMMYYB" => "dd MM yyyy",
        "DDMMYYC" => "dd:MM:yyyy",
        "DDMMYYD" => "dd-MM-yyyy",
        "DDMMYYN" => "ddMMyyyy",
        "DDMMYYP" => "dd.MM.yyyy",
        "DDMMYYS" => "dd/MM/yyyy",
        "JULDAY" => "D",
        "JULIAN" => "yyyyD",
        "MMDDYY" => "MM/dd/yyyy",
        "MMDDYYB" => "MM dd yyyy",
        "MMDDYYC" => "MM:dd:yyyy",
        "MMDDYYD" => "MM-dd-yyyy",
        "MMDDYYN" => "MMddyyyy",
        "MMDDYYP" => "MM.dd.yyyy",
        "MMDDYYS" => "MM/dd/yyyy",
        "MMYY" => "MM'M'yyyy",
        "MMYYC" => "MM:yyyy",
        "MMYYD" => "MM-yyyy",
        "MMYYN" => "MMyyyy",
        "MMYYP" => "MM.yyyy",
        "MMYYS" => "MM/yyyy",
        "MONNAME" => "MMMM",
        "MONTH" => "M",
        "MONYY" => "MMMyyyy",
        "WEEKDATE" => "EEEE, MMMM dd, yyyy",
        "WEEKDATX" => "EEEE, dd MMMM, yyyy",
        "WEEKDAY" => "u",
        "DOWNAME" => "EEEE",
        "WORDDATE" => "MMMM d, yyyy",
        "WORDDATX" => "d MMMM yyyy",
        "YYMM" => "yyyy'M'MM",
        "YYMMC" => "yyyy:MM",
        "YYMMD" => "yyyy-MM",
        "YYMMN" => "yyyyMM",
        "YYMMP" => "yyyy.MM",
        "YYMMS" => "yyyy/MM",
        "YYMMDD" => "yyyy-MM-dd",
        "YYMMDDB" => "yyyy MM dd",
        "YYMMDDC" => "yyyy:MM:dd",
        "YYMMDDD" => "yyyy-MM-dd",
        "YYMMDDN" => "yyyyMMdd",
        "YYMMDDP" => "yyyy.MM.dd",
        "YYMMDDS" => "yyyy/MM/dd",
        "YYMON" => "yyyyMMM",
        "YEAR" => "yyyy",
        "B8601DN" => "yyyyMMdd",
        "B8601DT" => "yyyyMMdd'T'HHmmssSSS",
        "B8601DX" | "B8601DZ" | "B8601LX" => "yyyyMMdd'T'HHmmssZ",
        "E8601DN" => "yyyy-MM-dd",
        "E8601DT" => "yyyy-MM-dd'T'HH:mm:ss.SSS",
        "E8601DX" | "E8601DZ" | "E8601LX" => "yyyy-MM-dd'T'HH:mm:ssZ",
        "DATEAMPM" => "ddMMMyyyy:HH:mm:ss.SS a",
        "DATETIME" => "ddMMMyyyy:HH:mm:ss.SS",
        "DTDATE" => "ddMMMyyyy",
        "DTMONYY" => "MMMyyyy",
        "DTWKDATX" => "EEEE, dd MMMM, yyyy",
        "DTYEAR" => "yyyy",
        "MDYAMPM" => "MM/dd/yyyy H:mm a",
        "TOD" => "HH:mm:ss.SS",
        _ => return None,
    })
}

/// What parso reads a cell as.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    /// A number of at most two bytes, as an `Integer`.
    Short(i32),
    Long(i64),
    Double(f64),
    /// A `Date`, by its milliseconds since 1970.
    Date(i64),
    Text(String),
}

/// How parso reads a number of a temporal SAS format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Temporal {
    DateTime,
    Date,
    Time,
}

impl Temporal {
    /// The temporal kind of the SAS format `name`, as `SasTemporalFormatter` tells it.
    fn of(name: &str) -> Option<Self> {
        if DATE_TIME_FORMATS.contains(&name) {
            Some(Self::DateTime)
        } else if DATE_FORMATS.contains(&name) {
            Some(Self::Date)
        } else if TIME_FORMATS.contains(&name) {
            Some(Self::Time)
        } else {
            None
        }
    }

    /// `formatSasDateTime`, `formatSasDate` or `formatSasTime` with `JAVA_DATE_LEGACY`: a date,
    /// whole seconds as a `Long` and others as a `Double`, or nothing for NaN.
    pub(super) fn value(self, number: f64) -> Option<Value> {
        if number.is_nan() {
            return None;
        }
        Some(match self {
            Self::DateTime => Value::Date(seconds_to_millis(leap_seconds_fix(number))),
            Self::Date => {
                let days = leap_days_fix(number);
                Value::Date(epoch_seconds_to_millis(
                    days * SECONDS_IN_DAY - SAS_VS_EPOCH_DIFF_SECONDS,
                ))
            }
            Self::Time => {
                let whole = java::round(number);
                if (number - whole as f64).abs() > 0.0 {
                    Value::Double(number)
                } else {
                    Value::Long(whole)
                }
            }
        })
    }
}

/// What a column's SAS format makes of its cells: numbers parso reads as dates or times, and
/// `DataWriterUtil` writes by them, percentages, or plain values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Style {
    Temporal(Temporal),
    Percent,
    Plain,
}

impl Style {
    /// The style of the SAS format `name`.
    pub(super) fn of(name: &str) -> Self {
        match Temporal::of(name) {
            Some(temporal) => Self::Temporal(temporal),
            None if name == "PERCENT" => Self::Percent,
            None => Self::Plain,
        }
    }
}

/// `sasLeapSecondsFix`: the day SAS leaves out at 29 February 4000 and 8000 put back.
fn leap_seconds_fix(seconds: f64) -> f64 {
    let mut seconds = seconds;
    if seconds >= SAS_DAYS_29FEB4000 * SECONDS_IN_DAY {
        if seconds >= SAS_DAYS_29FEB8000 * SECONDS_IN_DAY {
            seconds += SECONDS_IN_DAY;
        }
        seconds += SECONDS_IN_DAY;
    }
    seconds
}

/// `sasLeapDaysFix`.
fn leap_days_fix(days: f64) -> f64 {
    let mut days = days;
    if days >= SAS_DAYS_29FEB4000 {
        if days >= SAS_DAYS_29FEB8000 {
            days += 1.0;
        }
        days += 1.0;
    }
    days
}

/// `sasSecondsToDate`'s milliseconds: the seconds since 1960 as milliseconds since 1970.
pub(super) fn seconds_to_millis(seconds: f64) -> i64 {
    epoch_seconds_to_millis(seconds - SAS_VS_EPOCH_DIFF_SECONDS)
}

/// `(long) (epochSeconds * 1000)`.
fn epoch_seconds_to_millis(epoch_seconds: f64) -> i64 {
    (epoch_seconds * 1000.0) as i64
}

/// `formatSasSecondsAsJavaDate`: the date of a SAS date and time, the leap days fixed.
pub(super) fn sas_date_millis(seconds: f64) -> i64 {
    seconds_to_millis(leap_seconds_fix(seconds))
}

/// `convertByteArrayToNumber`'s value of `number`: nothing for NaN and for a positive number
/// below `NAN_EPSILON`, a `Long` for one within `EPSILON` of a whole number, else a `Double`.
pub(super) fn number(number: f64) -> Option<Value> {
    if number.is_nan() || (number < NAN_EPSILON && number > 0.0) {
        return None;
    }
    let whole = java::round(number);
    if (number - whole as f64).abs() >= EPSILON {
        Some(Value::Double(number))
    } else {
        Some(Value::Long(whole))
    }
}

impl Value {
    /// Whether it is an infinite number, which `DataWriterUtil` writes as missing. Text that
    /// holds the word "Infinity", which parso writes as missing too, is written whole (#155).
    fn is_infinite(&self) -> bool {
        matches!(self, Self::Double(value) if value.is_infinite())
    }

    fn class(&self) -> &'static str {
        match self {
            Self::Short(_) => "java.lang.Integer",
            Self::Long(_) => "java.lang.Long",
            Self::Double(_) => "java.lang.Double",
            Self::Date(_) => "java.util.Date",
            Self::Text(_) => "java.lang.String",
        }
    }
}

fn class_cast(value: &Value, target: &str) -> Thrown {
    Thrown::Runtime(Exception::new(
        "java.lang.ClassCastException",
        format!("class {} cannot be cast to class {target}", value.class()),
    ))
}

/// The formatter `DataWriterUtil` makes once for a column and keeps.
enum Formatter {
    Date(SimpleDateFormat),
    Percent(DecimalFormat),
}

/// `DataWriterUtil`, with the formatters it has made for the columns, by their ids.
#[derive(Default)]
pub(super) struct Writer {
    formatters: HashMap<i32, Formatter>,
}

/// The column of a cell, as `DataWriterUtil` reads it: its id and its format's name, precision
/// and style.
pub(super) struct Format<'a> {
    pub(super) column: i32,
    pub(super) name: &'a str,
    pub(super) precision: i32,
    pub(super) style: Style,
}

impl Writer {
    /// `DataWriterUtil.getValue`: the text of a cell, or what it throws on it.
    pub(super) fn text(&mut self, format: &Format, value: Option<Value>) -> Result<String, Thrown> {
        let Some(value) = value else {
            return Ok(String::new());
        };
        if value.is_infinite() {
            return Ok(String::new());
        }
        if let Value::Date(millis) = &value {
            let Formatter::Date(dates) = self.formatter(format, Self::date_formatter)? else {
                return Err(Thrown::Runtime(Exception::new(
                    "java.lang.ClassCastException",
                    "class java.text.DecimalFormat cannot be cast to class java.text.SimpleDateFormat",
                )));
            };
            return Ok(dates
                .format(*millis, &TimeZone::fixed(0))
                .expect("the date patterns of SAS formats name no time zone"));
        }
        if format.style == Style::Temporal(Temporal::Time) {
            return match value {
                Value::Long(seconds) => Ok(clock(seconds)),
                Value::Double(seconds) => Ok(fractional_clock(seconds)),
                other => Err(class_cast(&other, "java.lang.Long")),
            };
        }
        if format.style == Style::Percent {
            let number = match value {
                Value::Long(number) => number as f64,
                Value::Double(number) => number,
                other => return Err(class_cast(&other, "java.lang.Double")),
            };
            let Formatter::Percent(percent) = self.formatter(format, Self::percent_formatter)?
            else {
                return Err(Thrown::Runtime(Exception::new(
                    "java.lang.ClassCastException",
                    "class java.text.SimpleDateFormat cannot be cast to class java.text.DecimalFormat",
                )));
            };
            return Ok(percent.format_f64(number));
        }
        Ok(match value {
            Value::Short(number) => number.to_string(),
            Value::Long(number) => number.to_string(),
            Value::Double(number) => double_text(number),
            Value::Text(text) => text,
            Value::Date(_) => unreachable!("a date is written by its format above"),
        })
    }

    /// The column's formatter, made by `make` the first time.
    fn formatter(
        &mut self,
        format: &Format,
        make: fn(&Format) -> Result<Formatter, Thrown>,
    ) -> Result<&Formatter, Thrown> {
        Ok(match self.formatters.entry(format.column) {
            Entry::Occupied(made) => made.into_mut(),
            Entry::Vacant(slot) => slot.insert(make(format)?),
        })
    }

    /// `getDateFormatProcessor`.
    fn date_formatter(format: &Format) -> Result<Formatter, Thrown> {
        let pattern = date_pattern(format.name).ok_or_else(|| {
            Thrown::Runtime(Exception::new(
                "java.util.NoSuchElementException",
                "Unknown date format",
            ))
        })?;
        Ok(Formatter::Date(
            SimpleDateFormat::new(pattern).expect("the date patterns of SAS formats compile"),
        ))
    }

    /// `getPercentFormatProcessor`: a whole percentage, or one with the format's precision.
    fn percent_formatter(format: &Format) -> Result<Formatter, Thrown> {
        let decimals = usize::try_from(format.precision).map_err(|_| {
            Thrown::Runtime(Exception::new(
                "java.lang.NegativeArraySizeException",
                format.precision.to_string(),
            ))
        })?;
        let pattern = if decimals == 0 {
            "0%".to_owned()
        } else {
            format!("0%.{}", "0".repeat(decimals))
        };
        Ok(Formatter::Percent(
            DecimalFormat::new(&pattern, Rounding::HalfEven)
                .expect("a percent pattern of zeros is valid"),
        ))
    }
}

/// `DataWriterUtil`'s time of whole `seconds`: hours, minutes and seconds of two digits.
fn clock(seconds: i64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 60 / 60,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// The time of `seconds` with a fraction, which parso throws on (#155): the [`clock`] of its
/// whole seconds, then its fraction's shortest decimal digits.
fn fractional_clock(seconds: f64) -> String {
    let decimal = seconds.to_string();
    let fraction = decimal.split_once('.').map_or("", |(_, fraction)| fraction);
    format!("{}.{fraction}", clock(seconds.trunc() as i64))
}

/// `convertDoubleElementToString`: `Double.toString`, a long one rounded to `ACCURACY` digits
/// around its point first, without the zeros its decimal ends with, nor then a point. A number
/// in scientific notation is written whole, where parso strips the zeros of its exponent (#155).
fn double_text(value: f64) -> String {
    let mut text = java::dtoa::java_double_to_string(value);
    if text.len() > ROUNDING_LENGTH {
        let before_point = value.abs().log10().ceil() as i32;
        let rounded = BigDecimal::from_f64(value)
            .expect("a number written is finite")
            .set_scale(
                i64::from(ACCURACY.wrapping_sub(before_point)),
                Rounding::HalfUp,
            );
        text = java::dtoa::java_double_to_string(rounded.to_f64());
    }
    if text.contains('.') && !text.contains('E') {
        let trimmed = text.trim_end_matches('0');
        trimmed.strip_suffix('.').unwrap_or(trimmed).to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(name: &str, precision: i32, value: Value) -> Result<String, Thrown> {
        let format = Format {
            column: 1,
            name,
            precision,
            style: Style::of(name),
        };
        Writer::default().text(&format, Some(value))
    }

    #[test]
    fn tells_the_styles_of_formats_as_their_names_do() {
        const WRITTEN_TIME_FORMATS: [&str; 7] = [
            "TIME", "HHMM", "E8601LZ", "E8601TM", "HOUR", "MMSS", "TIMEAMPM",
        ];
        let others = [
            "", "PERCENT", "BEST", "percent", "PERCENTN", "time", "MDYAMPM", " DATE",
        ];
        let names = DATE_FORMATS
            .iter()
            .chain(&TIME_FORMATS)
            .chain(&DATE_TIME_FORMATS)
            .chain(&WRITTEN_TIME_FORMATS)
            .chain(&others);
        for &name in names {
            let style = Style::of(name);
            let temporal = match style {
                Style::Temporal(temporal) => Some(temporal),
                Style::Percent | Style::Plain => None,
            };
            assert_eq!(temporal, Temporal::of(name), "{name}");
            assert_eq!(
                style == Style::Temporal(Temporal::Time),
                WRITTEN_TIME_FORMATS.contains(&name),
                "{name}"
            );
            assert_eq!(style == Style::Percent, name == "PERCENT", "{name}");
        }
    }

    #[test]
    fn writes_values_as_data_writer_util_does() {
        assert_eq!(text("", 0, Value::Double(0.1 + 0.2)).unwrap(), "0.3");
        assert_eq!(text("", 0, Value::Double(1e10)).unwrap(), "1.0E10");
        assert_eq!(text("", 0, Value::Double(1.5e-7)).unwrap(), "1.5E-7");
        assert_eq!(text("", 0, Value::Double(2.5)).unwrap(), "2.5");
        assert_eq!(text("", 0, Value::Double(f64::INFINITY)).unwrap(), "");
        assert_eq!(
            text("", 0, Value::Text("-Infinity!".into())).unwrap(),
            "-Infinity!"
        );
        assert_eq!(text("PERCENT", 1, Value::Double(0.5)).unwrap(), "50.0%");
        assert_eq!(text("PERCENT", 0, Value::Long(1)).unwrap(), "100%");
        assert_eq!(text("TIME", 0, Value::Long(3723)).unwrap(), "01:02:03");
        assert_eq!(
            text("TIME", 0, Value::Double(3723.25)).unwrap(),
            "01:02:03.25"
        );
        assert_eq!(text("HHMM", 0, Value::Double(0.1)).unwrap(), "00:00:00.1");
        assert!(text("TIME", 0, Value::Date(0)).is_err());
        let day = Temporal::Date.value(1.0).unwrap();
        assert_eq!(text("DDMMYYD", 0, day).unwrap(), "02-01-1960");
        let moment = Temporal::DateTime.value(100.0).unwrap();
        assert_eq!(
            text("DATETIME", 0, moment).unwrap(),
            "01Jan1960:00:01:40.00"
        );
        let epoch = Temporal::Date.value(3653.0).unwrap();
        assert_eq!(text("DATE", 0, epoch).unwrap(), "01Jan1970");
        assert!(text("QTR", 0, Temporal::Date.value(1.0).unwrap()).is_err());
    }

    #[test]
    fn reads_numbers_as_parso_does() {
        assert_eq!(number(f64::NAN), None);
        assert_eq!(number(1e-301), None);
        assert_eq!(number(3.0), Some(Value::Long(3)));
        assert_eq!(number(3.5), Some(Value::Double(3.5)));
        assert_eq!(Temporal::Time.value(2.0), Some(Value::Long(2)));
        assert_eq!(Temporal::Time.value(2.5), Some(Value::Double(2.5)));
        assert_eq!(Temporal::of("DATETIME"), Some(Temporal::DateTime));
        assert_eq!(Temporal::of("BEST"), None);
    }
}
