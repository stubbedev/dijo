//! AutoCAD drawings, as Tika's `DWGParser` reads them without a `dwgread` program: the
//! drawing's summary properties as metadata and paragraphs, found in the property section the
//! header points to (2004 and later) or after the `DWGPROPS COOKIE` marker (2000), and its
//! custom properties. Where Tika's parser slips (#127), this one reads on: the marker is found
//! wherever it starts, an empty string is empty, a 2000 property of an index without a name is
//! written as text alone, and a second last author replaces the first.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
use crate::java;
use crate::text::charset::Charset;

/// The media type `DWGParser` sets as the content type.
const DWG_TYPE: &str = "image/vnd.dwg";
/// `DWG_CUSTOM_META_PREFIX`.
const CUSTOM_PREFIX: &str = "dwg-custom:";
/// The header `DWGParser` reads first.
const HEADER_LENGTH: usize = 128;
/// The farthest offset of the property section `skipToPropertyInfoSection` follows.
const MAX_SECTION_OFFSET: i64 = 0xa0_0000;
/// `HEADER_2000_PROPERTIES_MARKER`.
const MARKER_2000: &[u8] = b"DWGPROPS COOKIE";
/// `CUSTOM_PROPERTIES_SKIP`.
const CUSTOM_PROPERTIES_SKIP: usize = 20;
/// The most properties `get2000Props` reads.
const MAX_2000_PROPERTIES: usize = 30;

/// A summary property and the metadata name `DWGParser` gives it, with whether that name takes
/// more than one value, as its Tika `Property` does.
#[derive(Debug, Clone, Copy)]
struct Field {
    name: &'static str,
    bag: bool,
}

const TITLE: Field = Field {
    name: "dc:title",
    bag: true,
};
const DESCRIPTION: Field = Field {
    name: "dc:description",
    bag: true,
};
const CREATOR: Field = Field {
    name: "dc:creator",
    bag: true,
};
const SUBJECT: Field = Field {
    name: "dc:subject",
    bag: true,
};
const COMMENTS: Field = Field {
    name: "w:Comments",
    bag: true,
};
const MODIFIER: Field = Field {
    name: "meta:last-author",
    bag: false,
};
const RELATION: Field = Field {
    name: "dc:relation",
    bag: true,
};

/// `HEADER_PROPERTIES_ENTRIES`: the summary properties of a 2004 or later drawing, in order.
const HEADER_PROPERTIES: [Option<Field>; 8] = [
    Some(TITLE),
    Some(DESCRIPTION),
    Some(CREATOR),
    Some(SUBJECT),
    Some(COMMENTS),
    Some(MODIFIER),
    None,
    Some(RELATION),
];

/// `HEADER_2000_PROPERTIES_ENTRIES`: the summary properties of a 2000 drawing, by index.
const HEADER_2000_PROPERTIES: [Option<Field>; 9] = [
    None,
    Some(RELATION),
    Some(TITLE),
    Some(DESCRIPTION),
    Some(CREATOR),
    None,
    Some(COMMENTS),
    Some(SUBJECT),
    Some(MODIFIER),
];

/// How the strings of a drawing's property section are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strings {
    /// 2004: a byte each, as ISO-8859-1.
    Bytes,
    /// 2007 and later: UTF-16LE.
    Wide,
}

/// Tika's `DWGParser`.
pub struct DwgParser;

impl Parser for DwgParser {
    fn name(&self) -> &'static str {
        "dijo::DWGParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.dwg.DWGParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[DWG_TYPE]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        Ok(Box::new(move |xhtml| {
            parse(&mut Stream::new(input, pages), xhtml)
        }))
    }
}

/// The drawing as `DWGParser` reads its stream, whose pages it releases as it reads on.
struct Stream<'a> {
    data: &'a [u8],
    at: usize,
    paging: Paging<'a>,
}

impl<'a> Stream<'a> {
    fn new(data: &'a [u8], pages: Pages<'a>) -> Self {
        Self {
            data,
            at: 0,
            paging: Paging::new(pages),
        }
    }

    /// `InputStream.read()`: a byte, or `None` at the end, where Java reads -1.
    fn read(&mut self) -> Option<u8> {
        let byte = self.data.get(self.at).copied()?;
        self.at += 1;
        self.paging.read(1);
        Some(byte)
    }

    /// `IOUtils.readFully`.
    fn read_fully(&mut self, length: usize) -> Result<&'a [u8], Failure> {
        let read = self
            .data
            .get(self.at..self.at.saturating_add(length))
            .ok_or_else(|| {
                Failure::Malformed(format!(
                    "EOFException: Length to read: {length} actual: {}",
                    self.data.len() - self.at
                ))
            })?;
        self.at += length;
        self.paging.read(length);
        Ok(read)
    }

    /// `EndianUtils.readUShortLE`.
    fn u16(&mut self) -> Result<u16, Failure> {
        let low = self.read();
        let high = self.read();
        match (low, high) {
            (Some(low), Some(high)) => Ok(u16::from_le_bytes([low, high])),
            _ => Err(Failure::Malformed(
                "BufferUnderrunException: the drawing ends inside a number".into(),
            )),
        }
    }

    /// `IOUtils.skipFully`.
    fn skip_fully(&mut self, length: i64) -> Result<(), Failure> {
        let length = usize::try_from(length).map_err(|_| {
            Failure::Malformed(format!(
                "IllegalArgumentException: Bytes to skip must not be negative: {length}"
            ))
        })?;
        self.read_fully(length).map(|_| ())
    }
}

fn parse(stream: &mut Stream, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let header = stream.read_fully(HEADER_LENGTH)?;
    let version = java::ascii(&header[..6]);
    match version.as_str() {
        "AC1015" => {
            xhtml.metadata_mut().set("Content-Type", DWG_TYPE);
            if skip_to_2000_properties(stream) {
                properties_2000(stream, xhtml)?;
            }
        }
        "AC1018" | "AC1021" | "AC1024" | "AC1027" | "AC1032" => {
            xhtml.metadata_mut().set("Content-Type", DWG_TYPE);
            let strings = if version == "AC1018" {
                Strings::Bytes
            } else {
                Strings::Wide
            };
            if skip_to_property_section(stream, header)? {
                properties(stream, strings, xhtml)?;
            }
        }
        _ => {
            return Err(Failure::Malformed(format!(
                "Unsupported AutoCAD drawing version: {version}"
            )))
        }
    }
    Ok(())
}

/// `skipToPropertyInfoSection`: whether the header points to a property section, skipped to.
fn skip_to_property_section(stream: &mut Stream, header: &[u8]) -> Result<bool, Failure> {
    let offset = i64::from_le_bytes(header[0x20..0x28].try_into().expect("eight bytes"));
    if offset == 0 || offset > MAX_SECTION_OFFSET {
        return Ok(false);
    }
    stream.skip_fully(offset - HEADER_LENGTH as i64)?;
    Ok(true)
}

/// `skipTo2000PropertyInfoSection`: whether the marker follows, read past, or the stream read
/// to its end. Tika does not try the byte that breaks off a match as the start of another.
fn skip_to_2000_properties(stream: &mut Stream) -> bool {
    let rest = &stream.data[stream.at..];
    match stream.paging.find(rest, MARKER_2000) {
        Some(found) => {
            stream.at += found + MARKER_2000.len();
            true
        }
        None => {
            stream.at = stream.data.len();
            false
        }
    }
}

/// A value of the property `field`: added to those of a property that takes several, in place
/// of the value of one that takes one, where Tika fails on a second.
fn add(metadata: &mut TikaMetadata, field: Field, value: &str) {
    if field.bag {
        metadata.add(field.name, value);
    } else {
        metadata.set(field.name, value);
    }
}

/// `get2000Props`.
fn properties_2000(stream: &mut Stream, xhtml: &mut Xhtml) -> Result<(), Failure> {
    for _ in 0..MAX_2000_PROPERTIES {
        let index = stream.u16()?;
        let mut length = stream.u16()?;
        let kind = stream.read();
        if index == 0x28 {
            length = 0x19;
        } else if index == 90 {
            break;
        }
        let value = stream.read_fully(usize::from(length))?;
        if kind != Some(0x1e) {
            continue;
        }
        let value = java::latin1(value);
        if let Some(entry) = HEADER_2000_PROPERTIES.get(usize::from(index)) {
            if let Some(field) = entry {
                add(xhtml.metadata_mut(), *field, &value);
            }
            xhtml.element("p", &value)?;
        } else if index == 0x012c {
            if let Some((name, value)) = value.split_once('=') {
                xhtml
                    .metadata_mut()
                    .add_prefixed(CUSTOM_PREFIX, name, value);
            }
        }
    }
    Ok(())
}

/// `get2004Props` and `get2007and2010Props`.
fn properties(stream: &mut Stream, strings: Strings, xhtml: &mut Xhtml) -> Result<(), Failure> {
    for field in HEADER_PROPERTIES {
        let value = string(stream, strings)?;
        if value.is_empty() {
            continue;
        }
        if let Some(field) = field {
            xhtml.metadata_mut().set(field.name, value.as_str());
        }
        xhtml.element("p", &value)?;
    }
    for _ in 0..custom_properties(stream)? {
        let name = string(stream, strings)?;
        let value = string(stream, strings)?;
        if !name.is_empty() && !value.is_empty() {
            xhtml
                .metadata_mut()
                .add_prefixed(CUSTOM_PREFIX, &name, value);
        }
    }
    Ok(())
}

/// `read2004String` and `read2007and2010String`: a string after its 16-bit length, its last
/// character dropped when it is zero. Tika fails on an empty one, reading its last character.
fn string(stream: &mut Stream, strings: Strings) -> Result<String, Failure> {
    let length = usize::from(stream.u16()?);
    let mut value = match strings {
        Strings::Bytes => java::latin1(stream.read_fully(length)?),
        Strings::Wide => Charset::named("UTF-16LE")
            .decode(stream.read_fully(length * 2)?)
            .expect("the JDK decoders decode UTF-16LE"),
    };
    if value.ends_with('\0') {
        value.pop();
    }
    Ok(value)
}

/// `skipToCustomProperties`: the number of custom properties, after the padding that says
/// there are any.
fn custom_properties(stream: &mut Stream) -> Result<u16, Failure> {
    let padding = stream.read_fully(4)?;
    if padding != [0, 0, 0, 0] && padding != [2, 0, 0, 0] {
        return Ok(0);
    }
    stream.read_fully(CUSTOM_PROPERTIES_SKIP)?;
    let count = stream.u16()?;
    Ok(if count > 0 && count < 0x7f { count } else { 0 })
}

#[cfg(test)]
mod tests {

    use dijo_core::large::LargeFile;

    use super::*;

    /// A drawing of `version` whose property section, at 0x100, holds `section`.
    fn drawing(version: &[u8; 6], section: &[u8]) -> Vec<u8> {
        let mut file = version.to_vec();
        file.resize(HEADER_LENGTH, 0);
        file[0x20..0x28].copy_from_slice(&0x100i64.to_le_bytes());
        file.resize(0x100, 0);
        file.extend_from_slice(section);
        file
    }

    fn wide(text: &str) -> Vec<u8> {
        let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let mut bytes = (units.len() as u16).to_le_bytes().to_vec();
        bytes.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
        bytes
    }

    fn custom(count: u16) -> Vec<u8> {
        let mut bytes = vec![0; 4 + CUSTOM_PROPERTIES_SKIP];
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes
    }

    fn property_2000(index: u16, kind: u8, value: &[u8]) -> Vec<u8> {
        let mut bytes = index.to_le_bytes().to_vec();
        bytes.extend_from_slice(&(value.len() as u16).to_le_bytes());
        bytes.push(kind);
        bytes.extend_from_slice(value);
        bytes
    }

    /// A 2000 drawing whose header `rest` follows.
    fn drawing_2000(rest: &[u8]) -> Vec<u8> {
        let mut file = b"AC1015".to_vec();
        file.resize(HEADER_LENGTH, 0);
        file.extend_from_slice(rest);
        file
    }

    /// The XHTML body and metadata of a drawing.
    fn extracted(file: &[u8]) -> (String, crate::Metadata) {
        let (xml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(file)
            .unwrap();
        let body = xml[xml.find("<body>").unwrap()..].to_owned();
        (body, metadata)
    }

    /// Where Tika's parser fails or reads past properties, the port reads them (#127).
    #[test]
    fn reads_what_tikas_parser_slips_on() {
        let marker_after_its_first_letter = drawing_2000(
            &[
                &b"junk DDWGPROPS COOKIE"[..],
                &property_2000(2, 0x1e, b"Plan"),
                &property_2000(90, 0, b""),
            ]
            .concat(),
        );
        let (body, metadata) = extracted(&marker_after_its_first_letter);
        assert_eq!(body, "<body><p>Plan</p>\n</body></html>");
        assert_eq!(metadata["dc:title"], ["Plan"]);
        let twice = drawing_2000(
            &[
                &b"DWGPROPS COOKIE"[..],
                &property_2000(8, 0x1e, b"A"),
                &property_2000(8, 0x1e, b"B"),
                &property_2000(5, 0x1e, b"unnamed"),
                &property_2000(90, 0, b""),
            ]
            .concat(),
        );
        let (body, metadata) = extracted(&twice);
        assert_eq!(
            body,
            "<body><p>A</p>\n<p>B</p>\n<p>unnamed</p>\n</body></html>"
        );
        assert_eq!(metadata["meta:last-author"], ["B"]);
        let mut empty = wide("Plan");
        empty.extend_from_slice(&[0, 0]);
        empty.extend(wide("Ana"));
        empty.extend_from_slice(&[0; 10]);
        empty.extend(custom(0));
        let (body, metadata) = extracted(&drawing(b"AC1024", &empty));
        assert_eq!(body, "<body><p>Plan</p>\n<p>Ana</p>\n</body></html>");
        assert_eq!(metadata["dc:creator"], ["Ana"]);
        assert!(!metadata.contains_key("dc:description"));
    }

    /// A 2000 drawing too large to read whole is searched for its properties a window at a
    /// time.
    #[test]
    fn reads_drawings_too_large_to_read_whole() {
        let window = vec![0; 1 << 20];
        let tail = [
            MARKER_2000,
            &property_2000(2, 0x1e, b"Plan"),
            &property_2000(90, 0, b""),
        ]
        .concat();
        let file = LargeFile::new(
            "long.dwg",
            &drawing_2000(b""),
            dijo_core::large::bodies_past_limit(window.len()),
            |_| window.clone(),
            &tail,
        );
        let mut text = String::new();
        let metadata = file.extract_each(DwgParser.name(), |written| text.push_str(written));
        assert_eq!(text.trim(), "Plan");
        assert_eq!(metadata["dc:title"], ["Plan"]);
    }
}
