//! ARJ archives as commons-compress's `ArjArchiveInputStream` reads them for Tika's
//! `PackageParser`: the main header, then each local header, found by its magic and checked
//! against its CRC-32, with its name and comment in CP437; a stored file's data in place,
//! checked against its CRC-32 once read to its size; a compressed one's, which the stream does
//! not decompress, unread. Encrypted and multi-volume archives are refused when the stream is
//! opened.
//!
//! A header is looked for until both bytes of its magic are in place, where the stream stops at
//! either, its `&&` where `||` was meant (#184). A DOS time is read as a wall-clock time in UTC,
//! where commons-compress reads it in the JVM's time zone.

use super::{crc32, dos_millis, throws, Undecoded};

const MAGIC: [u8; 2] = [0x60, 0xEA];
/// The largest basic header the stream reads, past which it looks for another.
const MAX_BASIC_HEADER: usize = 2600;
/// The fixed fields of a first header, before its optional ones.
const FIRST_HEADER_FIELDS: usize = 29;
/// A first header that holds the extended file position, then the other optional fields.
const WITH_EXTENDED_POSITION: usize = 33;
const WITH_TIMES: usize = 45;
const CP437: &str = "IBM437";

const FLAG_GARBLED: u8 = 0x01;
const FLAG_VOLUME: u8 = 0x04;
const METHOD_STORED: u8 = 0;
const FILE_TYPE_DIRECTORY: u8 = 3;
const HOST_UNIX: u8 = 2;
const HOST_NEXT: u8 = 8;

/// What a file of the archive holds, as the stream gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArjData<'a> {
    /// Stored data, in place, as much of it as the archive holds.
    Stored(&'a [u8]),
    /// Compressed data, which the stream does not read: `canReadEntryData` is false.
    Compressed,
}

/// `ArjArchiveEntry`, as Tika's parser reads one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArjEntry<'a> {
    pub name: String,
    pub directory: bool,
    /// `getSize`: the original size.
    pub size: i64,
    /// `getLastModifiedDate`, in milliseconds since the epoch; none for a DOS time out of range.
    pub modified_millis: Option<i64>,
    pub data: ArjData<'a>,
}

/// Why the stream does not read an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// Opening it throws this `ArchiveException` message, which Tika's parser reports as unable
    /// to unpack the document.
    Unopened(String),
    /// Reading it on throws.
    Fails(Undecoded),
}

/// The entries of the ARJ archive `data`, as `getNextEntry` gives them, each read to its end
/// before the next.
pub fn entries(data: &[u8]) -> Result<Vec<ArjEntry<'_>>, Unread> {
    let mut stream = Stream { data, at: 0 };
    let main = stream
        .basic_header()
        .map_err(|undecoded| Unread::Unopened(undecoded.to_string()))?
        .ok_or_else(|| Unread::Unopened("Archive ends without any headers".to_owned()))?;
    let flags = main_flags(main).map_err(|undecoded| Unread::Unopened(undecoded.to_string()))?;
    stream
        .main_extended_header()
        .map_err(|undecoded| Unread::Unopened(undecoded.to_string()))?;
    if flags & FLAG_GARBLED != 0 {
        return Err(Unread::Unopened(
            "Encrypted ARJ files are unsupported".to_owned(),
        ));
    }
    if flags & FLAG_VOLUME != 0 {
        return Err(Unread::Unopened(
            "Multi-volume ARJ files are unsupported".to_owned(),
        ));
    }
    let mut entries = Vec::new();
    while let Some(entry) = stream.local_entry().map_err(Unread::Fails)? {
        entries.push(entry);
    }
    Ok(entries)
}

/// The archive's flags, from the first header of its main header.
fn main_flags(basic: &[u8]) -> Result<u8, Undecoded> {
    let first = first_header(basic)?;
    if first.len() < FIRST_HEADER_FIELDS || cut_short(first, WITH_EXTENDED_POSITION) {
        return Err(eof());
    }
    let mut rest = &basic[1 + first.len()..];
    string(&mut rest)?;
    string(&mut rest)?;
    Ok(first[3])
}

/// Whether the first header `first`, whose size counts its size byte too, is as large as the
/// fields that end at `end` say it holds them, yet does not.
fn cut_short(first: &[u8], end: usize) -> bool {
    first.len() + 1 >= end && first.len() < end
}

/// The first header of a basic header: its size, one byte, then the bytes after it.
fn first_header(basic: &[u8]) -> Result<&[u8], Undecoded> {
    let size = usize::from(*basic.first().ok_or_else(eof)?);
    let length = size.checked_sub(1).ok_or_else(eof)?;
    basic.get(1..1 + length).ok_or_else(eof)
}

/// `readString`: bytes up to a NUL, decoded as CP437.
fn string(bytes: &mut &[u8]) -> Result<String, Undecoded> {
    let end = bytes.iter().position(|&byte| byte == 0).ok_or_else(eof)?;
    let text = crate::text::charset::Charset::named(CP437)
        .decode(&bytes[..end])
        .expect("IBM437 decodes every byte");
    *bytes = &bytes[end + 1..];
    Ok(text)
}

fn eof() -> Undecoded {
    throws("java.io.EOFException")
}

fn u16_le(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_le(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The stream over the archive's bytes.
struct Stream<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Stream<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Undecoded> {
        let bytes = self
            .data
            .get(self.at..self.at.checked_add(count).ok_or_else(eof)?)
            .ok_or_else(eof)?;
        self.at += count;
        Ok(bytes)
    }

    fn read8(&mut self) -> Result<u8, Undecoded> {
        Ok(self.take(1)?[0])
    }

    fn read16(&mut self) -> Result<u16, Undecoded> {
        Ok(u16_le(self.take(2)?, 0))
    }

    fn read32(&mut self) -> Result<u32, Undecoded> {
        Ok(u32_le(self.take(4)?, 0))
    }

    /// `readHeader`: the next basic header whose CRC-32 matches, found by its magic; none at
    /// the end of the archive, a basic header of no bytes.
    fn basic_header(&mut self) -> Result<Option<&'a [u8]>, Undecoded> {
        loop {
            let mut second = self.read8()?;
            loop {
                let first = second;
                second = self.read8()?;
                if [first, second] == MAGIC {
                    break;
                }
            }
            let size = usize::from(self.read16()?);
            if size == 0 {
                return Ok(None);
            }
            if size <= MAX_BASIC_HEADER {
                let basic = self.take(size)?;
                if self.read32()? == crc32(basic) {
                    return Ok(Some(basic));
                }
            }
        }
    }

    /// The main header's extended header, if any, which must match its CRC-32.
    fn main_extended_header(&mut self) -> Result<(), Undecoded> {
        let size = usize::from(self.read16()?);
        if size > 0 {
            self.extended_header(size)?;
        }
        Ok(())
    }

    fn extended_header(&mut self, size: usize) -> Result<(), Undecoded> {
        let bytes = self.take(size)?;
        if self.read32()? != crc32(bytes) {
            return Err(throws("Extended header CRC32 verification failure"));
        }
        Ok(())
    }

    /// `readLocalFileHeader`, then the entry's data, which `getNextEntry` reads to its end
    /// before the next header: as much as the archive holds of its compressed size.
    fn local_entry(&mut self) -> Result<Option<ArjEntry<'a>>, Undecoded> {
        let Some(basic) = self.basic_header()? else {
            return Ok(None);
        };
        let first = first_header(basic)?;
        if first.len() < FIRST_HEADER_FIELDS
            || cut_short(first, WITH_EXTENDED_POSITION)
            || cut_short(first, WITH_TIMES)
        {
            return Err(eof());
        }
        let host = first[2];
        let method = first[4];
        let file_type = first[5];
        let modified = u32_le(first, 7);
        let compressed_size = u64::from(u32_le(first, 11));
        let original_size = u32_le(first, 15);
        let original_crc = u32_le(first, 19);
        let mut rest = &basic[1 + first.len()..];
        let name = string(&mut rest)?;
        string(&mut rest)?;
        loop {
            let size = usize::from(self.read16()?);
            if size == 0 {
                break;
            }
            self.extended_header(size)?;
        }
        let available = self.data.len() - self.at;
        let length = usize::try_from(compressed_size).map_or(available, |size| size.min(available));
        let bytes = self.take(length)?;
        let data = if method == METHOD_STORED {
            if original_size > 0
                && bytes.len() as u64 >= u64::from(original_size)
                && crc32(bytes) != original_crc
            {
                return Err(throws("Checksum verification failed"));
            }
            ArjData::Stored(bytes)
        } else {
            ArjData::Compressed
        };
        let modified_millis = if host == HOST_UNIX || host == HOST_NEXT {
            Some(i64::from(modified as i32) * 1000)
        } else {
            dos_millis(i64::from(modified))
        };
        Ok(Some(ArjEntry {
            name,
            directory: file_type == FILE_TYPE_DIRECTORY,
            size: i64::from(original_size),
            modified_millis,
            data,
        }))
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use super::*;

    /// A basic header of a first header with `fields`, a name and a comment, preceded by the
    /// magic and its size, followed by its CRC-32 and no extended header.
    fn header(fields: &[u8], name: &[u8]) -> Vec<u8> {
        let mut basic = vec![(fields.len() + 1) as u8];
        basic.extend_from_slice(fields);
        basic.extend_from_slice(name);
        basic.push(0);
        basic.push(0);
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&(basic.len() as u16).to_le_bytes());
        header.extend_from_slice(&basic);
        header.extend_from_slice(&crc32(&basic).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes());
        header
    }

    /// An ARJ archive of `entries`, each a name, a method, a file type and the data stored,
    /// made on MS-DOS.
    pub fn arj(entries: &[(&[u8], u8, u8, &[u8])]) -> Vec<u8> {
        let mut main = vec![11, 1, 0, 0, 0, 2, 0];
        main.extend_from_slice(&[0; 22]);
        let mut archive = header(&main, b"archive.arj");
        for (name, method, file_type, data) in entries {
            let mut fields = vec![11, 1, 0, 0, *method, *file_type, 0];
            fields.extend_from_slice(&0x5264_8c4au32.to_le_bytes());
            fields.extend_from_slice(&(data.len() as u32).to_le_bytes());
            fields.extend_from_slice(&(data.len() as u32).to_le_bytes());
            fields.extend_from_slice(&crc32(data).to_le_bytes());
            fields.extend_from_slice(&[0; 6]);
            archive.extend(header(&fields, name));
            archive.extend_from_slice(data);
        }
        archive.extend_from_slice(&MAGIC);
        archive.extend_from_slice(&0u16.to_le_bytes());
        archive
    }

    /// `archive` with `stray` before its first local header, then the size of the largest basic
    /// header, which runs past the archive's end.
    pub fn with_stray_bytes(archive: &[u8], stray: [u8; 2]) -> Vec<u8> {
        let local = 4 + usize::from(u16_le(archive, 2)) + 4 + 2;
        let size = u16::try_from(MAX_BASIC_HEADER).expect("a basic header's size fits 16 bits");
        [
            &archive[..local],
            &stray,
            &size.to_le_bytes(),
            &archive[local..],
        ]
        .concat()
    }

    /// The search for a header goes on past a stray byte of its magic until both are in place,
    /// where commons-compress stops at either and fails on the size it reads after it (#184).
    #[test]
    fn finds_a_header_past_a_stray_byte_of_its_magic() {
        let archive = arj(&[(b"A.TXT", METHOD_STORED, 0, b"stored text")]);
        for stray in [[MAGIC[0], 0], [0, MAGIC[1]]] {
            let strayed = with_stray_bytes(&archive, stray);
            let entries = entries(&strayed).unwrap();
            let read: Vec<(&str, &ArjData)> = entries
                .iter()
                .map(|entry| (entry.name.as_str(), &entry.data))
                .collect();
            assert_eq!(read, [("A.TXT", &ArjData::Stored(b"stored text"))]);
        }
    }

    #[test]
    fn reads_stored_files_and_leaves_compressed_ones_unread() {
        let archive = arj(&[
            (b"DIR", METHOD_STORED, FILE_TYPE_DIRECTORY, b""),
            (b"DIR/K\x84SE.TXT", METHOD_STORED, 0, b"stored text"),
            (b"PACKED.BIN", 1, 0, b"\x01\x02\x03"),
        ]);
        let entries = entries(&archive).unwrap();
        let read: Vec<(&str, bool, &ArjData)> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.directory, &entry.data))
            .collect();
        assert_eq!(
            read,
            [
                ("DIR", true, &ArjData::Stored(b"")),
                ("DIR/K\u{e4}SE.TXT", false, &ArjData::Stored(b"stored text")),
                ("PACKED.BIN", false, &ArjData::Compressed),
            ]
        );
        assert_eq!(entries[1].size, 11);
        assert!(entries[1].modified_millis.is_some());
    }

    #[test]
    fn fails_as_the_stream_fails() {
        let mut corrupt = arj(&[(b"A.TXT", METHOD_STORED, 0, b"data")]);
        let at = corrupt.len() - 5;
        corrupt[at] ^= 0xFF;
        assert!(matches!(entries(&corrupt), Err(Unread::Fails(_))));
        let mut encrypted = arj(&[]);
        encrypted[8] = FLAG_GARBLED;
        let crc_at = 4 + usize::from(encrypted[2]);
        let crc = crc32(&encrypted[4..crc_at]);
        encrypted[crc_at..crc_at + 4].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            entries(&encrypted),
            Err(Unread::Unopened(
                "Encrypted ARJ files are unsupported".to_owned()
            ))
        );
        let cut = arj(&[(b"A.TXT", METHOD_STORED, 0, b"data")]);
        assert!(matches!(
            entries(&cut[..cut.len() - 3]),
            Err(Unread::Fails(_))
        ));
    }
}
