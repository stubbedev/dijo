//! The directory records of an ISO 9660 image (ECMA-119 9.1), the extents they point to, and
//! the names they carry: ISO 9660 identifiers, their `;1` versions stripped, or Joliet's UCS-2.

use super::dates;
use super::volume::SECTOR;

/// The length of a directory record without its identifier.
const FIXED: usize = 33;
const DIRECTORY: u8 = 0x02;
const MULTI_EXTENT: u8 = 0x80;

/// How the identifiers of a tree are encoded: ISO 9660's d-characters, read byte by byte as
/// ISO 8859-1, which holds them and the extended characters careless writers put there, or
/// Joliet's UCS-2, big-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Encoding {
    Iso,
    Joliet,
}

impl Encoding {
    /// The text of the identifier `bytes`, without the spaces and NULs that pad it.
    pub(super) fn decode(self, bytes: &[u8]) -> String {
        let text: String = match self {
            Encoding::Iso => bytes.iter().map(|&b| char::from(b)).collect(),
            Encoding::Joliet => char::decode_utf16(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&unit| u16::from_be_bytes(unit)),
            )
            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect(),
        };
        text.trim_end_matches([' ', '\0']).to_owned()
    }

    /// The name of a file or directory identified by `identifier`: a file's `;` and version
    /// number stripped, and, in ISO 9660's form, the dot of a name without an extension.
    pub(super) fn name(self, identifier: &[u8], directory: bool) -> String {
        let mut name = self.decode(identifier);
        if directory {
            return name;
        }
        if let Some(separator) = name.rfind(';') {
            if name[separator + 1..].bytes().all(|b| b.is_ascii_digit()) {
                name.truncate(separator);
            }
        }
        if self == Encoding::Iso && name.len() > 1 && name.ends_with('.') {
            name.pop();
        }
        name
    }
}

/// A directory record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record<'a> {
    /// The logical block the extent starts at, its extended attribute record included.
    pub(super) extent: u64,
    /// The logical blocks of the extended attribute record that comes before the data.
    pub(super) attribute_blocks: u64,
    pub(super) length: u64,
    pub(super) recorded: Option<i64>,
    flags: u8,
    /// The interleaving of the extent: its file unit size and interleave gap, in blocks.
    interleaving: (u8, u8),
    pub(super) identifier: &'a [u8],
    pub(super) system_use: &'a [u8],
}

impl<'a> Record<'a> {
    /// The record `bytes` holds, exactly its length long.
    pub(super) fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < FIXED + 1 {
            return Err(format!(
                "a directory record is {} bytes long, shorter than the {} its fields take",
                bytes.len(),
                FIXED + 1
            ));
        }
        let identifier_length = usize::from(bytes[32]);
        let identifier = bytes.get(FIXED..FIXED + identifier_length).ok_or_else(|| {
            format!(
                "a directory record of {} bytes holds an identifier of {identifier_length}",
                bytes.len()
            )
        })?;
        let padded = FIXED + identifier_length + usize::from(identifier_length.is_multiple_of(2));
        let word = |at: usize| u64::from(u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()));
        Ok(Record {
            extent: word(2),
            attribute_blocks: u64::from(bytes[1]),
            length: word(10),
            recorded: dates::short(bytes[18..25].try_into().unwrap()),
            flags: bytes[25],
            interleaving: (bytes[26], bytes[27]),
            identifier,
            system_use: bytes.get(padded..).unwrap_or(&[]),
        })
    }

    pub(super) fn is_directory(&self) -> bool {
        self.flags & DIRECTORY != 0
    }

    /// Whether the file goes on in the next record, in another extent.
    pub(super) fn continues(&self) -> bool {
        self.flags & MULTI_EXTENT != 0
    }

    pub(super) fn is_interleaved(&self) -> bool {
        self.interleaving != (0, 0)
    }

    /// Whether it is the record of the directory itself or of its parent.
    pub(super) fn is_self_or_parent(&self) -> bool {
        matches!(self.identifier, [0] | [1])
    }

    /// The logical block the data starts at, past the extended attribute record.
    pub(super) fn data_block(&self) -> u64 {
        self.extent + self.attribute_blocks
    }
}

/// The `length` bytes of `input` from logical block `block` of `block_size` bytes on.
pub(super) fn extent(
    input: &[u8],
    block: u64,
    block_size: u64,
    length: u64,
) -> Result<&[u8], String> {
    let start = block
        .checked_mul(block_size)
        .ok_or_else(|| format!("its extent at block {block} starts past any image's end"))?;
    span(input, start, length)
}

/// The `length` bytes of `input` from byte `start` on.
pub(super) fn span(input: &[u8], start: u64, length: u64) -> Result<&[u8], String> {
    match start.checked_add(length) {
        Some(end) if end <= input.len() as u64 => Ok(&input[start as usize..end as usize]),
        _ => Err(format!(
            "its extent of {length} bytes at byte {start} ends past the image's end at byte {}",
            input.len()
        )),
    }
}

/// The records of the directory whose data is `data`, in order: none crosses a sector, the
/// rest of a sector after its last record zeros.
pub(super) struct Records<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Records<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Records { data, at: 0 }
    }
}

impl<'a> Iterator for Records<'a> {
    type Item = Result<Record<'a>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let length = usize::from(*self.data.get(self.at)?);
            if length == 0 {
                self.at = (self.at / SECTOR + 1) * SECTOR;
                continue;
            }
            let at = self.at;
            let sector_end = (at / SECTOR + 1) * SECTOR;
            let Some(bytes) = self
                .data
                .get(at..at + length)
                .filter(|_| at + length <= sector_end)
            else {
                self.at = self.data.len();
                return Some(Err(format!(
                    "the directory record of {length} bytes at byte {at} of the directory \
                     crosses the end of its sector or of the directory"
                )));
            };
            self.at += length;
            return Some(Record::parse(bytes).map_err(|reason| {
                self.at = self.data.len();
                format!("at byte {at} of the directory, {reason}")
            }));
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A directory record of `identifier` at `extent`, `length` bytes long, with `flags` and
    /// `system_use`.
    pub(crate) fn record(
        identifier: &[u8],
        extent: u32,
        length: u32,
        flags: u8,
        system_use: &[u8],
    ) -> Vec<u8> {
        let padding = usize::from(identifier.len().is_multiple_of(2));
        let total = FIXED + identifier.len() + padding + system_use.len();
        let mut bytes = vec![0u8; FIXED];
        bytes[0] = u8::try_from(total).expect("a record fits in 255 bytes");
        bytes[2..6].copy_from_slice(&extent.to_le_bytes());
        bytes[6..10].copy_from_slice(&extent.to_be_bytes());
        bytes[10..14].copy_from_slice(&length.to_le_bytes());
        bytes[14..18].copy_from_slice(&length.to_be_bytes());
        bytes[18..25].copy_from_slice(&[120, 6, 15, 12, 30, 0, 0]);
        bytes[25] = flags;
        bytes[28..32].copy_from_slice(&[1, 0, 0, 1]);
        bytes[32] = identifier.len() as u8;
        bytes.extend_from_slice(identifier);
        bytes.resize(FIXED + identifier.len() + padding, 0);
        bytes.extend_from_slice(system_use);
        bytes
    }

    #[test]
    fn strips_versions_and_the_dot_of_names_without_an_extension() {
        assert_eq!(Encoding::Iso.name(b"README.TXT;1", false), "README.TXT");
        assert_eq!(Encoding::Iso.name(b"MAKEFILE.;1", false), "MAKEFILE");
        assert_eq!(Encoding::Iso.name(b"DIR.D", true), "DIR.D");
        assert_eq!(Encoding::Iso.name(b"A;B", false), "A;B");
        let joliet: Vec<u8> = "Caf\u{e9} \u{1F600}.txt;1"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        assert_eq!(
            Encoding::Joliet.name(&joliet, false),
            "Caf\u{e9} \u{1F600}.txt"
        );
    }

    #[test]
    fn reads_records_sector_by_sector_and_refuses_one_crossing_a_sector() {
        let mut data = record(b"\0", 20, 2048, DIRECTORY, b"");
        data.extend(record(b"A.TXT;1", 21, 5, 0, b"XY"));
        data.resize(SECTOR, 0);
        data.extend(record(b"B.TXT;1", 22, 7, MULTI_EXTENT, b""));
        let records: Vec<Record> = Records::new(&data).map(Result::unwrap).collect();
        assert_eq!(records.len(), 3);
        assert!(records[0].is_self_or_parent() && records[0].is_directory());
        assert_eq!(records[1].identifier, b"A.TXT;1");
        assert_eq!(records[1].system_use, b"XY");
        assert_eq!((records[2].extent, records[2].length), (22, 7));
        assert!(records[2].continues());
        let mut crossing = Vec::new();
        for _ in 0..11 {
            crossing.extend(record(b"C.TXT;1", 23, 1, 0, &[0; 160]));
        }
        let read: Vec<_> = Records::new(&crossing).collect();
        assert_eq!(read.len(), 11);
        assert!(matches!(&read[10], Err(reason) if reason.contains("crosses")));
        let cut = record(b"D.TXT;1", 23, 1, 0, b"");
        let failed: Vec<_> = Records::new(&cut[..cut.len() - 1]).collect();
        assert!(matches!(&failed[..], [Err(reason)] if reason.contains("crosses")));
    }

    #[test]
    fn refuses_extents_past_the_end() {
        let input = [0u8; 4096];
        assert_eq!(extent(&input, 1, 2048, 2048).unwrap().len(), 2048);
        assert!(extent(&input, 1, 2048, 2049).is_err());
        assert!(extent(&input, u64::MAX, 2048, 1).is_err());
    }
}
