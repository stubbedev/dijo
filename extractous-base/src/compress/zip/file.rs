//! ZIP archives as commons-compress's `ZipFile` opens them: by the central directory, each
//! entry's data located through its local header, failing where `ZipFile` fails. Entries keep
//! what `ZipArchiveEntry` reports of them: names and comments with their Unicode extra fields,
//! sizes, CRC, attributes and times.

use super::extra::{self, ExtraFields, Field, Instant, Times};
use super::{
    decode_into, decode_name, decompresses, inflate_into, method_name, u16_at, u32_at, u64_at,
    Content, Corrupt, Decoded, Spooled, CFH_SIG, DEFLATED, LFH_SIG, STORED,
};
use crate::compress::{crc32, Decoding, Held, Undecoded};
use crate::input::Pages;

const EOCD_SIG: [u8; 4] = *b"PK\x05\x06";
const ZIP64_EOCD_LOCATOR_SIG: [u8; 4] = *b"PK\x06\x07";
const ZIP64_EOCD_SIG: [u8; 4] = *b"PK\x06\x06";
const MIN_EOCD_SIZE: usize = 22;
const MAX_EOCD_SIZE: usize = MIN_EOCD_SIZE + 0xFFFF;
const ZIP64_MAGIC: i64 = 0xFFFF_FFFF;
const ZIP64_MAGIC_SHORT: i64 = 0xFFFF;
const PLATFORM_FAT: u16 = 0;
const PLATFORM_UNIX: u16 = 3;
/// The hosts of "version made by" whose file systems separate paths with `\`: MS-DOS and OS/2
/// FAT, Windows NTFS and VFAT.
const WINDOWS_PLATFORMS: [u16; 3] = [PLATFORM_FAT, 10, 14];

type Result<T> = std::result::Result<T, Corrupt>;

/// Where an entry's name comes from, `ZipArchiveEntry.NameSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSource {
    /// The name's bytes, decoded as UTF-8 though nothing declares them so.
    Name,
    /// The name's bytes, which the language encoding flag declares UTF-8.
    NameWithEfsFlag,
    /// A Unicode path extra field whose CRC matches the name's bytes.
    UnicodeExtraField,
}

/// An entry of the central directory, or of a local header read front to back, as
/// `ZipArchiveEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    pub name: String,
    pub raw_name: Vec<u8>,
    pub name_source: NameSource,
    pub comment: String,
    pub version_made_by: u16,
    pub(super) flags: u16,
    pub method: u16,
    pub(super) dos_time: i64,
    pub crc: i64,
    pub compressed_size: i64,
    pub size: i64,
    pub(super) external_attributes: i64,
    pub(super) times: Times,
    pub(super) data_offset: i64,
}

impl ZipEntry {
    pub fn is_directory(&self) -> bool {
        self.name.ends_with('/')
    }

    /// `getGeneralPurposeBit().usesEncryption()`.
    pub fn uses_encryption(&self) -> bool {
        self.flags & 0x0001 != 0
    }

    /// `ZipUtil.canHandleEntryData`: not encrypted, and compressed a way commons-compress reads.
    pub fn can_read(&self) -> bool {
        self.unsupported_feature().is_none()
    }

    /// The message of the `UnsupportedZipFeatureException` that `ZipUtil.checkRequestedFeatures`
    /// throws on an entry commons-compress does not read: encrypted, or compressed a way it does
    /// not decompress.
    pub fn unsupported_feature(&self) -> Option<String> {
        if self.uses_encryption() {
            return Some(format!(
                "Unsupported feature encryption used in entry {}",
                self.name
            ));
        }
        if matches!(self.method, STORED | DEFLATED) || decompresses(self.method) {
            return None;
        }
        Some(match method_name(self.method) {
            Some(method) => format!(
                "Unsupported compression method {} ({method}) used in entry {}",
                self.method, self.name
            ),
            None => format!(
                "Unsupported feature compression method used in entry {}",
                self.name
            ),
        })
    }

    /// `getPlatform`: the high byte of the version made by.
    pub fn platform(&self) -> u16 {
        self.version_made_by >> 8 & 0x0F
    }

    /// `getUnixMode`: the high half of the external attributes on Unix, 0 elsewhere.
    pub fn unix_mode(&self) -> i64 {
        if self.platform() == PLATFORM_UNIX {
            self.external_attributes >> 16 & 0xFFFF
        } else {
            0
        }
    }

    /// `getLastModifiedTime`: from the extra fields, or else the DOS time read as a wall-clock
    /// time in UTC, none when a field of it is out of range. commons-compress reads the DOS time
    /// with a lenient `Calendar` in the JVM's default time zone, so the instant depends on the
    /// machine's zone and out-of-range fields roll over (#99).
    pub fn modified(&self) -> Option<Instant> {
        self.times
            .modified
            .or_else(|| crate::compress::dos_millis(self.dos_time).map(Instant::from_millis))
    }

    /// `getCreationTime`.
    pub fn created(&self) -> Option<Instant> {
        self.times.created
    }
}

/// An archive `ZipFile` opened.
pub struct ZipFile<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntry>,
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

/// The raw data of each field of an extra field block, for the ZIP64 field the sizes come from.
pub(super) fn zip64_field(extra: &[u8]) -> Option<&[u8]> {
    let mut start = 0;
    while start + 4 <= extra.len() {
        let id = u16_at(extra, start);
        let length = usize::from(u16_at(extra, start + 2));
        let field = extra.get(start + 4..start + 4 + length)?;
        if id == extra::ZIP64 {
            return Some(field);
        }
        start += 4 + length;
    }
    None
}

/// `tryToLocateSignature`: the end of central directory record, searched backwards from the
/// last place it fits to as far as the longest comment allows.
fn locate_eocd(data: &[u8]) -> Option<usize> {
    let last = data.len().checked_sub(MIN_EOCD_SIZE)?;
    let stop = data.len().saturating_sub(MAX_EOCD_SIZE);
    (stop..=last).rev().find(|&at| data[at..at + 4] == EOCD_SIG)
}

/// Whether `data` has an end of central directory record where `ZipFile` looks for one: without
/// one, it throws a `ZipException` that the archive is not a ZIP archive before it reads
/// anything else.
pub fn has_end_of_central_directory(data: &[u8]) -> bool {
    locate_eocd(data).is_some()
}

/// A central directory record before its local header is resolved.
struct Central<'a> {
    entry: ZipEntry,
    fields: ExtraFields,
    raw_comment: &'a [u8],
    local_header_offset: i64,
}

impl<'a> ZipFile<'a> {
    /// `ZipFile.builder().setSeekableByteChannel(channel).get()`.
    pub fn open(data: &'a [u8]) -> Result<ZipFile<'a>> {
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
        let version_made_by = u16_at(header, 0);
        let flags = u16_at(header, 4);
        let method = u16_at(header, 6);
        let dos_time = u32_at(header, 8);
        let crc = u32_at(header, 12);
        let mut compressed_size = u32_at(header, 16);
        let mut size = u32_at(header, 20);
        let name_length = usize::from(u16_at(header, 24));
        let extra_length = usize::from(u16_at(header, 26));
        let comment_length = usize::from(u16_at(header, 28));
        let mut disk_start = i64::from(u16_at(header, 30));
        let external_attributes = u32_at(header, 34);
        let mut local_header_offset = u32_at(header, 38) + first_lfh_offset;
        let raw_name = cursor.read(name_length)?;
        let extra = cursor.read(extra_length)?;
        let mut times = Times::default();
        let fields = ExtraFields::from_central(extra, &mut times)?;
        if let Some(z64) = zip64_field(extra) {
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
        let raw_comment = cursor.read(comment_length)?;
        let utf8 = flags & 0x0800 != 0;
        Ok(Central {
            entry: ZipEntry {
                name: entry_name(decode_name(raw_name), version_made_by),
                raw_name: raw_name.to_vec(),
                name_source: if utf8 {
                    NameSource::NameWithEfsFlag
                } else {
                    NameSource::Name
                },
                comment: decode_name(raw_comment),
                version_made_by,
                flags,
                method,
                dos_time,
                crc,
                compressed_size,
                size,
                external_attributes,
                times,
                data_offset: 0,
            },
            fields,
            raw_comment,
            local_header_offset,
        })
    }

    /// `resolveLocalFileHeaderData` for one entry: where its data starts, the local header's
    /// extra fields merged in, and its name and comment from the Unicode extra fields when the
    /// entry does not declare UTF-8.
    fn resolve(data: &'a [u8], record: Central<'a>, cd_start: i64) -> Result<ZipEntry> {
        let Central {
            mut entry,
            mut fields,
            raw_comment,
            local_header_offset,
        } = record;
        let mut local = Cursor::at(data, local_header_offset + 26);
        let lengths = local.read(4)?;
        let (name_length, extra_length) = (u16_at(lengths, 0), u16_at(lengths, 2));
        entry.data_offset =
            local_header_offset + 30 + i64::from(name_length) + i64::from(extra_length);
        if entry.data_offset + entry.compressed_size > cd_start {
            return Err(Corrupt);
        }
        local.skip(i64::from(name_length))?;
        let local_extra = local.read(usize::from(extra_length))?;
        fields.merge_local(local_extra, &mut entry.times)?;
        if entry.name_source == NameSource::NameWithEfsFlag {
            return Ok(entry);
        }
        if let Some(name) = unicode_text(&fields, extra::UNICODE_PATH, &entry.raw_name) {
            entry.name = entry_name(name, entry.version_made_by);
            entry.name_source = NameSource::UnicodeExtraField;
        }
        if !raw_comment.is_empty() {
            if let Some(comment) = unicode_text(&fields, extra::UNICODE_COMMENT, raw_comment) {
                entry.comment = comment;
            }
        }
        Ok(entry)
    }

    /// The entries in central directory order, as `getEntries` enumerates them.
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// `getEntry`: the first entry of the name.
    pub fn entry(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// `getInputStream(entry)` read to its end into memory, `None` for an entry encrypted or
    /// compressed a way the port does not decompress.
    pub fn read(&self, entry: &ZipEntry) -> Option<Content<'a>> {
        self.read_into(entry, Pages::IN_MEMORY, |_, _| Decoding::in_memory())
            .map(|read| read.expect("memory takes what is decoded"))
    }

    /// [`ZipFile::read`] as a document embedded in the archive, whose pages are `pages`: stored
    /// data in place, decoded data spooled, or why it cannot be spooled.
    pub fn spool(
        &self,
        entry: &ZipEntry,
        pages: Pages<'a>,
    ) -> Option<std::result::Result<Spooled<'a>, Undecoded>> {
        self.read_into(entry, pages, Decoding::spooled)
    }

    /// [`ZipFile::spool`] keeping nothing, to tell whether the entry reads whole.
    pub fn verify(&self, entry: &ZipEntry) -> Option<Decoded<()>> {
        self.read_into(entry, Pages::IN_MEMORY, |compressed, _| {
            Decoding::discarded(compressed)
        })
        .map(|read| read.expect("nothing fails to take what is decoded"))
    }

    /// `getInputStream(entry)` read to its end into what `decoding` makes of the length of its
    /// data and the size the archive declares it decodes into, where the data of the archive
    /// has the pages `pages`.
    fn read_into<W: Held>(
        &self,
        entry: &ZipEntry,
        pages: Pages<'a>,
        decoding: impl FnOnce(usize, Option<u64>) -> Decoding<W>,
    ) -> Option<std::result::Result<Decoded<W::Bytes<'a>>, Undecoded>> {
        if entry.uses_encryption() {
            return None;
        }
        let start = usize::try_from(entry.data_offset).ok()?;
        let length = usize::try_from(entry.compressed_size).ok()?;
        let end = start.saturating_add(length).min(self.data.len());
        let raw = self.data.get(start.min(end)..end)?;
        match entry.method {
            STORED => Some(Ok(if raw.len() == length {
                Decoded::Complete(W::stored(raw, pages))
            } else {
                Decoded::Truncated(W::stored(raw, pages))
            })),
            DEFLATED => Some(inflate_into(
                raw,
                decoding(raw.len(), u64::try_from(entry.size).ok()),
            )),
            method => {
                let size = u64::try_from(entry.size).ok()?;
                decode_into(
                    method,
                    entry.flags,
                    raw,
                    size,
                    decoding(raw.len(), Some(size)),
                )
            }
        }
    }
}

/// The name of an entry made on a Windows file system, where `\` separates paths and no name
/// holds one, with its backslashes turned into slashes; any other name as stored, where `\` is
/// a character of the name. commons-compress turns them only for FAT names without a slash,
/// and Tika then turns every backslash of every name (#101).
pub fn entry_name(name: String, version_made_by: u16) -> String {
    if WINDOWS_PLATFORMS.contains(&(version_made_by >> 8)) {
        name.replace('\\', "/")
    } else {
        name
    }
}

/// `getUnicodeStringIfOriginalMatches`: the text of a Unicode extra field of `id` whose CRC is
/// that of the original bytes.
pub(super) fn unicode_text(fields: &ExtraFields, id: u16, original: &[u8]) -> Option<String> {
    match fields.get(id)? {
        Field::Unicode { crc, text, .. } if *crc == crc32(original) => Some(decode_name(text)),
        _ => None,
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    #[test]
    fn turns_backslashes_only_in_names_made_on_windows() {
        let made_on = |host: u16| host << 8 | 20;
        assert_eq!(entry_name("a\\b.txt".into(), made_on(0)), "a/b.txt");
        assert_eq!(entry_name("d/a\\b.txt".into(), made_on(10)), "d/a/b.txt");
        assert_eq!(entry_name("a\\b.txt".into(), made_on(14)), "a/b.txt");
        assert_eq!(entry_name("a\\b.txt".into(), made_on(3)), "a\\b.txt");
        assert_eq!(entry_name("a\\b.txt".into(), made_on(19)), "a\\b.txt");
    }

    use std::io::Write;

    #[cfg(test)]
    use super::*;

    pub fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in entries {
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            writer.start_file(*name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// An archive of one entry, `name`, whose data is `data`, compressed with `method` from
    /// `size` bytes.
    pub fn zip_compressed(name: &str, method: u16, data: &[u8], size: u32) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file(name, options).unwrap();
        writer.write_all(data).unwrap();
        let mut archive = writer.finish().unwrap().into_inner();
        let central = archive.len() - 22 - 46 - name.len();
        assert_eq!(archive[central..central + 4], *b"PK\x01\x02");
        for (method_at, size_at) in [(8, 22), (central + 10, central + 24)] {
            archive[method_at..method_at + 2].copy_from_slice(&method.to_le_bytes());
            archive[size_at..size_at + 4].copy_from_slice(&size.to_le_bytes());
        }
        archive
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
        assert_eq!(
            file.read(entry),
            Some(Content::Complete(std::borrow::Cow::Borrowed(&b"<a/>"[..])))
        );
        assert_eq!(entry.size, 4);
        assert_eq!(entry.method, DEFLATED);
        assert!(entry.can_read());
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
    fn names_the_feature_commons_compress_does_not_read() {
        let feature = |method: u16| {
            let data = zip_compressed("a", method, b"x", 1);
            ZipFile::open(&data).unwrap().entries()[0].unsupported_feature()
        };
        assert_eq!(feature(DEFLATED), None);
        assert_eq!(
            feature(14).as_deref(),
            Some("Unsupported compression method 14 (LZMA) used in entry a")
        );
        assert_eq!(
            feature(11).as_deref(),
            Some("Unsupported feature compression method used in entry a")
        );
        assert!(!has_end_of_central_directory(b"not a zip"));
        assert!(has_end_of_central_directory(&zip(&[("a", b"x")])));
    }

    #[test]
    fn inflates_what_it_can() {
        let data = zip(&[("a", &[7u8; 5000])]);
        let file = ZipFile::open(&data).unwrap();
        let entry = file.entry("a").unwrap().clone();
        assert_eq!(file.read(&entry).unwrap().bytes().len(), 5000);
    }
}
