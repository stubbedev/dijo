//! The parts of Drew Noakes' metadata-extractor 2.21.0 Tika's image parsers use: its readers of
//! the segments, IFDs and chunks image files keep metadata in, and its directories of tags,
//! named and described as its `Directory` and `TagDescriptor` classes name and describe them.
//! Values are kept as the Java types metadata-extractor stores them as ([`Value`]), since a
//! description depends on the type as much as on the value.

mod apple;
pub(crate) mod dates;
mod descriptions;
pub(crate) mod exif;
pub(crate) mod geo;
pub(crate) mod heif;
pub(crate) mod icc;
pub(crate) mod iptc;
pub(crate) mod jpeg;
mod names;
pub(crate) mod photoshop;
pub(crate) mod reader;
pub(crate) mod webp;
mod xmp;

use crate::java::decimal::Rounding;
use crate::java::decimal_format::DecimalFormat;
use crate::java::dtoa::java_double_to_string;
use crate::java::parse_long;
use crate::java::time::TimeZone;
use crate::text::charset::Charset;

/// metadata-extractor's `Rational`: a numerator and a denominator, neither reduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rational {
    pub(crate) numerator: i64,
    pub(crate) denominator: i64,
}

impl Rational {
    pub(crate) fn new(numerator: i64, denominator: i64) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    pub(crate) fn double_value(self) -> f64 {
        if self.numerator == 0 {
            0.0
        } else {
            self.numerator as f64 / self.denominator as f64
        }
    }

    pub(crate) fn float_value(self) -> f32 {
        if self.numerator == 0 {
            0.0
        } else {
            self.numerator as f32 / self.denominator as f32
        }
    }

    /// `intValue`: the double value cast as Java casts it.
    pub(crate) fn int_value(self) -> i32 {
        self.double_value() as i32
    }

    pub(crate) fn long_value(self) -> i64 {
        self.double_value() as i64
    }

    pub(crate) fn byte_value(self) -> u8 {
        (self.double_value() as i32) as u8
    }

    pub(crate) fn is_integer(self) -> bool {
        self.denominator == 1
            || (self.denominator != 0 && self.numerator % self.denominator == 0)
            || (self.denominator == 0 && self.numerator == 0)
    }

    pub(crate) fn is_zero(self) -> bool {
        self.numerator == 0 || self.denominator == 0
    }

    pub(crate) fn reciprocal(self) -> Self {
        Self::new(self.denominator, self.numerator)
    }

    /// `equals(Rational)`: the same double value.
    pub(crate) fn same_value(self, other: Self) -> bool {
        self.double_value() == other.double_value()
    }

    fn simplified(self) -> Self {
        let (mut n, mut d) = (self.numerator, self.denominator);
        if d < 0 {
            n = n.wrapping_neg();
            d = d.wrapping_neg();
        }
        let gcd = gcd(n, d);
        Self::new(n / gcd, d / gcd)
    }

    /// `toSimpleString`.
    pub(crate) fn to_simple_string(self, allow_decimal: bool) -> String {
        if self.denominator == 0 && self.numerator != 0 {
            return self.to_string();
        }
        if self.is_integer() {
            return self.int_value().to_string();
        }
        let simplified = self.simplified();
        if allow_decimal {
            let decimal = java_double_to_string(simplified.double_value());
            if decimal.encode_utf16().count() < 5 {
                return decimal;
            }
        }
        simplified.to_string()
    }
}

/// `toString`: `numerator/denominator`, the signs moved to the numerator.
impl std::fmt::Display for Rational {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.denominator < 0 {
            write!(
                f,
                "{}/{}",
                self.numerator.wrapping_neg(),
                self.denominator.wrapping_neg()
            )
        } else {
            write!(f, "{}/{}", self.numerator, self.denominator)
        }
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.wrapping_abs(), b.wrapping_abs());
    while a != 0 && b != 0 {
        if a > b {
            a %= b;
        } else {
            b %= a;
        }
    }
    if a == 0 {
        b
    } else {
        a
    }
}

/// metadata-extractor's `StringValue`: bytes and the charset to read them in, Java's default
/// (UTF-8) when it names none.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StringValue {
    pub(crate) bytes: Vec<u8>,
    pub(crate) charset: Option<Charset>,
}

impl std::fmt::Display for StringValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&java_string(&self.bytes, self.charset))
    }
}

/// A `StringValue` of `bytes` in `charset`, which must be one the port decodes as the JDK does.
pub(crate) fn string_value(
    bytes: &[u8],
    charset: Option<Charset>,
) -> Result<StringValue, Unported> {
    if let Some(charset) = charset {
        if charset.decode(&[]).is_none() {
            return Err(Unported::new(format!(
                "text in {}, a charset the port does not decode",
                charset.name()
            )));
        }
    }
    Ok(StringValue {
        bytes: bytes.to_vec(),
        charset,
    })
}

/// `new String(bytes, charset)`, or with Java's default charset, UTF-8, when `charset` is none.
pub(crate) fn java_string(bytes: &[u8], charset: Option<Charset>) -> String {
    charset
        .unwrap_or_else(|| Charset::named("UTF-8"))
        .decode(bytes)
        .unwrap_or_else(|| panic!("metadata-extractor reads only charsets the JDK port decodes"))
}

/// A tag's value, as the Java type metadata-extractor stores it.
#[derive(Debug, Clone)]
pub(crate) enum Value {
    /// `Integer`.
    Int(i32),
    /// `Long`.
    Long(i64),
    /// `Float`.
    Float(f32),
    /// `Double`.
    Double(f64),
    /// `Boolean`.
    Boolean(bool),
    Rational(Rational),
    /// `java.util.Date`, as milliseconds since the epoch, with Java's default time zone, in which
    /// its `toString` writes it.
    Date(i64, &'static TimeZone),
    /// `String`.
    Text(String),
    StringValue(StringValue),
    /// `byte[]`.
    Bytes(Vec<u8>),
    /// `short[]`.
    Shorts(Vec<i16>),
    /// `int[]`.
    Ints(Vec<i32>),
    /// `long[]`.
    Longs(Vec<i64>),
    /// `float[]`.
    Floats(Vec<f32>),
    /// `double[]`.
    Doubles(Vec<f64>),
    /// `Rational[]`.
    Rationals(Vec<Rational>),
    /// `StringValue[]`.
    StringValues(Vec<StringValue>),
    /// `String[]`.
    Texts(Vec<String>),
    /// `JpegComponent`: a component's id, sampling factors and quantization table.
    Component {
        id: i32,
        sampling: i32,
        quantization: i32,
    },
}

impl Value {
    /// The length of an array, `None` for a value that is not one.
    fn array_length(&self) -> Option<usize> {
        Some(match self {
            Value::Bytes(values) => values.len(),
            Value::Shorts(values) => values.len(),
            Value::Ints(values) => values.len(),
            Value::Longs(values) => values.len(),
            Value::Floats(values) => values.len(),
            Value::Doubles(values) => values.len(),
            Value::Rationals(values) => values.len(),
            Value::StringValues(values) => values.len(),
            Value::Texts(values) => values.len(),
            _ => return None,
        })
    }

    /// `Number.intValue` of a value that is a `Number`.
    fn number_int(&self) -> Option<i32> {
        Some(match self {
            Value::Int(value) => *value,
            Value::Long(value) => *value as i32,
            Value::Float(value) => *value as i32,
            Value::Double(value) => *value as i32,
            Value::Rational(value) => value.int_value(),
            _ => return None,
        })
    }

    fn number_long(&self) -> Option<i64> {
        Some(match self {
            Value::Int(value) => i64::from(*value),
            Value::Long(value) => *value,
            Value::Float(value) => *value as i64,
            Value::Double(value) => *value as i64,
            Value::Rational(value) => value.long_value(),
            _ => return None,
        })
    }

    fn number_double(&self) -> Option<f64> {
        Some(match self {
            Value::Int(value) => f64::from(*value),
            Value::Long(value) => *value as f64,
            Value::Float(value) => f64::from(*value),
            Value::Double(value) => *value,
            Value::Rational(value) => value.double_value(),
            _ => return None,
        })
    }

    fn number_float(&self) -> Option<f32> {
        Some(match self {
            Value::Int(value) => *value as f32,
            Value::Long(value) => *value as f32,
            Value::Float(value) => *value,
            Value::Double(value) => *value as f32,
            Value::Rational(value) => value.float_value(),
            _ => return None,
        })
    }

    /// The text of a `String` or `StringValue`.
    fn text(&self) -> Option<String> {
        match self {
            Value::Text(text) => Some(text.clone()),
            Value::StringValue(value) => Some(value.to_string()),
            _ => None,
        }
    }
}

/// The kinds of directory the readers make, each with its name, tag names and descriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    ExifIfd0,
    ExifSubIfd,
    ExifInterop,
    /// A thumbnail IFD, whose offsets count from where the Exif data starts.
    ExifThumbnail {
        exif_start: i32,
    },
    ExifImage,
    Gps,
    PrintIm,
    Iptc,
    Icc,
    Photoshop,
    Jpeg,
    JpegComment,
    Jfif,
    Jfxx,
    Adobe,
    Ducky,
    Huffman,
    Webp,
    FileSystem,
    AppleMakernote,
    AppleRunTime,
    Heif,
    Xmp,
    Error,
}

impl Kind {
    /// `getName`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::ExifIfd0 => "Exif IFD0",
            Kind::ExifSubIfd => "Exif SubIFD",
            Kind::ExifInterop => "Interoperability",
            Kind::ExifThumbnail { .. } => "Exif Thumbnail",
            Kind::ExifImage => "Exif Image",
            Kind::Gps => "GPS",
            Kind::PrintIm => "PrintIM",
            Kind::Iptc => "IPTC",
            Kind::Icc => "ICC Profile",
            Kind::Photoshop => "Photoshop",
            Kind::Jpeg => "JPEG",
            Kind::JpegComment => "JpegComment",
            Kind::Jfif => "JFIF",
            Kind::Jfxx => "JFXX",
            Kind::Adobe => "Adobe JPEG",
            Kind::Ducky => "Ducky",
            Kind::Huffman => "Huffman",
            Kind::Webp => "WebP",
            Kind::FileSystem => "File",
            Kind::AppleMakernote => "Apple Makernote",
            Kind::AppleRunTime => "Apple Run Time",
            Kind::Heif => "HEIF",
            Kind::Xmp => "XMP",
            Kind::Error => "Error",
        }
    }

    /// Whether the directory's class extends `ExifDirectoryBase`, whose tags Tika names after it.
    pub(crate) fn is_exif(self) -> bool {
        matches!(
            self,
            Kind::ExifIfd0
                | Kind::ExifSubIfd
                | Kind::ExifInterop
                | Kind::ExifThumbnail { .. }
                | Kind::ExifImage
                | Kind::Gps
        )
    }

    fn names(self) -> &'static [(i32, &'static str)] {
        match self {
            Kind::ExifIfd0 | Kind::ExifSubIfd | Kind::ExifInterop | Kind::ExifImage => names::EXIF,
            Kind::ExifThumbnail { .. } => names::THUMBNAIL,
            Kind::Gps => names::GPS,
            Kind::PrintIm => names::PRINT_IM,
            Kind::Iptc => names::IPTC,
            Kind::Icc => names::ICC,
            Kind::Photoshop => names::PHOTOSHOP,
            Kind::Jpeg => names::JPEG,
            Kind::JpegComment => names::JPEG_COMMENT,
            Kind::Jfif => names::JFIF,
            Kind::Jfxx => names::JFXX,
            Kind::Adobe => names::ADOBE,
            Kind::Ducky => names::DUCKY,
            Kind::Huffman => names::HUFFMAN,
            Kind::Webp => names::WEBP,
            Kind::FileSystem => names::FILE_SYSTEM,
            Kind::AppleMakernote => names::APPLE_MAKERNOTE,
            Kind::AppleRunTime => names::APPLE_RUN_TIME,
            Kind::Heif => names::HEIF,
            Kind::Xmp => names::XMP,
            Kind::Error => &[],
        }
    }
}

/// A directory of tags, in the order they were first set.
#[derive(Debug, Clone)]
pub(crate) struct Directory {
    pub(crate) kind: Kind,
    tags: Vec<(i32, Value)>,
    errors: Vec<String>,
}

impl Directory {
    pub(crate) fn new(kind: Kind) -> Self {
        Self {
            kind,
            tags: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// `setObject`: a tag set again keeps its place.
    pub(crate) fn set(&mut self, tag: i32, value: Value) {
        match self.tags.iter_mut().find(|(defined, _)| *defined == tag) {
            Some((_, slot)) => *slot = value,
            None => self.tags.push((tag, value)),
        }
    }

    pub(crate) fn add_error(&mut self, error: impl Into<String>) {
        self.errors.push(error.into());
    }

    /// The tags in the order they were first set, as `getTags` lists them.
    pub(crate) fn tags(&self) -> impl Iterator<Item = i32> + '_ {
        self.tags.iter().map(|(tag, _)| *tag)
    }

    pub(crate) fn contains(&self, tag: i32) -> bool {
        self.tags.iter().any(|(defined, _)| *defined == tag)
    }

    pub(crate) fn object(&self, tag: i32) -> Option<&Value> {
        self.tags
            .iter()
            .find(|(defined, _)| *defined == tag)
            .map(|(_, value)| value)
    }

    /// `getTagName`: the tag's name, or `Unknown tag (0x....)`.
    pub(crate) fn tag_name(&self, tag: i32) -> String {
        if self.kind == Kind::Photoshop {
            if let Some(name) = photoshop::dynamic_name(tag) {
                return name;
            }
        }
        match self.kind.names().iter().find(|(named, _)| *named == tag) {
            Some((_, name)) => (*name).to_owned(),
            None => format!("Unknown tag (0x{tag:04x})"),
        }
    }

    /// `getDescription` of the tag, by the directory's `TagDescriptor`.
    pub(crate) fn description(&self, tag: i32) -> descriptions::Described {
        descriptions::describe(self, tag)
    }

    /// `getInteger`.
    pub(crate) fn integer(&self, tag: i32) -> Option<i32> {
        let value = self.object(tag)?;
        if let Some(int) = value.number_int() {
            return Some(int);
        }
        match value {
            Value::Text(_) | Value::StringValue(_) => {
                let text = value.text()?;
                Some(match crate::java::parse_int(&text) {
                    Some(int) => int,
                    None => chars_as_number(&text) as i32,
                })
            }
            Value::Rationals(values) if values.len() == 1 => Some(values[0].int_value()),
            Value::Bytes(values) if values.len() == 1 => Some(i32::from(values[0] as i8)),
            Value::Ints(values) if values.len() == 1 => Some(values[0]),
            Value::Shorts(values) if values.len() == 1 => Some(i32::from(values[0])),
            _ => None,
        }
    }

    /// `getLongObject`.
    pub(crate) fn long(&self, tag: i32) -> Option<i64> {
        let value = self.object(tag)?;
        if let Some(long) = value.number_long() {
            return Some(long);
        }
        match value {
            Value::Text(_) | Value::StringValue(_) => {
                let text = value.text()?;
                Some(parse_long(&text).unwrap_or_else(|| chars_as_number(&text)))
            }
            Value::Rationals(values) if values.len() == 1 => Some(values[0].long_value()),
            Value::Bytes(values) if values.len() == 1 => Some(i64::from(values[0] as i8)),
            Value::Ints(values) if values.len() == 1 => Some(i64::from(values[0])),
            Value::Shorts(values) if values.len() == 1 => Some(i64::from(values[0])),
            _ => None,
        }
    }

    /// `getDoubleObject`.
    pub(crate) fn double(&self, tag: i32) -> Option<f64> {
        let value = self.object(tag)?;
        match value {
            Value::Text(_) | Value::StringValue(_) => crate::java::parse_double(&value.text()?),
            other => other.number_double(),
        }
    }

    /// `getFloatObject`.
    pub(crate) fn float(&self, tag: i32) -> Option<f32> {
        let value = self.object(tag)?;
        match value {
            Value::Text(_) | Value::StringValue(_) => {
                crate::java::parse_double(&value.text()?).map(|double| double as f32)
            }
            other => other.number_float(),
        }
    }

    /// `getRational`.
    pub(crate) fn rational(&self, tag: i32) -> Option<Rational> {
        match self.object(tag)? {
            Value::Rational(value) => Some(*value),
            Value::Int(value) => Some(Rational::new(i64::from(*value), 1)),
            Value::Long(value) => Some(Rational::new(*value, 1)),
            _ => None,
        }
    }

    /// `getRationalArray`.
    pub(crate) fn rationals(&self, tag: i32) -> Option<&[Rational]> {
        match self.object(tag)? {
            Value::Rationals(values) => Some(values),
            _ => None,
        }
    }

    /// `getIntArray`.
    pub(crate) fn ints(&self, tag: i32) -> Option<Vec<i32>> {
        Some(match self.object(tag)? {
            Value::Ints(values) => values.clone(),
            Value::Rationals(values) => values.iter().map(|value| value.int_value()).collect(),
            Value::Shorts(values) => values.iter().map(|value| i32::from(*value)).collect(),
            Value::Bytes(values) => values.iter().map(|value| i32::from(*value as i8)).collect(),
            Value::Text(text) => text.encode_utf16().map(i32::from).collect(),
            Value::Int(value) => vec![*value],
            _ => return None,
        })
    }

    /// `getByteArray`.
    pub(crate) fn bytes(&self, tag: i32) -> Option<Vec<u8>> {
        Some(match self.object(tag)? {
            Value::StringValue(value) => value.bytes.clone(),
            Value::Rationals(values) => values.iter().map(|value| value.byte_value()).collect(),
            Value::Bytes(values) => values.clone(),
            Value::Ints(values) => values.iter().map(|value| *value as u8).collect(),
            Value::Shorts(values) => values.iter().map(|value| *value as u8).collect(),
            Value::Text(text) => text.encode_utf16().map(|unit| unit as u8).collect(),
            Value::Int(value) => vec![*value as u8],
            _ => return None,
        })
    }

    /// `getStringArray`.
    pub(crate) fn strings(&self, tag: i32) -> Option<Vec<String>> {
        Some(match self.object(tag)? {
            Value::Text(text) => vec![text.clone()],
            Value::StringValue(value) => vec![value.to_string()],
            Value::StringValues(values) => values.iter().map(ToString::to_string).collect(),
            Value::Texts(values) => values.clone(),
            Value::Ints(values) => values.iter().map(ToString::to_string).collect(),
            Value::Bytes(values) => values
                .iter()
                .map(|byte| (*byte as i8).to_string())
                .collect(),
            Value::Rationals(values) => values
                .iter()
                .map(|value| value.to_simple_string(false))
                .collect(),
            _ => return None,
        })
    }

    /// `getString`.
    pub(crate) fn string(&self, tag: i32) -> Option<String> {
        let value = self.object(tag)?;
        Some(match value {
            Value::Rational(rational) => rational.to_simple_string(true),
            Value::Int(int) => int.to_string(),
            Value::Long(long) => long.to_string(),
            Value::Boolean(boolean) => boolean.to_string(),
            Value::Float(float) => float_format().format_f64(f64::from(*float)),
            Value::Double(double) => float_format().format_f64(*double),
            Value::Date(millis, zone) => dates::date_to_string(*millis, zone),
            Value::Text(text) => text.clone(),
            Value::StringValue(text) => text.to_string(),
            Value::Bytes(values) => join(values.iter().map(|byte| byte.to_string())),
            Value::Shorts(values) => join(values.iter().map(ToString::to_string)),
            Value::Ints(values) => join(values.iter().map(ToString::to_string)),
            Value::Longs(values) => join(values.iter().map(ToString::to_string)),
            Value::Floats(values) => join(
                values
                    .iter()
                    .map(|value| float_array_element(f64::from(*value))),
            ),
            Value::Doubles(values) => join(values.iter().map(|value| float_array_element(*value))),
            Value::Rationals(values) => join(values.iter().map(ToString::to_string)),
            Value::StringValues(values) => join(values.iter().map(ToString::to_string)),
            Value::Texts(values) => join(values.iter().cloned()),
            Value::Component {
                sampling,
                quantization,
                ..
            } => jpeg::component_text(*sampling, *quantization),
        })
    }

    /// `getDate` with the date patterns metadata-extractor tries, in Java's default time zone.
    pub(crate) fn date(&self, tag: i32, zone: &TimeZone) -> Option<i64> {
        match self.object(tag)? {
            Value::Date(millis, _) => Some(*millis),
            value @ (Value::Text(_) | Value::StringValue(_)) => {
                dates::parse_text_date(&value.text()?, zone)
            }
            _ => None,
        }
    }
}

/// `_floatFormatPattern`, with which `getString` writes floats and doubles.
fn float_format() -> DecimalFormat {
    DecimalFormat::new("0.###", Rounding::HalfEven)
        .expect("metadata-extractor's float pattern compiles")
}

fn float_array_element(value: f64) -> String {
    let text = float_format().format_f64(value);
    if text == "-0" {
        "0".to_owned()
    } else {
        text
    }
}

fn join(items: impl Iterator<Item = String>) -> String {
    items.collect::<Vec<_>>().join(" ")
}

/// The number the bytes of a text make, read big-endian, as `getInteger` and `getLongObject`
/// take a text that is not a number: Java's default charset encodes it first.
fn chars_as_number(text: &str) -> i64 {
    text.bytes().fold(0i64, |value, byte| {
        (value << 8).wrapping_add(i64::from(byte))
    })
}

/// What the readers found in a file: its directories, in the order they were added.
#[derive(Debug, Default)]
pub(crate) struct Metadata {
    pub(crate) directories: Vec<Directory>,
}

impl Metadata {
    pub(crate) fn add(&mut self, directory: Directory) {
        self.directories.push(directory);
    }

    pub(crate) fn first(&self, kind: Kind) -> Option<&Directory> {
        self.directories
            .iter()
            .find(|directory| directory.kind == kind)
    }

    /// The first directory of the kind `matches` accepts, as `getFirstDirectoryOfType`.
    pub(crate) fn first_index(&self, matches: impl Fn(Kind) -> bool) -> Option<usize> {
        self.directories
            .iter()
            .position(|directory| matches(directory.kind))
    }
}

pub(crate) use crate::formats::Unported;
