//! netcdf-java's Common Data Model, which Tika's scientific parsers read every format into, as
//! they write it: a file's attributes, dimensions and variables, each attribute written as CDL,
//! as `Attribute.toString` writes it, each variable as its data type and
//! `Variable.getNameAndDimensions`, and the global attributes as metadata.

use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::java::dtoa::{java_double_to_string, java_float_to_string};

/// `TikaCoreProperties.TITLE`, which a global attribute `title` sets.
const TITLE: &str = "dc:title";

/// The global attributes `ClimateForecast.byName` maps to a property of their own, `cf:` and
/// their name, each with whether the property holds several values (a bag).
const CLIMATE_FORECAST: [(&str, bool); 15] = [
    ("prg_ID", false),
    ("cmd_ln", false),
    ("history", true),
    ("table_id", false),
    ("institution", false),
    ("source", false),
    ("contact", false),
    ("project_id", false),
    ("Conventions", false),
    ("references", false),
    ("acknowledgement", false),
    ("realization", false),
    ("experiment_id", false),
    ("comment", false),
    ("model_name_english", false),
];

/// A CDM data type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataType {
    Byte,
    UByte,
    Char,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    Float,
    Double,
    String,
    Structure,
    Sequence,
    Enum1,
    Enum2,
    Enum4,
    Opaque,
}

impl DataType {
    /// `DataType.toString`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Byte => "byte",
            Self::UByte => "ubyte",
            Self::Char => "char",
            Self::Short => "short",
            Self::UShort => "ushort",
            Self::Int => "int",
            Self::UInt => "uint",
            Self::Long => "long",
            Self::ULong => "ulong",
            Self::Float => "float",
            Self::Double => "double",
            Self::String => "String",
            Self::Structure => "Structure",
            Self::Sequence => "Sequence",
            Self::Enum1 => "enum1",
            Self::Enum2 => "enum2",
            Self::Enum4 => "enum4",
            Self::Opaque => "opaque",
        }
    }

    /// The suffix CDL writes after a number of the type.
    fn suffix(self) -> &'static str {
        match self {
            Self::Byte => "b",
            Self::UByte => "UB",
            Self::Short => "s",
            Self::UShort => "US",
            Self::UInt => "U",
            Self::Long => "L",
            Self::ULong => "UL",
            Self::Float => "f",
            _ => "",
        }
    }
}

/// One value of a numeric attribute.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Number {
    Signed(i64),
    Unsigned(u64),
    Float(f32),
    Double(f64),
}

impl Number {
    /// The value as Java writes the `Number` netcdf-java holds it in.
    pub(crate) fn java(self) -> String {
        match self {
            Self::Signed(value) => value.to_string(),
            Self::Unsigned(value) => value.to_string(),
            Self::Float(value) => java_float_to_string(value),
            Self::Double(value) => java_double_to_string(value),
        }
    }
}

/// The values of an attribute.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Values {
    Text(Vec<String>),
    Numbers(DataType, Vec<Number>),
    /// Values of a type CDL has no literals for, such as a structure: only the type is written.
    Unwritten(DataType),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Attribute {
    pub(crate) name: String,
    pub(crate) values: Values,
}

impl Attribute {
    pub(crate) fn text(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            values: Values::Text(vec![value.into()]),
        }
    }

    pub(crate) fn numbers(
        name: impl Into<String>,
        data_type: DataType,
        values: Vec<Number>,
    ) -> Self {
        Self {
            name: name.into(),
            values: Values::Numbers(data_type, values),
        }
    }

    /// `Attribute.toString`: the attribute as CDL, `name = "text"` or `name = 1.5f, 2.5f`, each
    /// number with the suffix of its type.
    pub(crate) fn cdl(&self) -> String {
        let mut cdl = format!("{} = ", self.name);
        match &self.values {
            Values::Text(texts) => {
                for (index, text) in texts.iter().enumerate() {
                    if index > 0 {
                        cdl.push_str(", ");
                    }
                    cdl.push('"');
                    escape_cdl(text, &mut cdl);
                    cdl.push('"');
                }
            }
            Values::Numbers(data_type, numbers) => {
                for (index, number) in numbers.iter().enumerate() {
                    if index > 0 {
                        cdl.push_str(", ");
                    }
                    cdl.push_str(&number.java());
                    cdl.push_str(data_type.suffix());
                }
            }
            Values::Unwritten(data_type) => {
                cdl.truncate(self.name.len());
                cdl.push_str(" (");
                cdl.push_str(data_type.name());
                cdl.push(')');
            }
        }
        cdl
    }

    /// Each value as text, as the metadata holds it: the texts, or each number as Java writes
    /// it. Values of a type CDL has no literals for give none.
    pub(crate) fn metadata_values(&self) -> Vec<String> {
        match &self.values {
            Values::Text(texts) => texts.clone(),
            Values::Numbers(_, numbers) => numbers.iter().map(|number| number.java()).collect(),
            Values::Unwritten(_) => Vec::new(),
        }
    }
}

/// A string as CDL writes it between quotes: a backslash before each quote and backslash, and
/// line feeds, carriage returns and tabs escaped.
fn escape_cdl(text: &str, out: &mut String) {
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(character),
        }
    }
}

/// A shared dimension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dimension {
    pub(crate) name: String,
    pub(crate) length: u64,
}

/// One dimension of a variable's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Axis {
    /// A shared dimension, by its name.
    Shared(Dimension),
    /// A dimension of the variable's own, of this length.
    Anonymous(u64),
    /// A dimension of variable length.
    VariableLength,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Variable {
    pub(crate) data_type: DataType,
    pub(crate) name: String,
    pub(crate) axes: Vec<Axis>,
    pub(crate) attributes: Vec<Attribute>,
}

impl Variable {
    /// `Variable.getNameAndDimensions`: the name, and the shape in parentheses, each shared
    /// dimension as `name=length`, a dimension of the variable's own as its length.
    pub(crate) fn name_and_dimensions(&self) -> String {
        let mut written = self.name.clone();
        if self.axes.is_empty() {
            return written;
        }
        written.push('(');
        for (index, axis) in self.axes.iter().enumerate() {
            if index > 0 {
                written.push_str(", ");
            }
            match axis {
                Axis::Shared(dimension) => {
                    written.push_str(&format!("{}={}", dimension.name, dimension.length));
                }
                Axis::Anonymous(length) => written.push_str(&length.to_string()),
                Axis::VariableLength => written.push('*'),
            }
        }
        written.push(')');
        written
    }

    /// The variable as `NetCDFParser` lists it: its data type, a space, and
    /// [`Variable::name_and_dimensions`].
    pub(crate) fn declaration(&self) -> String {
        format!("{} {}", self.data_type.name(), self.name_and_dimensions())
    }
}

/// `NetCDFParser`'s list of the dimensions: a heading, then each dimension as `name = length`.
pub(crate) fn write_dimensions(xhtml: &mut Xhtml, dimensions: &[Dimension]) -> Result<(), Stop> {
    xhtml.element("h1", "dimensions")?;
    xhtml.start_element("ul")?;
    xhtml.newline()?;
    for dimension in dimensions {
        xhtml.element("li", &format!("{} = {}", dimension.name, dimension.length))?;
    }
    xhtml.end_element("ul")
}

/// `NetCDFParser`'s list of the variables: a heading, then each variable's declaration with its
/// attributes in a list of their own.
pub(crate) fn write_variables(xhtml: &mut Xhtml, variables: &[Variable]) -> Result<(), Stop> {
    xhtml.element("h1", "variables")?;
    xhtml.start_element("ul")?;
    xhtml.newline()?;
    for variable in variables {
        xhtml.start_element("li")?;
        xhtml.characters(&variable.declaration())?;
        xhtml.newline()?;
        if !variable.attributes.is_empty() {
            xhtml.start_element("ul")?;
            for attribute in &variable.attributes {
                xhtml.element("li", &attribute.cdl())?;
            }
            xhtml.end_element("ul")?;
        }
        xhtml.end_element("li")?;
    }
    xhtml.end_element("ul")
}

/// `NetCDFParser.addGlobalAttribute` and `GribParser`'s: a global attribute `title` as the
/// title, one `ClimateForecast` names as its `cf:` property, and any other under `prefix`.
pub(crate) fn add_global_attribute(
    metadata: &mut TikaMetadata,
    prefix: &str,
    attribute: &Attribute,
) {
    let values = attribute.metadata_values();
    if attribute.name == "title" {
        for value in values {
            metadata.add(TITLE, value);
        }
        return;
    }
    let climate = CLIMATE_FORECAST
        .iter()
        .find(|(name, _)| *name == attribute.name);
    match climate {
        Some((name, true)) => {
            for value in values {
                metadata.add(&format!("cf:{name}"), value);
            }
        }
        Some((name, false)) => {
            if !values.is_empty() {
                metadata.set_values(&format!("cf:{name}"), values);
            }
        }
        None => {
            for value in values {
                metadata.add_prefixed(prefix, &attribute.name, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_attributes_and_variables_as_cdl() {
        let units = Attribute::text("units", "K \"kelvin\"\nabsolute");
        assert_eq!(units.cdl(), "units = \"K \\\"kelvin\\\"\\nabsolute\"");
        let fill = Attribute::numbers(
            "_FillValue",
            DataType::Float,
            vec![Number::Float(1.0e20), Number::Float(0.5)],
        );
        assert_eq!(fill.cdl(), "_FillValue = 1.0E20f, 0.5f");
        let count = Attribute::numbers("count", DataType::Int, vec![Number::Signed(-3)]);
        assert_eq!(count.cdl(), "count = -3");
        let reference = Attribute {
            name: "REFERENCE_LIST".to_owned(),
            values: Values::Unwritten(DataType::Structure),
        };
        assert_eq!(reference.cdl(), "REFERENCE_LIST (Structure)");
        assert!(reference.metadata_values().is_empty());
        let variable = Variable {
            data_type: DataType::Float,
            name: "tas".to_owned(),
            axes: vec![
                Axis::Shared(Dimension {
                    name: "time".to_owned(),
                    length: 1,
                }),
                Axis::Anonymous(128),
                Axis::VariableLength,
            ],
            attributes: vec![units],
        };
        assert_eq!(variable.declaration(), "float tas(time=1, 128, *)");
    }

    #[test]
    fn maps_global_attributes_as_net_cdf_parser_does() {
        let mut metadata = TikaMetadata::new();
        for attribute in [
            Attribute::text("title", "Model output"),
            Attribute::text("history", "created"),
            Attribute::text("history", "regridded"),
            Attribute::text("institution", "first"),
            Attribute::text("institution", "NCAR"),
            Attribute::numbers("realization", DataType::Int, vec![Number::Signed(1)]),
            Attribute::numbers("scale", DataType::Double, vec![Number::Double(1.5)]),
        ] {
            add_global_attribute(&mut metadata, "netcdf:", &attribute);
        }
        assert_eq!(metadata.values(TITLE), ["Model output"]);
        assert_eq!(metadata.values("cf:history"), ["created", "regridded"]);
        assert_eq!(metadata.values("cf:institution"), ["NCAR"]);
        assert_eq!(metadata.values("cf:realization"), ["1"]);
        assert_eq!(metadata.values("netcdf:scale"), ["1.5"]);
    }
}
