//! The partitions of a raw disk, which names none of its own: those of its GUID partition table
//! or its Apple partition map, else the whole disk as one partition, as a volume written
//! without a map is.

use super::disk::{Disk, Fault};
use super::udif::{Partition, SECTOR};

/// The most GPT entries read, and the most bytes of them.
const MAX_GPT_ENTRIES: u64 = 1024;
const MAX_GPT_ENTRY: u64 = 4096;
/// The most Apple partition map entries read.
const MAX_APM_ENTRIES: u64 = 256;

/// The partitions of `disk`, of `sectors` sectors.
pub(super) fn partitions(disk: &mut Disk, sectors: u64) -> Result<Vec<Partition>, Fault> {
    let whole = || {
        vec![Partition {
            name: "the whole disk".into(),
            sector: 0,
            sectors,
        }]
    };
    if sectors < 2 {
        return Ok(whole());
    }
    let mut sectors_read = [0u8; 2 * SECTOR as usize];
    disk.read(0, &mut sectors_read)?;
    let (descriptor, header) = sectors_read.split_at(SECTOR as usize);
    if header.starts_with(b"EFI PART") {
        return gpt(disk, header).map_err(|fault| fault.of("the GUID partition table"));
    }
    let block = if descriptor.starts_with(b"ER") {
        u64::from(u16::from_be_bytes([descriptor[2], descriptor[3]]))
    } else {
        SECTOR
    };
    if block < SECTOR || block % SECTOR != 0 {
        return Ok(whole());
    }
    let mut first = [0u8; SECTOR as usize];
    if block + SECTOR > sectors * SECTOR {
        return Ok(whole());
    }
    disk.read(block, &mut first)?;
    if first.starts_with(b"PM") {
        return apm(disk, &first, block).map_err(|fault| fault.of("the Apple partition map"));
    }
    Ok(whole())
}

/// The partitions of the GUID partition table whose header is `header`.
fn gpt(disk: &mut Disk, header: &[u8]) -> Result<Vec<Partition>, Fault> {
    let long = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().expect("eight"));
    let word = |at: usize| {
        u64::from(u32::from_le_bytes(
            header[at..at + 4].try_into().expect("4"),
        ))
    };
    let (first, count, size) = (long(72), word(80), word(84));
    if count > MAX_GPT_ENTRIES || !(128..=MAX_GPT_ENTRY).contains(&size) || size % 8 != 0 {
        return Err(Fault::Corrupt(format!(
            "it gives {count} entries of {size} bytes, more than dijo reads"
        )));
    }
    let at = first
        .checked_mul(SECTOR)
        .ok_or_else(|| Fault::Corrupt(format!("its entries at sector {first} are out of range")))?;
    let mut entries = vec![0u8; (count * size) as usize];
    disk.read(at, &mut entries)?;
    let mut partitions = Vec::new();
    for (index, entry) in entries.chunks_exact(size as usize).enumerate() {
        if entry[..16].iter().all(|&byte| byte == 0) {
            continue;
        }
        let long = |at: usize| u64::from_le_bytes(entry[at..at + 8].try_into().expect("eight"));
        let (start, last) = (long(32), long(40));
        if last < start {
            return Err(Fault::Corrupt(format!(
                "its entry {index} ends at sector {last}, before it starts at {start}"
            )));
        }
        let units: Vec<u16> = entry[56..128]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .take_while(|&unit| unit != 0)
            .collect();
        let name = String::from_utf16_lossy(&units);
        partitions.push(Partition {
            name: format!("{name} (GPT partition {})", index + 1),
            sector: start,
            sectors: last - start + 1,
        });
    }
    Ok(partitions)
}

/// The partitions of the Apple partition map whose first entry is `first`, in its blocks of
/// `block` bytes, the size its driver descriptor map gives, else 512.
fn apm(disk: &mut Disk, first: &[u8], block: u64) -> Result<Vec<Partition>, Fault> {
    let count = u64::from(u32::from_be_bytes(first[4..8].try_into().expect("four")));
    if count > MAX_APM_ENTRIES {
        return Err(Fault::Corrupt(format!(
            "it gives {count} entries, more than dijo reads"
        )));
    }
    let mut partitions = Vec::new();
    let mut entry = [0u8; SECTOR as usize];
    for index in 1..=count {
        disk.read(index * block, &mut entry)?;
        if !entry.starts_with(b"PM") {
            return Err(Fault::Corrupt(format!(
                "its entry {index} does not start with PM"
            )));
        }
        let word =
            |at: usize| u64::from(u32::from_be_bytes(entry[at..at + 4].try_into().expect("4")));
        let text = |range: std::ops::Range<usize>| {
            let field = &entry[range];
            let end = field
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(field.len());
            String::from_utf8_lossy(&field[..end]).into_owned()
        };
        let scale = block / SECTOR;
        partitions.push(Partition {
            name: format!("{} ({})", text(16..48), text(48..80)),
            sector: word(8) * scale,
            sectors: word(12) * scale,
        });
    }
    Ok(partitions)
}

#[cfg(test)]
mod tests {
    use super::super::udif::{Chunk, Coding};
    use super::*;
    use crate::input::Pages;

    fn disk(bytes: &[u8]) -> Disk<'_> {
        let sectors = bytes.len() as u64 / SECTOR;
        Disk::new(
            bytes,
            Pages::IN_MEMORY,
            vec![Chunk {
                sector: 0,
                sectors,
                coding: Coding::Stored,
                offset: 0,
                length: bytes.len(),
            }],
        )
    }

    #[test]
    fn reads_a_guid_partition_table() {
        let mut bytes = vec![0u8; 64 * 512];
        bytes[512..520].copy_from_slice(b"EFI PART");
        bytes[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
        bytes[512 + 80..512 + 84].copy_from_slice(&4u32.to_le_bytes());
        bytes[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
        let entry = 1024 + 128;
        bytes[entry..entry + 16].fill(0x48);
        bytes[entry + 32..entry + 40].copy_from_slice(&40u64.to_le_bytes());
        bytes[entry + 40..entry + 48].copy_from_slice(&63u64.to_le_bytes());
        for (at, unit) in "disk image".encode_utf16().enumerate() {
            bytes[entry + 56 + 2 * at..entry + 58 + 2 * at].copy_from_slice(&unit.to_le_bytes());
        }
        let found = partitions(&mut disk(&bytes), 64).unwrap();
        assert_eq!(
            found,
            [Partition {
                name: "disk image (GPT partition 2)".into(),
                sector: 40,
                sectors: 24,
            }]
        );
        bytes[512 + 80..512 + 84].copy_from_slice(&5000u32.to_le_bytes());
        assert!(matches!(
            partitions(&mut disk(&bytes), 64),
            Err(Fault::Corrupt(reason)) if reason.contains("5000 entries of 128 bytes")
        ));
    }

    #[test]
    fn reads_an_apple_partition_map_in_its_blocks() {
        let mut bytes = vec![0u8; 16 * 2048];
        bytes[..2].copy_from_slice(b"ER");
        bytes[2..4].copy_from_slice(&2048u16.to_be_bytes());
        for (index, (start, count, name, kind)) in [
            (1u32, 2u32, "Apple", "Apple_partition_map"),
            (4, 12, "disk image", "Apple_HFS"),
        ]
        .into_iter()
        .enumerate()
        {
            let at = 2048 * (index + 1);
            bytes[at..at + 2].copy_from_slice(b"PM");
            bytes[at + 4..at + 8].copy_from_slice(&2u32.to_be_bytes());
            bytes[at + 8..at + 12].copy_from_slice(&start.to_be_bytes());
            bytes[at + 12..at + 16].copy_from_slice(&count.to_be_bytes());
            bytes[at + 16..at + 16 + name.len()].copy_from_slice(name.as_bytes());
            bytes[at + 48..at + 48 + kind.len()].copy_from_slice(kind.as_bytes());
        }
        let found = partitions(&mut disk(&bytes), 64).unwrap();
        assert_eq!(found[1].name, "disk image (Apple_HFS)");
        assert_eq!((found[1].sector, found[1].sectors), (16, 48));
    }

    #[test]
    fn takes_a_disk_without_a_map_whole() {
        let bytes = vec![0u8; 8 * 512];
        let found = partitions(&mut disk(&bytes), 8).unwrap();
        assert_eq!(
            found,
            [Partition {
                name: "the whole disk".into(),
                sector: 0,
                sectors: 8,
            }]
        );
    }
}
