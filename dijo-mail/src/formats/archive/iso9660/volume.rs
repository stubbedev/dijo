//! The volume recognition area of an image, from sector 16 on (ECMA-119 8, ECMA-167 2/9): the
//! ISO 9660 volume descriptors, the primary one and a Joliet supplementary one, and the UDF
//! descriptors that tell an image of UDF alone.

use super::dates;
use super::directory::{Encoding, Record};
use crate::formats::metadata::TikaMetadata;

/// The bytes of a logical sector, which holds one volume descriptor.
pub(super) const SECTOR: usize = 2048;
const FIRST_DESCRIPTOR: usize = 16;

const PRIMARY: u8 = 1;
const SUPPLEMENTARY: u8 = 2;
const TERMINATOR: u8 = 255;
/// The escape sequences of a Joliet supplementary volume descriptor, for its three levels of
/// UCS-2.
const JOLIET_ESCAPES: [&[u8; 3]; 3] = [b"%/@", b"%/C", b"%/E"];

/// The fields of a descriptor that become metadata, by their ranges in the sector.
const VOLUME: (usize, usize) = (40, 72);
const PUBLISHER: (usize, usize) = (318, 446);
const PREPARER: (usize, usize) = (446, 574);
const APPLICATION: (usize, usize) = (574, 702);
const CREATED: usize = 813;
const MODIFIED: usize = 830;

/// What the recognition area of an image holds.
#[derive(Debug)]
pub(super) enum Recognized<'a> {
    /// An ISO 9660 file system: its primary volume descriptor and, if it has one, a Joliet
    /// supplementary volume descriptor.
    Iso9660 {
        primary: Descriptor<'a>,
        joliet: Option<Descriptor<'a>>,
    },
    /// No ISO 9660 file system, only the descriptors of a UDF one.
    UdfOnly,
}

/// A primary or supplementary volume descriptor, the sector holding it, and how its
/// identifiers are encoded.
#[derive(Debug, Clone, Copy)]
pub(super) struct Descriptor<'a> {
    sector: &'a [u8; SECTOR],
    pub(super) encoding: Encoding,
}

impl<'a> Descriptor<'a> {
    /// The size of a logical block: a power of two from 512 to a sector.
    pub(super) fn block_size(&self) -> Result<u64, String> {
        let size = u16::from_le_bytes([self.sector[128], self.sector[129]]);
        if size.is_power_of_two() && (512..=SECTOR as u16).contains(&size) {
            Ok(u64::from(size))
        } else {
            Err(format!(
                "the volume descriptor gives a logical block of {size} bytes, not a power of two \
                 from 512 to {SECTOR}"
            ))
        }
    }

    /// The record of the root directory.
    pub(super) fn root(&self) -> Result<Record<'a>, String> {
        Record::parse(&self.sector[156..190])
            .map_err(|reason| format!("the root directory's record is unread: {reason}"))
    }

    fn text(&self, (from, to): (usize, usize)) -> Option<String> {
        Some(self.encoding.decode(&self.sector[from..to])).filter(|text| !text.is_empty())
    }

    fn date(&self, at: usize) -> Option<i64> {
        dates::long(self.sector[at..at + 17].try_into().unwrap())
    }

    /// Sets the volume's identifier, publisher, data preparer, application and dates.
    pub(super) fn set_metadata(&self, metadata: &mut TikaMetadata) {
        for (key, field) in [
            ("dc:title", VOLUME),
            ("dc:publisher", PUBLISHER),
            ("dc:creator", PREPARER),
            ("xmp:CreatorTool", APPLICATION),
        ] {
            if let Some(text) = self.text(field) {
                metadata.set(key, text);
            }
        }
        for (key, at) in [("dcterms:created", CREATED), ("dcterms:modified", MODIFIED)] {
            if let Some(millis) = self.date(at) {
                metadata.set(key, crate::java::time::format_instant(millis));
            }
        }
    }
}

/// What the recognition area of `input` holds: the sectors from 16 on whose standard identifier
/// names an ISO 9660 descriptor or the extended area of ECMA-167, up to an ISO 9660 set's
/// terminator, the end of the extended area or the first sector of neither.
pub(super) fn recognize(input: &[u8]) -> Result<Recognized<'_>, String> {
    let mut primary = None;
    let mut joliet = None;
    let mut udf = false;
    let mut iso_ended = false;
    let area = input.get(FIRST_DESCRIPTOR * SECTOR..).unwrap_or(&[]);
    for sector in area.as_chunks::<SECTOR>().0 {
        match &sector[1..6] {
            b"CD001" if iso_ended => {}
            b"CD001" => match sector[0] {
                PRIMARY => {
                    primary.get_or_insert(Descriptor {
                        sector,
                        encoding: Encoding::Iso,
                    });
                }
                SUPPLEMENTARY if JOLIET_ESCAPES.iter().any(|e| sector[88..91] == e[..]) => {
                    joliet.get_or_insert(Descriptor {
                        sector,
                        encoding: Encoding::Joliet,
                    });
                }
                TERMINATOR => iso_ended = true,
                _ => {}
            },
            b"NSR02" | b"NSR03" => udf = true,
            b"BEA01" | b"BOOT2" | b"CDW02" => {}
            _ => break,
        }
    }
    match (primary, udf) {
        (Some(primary), _) => Ok(Recognized::Iso9660 { primary, joliet }),
        (None, true) => Ok(Recognized::UdfOnly),
        (None, false) => Err(format!(
            "no primary volume descriptor in the sectors from {FIRST_DESCRIPTOR} on"
        )),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A sector of the recognition area of `kind` with the standard identifier `identifier`.
    pub(crate) fn structure(kind: u8, identifier: &[u8; 5]) -> Vec<u8> {
        let mut sector = vec![0u8; SECTOR];
        sector[0] = kind;
        sector[1..6].copy_from_slice(identifier);
        sector[6] = 1;
        sector
    }

    fn area(sectors: &[Vec<u8>]) -> Vec<u8> {
        let mut input = vec![0u8; FIRST_DESCRIPTOR * SECTOR];
        for sector in sectors {
            input.extend_from_slice(sector);
        }
        input
    }

    #[test]
    fn tells_udf_alone_and_neither_apart() {
        let udf = area(&[
            structure(0, b"BEA01"),
            structure(0, b"NSR02"),
            structure(0, b"TEA01"),
        ]);
        assert!(matches!(recognize(&udf), Ok(Recognized::UdfOnly)));
        let neither = area(&[structure(0, b"BEA01"), structure(0, b"TEA01")]);
        assert!(recognize(&neither).unwrap_err().contains("no primary"));
        assert!(recognize(b"short").is_err());
    }

    #[test]
    fn takes_a_supplementary_descriptor_for_joliet_by_its_escapes() {
        let mut joliet = structure(SUPPLEMENTARY, b"CD001");
        joliet[88..91].copy_from_slice(b"%/E");
        let plain = structure(SUPPLEMENTARY, b"CD001");
        let input = area(&[
            structure(PRIMARY, b"CD001"),
            plain,
            joliet,
            structure(TERMINATOR, b"CD001"),
        ]);
        let found = recognize(&input);
        let Ok(Recognized::Iso9660 {
            joliet: Some(found),
            ..
        }) = found
        else {
            panic!("no Joliet descriptor: {found:?}");
        };
        assert_eq!(found.sector[88..91], *b"%/E");
    }
}
