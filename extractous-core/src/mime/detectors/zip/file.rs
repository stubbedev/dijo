//! ZIP archives as commons-compress's `ZipFile` opens them: by the central directory, each
//! entry's data located through its local header, failing where `ZipFile` fails.
//!
//! Entries stored or deflated are read; an entry compressed another way (BZIP2, Deflate64, XZ,
//! Zstandard, shrinking or imploding), which commons-compress also reads, is read as unreadable.

use flate2::{Decompress, FlushDecompress, Status};

pub(crate) const LFH_SIG: [u8; 4] = *b"PK\x03\x04";
pub(crate) const CFH_SIG: [u8; 4] = *b"PK\x01\x02";
const EOCD_SIG: [u8; 4] = *b"PK\x05\x06";
const ZIP64_EOCD_LOCATOR_SIG: [u8; 4] = *b"PK\x06\x07";
const ZIP64_EOCD_SIG: [u8; 4] = *b"PK\x06\x06";
const MIN_EOCD_SIZE: usize = 22;
const MAX_EOCD_SIZE: usize = MIN_EOCD_SIZE + 0xFFFF;
const ZIP64_MAGIC: i64 = 0xFFFF_FFFF;
const ZIP64_MAGIC_SHORT: i64 = 0xFFFF;
const UNICODE_PATH_ID: u16 = 0x7075;
const ZIP64_ID: u16 = 0x0001;
pub(crate) const STORED: u16 = 0;
pub(crate) const DEFLATED: u16 = 8;

/// Why `ZipFile` cannot open an archive, or an entry cannot be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Corrupt;

type Result<T> = std::result::Result<T, Corrupt>;

/// An entry of the central directory.
#[derive(Debug, Clone)]
pub(crate) struct ZipEntry {
    pub(crate) name: String,
    flags: u16,
    method: u16,
    compressed_size: i64,
    pub(crate) size: i64,
    data_offset: i64,
}

impl ZipEntry {
    pub(crate) fn is_directory(&self) -> bool {
        self.name.ends_with('/')
    }
}

/// An entry's content: all of it, or what was read before the data failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Content {
    Complete(Vec<u8>),
    Truncated(Vec<u8>),
}

impl Content {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Content::Complete(bytes) | Content::Truncated(bytes) => bytes,
        }
    }
}

/// An archive `ZipFile` opened.
pub(crate) struct ZipFile<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
}

/// Bytes decoded as UTF-8, a malformed sequence replaced by `?`, as commons-compress decodes
/// entry names.
pub(crate) fn decode_name(bytes: &[u8]) -> String {
    let mut name = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        name.push_str(chunk.valid());
        if !chunk.invalid().is_empty() {
            name.push('?');
        }
    }
    name
}

pub(crate) fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

pub(crate) fn u32_at(data: &[u8], offset: usize) -> i64 {
    i64::from(u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

fn u64_at(data: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(data[offset..offset + 8].try_into().expect("eight bytes"))
}

/// A read position over the archive, failing as a channel read past the end fails.
struct Cursor<'a> {
    data: &'a [u8],
    position: i64,
}

impl<'a> Cursor<'a> {
    fn at(data: &'a [u8], position: i64) -> Self {
        Cursor { data, position }
    }

    fn read(&mut self, length: usize) -> Result<&'a [u8]> {
        let start = usize::try_from(self.position).map_err(|_| Corrupt)?;
        let end = start.checked_add(length).ok_or(Corrupt)?;
        let bytes = self.data.get(start..end).ok_or(Corrupt)?;
        self.position += length as i64;
        Ok(bytes)
    }

    fn skip(&mut self, count: i64) -> Result<()> {
        let target = self.position + count;
        if target > self.data.len() as i64 {
            return Err(Corrupt);
        }
        self.position = target;
        Ok(())
    }
}

/// The extra fields `ExtraFieldUtils.parse` splits out, by header id; trailing bytes that do
/// not form a field are kept apart and play no part here.
pub(crate) fn extra_fields(extra: &[u8]) -> Vec<(u16, &[u8])> {
    let mut fields = Vec::new();
    let mut start = 0;
    while start + 4 <= extra.len() {
        let id = u16_at(extra, start);
        let length = usize::from(u16_at(extra, start + 2));
        let Some(data) = extra.get(start + 4..start + 4 + length) else {
            break;
        };
        fields.push((id, data));
        start += 4 + length;
    }
    fields
}

/// A Unicode path extra field: the CRC-32 of the name it replaces and the name in UTF-8, or
/// `None` where commons-compress keeps it as an unrecognized field.
fn unicode_path(data: &[u8]) -> Option<(u32, &[u8])> {
    (data.len() >= 5 && data[0] == 1).then(|| {
        let crc = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
        (crc, &data[5..])
    })
}

/// The Unicode path field of an entry's central directory record.
enum CentralPath<'a> {
    Absent,
    Valid(u32, &'a [u8]),
    Invalid,
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(bytes);
    crc.sum()
}

/// `tryToLocateSignature`: the end of central directory record, searched backwards from the
/// last place it fits to as far as the longest comment allows.
fn locate_eocd(data: &[u8]) -> Option<usize> {
    let last = data.len().checked_sub(MIN_EOCD_SIZE)?;
    let stop = data.len().saturating_sub(MAX_EOCD_SIZE);
    (stop..=last).rev().find(|&at| data[at..at + 4] == EOCD_SIG)
}

/// A central directory record before its local header is resolved.
struct Central<'a> {
    entry: ZipEntry,
    raw_name: &'a [u8],
    utf8: bool,
    local_header_offset: i64,
    path: CentralPath<'a>,
}

impl<'a> ZipFile<'a> {
    /// `ZipFile.builder().setSeekableByteChannel(channel).get()`.
    pub(crate) fn open(data: &'a [u8]) -> Result<ZipFile<'a>> {
        let eocd = locate_eocd(data).ok_or(Corrupt)?;
        let (first_lfh_offset, cd_start) =
            if eocd > 20 && data[eocd - 20..eocd - 16] == ZIP64_EOCD_LOCATOR_SIG {
                let mut cursor = Cursor::at(data, eocd as i64 - 12);
                cursor.position = u64_at(cursor.read(8)?, 0);
                if cursor.read(4)? != ZIP64_EOCD_SIG {
                    return Err(Corrupt);
                }
                cursor.skip(44)?;
                (0, u64_at(cursor.read(8)?, 0))
            } else {
                let mut cursor = Cursor::at(data, eocd as i64);
                cursor.skip(12)?;
                let length = u32_at(cursor.read(4)?, 0);
                let offset = u32_at(cursor.read(4)?, 0);
                let first = (eocd as i64 - length - offset).max(0);
                (first, offset + first)
            };
        let mut cursor = Cursor::at(data, cd_start);
        let mut signature = cursor.read(4)?;
        if signature != CFH_SIG && Cursor::at(data, first_lfh_offset).read(4)? == LFH_SIG {
            return Err(Corrupt);
        }
        let mut records = Vec::new();
        while signature == CFH_SIG {
            records.push(Self::central_record(
                &mut cursor,
                first_lfh_offset,
                cd_start,
            )?);
            signature = cursor.read(4)?;
        }
        let mut entries = Vec::with_capacity(records.len());
        for record in records {
            entries.push(Self::resolve(data, record, cd_start)?);
        }
        Ok(ZipFile { data, entries })
    }

    /// `readCentralDirectoryEntry`.
    fn central_record(
        cursor: &mut Cursor<'a>,
        first_lfh_offset: i64,
        cd_start: i64,
    ) -> Result<Central<'a>> {
        let header = cursor.read(42)?;
        let flags = u16_at(header, 4);
        let method = u16_at(header, 6);
        let mut compressed_size = u32_at(header, 16);
        let mut size = u32_at(header, 20);
        let name_length = usize::from(u16_at(header, 24));
        let extra_length = usize::from(u16_at(header, 26));
        let comment_length = usize::from(u16_at(header, 28));
        let mut disk_start = i64::from(u16_at(header, 30));
        let mut local_header_offset = u32_at(header, 38) + first_lfh_offset;
        let raw_name = cursor.read(name_length)?;
        let extra = cursor.read(extra_length)?;
        let fields = extra_fields(extra);
        if let Some(&(_, z64)) = fields.iter().find(|(id, _)| *id == ZIP64_ID) {
            let wanted = [
                size == ZIP64_MAGIC,
                compressed_size == ZIP64_MAGIC,
                local_header_offset == ZIP64_MAGIC,
            ];
            let has_disk = disk_start == ZIP64_MAGIC_SHORT;
            let expected = wanted.iter().filter(|&&w| w).count() * 8 + if has_disk { 4 } else { 0 };
            if z64.len() < expected {
                return Err(Corrupt);
            }
            let mut at = 0;
            let mut eight = || {
                let value = u64_at(z64, at);
                at += 8;
                value
            };
            if wanted[0] {
                size = eight();
            }
            if wanted[1] {
                compressed_size = eight();
            }
            if wanted[2] {
                local_header_offset = eight();
            }
            if size < 0 || compressed_size < 0 {
                return Err(Corrupt);
            }
            if has_disk {
                disk_start = u32_at(z64, at);
            }
        }
        if disk_start < 0 || local_header_offset < 0 || local_header_offset > cd_start {
            return Err(Corrupt);
        }
        cursor.read(comment_length)?;
        let path = match fields.iter().find(|(id, _)| *id == UNICODE_PATH_ID) {
            None => CentralPath::Absent,
            Some(&(_, field)) => match unicode_path(field) {
                Some((crc, name)) => CentralPath::Valid(crc, name),
                None => CentralPath::Invalid,
            },
        };
        Ok(Central {
            entry: ZipEntry {
                name: decode_name(raw_name),
                flags,
                method,
                compressed_size,
                size,
                data_offset: 0,
            },
            raw_name,
            utf8: flags & 0x0800 != 0,
            local_header_offset,
            path,
        })
    }

    /// `resolveLocalFileHeaderData` for one entry: where its data starts, and its name from a
    /// Unicode path field, the local header's replacing the central directory's.
    fn resolve(data: &'a [u8], record: Central<'a>, cd_start: i64) -> Result<ZipEntry> {
        let mut entry = record.entry;
        let mut local = Cursor::at(data, record.local_header_offset + 26);
        let lengths = local.read(4)?;
        let (name_length, extra_length) = (u16_at(lengths, 0), u16_at(lengths, 2));
        entry.data_offset =
            record.local_header_offset + 30 + i64::from(name_length) + i64::from(extra_length);
        if entry.data_offset + entry.compressed_size > cd_start {
            return Err(Corrupt);
        }
        local.skip(i64::from(name_length))?;
        let local_extra = local.read(usize::from(extra_length))?;
        if record.utf8 {
            return Ok(entry);
        }
        let local_path = extra_fields(local_extra)
            .into_iter()
            .find(|(id, _)| *id == UNICODE_PATH_ID)
            .map(|(_, field)| unicode_path(field));
        let path = match (record.path, local_path) {
            (CentralPath::Invalid, _) | (CentralPath::Absent, None) => None,
            (_, Some(local)) => local,
            (CentralPath::Valid(crc, name), None) => Some((crc, name)),
        };
        if let Some((crc, unicode)) = path {
            if crc32(record.raw_name) == crc {
                entry.name = decode_name(unicode);
            }
        }
        Ok(entry)
    }

    /// The entries in central directory order, as `getEntries` enumerates them.
    pub(crate) fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// `getEntry`: the first entry of the name.
    pub(crate) fn entry(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// `getInputStream(entry)` read to its end; `None` where it throws at once.
    pub(crate) fn read(&self, entry: &ZipEntry) -> Option<Content> {
        if entry.flags & 0x0001 != 0 {
            return None;
        }
        let start = usize::try_from(entry.data_offset).ok()?;
        let length = usize::try_from(entry.compressed_size).ok()?;
        let end = start.saturating_add(length).min(self.data.len());
        let raw = self.data.get(start.min(end)..end)?;
        match entry.method {
            STORED => Some(if raw.len() == length {
                Content::Complete(raw.to_vec())
            } else {
                Content::Truncated(raw.to_vec())
            }),
            DEFLATED => Some(inflate(raw)),
            _ => None,
        }
    }
}

/// Raw deflate data inflated as `InflaterInputStream` inflates it: to the end of the deflate
/// stream, or as far as it goes before the input runs out or turns out corrupt.
pub(crate) fn inflate(raw: &[u8]) -> Content {
    let mut inflater = Decompress::new(false);
    let mut out = Vec::new();
    loop {
        let (before_in, before_out) = (inflater.total_in(), out.len());
        out.reserve(64 * 1024);
        let consumed = usize::try_from(before_in).expect("the input fits in memory");
        let input: &[u8] = match consumed.cmp(&raw.len()) {
            std::cmp::Ordering::Less => &raw[consumed..],
            std::cmp::Ordering::Equal => &[0],
            std::cmp::Ordering::Greater => return Content::Truncated(out),
        };
        match inflater.decompress_vec(input, &mut out, FlushDecompress::None) {
            Ok(Status::StreamEnd) => return Content::Complete(out),
            Err(_) => return Content::Truncated(out),
            Ok(_) if inflater.total_in() == before_in && out.len() == before_out => {
                return Content::Truncated(out)
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    pub(crate) fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in entries {
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn reads_entries_by_the_central_directory() {
        let data = zip(&[
            ("mimetype", b"application/vnd.oasis.opendocument.text"),
            ("a/b.xml", b"<a/>"),
        ]);
        let file = ZipFile::open(&data).unwrap();
        let names: Vec<&str> = file.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["mimetype", "a/b.xml"]);
        let entry = file.entry("a/b.xml").unwrap();
        assert_eq!(file.read(entry), Some(Content::Complete(b"<a/>".to_vec())));
    }

    #[test]
    fn fails_where_zip_file_fails() {
        let data = zip(&[("a", b"hello")]);
        assert!(ZipFile::open(&data[..data.len() - 10]).is_err());
        assert!(ZipFile::open(b"PK\x03\x04 not really").is_err());
        let mut lying = data.clone();
        let cd = lying.len() - 22 - 47;
        lying[cd + 42..cd + 46].copy_from_slice(&1000u32.to_le_bytes());
        assert!(ZipFile::open(&lying).is_err());
    }

    #[test]
    fn decodes_names_as_commons_compress_does() {
        assert_eq!(decode_name(b"caf\xC3\xA9"), "caf\u{E9}");
        assert_eq!(decode_name(b"caf\xE9"), "caf?");
    }

    #[test]
    fn inflates_what_it_can() {
        let data = zip(&[("a", &[7u8; 5000])]);
        let file = ZipFile::open(&data).unwrap();
        let entry = file.entry("a").unwrap().clone();
        assert_eq!(file.read(&entry).unwrap().bytes().len(), 5000);
    }
}
