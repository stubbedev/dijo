//! HDF4 and HDF5 files, as Tika's `HDFParser` reads them through netcdf-java: the file type and
//! the attributes of every group as metadata. Tika's path leaves them empty, as its native image
//! lacks the scientific module (#172). Where `HDFParser` writes an empty document, the native
//! parser goes beyond it and lists the variables, the datasets, with their shapes and
//! attributes, as `NetCDFParser` lists a NetCDF file's, and an HDF4 file's dimensions before
//! them. Only the headers are read, in place, never the data.
//!
//! Each value of an attribute is written, where `HDFParser` writes the first only.

mod hdf4;
mod hdf5;

use crate::config::ParserConfigs;
use crate::formats::bytes::Bytes;
use crate::formats::cdm::{write_dimensions, write_variables, Attribute, Dimension, Variable};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Footprint, Parser, Prepared};

const MEDIA_TYPE: &str = "application/x-hdf";
/// `HDFParser.FILE_TYPE_DESCRIPTION`.
const FILE_TYPE_DESCRIPTION: &str = "hdf:file-type-description";
/// `HDFParser.HDF`, the prefix of the attributes' names.
const HDF: &str = "hdf:";

/// Tika's `HDFParser`.
pub struct HdfParser;

impl Parser for HdfParser {
    fn name(&self) -> &'static str {
        "extractous::HDFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.hdf.HDFParser"
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
        let bytes = Bytes::new(input);
        let (version, contents) = if input.starts_with(hdf4::MAGIC) {
            (Version::Four, hdf4::read(bytes)?)
        } else {
            (Version::Five, hdf5::read(bytes)?)
        };
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set(FILE_TYPE_DESCRIPTION, version.description());
            for attribute in &contents.attributes {
                for value in attribute.metadata_values() {
                    metadata.add_prefixed(HDF, &attribute.name, value);
                }
            }
            xhtml.newline()?;
            if version == Version::Four {
                write_dimensions(xhtml, &contents.dimensions)?;
            }
            write_variables(xhtml, &contents.variables)?;
            Ok(())
        }))
    }
}

/// The version of the format, by the file's signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    Four,
    Five,
}

impl Version {
    /// `getFileTypeDescription` of netcdf-java's HDF4 and HDF5 readers.
    fn description(self) -> &'static str {
        match self {
            Self::Four => "Hierarchical Data Format, version 4",
            Self::Five => "Hierarchical Data Format, version 5",
        }
    }
}

/// What a file holds: the attributes of its groups, its shared dimensions and its variables.
#[derive(Debug, Default)]
struct Contents {
    attributes: Vec<Attribute>,
    dimensions: Vec<Dimension>,
    variables: Vec<Variable>,
}

#[cfg(test)]
mod tests;
