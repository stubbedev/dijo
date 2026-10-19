//! GRIB files, editions 1 and 2, in the layout of Tika's `GribParser`: the global attributes as
//! metadata, then a list of the dimensions and of the variables with their attributes. Tika
//! without its scientific module leaves them empty (#172), and
//! `GribParser` itself reads only the type `application/x-grib2`, which Tika's detection never
//! names. The native parser reads the sections of each message in place, never their data, and
//! gathers the messages into variables as netcdf-java does: one for each parameter, level type
//! and grid, over the times and levels its messages hold. It names a variable by its codes, as
//! netcdf-java names a parameter its tables lack, `VAR_0-0-0_L100`, and writes no units: the
//! WMO's tables of parameters are not read.
//!
//! Where `GribParser` writes a variable's data type and name without a space between them, the
//! native parser writes one.

use std::collections::{BTreeMap, BTreeSet};

use memchr::memmem;

use crate::config::ParserConfigs;
use crate::formats::bytes::{malformed, unported, Bytes, Cursor, Order, Read};
use crate::formats::cdm::{
    add_global_attribute, Attribute, Axis, DataType, Dimension, Number, Variable,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Footprint, Parser, Prepared};

const MEDIA_TYPE: &str = "application/x-grib";
/// `GribParser.GRIB`, the prefix of the global attributes' names.
const GRIB: &str = "grib:";

const MAGIC: &[u8; 4] = b"GRIB";
const END: &[u8; 4] = b"7777";
/// The bit of a GRIB 1 message's length that marks the encoding ECMWF uses for messages longer
/// than 8 MB.
const LARGE_MESSAGE: u64 = 0x80_0000;

/// Tika's `GribParser`.
pub struct GribParser;

impl Parser for GribParser {
    fn name(&self) -> &'static str {
        "extractous::GribParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.grib.GribParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let collection = Collection::read(Bytes::new(input))?;
        Ok(Box::new(move |xhtml| {
            let (attributes, dimensions, variables) = collection.model();
            for attribute in &attributes {
                add_global_attribute(xhtml.metadata_mut(), GRIB, attribute);
            }
            write(xhtml, &dimensions, &variables)?;
            Ok(())
        }))
    }
}

/// `GribParser`'s lists: the dimensions as `name=length;`, then each variable's declaration in
/// a paragraph, its attributes after it as ` :name = value;`.
fn write(xhtml: &mut Xhtml, dimensions: &[Dimension], variables: &[Variable]) -> Result<(), Stop> {
    xhtml.newline()?;
    xhtml.start_element("ul")?;
    xhtml.characters("dimensions:")?;
    xhtml.newline()?;
    for dimension in dimensions {
        xhtml.element("li", &format!("{}={};", dimension.name, dimension.length))?;
        xhtml.newline()?;
    }
    xhtml.start_element("ul")?;
    xhtml.characters("variables:")?;
    xhtml.newline()?;
    for variable in variables {
        xhtml.element("p", &format!("{};", variable.declaration()))?;
        for attribute in &variable.attributes {
            xhtml.element("li", &format!(" :{};", attribute.cdl()))?;
            xhtml.newline()?;
        }
    }
    xhtml.end_element("ul")?;
    xhtml.end_element("ul")
}

/// The edition of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Edition {
    One,
    Two,
}

/// What identifies a variable: the parameter and the type of its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Parameter {
    One {
        center: u8,
        subcenter: u8,
        table: u8,
        parameter: u8,
        level_type: u8,
    },
    Two {
        discipline: u8,
        category: u8,
        number: u8,
        level_type: Option<u8>,
    },
}

impl Parameter {
    /// The name netcdf-java gives a parameter its tables lack.
    fn name(self) -> String {
        match self {
            Self::One {
                center,
                subcenter,
                table,
                parameter,
                level_type,
            } => format!("VAR_{center}-{subcenter}-{table}-{parameter}_L{level_type}"),
            Self::Two {
                discipline,
                category,
                number,
                level_type: Some(level_type),
            } => format!("VAR_{discipline}-{category}-{number}_L{level_type}"),
            Self::Two {
                discipline,
                category,
                number,
                level_type: None,
            } => format!("VAR_{discipline}-{category}-{number}"),
        }
    }

    fn attributes(self) -> Vec<Attribute> {
        let int = |name: &str, values: &[u8]| {
            Attribute::numbers(
                name,
                DataType::Int,
                values
                    .iter()
                    .map(|&value| Number::Signed(i64::from(value)))
                    .collect(),
            )
        };
        let mut attributes = vec![Attribute::text("Grib_Variable_Id", self.name())];
        match self {
            Self::One {
                center,
                subcenter,
                table,
                parameter,
                level_type,
            } => {
                attributes.push(int("Grib1_Center", &[center]));
                attributes.push(int("Grib1_Subcenter", &[subcenter]));
                attributes.push(int("Grib1_TableVersion", &[table]));
                attributes.push(int("Grib1_Parameter", &[parameter]));
                attributes.push(int("Grib1_Level_Type", &[level_type]));
            }
            Self::Two {
                discipline,
                category,
                number,
                level_type,
            } => {
                attributes.push(int("Grib2_Parameter", &[discipline, category, number]));
                if let Some(level_type) = level_type {
                    attributes.push(int("Grib2_Level_Type", &[level_type]));
                }
            }
        }
        attributes
    }
}

/// The grid of a message's values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Grid {
    /// Rows and columns of latitude and longitude, regular or Gaussian.
    Geographic { rows: u64, columns: u64 },
    /// Rows and columns of a projection.
    Projected { rows: u64, columns: u64 },
    /// Points of a grid laid out otherwise, as many as the message counts.
    Points(u64),
    /// A grid the message names by number only, or lays out without counting its points.
    Unknown,
}

impl Grid {
    /// The grid's dimensions, their names numbered after the first grid's.
    fn dimensions(self, index: usize) -> Vec<Dimension> {
        let suffix = if index == 0 {
            String::new()
        } else {
            index.to_string()
        };
        let dimension = |name: &str, length| Dimension {
            name: format!("{name}{suffix}"),
            length,
        };
        match self {
            Self::Geographic { rows, columns } => {
                vec![dimension("lat", rows), dimension("lon", columns)]
            }
            Self::Projected { rows, columns } => {
                vec![dimension("y", rows), dimension("x", columns)]
            }
            Self::Points(points) => vec![dimension("points", points)],
            Self::Unknown => Vec::new(),
        }
    }
}

/// When a message's values are valid: its reference time, and the forecast time or range after
/// it, as the message encodes them.
type ValidTime = (Edition, [u16; 6], [u32; 4]);

/// The messages of one variable.
#[derive(Default)]
struct Gathered {
    levels: BTreeSet<(u32, u32)>,
    times: BTreeSet<ValidTime>,
}

/// The messages of a file, gathered into variables.
struct Collection {
    edition: Edition,
    center: u16,
    subcenter: u16,
    table_version: String,
    reference_time: [u16; 6],
    grids: Vec<Grid>,
    /// The variables in the order their first messages come, by grid and parameter.
    order: Vec<(usize, Parameter)>,
    variables: BTreeMap<(usize, Parameter), Gathered>,
    times: BTreeSet<ValidTime>,
}

/// One field of a message: its parameter, level, valid time and grid.
struct Field {
    parameter: Parameter,
    level: (u32, u32),
    time: ValidTime,
    grid: Grid,
}

/// What the first message says of the file.
struct Origin {
    edition: Edition,
    center: u16,
    subcenter: u16,
    table_version: String,
    reference_time: [u16; 6],
}

impl Collection {
    fn read(bytes: Bytes) -> Read<Self> {
        let all = bytes.slice(0, bytes.len(), "the GRIB file")?;
        let mut at = 0usize;
        let mut collection: Option<Self> = None;
        while let Some(found) = memmem::find(&all[at..], MAGIC) {
            let start = (at + found) as u64;
            let (length, origin, fields) = message(bytes, start)?;
            let collection = collection.get_or_insert_with(|| Self::new(origin));
            for field in fields {
                collection.gather(field);
            }
            at = usize::try_from(start + length)
                .map_err(|_| malformed(format!("a GRIB message at {start} of {length} bytes")))?;
        }
        collection.ok_or_else(|| malformed("no GRIB message"))
    }

    fn new(origin: Origin) -> Self {
        Self {
            edition: origin.edition,
            center: origin.center,
            subcenter: origin.subcenter,
            table_version: origin.table_version,
            reference_time: origin.reference_time,
            grids: Vec::new(),
            order: Vec::new(),
            variables: BTreeMap::new(),
            times: BTreeSet::new(),
        }
    }

    fn gather(&mut self, field: Field) {
        let grid = match self.grids.iter().position(|&grid| grid == field.grid) {
            Some(grid) => grid,
            None => {
                self.grids.push(field.grid);
                self.grids.len() - 1
            }
        };
        let key = (grid, field.parameter);
        let gathered = self.variables.entry(key).or_insert_with(|| {
            self.order.push(key);
            Gathered::default()
        });
        gathered.levels.insert(field.level);
        gathered.times.insert(field.time);
        self.times.insert(field.time);
    }

    /// The global attributes, the dimensions and the variables.
    fn model(&self) -> (Vec<Attribute>, Vec<Dimension>, Vec<Variable>) {
        let int = |name: &str, value: u16| {
            Attribute::numbers(name, DataType::Int, vec![Number::Signed(i64::from(value))])
        };
        let [year, month, day, hour, minute, second] = self.reference_time;
        let attributes = vec![
            int("Originating_or_generating_Center", self.center),
            int("Originating_or_generating_Subcenter", self.subcenter),
            Attribute::text("GRIB_table_version", self.table_version.clone()),
            Attribute::text(
                "file_format",
                match self.edition {
                    Edition::One => "GRIB-1",
                    Edition::Two => "GRIB-2",
                },
            ),
            Attribute::text(
                "reference_time",
                format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"),
            ),
        ];
        let time = Dimension {
            name: "time".to_owned(),
            length: self.times.len() as u64,
        };
        let mut dimensions = vec![time.clone()];
        let grids: Vec<Vec<Dimension>> = self
            .grids
            .iter()
            .enumerate()
            .map(|(index, grid)| grid.dimensions(index))
            .collect();
        dimensions.extend(grids.iter().flatten().cloned());
        let variables = self
            .order
            .iter()
            .map(|key| {
                let (grid, parameter) = *key;
                let gathered = &self.variables[key];
                let mut axes = vec![if gathered.times.len() == self.times.len() {
                    Axis::Shared(time.clone())
                } else {
                    Axis::Anonymous(gathered.times.len() as u64)
                }];
                if gathered.levels.len() > 1 {
                    axes.push(Axis::Anonymous(gathered.levels.len() as u64));
                }
                axes.extend(grids[grid].iter().cloned().map(Axis::Shared));
                let suffix = if grid == 0 {
                    String::new()
                } else {
                    format!("_grid{grid}")
                };
                Variable {
                    data_type: DataType::Float,
                    name: format!("{}{suffix}", parameter.name()),
                    axes,
                    attributes: parameter.attributes(),
                }
            })
            .collect();
        (attributes, dimensions, variables)
    }
}

/// The message at `start`: its length, what it says of the file, and its fields.
fn message(bytes: Bytes, start: u64) -> Read<(u64, Origin, Vec<Field>)> {
    let mut indicator = bytes.cursor(start, 8, Order::Big, "a GRIB indicator section")?;
    indicator.seek(7)?;
    match indicator.u8()? {
        1 => grib1(bytes, start),
        2 => grib2(bytes, start),
        edition => Err(indicator.malformed(format!("GRIB edition {edition}"))),
    }
}

/// A GRIB 1 message: its product definition section, and its grid description section if it
/// has one.
fn grib1(bytes: Bytes, start: u64) -> Read<(u64, Origin, Vec<Field>)> {
    let mut indicator = bytes.cursor(start + 4, 3, Order::Big, "a GRIB 1 indicator section")?;
    let length = indicator.uint(3)?;
    if length & LARGE_MESSAGE != 0 {
        return Err(unported(
            "a GRIB 1 message in ECMWF's encoding of large messages",
        ));
    }
    let message = bytes.slice(start, length, "a GRIB 1 message")?;
    if !message.ends_with(END) {
        return Err(malformed(format!(
            "the GRIB 1 message at {start} does not end with 7777"
        )));
    }
    let mut product = section(message, 8, start, "a GRIB 1 product definition section")?;
    let product_length = product.remaining() as u64;
    let bytes_at = |cursor: &mut Cursor, at: usize| -> Read<u8> {
        cursor.seek(at)?;
        cursor.u8()
    };
    let table = bytes_at(&mut product, 3)?;
    let center = bytes_at(&mut product, 4)?;
    let flags = bytes_at(&mut product, 7)?;
    let parameter = bytes_at(&mut product, 8)?;
    let level_type = bytes_at(&mut product, 9)?;
    let level = u32::from(product.u16()?);
    let year_of_century = u16::from(product.u8()?);
    let month = u16::from(product.u8()?);
    let day = u16::from(product.u8()?);
    let hour = u16::from(product.u8()?);
    let minute = u16::from(product.u8()?);
    let unit = u32::from(product.u8()?);
    let first = u32::from(product.u8()?);
    let second = u32::from(product.u8()?);
    let range = u32::from(product.u8()?);
    let century = u16::from(bytes_at(&mut product, 24)?);
    let subcenter = bytes_at(&mut product, 25)?;
    let year = century.saturating_sub(1) * 100 + year_of_century;
    let reference = [year, month, day, hour, minute, 0];
    let grid = if flags & 0x80 != 0 {
        let mut grid = section(
            message,
            8 + product_length as usize,
            start,
            "a GRIB 1 grid description section",
        )?;
        grid.seek(5)?;
        let representation = grid.u8()?;
        let columns = u64::from(grid.u16()?);
        let rows = u64::from(grid.u16()?);
        let regular = columns != 0xFFFF && rows != 0xFFFF;
        match representation {
            0 | 4 | 10 | 14 | 20 | 24 | 30 | 34 if regular => Grid::Geographic { rows, columns },
            1 | 3 | 5 | 13 if regular => Grid::Projected { rows, columns },
            _ => Grid::Unknown,
        }
    } else {
        Grid::Unknown
    };
    let origin = Origin {
        edition: Edition::One,
        center: u16::from(center),
        subcenter: u16::from(subcenter),
        table_version: table.to_string(),
        reference_time: reference,
    };
    let field = Field {
        parameter: Parameter::One {
            center,
            subcenter,
            table,
            parameter,
            level_type,
        },
        level: (level, 0),
        time: (Edition::One, reference, [unit, first, second, range]),
        grid,
    };
    Ok((length, origin, vec![field]))
}

/// The section of a GRIB 1 message at `at`, of the length its first three bytes give.
fn section<'a>(message: &'a [u8], at: usize, start: u64, what: &'static str) -> Read<Cursor<'a>> {
    let mut header = Cursor::new(
        message.get(at..).unwrap_or_default(),
        start + at as u64,
        Order::Big,
        what,
    );
    let length = header.size(3, "a section's length")?;
    if length < 3 {
        return Err(header.malformed(format!("a section of {length} bytes")));
    }
    header.seek(0)?;
    Ok(Cursor::new(
        header.take(length)?,
        start + at as u64,
        Order::Big,
        what,
    ))
}

/// A GRIB 2 message: its identification section, then each field its product definition
/// sections define, on the grid of the grid definition section before each.
fn grib2(bytes: Bytes, start: u64) -> Read<(u64, Origin, Vec<Field>)> {
    let mut indicator = bytes.cursor(start, 16, Order::Big, "a GRIB 2 indicator section")?;
    indicator.seek(6)?;
    let discipline = indicator.u8()?;
    indicator.u8()?;
    let length = indicator.uint(8)?;
    let message = bytes.slice(start, length, "a GRIB 2 message")?;
    let mut sections = Cursor::new(message, start, Order::Big, "a GRIB 2 message");
    sections.seek(16)?;
    let mut origin = None;
    let mut grid = None;
    let mut fields = Vec::new();
    loop {
        if sections.rest().starts_with(END) {
            break;
        }
        let at = sections.position();
        let section_length = sections.size(4, "a section's length")?;
        let number = sections.u8()?;
        if section_length < 5 {
            return Err(sections.malformed(format!("section {number} of {section_length} bytes")));
        }
        sections.seek(at)?;
        let mut section = Cursor::new(
            sections.take(section_length)?,
            start + at as u64,
            Order::Big,
            "a GRIB 2 section",
        );
        match number {
            1 => origin = Some(identification(&mut section)?),
            3 => grid = Some(grid_definition(&mut section)?),
            4 => {
                let Some(identified) = &origin else {
                    return Err(section.malformed("a product definition before the identification"));
                };
                let Some(grid) = grid else {
                    return Err(section.malformed("a product definition before any grid"));
                };
                fields.push(product(&mut section, discipline, identified, grid)?);
            }
            2 | 5..=7 => {}
            _ => return Err(section.malformed(format!("a section numbered {number}"))),
        }
    }
    let origin = origin.ok_or_else(|| {
        malformed(format!(
            "the GRIB 2 message at {start} has no identification section"
        ))
    })?;
    Ok((length, origin, fields))
}

/// A GRIB 2 identification section: the centre, the tables and the reference time.
fn identification(section: &mut Cursor) -> Read<Origin> {
    section.seek(5)?;
    let center = section.u16()?;
    let subcenter = section.u16()?;
    let master = section.u8()?;
    let local = section.u8()?;
    section.u8()?;
    let year = section.u16()?;
    let mut reference = [year, 0, 0, 0, 0, 0];
    for part in &mut reference[1..] {
        *part = u16::from(section.u8()?);
    }
    Ok(Origin {
        edition: Edition::Two,
        center,
        subcenter,
        table_version: format!("{master},{local}"),
        reference_time: reference,
    })
}

/// A GRIB 2 grid definition section: the rows and columns of the templates that count them
/// where the latitude and longitude grids do, else the number of points.
fn grid_definition(section: &mut Cursor) -> Read<Grid> {
    section.seek(6)?;
    let points = u64::from(section.u32()?);
    section.seek(12)?;
    let template = section.u16()?;
    let dimensions = |section: &mut Cursor| -> Read<(u64, u64)> {
        section.seek(30)?;
        let columns = u64::from(section.u32()?);
        let rows = u64::from(section.u32()?);
        Ok((rows, columns))
    };
    Ok(match template {
        0..=3 | 40..=43 => {
            let (rows, columns) = dimensions(section)?;
            Grid::Geographic { rows, columns }
        }
        10 | 20 | 30 | 31 | 90 => {
            let (rows, columns) = dimensions(section)?;
            Grid::Projected { rows, columns }
        }
        _ => Grid::Points(points),
    })
}

/// A GRIB 2 product definition section: the parameter, and for the templates laid out as
/// template 4.0 is, the forecast time and the first fixed surface.
fn product(section: &mut Cursor, discipline: u8, origin: &Origin, grid: Grid) -> Read<Field> {
    section.seek(7)?;
    let template = section.u16()?;
    let category = section.u8()?;
    let number = section.u8()?;
    let shift = match template {
        0..=15 => Some(0),
        40..=43 => Some(2),
        _ => None,
    };
    let (level_type, level, forecast) = match shift {
        Some(shift) => {
            section.seek(17 + shift)?;
            let unit = u32::from(section.u8()?);
            let forecast = section.u32()?;
            let surface = section.u8()?;
            let scale = u32::from(section.u8()?);
            let value = section.u32()?;
            let level_type = (surface != 255).then_some(surface);
            (level_type, (scale, value), [unit, forecast, 0, 0])
        }
        None => (None, (0, 0), [0; 4]),
    };
    Ok(Field {
        parameter: Parameter::Two {
            discipline,
            category,
            number,
            level_type,
        },
        level,
        time: (Edition::Two, origin.reference_time, forecast),
        grid,
    })
}

#[cfg(test)]
mod tests;
