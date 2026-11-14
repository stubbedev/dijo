//! The Universal Disk Image Format: the `koly` trailer of a disk image's last 512 bytes, which
//! places its data fork and its property list, and the property list's `blkx` resources, one
//! `mish` table per partition of the disk the image holds, each mapping runs of the
//! partition's 512-byte sectors to chunks of the data fork: zeros, stored bytes, or bytes
//! coded with ADC, zlib, bzip2, LZFSE or LZMA. An image old enough to keep its resources in a
//! resource fork rather than a property list has them read from there.
//!
//! A disk image with no trailer is read as a raw disk, its bytes the disk's sectors; one that
//! is encrypted or sparse is told apart and not read.

use crate::java::base64_mime_decode;
use crate::xml::{self, Event, Flow};

/// The bytes of a sector of the disk an image holds.
pub(super) const SECTOR: u64 = 512;
const TRAILER: usize = 512;
const MISH_HEADER: usize = 204;
const MISH_CHUNK: usize = 40;

/// How a chunk of an image holds its sectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Coding {
    /// As zeros, written or not.
    Zeros,
    /// As they are.
    Stored,
    Adc,
    Zlib,
    Bzip2,
    Lzfse,
    /// Coded in a way the parser does not decode, as this names it.
    Undecoded(&'static str),
    /// Coded by a method of this number, which the format does not define.
    Unknown(u32),
}

impl Coding {
    fn of(kind: u32) -> Self {
        match kind {
            0 | 2 => Coding::Zeros,
            1 => Coding::Stored,
            0x8000_0004 => Coding::Adc,
            0x8000_0005 => Coding::Zlib,
            0x8000_0006 => Coding::Bzip2,
            0x8000_0007 => Coding::Lzfse,
            0x8000_0008 => Coding::Undecoded("LZMA (ULMO)"),
            other => Coding::Unknown(other),
        }
    }
}

/// A run of the disk's sectors and where the image holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Chunk {
    /// The first sector of the run.
    pub(super) sector: u64,
    pub(super) sectors: u64,
    pub(super) coding: Coding,
    /// Where its bytes are in the image, and how many there are: none for zeros.
    pub(super) offset: usize,
    pub(super) length: usize,
}

impl Chunk {
    /// The sector after the run.
    pub(super) fn end(&self) -> u64 {
        self.sector + self.sectors
    }
}

/// A partition of the disk, as the image names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Partition {
    pub(super) name: String,
    pub(super) sector: u64,
    pub(super) sectors: u64,
}

/// What a disk image holds: the chunks of its disk, in the order of their sectors, and the
/// partitions it names, none for a raw disk.
#[derive(Debug)]
pub(super) struct Image {
    pub(super) chunks: Vec<Chunk>,
    pub(super) partitions: Vec<Partition>,
}

/// Why an image is not read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unread {
    /// It is encrypted.
    Encrypted,
    /// It is of a kind the parser does not read, as this says.
    Unsupported(String),
    /// It is malformed, as this says.
    Malformed(String),
}

fn malformed<T>(reason: impl Into<String>) -> Result<T, Unread> {
    Err(Unread::Malformed(reason.into()))
}

/// The chunks and partitions of the disk image `input`.
pub(super) fn read(input: &[u8]) -> Result<Image, Unread> {
    if input.starts_with(b"encrcdsa") || input.ends_with(b"cdsaencr") {
        return Err(Unread::Encrypted);
    }
    if input.starts_with(b"sprs") {
        return Err(Unread::Unsupported(
            "it is a sparse disk image, which dijo does not read".into(),
        ));
    }
    let Some(trailer) = input
        .len()
        .checked_sub(TRAILER)
        .map(|at| &input[at..])
        .filter(|trailer| trailer.starts_with(b"koly"))
    else {
        return Ok(raw(input));
    };
    let koly = Koly::parse(trailer, input.len() - TRAILER)?;
    let resources = if koly.xml.1 > 0 {
        blkx_of_plist(&input[koly.xml.0..koly.xml.0 + koly.xml.1])?
    } else if koly.resource_fork.1 > 0 {
        let (at, length) = koly.resource_fork;
        blkx_of_resource_fork(&input[at..at + length])?
    } else {
        return malformed("its trailer places neither a property list nor a resource fork");
    };
    let mut chunks = Vec::new();
    let mut partitions = Vec::with_capacity(resources.len());
    for (name, mish) in resources {
        let partition = read_mish(&mish, &name, &koly, input.len(), &mut chunks)
            .map_err(|reason| Unread::Malformed(format!("the table of {name}: {reason}")))?;
        partitions.push(partition);
    }
    chunks.sort_unstable_by_key(|chunk| chunk.sector);
    if let Some(pair) = chunks
        .windows(2)
        .find(|pair| pair[0].end() > pair[1].sector)
    {
        return malformed(format!("two of its chunks hold sector {}", pair[1].sector));
    }
    partitions.sort_by_key(|partition| partition.sector);
    Ok(Image { chunks, partitions })
}

/// A raw disk: `input`'s whole sectors, stored.
fn raw(input: &[u8]) -> Image {
    let sectors = input.len() as u64 / SECTOR;
    let chunks = (sectors > 0)
        .then(|| Chunk {
            sector: 0,
            sectors,
            coding: Coding::Stored,
            offset: 0,
            length: (sectors * SECTOR) as usize,
        })
        .into_iter()
        .collect();
    Image {
        chunks,
        partitions: Vec::new(),
    }
}

/// The fields of a `koly` trailer the parser reads, each range checked to lie before the
/// trailer.
struct Koly {
    data_fork: (usize, usize),
    resource_fork: (usize, usize),
    xml: (usize, usize),
}

impl Koly {
    fn parse(trailer: &[u8], before: usize) -> Result<Koly, Unread> {
        let word = |at: usize| u32::from_be_bytes(trailer[at..at + 4].try_into().expect("four"));
        let long = |at: usize| u64::from_be_bytes(trailer[at..at + 8].try_into().expect("eight"));
        let (version, size) = (word(4), word(8));
        if version != 4 || size as usize != TRAILER {
            return malformed(format!(
                "its trailer is of version {version} and {size} bytes, not of version 4 and \
                 {TRAILER} bytes"
            ));
        }
        let range = |what: &str, at: usize| {
            let (offset, length) = (long(at), long(at + 8));
            offset
                .checked_add(length)
                .filter(|&end| end <= before as u64)
                .map(|_| (offset as usize, length as usize))
                .ok_or_else(|| {
                    Unread::Malformed(format!(
                        "its {what} of {length} bytes at byte {offset} runs past its trailer"
                    ))
                })
        };
        Ok(Koly {
            data_fork: range("data fork", 24)?,
            resource_fork: range("resource fork", 40)?,
            xml: range("property list", 216)?,
        })
    }
}

/// The `blkx` resources of the property list `plist`, each its name and data, in order.
fn blkx_of_plist(plist: &[u8]) -> Result<Vec<(String, Vec<u8>)>, Unread> {
    /// An open `dict` or `array`, and the key it is the value of.
    struct Open {
        dict: bool,
        key: Option<String>,
    }
    let mut stack: Vec<Open> = Vec::new();
    let mut key: Option<String> = None;
    let mut text: Option<String> = None;
    let mut entry: (Option<String>, Option<String>) = (None, None);
    let mut resources = Vec::new();
    let mut failure = None;
    let in_entry = |stack: &[Open]| {
        matches!(
            stack,
            [.., Open { dict: true, key: Some(fork) }, Open { dict: false, key: Some(blkx) }, Open { dict: true, .. }]
                if fork == "resource-fork" && blkx == "blkx"
        )
    };
    let parsed = xml::parse(plist, &mut |event| {
        match event {
            Event::Start(name, _) => match name.local_name() {
                kind @ ("dict" | "array") => {
                    let key = if stack.last().is_some_and(|open| open.dict) {
                        key.take()
                    } else {
                        None
                    };
                    stack.push(Open {
                        dict: kind == "dict",
                        key,
                    });
                }
                "key" | "string" | "data" => text = Some(String::new()),
                _ => {}
            },
            Event::Text(piece, _) => {
                if let Some(text) = &mut text {
                    text.push_str(piece);
                }
            }
            Event::End(name) => match name.local_name() {
                "dict" | "array" => {
                    if name.local_name() == "dict" && in_entry(&stack) {
                        match std::mem::take(&mut entry) {
                            (Some(name), Some(data)) => match base64_mime_decode(&data) {
                                Some(data) => resources.push((name, data)),
                                None => {
                                    failure = Some(format!(
                                        "the data of its blkx resource {name} is not base64"
                                    ));
                                    return Flow::Stop;
                                }
                            },
                            (name, _) => {
                                failure = Some(format!(
                                    "its blkx resource {} has no data",
                                    name.as_deref().unwrap_or("without a name")
                                ));
                                return Flow::Stop;
                            }
                        }
                    }
                    stack.pop();
                    key = None;
                }
                "key" => key = text.take(),
                "string" | "data" => {
                    let value = text.take();
                    if in_entry(&stack) {
                        match key.as_deref() {
                            Some("Name") => entry.0 = value,
                            Some("CFName") if entry.0.is_none() => entry.0 = value,
                            Some("Data") => entry.1 = value,
                            _ => {}
                        }
                    }
                    key = None;
                }
                _ => {}
            },
            _ => {}
        }
        Flow::Continue
    });
    if let Err(error) = parsed {
        return malformed(format!("its property list is not well-formed: {error:?}"));
    }
    if let Some(failure) = failure {
        return malformed(failure);
    }
    if resources.is_empty() {
        return malformed("its property list holds no blkx resource");
    }
    Ok(resources)
}

/// The `blkx` resources of the classic Mac OS resource fork `fork`, each its name and data, in
/// the order of the fork's reference list.
fn blkx_of_resource_fork(fork: &[u8]) -> Result<Vec<(String, Vec<u8>)>, Unread> {
    let past = |what: &str| Unread::Malformed(format!("its resource fork's {what} runs past it"));
    let word = |at: usize| {
        fork.get(at..at + 4)
            .map(|bytes| u32::from_be_bytes(bytes.try_into().expect("four")) as usize)
    };
    let half = |at: usize| {
        fork.get(at..at + 2)
            .map(|bytes| usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
    };
    let data = word(0).ok_or_else(|| past("header"))?;
    let map = word(4).ok_or_else(|| past("header"))?;
    let types = map + half(map + 24).ok_or_else(|| past("map"))?;
    let names = map + half(map + 26).ok_or_else(|| past("map"))?;
    let count = half(types)
        .ok_or_else(|| past("type list"))?
        .wrapping_add(1)
        & 0xFFFF;
    for index in 0..count {
        let at = types + 2 + 8 * index;
        if fork.get(at..at + 4).ok_or_else(|| past("type list"))? != b"blkx" {
            continue;
        }
        let resources = half(at + 4).ok_or_else(|| past("type list"))? + 1;
        let references = types + half(at + 6).ok_or_else(|| past("type list"))?;
        let mut found = Vec::with_capacity(resources);
        for resource in 0..resources {
            let reference = references + 12 * resource;
            let name_at = half(reference + 2).ok_or_else(|| past("reference list"))?;
            let offset = word(reference + 4).ok_or_else(|| past("reference list"))? & 0x00FF_FFFF;
            let length = word(data + offset).ok_or_else(|| past("data"))?;
            let bytes = fork
                .get(data + offset + 4..data + offset + 4 + length)
                .ok_or_else(|| past("data"))?;
            let name = if name_at == 0xFFFF {
                format!("blkx resource {resource}")
            } else {
                let at = names + name_at;
                let length = usize::from(*fork.get(at).ok_or_else(|| past("name list"))?);
                let name = fork
                    .get(at + 1..at + 1 + length)
                    .ok_or_else(|| past("name list"))?;
                name.iter().map(|&byte| char::from(byte)).collect()
            };
            found.push((name, bytes.to_vec()));
        }
        return Ok(found);
    }
    malformed("its resource fork holds no blkx resource")
}

/// Reads the `mish` table `mish` of the partition `name` into `chunks`, each placed in the
/// image of `size` bytes as `koly` places its data fork: the partition.
fn read_mish(
    mish: &[u8],
    name: &str,
    koly: &Koly,
    size: usize,
    chunks: &mut Vec<Chunk>,
) -> Result<Partition, String> {
    if mish.len() < MISH_HEADER || &mish[..4] != b"mish" {
        return Err("it is no mish table".into());
    }
    let word = |at: usize| u32::from_be_bytes(mish[at..at + 4].try_into().expect("four"));
    let long = |at: usize| u64::from_be_bytes(mish[at..at + 8].try_into().expect("eight"));
    let (first, sectors, base) = (long(8), long(16), long(24));
    let count = word(200) as usize;
    let table = mish
        .get(MISH_HEADER..MISH_HEADER + count * MISH_CHUNK)
        .ok_or_else(|| format!("its {count} chunks run past it"))?;
    let base = (koly.data_fork.0 as u64)
        .checked_add(base)
        .ok_or("its data offset is out of range")?;
    for entry in table.as_chunks::<MISH_CHUNK>().0 {
        let word = |at: usize| u32::from_be_bytes(entry[at..at + 4].try_into().expect("four"));
        let long = |at: usize| u64::from_be_bytes(entry[at..at + 8].try_into().expect("eight"));
        let kind = word(0);
        match kind {
            0xFFFF_FFFF => break,
            0x7FFF_FFFE => continue,
            _ => {}
        }
        let (start, run, offset, length) = (long(8), long(16), long(24), long(32));
        if run == 0 {
            continue;
        }
        let sector = first
            .checked_add(start)
            .filter(|sector| sector.checked_add(run).is_some())
            .ok_or_else(|| format!("its chunk at sector {start} is out of range"))?;
        let coding = Coding::of(kind);
        let (offset, length) = match coding {
            Coding::Zeros => (0, 0),
            _ => {
                let at = base
                    .checked_add(offset)
                    .filter(|at| at.checked_add(length).is_some_and(|end| end <= size as u64))
                    .ok_or_else(|| {
                        format!(
                            "its chunk of {length} bytes at byte {offset} of the data fork runs \
                             past the image"
                        )
                    })?;
                (at as usize, length as usize)
            }
        };
        if coding == Coding::Stored && run.checked_mul(SECTOR) != Some(length as u64) {
            return Err(format!(
                "its stored chunk at sector {start} holds {length} bytes for {run} sectors"
            ));
        }
        chunks.push(Chunk {
            sector,
            sectors: run,
            coding,
            offset,
            length,
        });
    }
    Ok(Partition {
        name: name.to_owned(),
        sector: first,
        sectors,
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// `bytes` in base64, as a property list's `data` holds them.
    fn base64(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut text = String::new();
        for group in bytes.chunks(3) {
            let mut padded = [0u8; 3];
            padded[..group.len()].copy_from_slice(group);
            let bits = u32::from_be_bytes([0, padded[0], padded[1], padded[2]]);
            for digit in 0..4 {
                if digit <= group.len() {
                    text.push(char::from(DIGITS[(bits >> (18 - 6 * digit) & 63) as usize]));
                } else {
                    text.push('=');
                }
            }
        }
        text
    }

    /// A chunk as an image's `mish` table gives it: its coding, its first sector in its
    /// partition, its sectors and its bytes in the data fork.
    pub(crate) struct Run {
        pub(crate) kind: u32,
        pub(crate) sector: u64,
        pub(crate) sectors: u64,
        pub(crate) data: Vec<u8>,
    }

    /// A `mish` table of the partition of `sectors` sectors at `first`, of `runs`, whose bytes
    /// start at `offset` of the data fork.
    fn mish(first: u64, sectors: u64, runs: &[Run], mut offset: u64) -> Vec<u8> {
        let mut table = b"mish".to_vec();
        table.extend_from_slice(&1u32.to_be_bytes());
        table.extend_from_slice(&first.to_be_bytes());
        table.extend_from_slice(&sectors.to_be_bytes());
        table.extend_from_slice(&0u64.to_be_bytes());
        table.resize(200, 0);
        table.extend_from_slice(&(runs.len() as u32 + 1).to_be_bytes());
        for run in runs {
            table.extend_from_slice(&run.kind.to_be_bytes());
            table.extend_from_slice(&0u32.to_be_bytes());
            table.extend_from_slice(&run.sector.to_be_bytes());
            table.extend_from_slice(&run.sectors.to_be_bytes());
            table.extend_from_slice(&offset.to_be_bytes());
            table.extend_from_slice(&(run.data.len() as u64).to_be_bytes());
            offset += run.data.len() as u64;
        }
        table.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        table.extend_from_slice(&[0; 4]);
        table.extend_from_slice(&sectors.to_be_bytes());
        table.extend_from_slice(&[0; 16]);
        table.extend_from_slice(&offset.to_be_bytes());
        table
    }

    /// A UDIF image of `partitions`, each its name, first sector and runs, its block map in a
    /// property list.
    pub(crate) fn image(partitions: &[(&str, u64, Vec<Run>)]) -> Vec<u8> {
        let mut fork = Vec::new();
        let mut plist = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \
             \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n\t<key>resource-fork</key>\n\t<dict>\n\t\t\
             <key>blkx</key>\n\t\t<array>\n",
        );
        for (name, first, runs) in partitions {
            let sectors = runs
                .iter()
                .map(|run| run.sector + run.sectors)
                .max()
                .unwrap_or(0);
            let table = mish(*first, sectors, runs, fork.len() as u64);
            for run in runs {
                fork.extend_from_slice(&run.data);
            }
            plist.push_str(&format!(
                "\t\t\t<dict>\n\t\t\t\t<key>Attributes</key>\n\t\t\t\t<string>0x0050</string>\n\
                 \t\t\t\t<key>Data</key>\n\t\t\t\t<data>\n\t\t\t\t{}\n\t\t\t\t</data>\n\
                 \t\t\t\t<key>ID</key>\n\t\t\t\t<string>-1</string>\n\
                 \t\t\t\t<key>Name</key>\n\t\t\t\t<string>{name}</string>\n\t\t\t</dict>\n",
                base64(&table)
            ));
        }
        plist.push_str("\t\t</array>\n\t</dict>\n</dict>\n</plist>\n");
        let xml_at = fork.len() as u64;
        let mut image = fork;
        image.extend_from_slice(plist.as_bytes());
        image.extend(koly(xml_at, plist.len() as u64, xml_at, (0, 0)));
        image
    }

    /// A `koly` trailer of a data fork of `data_fork` bytes, its property list of `xml_length`
    /// bytes at `xml_at` and its resource fork at `resource_fork`.
    pub(crate) fn koly(
        data_fork: u64,
        xml_length: u64,
        xml_at: u64,
        resource_fork: (u64, u64),
    ) -> Vec<u8> {
        let mut trailer = b"koly".to_vec();
        trailer.extend_from_slice(&4u32.to_be_bytes());
        trailer.extend_from_slice(&512u32.to_be_bytes());
        trailer.extend_from_slice(&1u32.to_be_bytes());
        trailer.extend_from_slice(&0u64.to_be_bytes());
        trailer.extend_from_slice(&0u64.to_be_bytes());
        trailer.extend_from_slice(&data_fork.to_be_bytes());
        trailer.extend_from_slice(&resource_fork.0.to_be_bytes());
        trailer.extend_from_slice(&resource_fork.1.to_be_bytes());
        trailer.resize(216, 0);
        trailer.extend_from_slice(&xml_at.to_be_bytes());
        trailer.extend_from_slice(&xml_length.to_be_bytes());
        trailer.resize(512, 0);
        trailer
    }

    fn malformed_reason(input: &[u8]) -> String {
        match read(input) {
            Err(Unread::Malformed(reason)) => reason,
            other => panic!("not malformed: {other:?}"),
        }
    }

    fn stored(sector: u64, sectors: u64, byte: u8) -> Run {
        Run {
            kind: 1,
            sector,
            sectors,
            data: vec![byte; (sectors * SECTOR) as usize],
        }
    }

    #[test]
    fn reads_the_chunks_of_each_partition_in_sector_order() {
        let input = image(&[
            (
                "disk image (Apple_HFS : 1)",
                4,
                vec![
                    stored(0, 2, b'h'),
                    Run {
                        kind: 0x8000_0005,
                        sector: 2,
                        sectors: 8,
                        data: b"zlib".to_vec(),
                    },
                ],
            ),
            (
                "Driver Descriptor Map (DDM : 0)",
                0,
                vec![
                    stored(0, 1, b'd'),
                    Run {
                        kind: 2,
                        sector: 1,
                        sectors: 3,
                        data: Vec::new(),
                    },
                    Run {
                        kind: 0x7FFF_FFFE,
                        sector: 4,
                        sectors: 0,
                        data: Vec::new(),
                    },
                ],
            ),
        ]);
        let image = read(&input).unwrap();
        let names: Vec<(&str, u64, u64)> = image
            .partitions
            .iter()
            .map(|partition| (partition.name.as_str(), partition.sector, partition.sectors))
            .collect();
        assert_eq!(
            names,
            [
                ("Driver Descriptor Map (DDM : 0)", 0, 4),
                ("disk image (Apple_HFS : 1)", 4, 10),
            ]
        );
        let chunks: Vec<(u64, u64, Coding, usize, usize)> = image
            .chunks
            .iter()
            .map(|chunk| {
                (
                    chunk.sector,
                    chunk.sectors,
                    chunk.coding,
                    chunk.offset,
                    chunk.length,
                )
            })
            .collect();
        assert_eq!(
            chunks,
            [
                (0, 1, Coding::Stored, 1028, 512),
                (1, 3, Coding::Zeros, 0, 0),
                (4, 2, Coding::Stored, 0, 1024),
                (6, 8, Coding::Zlib, 1024, 4),
            ]
        );
    }

    #[test]
    fn reads_the_block_map_of_a_resource_fork() {
        let table = mish(0, 1, &[stored(0, 1, b'r')], 0);
        let name = b"\x0Ewhole disk (0)";
        let mut fork = vec![0u8; 256];
        let data_at = 256usize;
        let mut data = (table.len() as u32).to_be_bytes().to_vec();
        data.extend_from_slice(&table);
        let map_at = data_at + data.len();
        let mut map = vec![0u8; 28];
        map[24..26].copy_from_slice(&28u16.to_be_bytes());
        map[26..28].copy_from_slice(&(28u16 + 2 + 8 * 2 + 12).to_be_bytes());
        map.extend_from_slice(&1u16.to_be_bytes());
        map.extend_from_slice(b"plst\0\0\0\0");
        map.extend_from_slice(b"blkx\0\0");
        map.extend_from_slice(&(2u16 + 8 * 2).to_be_bytes());
        map.extend_from_slice(&[0xFF, 0xFE, 0, 0]);
        map.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        map.extend_from_slice(name);
        fork[0..4].copy_from_slice(&(data_at as u32).to_be_bytes());
        fork[4..8].copy_from_slice(&(map_at as u32).to_be_bytes());
        fork.extend_from_slice(&data);
        fork.extend_from_slice(&map);
        let mut input = vec![b'r'; 512];
        let fork_at = input.len() as u64;
        input.extend_from_slice(&fork);
        input.extend(koly(512, 0, 0, (fork_at, fork.len() as u64)));
        let image = read(&input).unwrap();
        assert_eq!(image.partitions[0].name, "whole disk (0)");
        assert_eq!(image.chunks[0].length, 512);
    }

    #[test]
    fn tells_raw_encrypted_and_sparse_images_apart() {
        let raw = read(&[7u8; 1500]).unwrap();
        assert_eq!(raw.partitions, []);
        assert_eq!(raw.chunks.len(), 1);
        assert_eq!((raw.chunks[0].sectors, raw.chunks[0].length), (2, 1024));
        assert_eq!(
            read(b"encrcdsa and the rest").unwrap_err(),
            Unread::Encrypted
        );
        let mut v1 = vec![0u8; 600];
        v1.extend_from_slice(b"cdsaencr");
        assert_eq!(read(&v1).unwrap_err(), Unread::Encrypted);
        assert!(matches!(read(b"sprs...."), Err(Unread::Unsupported(_))));
    }

    #[test]
    fn refuses_trailers_tables_and_chunks_out_of_bounds() {
        let good = image(&[("whole", 0, vec![stored(0, 1, b'a')])]);
        let trailer = good.len() - 512;
        let mut version = good.clone();
        version[trailer + 7] = 3;
        assert!(malformed_reason(&version).contains("of version 3"));
        let mut xml = good.clone();
        xml[trailer + 224..trailer + 232].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(malformed_reason(&xml).contains("property list of 18446744073709551615 bytes"));
        let zlib = Run {
            kind: 0x8000_0005,
            sector: 0,
            sectors: 1,
            data: vec![0; 10],
        };
        let koly = Koly {
            data_fork: (0, 50),
            resource_fork: (0, 0),
            xml: (0, 0),
        };
        assert!(
            read_mish(&mish(0, 1, &[zlib], 45), "p", &koly, 50, &mut Vec::new())
                .unwrap_err()
                .contains("of 10 bytes at byte 45 of the data fork runs past the image")
        );
        assert!(read_mish(b"mash", "p", &koly, 50, &mut Vec::new())
            .unwrap_err()
            .contains("no mish table"));
        let short = image(&[(
            "whole",
            0,
            vec![Run {
                kind: 1,
                sector: 0,
                sectors: 2,
                data: vec![0; 512],
            }],
        )]);
        assert!(malformed_reason(&short).contains("holds 512 bytes for 2 sectors"));
        let overlapping = image(&[
            ("first", 0, vec![stored(0, 4, b'a')]),
            ("second", 2, vec![stored(0, 1, b'b')]),
        ]);
        assert!(malformed_reason(&overlapping).contains("two of its chunks hold sector 2"));
        let mut unparsed = good.clone();
        let at = unparsed
            .windows(6)
            .position(|window| window == b"<dict>")
            .unwrap();
        unparsed[at + 1] = b'!';
        assert!(malformed_reason(&unparsed).contains("not well-formed"));
    }
}
