//! ZIP archives read front to back by their local headers, as commons-compress's
//! `ZipArchiveInputStream` reads them, for the archives `ZipFile` cannot open.
//!
//! A stored entry with a data descriptor is found by scanning for the next signature, as
//! `readStoredEntry` does, without its 512-byte buffering. Entries are read stored or deflated
//! only, as by [`super::file`]. Names come from the Unicode path extra field where its CRC
//! matches, as `useUnicodeExtraFields` asks.

use super::extra::{self, ExtraFields, Times};
use super::file::{entry_name, unicode_text};
use super::{decode_name, inflate, u16_at, u32_at, Content, CFH_SIG, DEFLATED, LFH_SIG, STORED};

const SPLIT_MARKER: [u8; 4] = *b"PK00";
const DD_SIG: [u8; 4] = *b"PK\x07\x08";
const EOCD_SIG: [u8; 4] = *b"PK\x05\x06";
const AED_SIG: [u8; 4] = *b"PK\x06\x08";
const PREAMBLE_GARBAGE_MAX_SIZE: usize = 4096;
const DEFLATE64: u16 = 9;
const XZ: u16 = 95;
const ZSTD: [u16; 2] = [93, 20];
/// Where an entry whose data the input cuts short ends: nowhere a next entry can start.
const TRUNCATED: usize = usize::MAX;

/// Why the stream stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// An entry with a data descriptor the stream cannot read past as configured.
    DataDescriptor,
    /// Anything else the stream throws.
    Io,
}

/// An entry as the stream reads it.
pub(crate) struct StreamEntry {
    pub(crate) name: String,
    /// What reading the entry's data gives.
    pub(crate) data: Result<Content, Failure>,
}

/// The local header of the entry the stream is in.
struct Current {
    flags: u16,
    method: u16,
    compressed_size: i64,
    size: i64,
    data_start: usize,
    zip64: bool,
}

impl Current {
    fn has_data_descriptor(&self) -> bool {
        self.flags & 0x0008 != 0
    }
}

/// A `ZipArchiveInputStream` over the whole input.
pub(crate) struct ZipStream<'a> {
    data: &'a [u8],
    position: usize,
    allow_stored_data_descriptor: bool,
    first: bool,
    done: bool,
    current: Option<Current>,
}

impl<'a> ZipStream<'a> {
    pub(crate) fn new(data: &'a [u8], allow_stored_data_descriptor: bool) -> ZipStream<'a> {
        ZipStream {
            data,
            position: 0,
            allow_stored_data_descriptor,
            first: true,
            done: false,
            current: None,
        }
    }

    fn bytes(&self, at: usize, length: usize) -> Result<&'a [u8], Failure> {
        self.data
            .get(at..at.saturating_add(length))
            .ok_or(Failure::Io)
    }

    /// `getNextZipEntry`: `None` at the end of the entries.
    pub(crate) fn next_entry(&mut self) -> Result<Option<StreamEntry>, Failure> {
        if self.done {
            return Ok(None);
        }
        let header_at = match self.current.take() {
            Some(current) => self.close(&current)?,
            None if self.first => {
                self.first = false;
                match self.first_header()? {
                    Some(at) => at,
                    None => {
                        self.done = true;
                        return Ok(None);
                    }
                }
            }
            None => self.position,
        };
        self.position = header_at;
        let Ok(header) = self.bytes(header_at, 30) else {
            self.done = true;
            return Ok(None);
        };
        if header[..4] != LFH_SIG {
            if header[..4] == CFH_SIG || header[..4] == AED_SIG {
                self.done = true;
                return Ok(None);
            }
            return Err(Failure::Io);
        }
        let version = u16_at(header, 4);
        let flags = u16_at(header, 6);
        let name_length = usize::from(u16_at(header, 26));
        let extra_length = usize::from(u16_at(header, 28));
        let raw_name = self.bytes(header_at + 30, name_length)?;
        let extra = self.bytes(header_at + 30 + name_length, extra_length)?;
        let fields =
            ExtraFields::from_central(extra, &mut Times::default()).map_err(|_| Failure::Io)?;
        let mut name = entry_name(decode_name(raw_name), version);
        if flags & 0x0800 == 0 {
            if let Some(unicode) = unicode_text(&fields, extra::UNICODE_PATH, raw_name) {
                name = entry_name(unicode, version);
            }
        }
        let zip64 = super::file::zip64_field(extra);
        let mut current = Current {
            flags,
            method: u16_at(header, 8),
            compressed_size: -1,
            size: -1,
            data_start: header_at + 30 + name_length + extra_length,
            zip64: zip64.is_some(),
        };
        if !current.has_data_descriptor() {
            let (compressed, size) = (u32_at(header, 18), u32_at(header, 22));
            match zip64 {
                Some(field) if compressed == 0xFFFF_FFFF || size == 0xFFFF_FFFF => {
                    if field.len() < 16 {
                        return Err(Failure::Io);
                    }
                    let eight = |at: usize| {
                        i64::from_le_bytes(field[at..at + 8].try_into().expect("eight bytes"))
                    };
                    current.size = eight(0);
                    current.compressed_size = eight(8);
                }
                _ => {
                    current.compressed_size = compressed;
                    current.size = size;
                }
            }
            if current.compressed_size < 0 || current.size < 0 {
                return Err(Failure::Io);
            }
        }
        let data = self.read(&current).map(|(content, _)| content);
        self.current = Some(current);
        Ok(Some(StreamEntry { name, data }))
    }

    /// `readFirstLocalFileHeader`: where the first local header starts, `None` for an empty
    /// archive.
    fn first_header(&self) -> Result<Option<usize>, Failure> {
        for at in 0..=PREAMBLE_GARBAGE_MAX_SIZE - 4 {
            let signature = self.bytes(at, 4)?;
            if signature == LFH_SIG {
                return Ok(Some(at));
            }
            if signature == SPLIT_MARKER {
                return Ok(Some(at + 4));
            }
            if signature == DD_SIG {
                return Err(Failure::Io);
            }
            if signature == EOCD_SIG {
                return Ok(None);
            }
        }
        Err(Failure::Io)
    }

    /// The entry's data, read to its end, and where the entry, data descriptor included, ends.
    fn read(&self, current: &Current) -> Result<(Content, usize), Failure> {
        if current.flags & 0x0001 != 0 {
            return Err(Failure::Io);
        }
        let method = current.method;
        if current.has_data_descriptor()
            && !(method == DEFLATED
                || method == DEFLATE64
                || method == XZ
                || ZSTD.contains(&method)
                || (method == STORED && self.allow_stored_data_descriptor))
        {
            return Err(Failure::DataDescriptor);
        }
        match method {
            STORED if current.has_data_descriptor() => self.read_stored_with_descriptor(current),
            STORED => {
                let size = usize::try_from(current.size).map_err(|_| Failure::Io)?;
                match self.bytes(current.data_start, size) {
                    Ok(bytes) => Ok((Content::Complete(bytes.to_vec()), current.data_start + size)),
                    Err(_) => {
                        let partial = self.data.get(current.data_start..).unwrap_or_default();
                        Ok((Content::Truncated(partial.to_vec()), TRUNCATED))
                    }
                }
            }
            DEFLATED => {
                let raw = self.data.get(current.data_start..).unwrap_or_default();
                let bytes = match inflate(raw) {
                    Content::Complete(bytes) => bytes,
                    truncated => return Ok((truncated, TRUNCATED)),
                };
                let end = current.data_start + deflate_length(raw);
                let end = if current.has_data_descriptor() {
                    self.data_descriptor_end(end)?
                } else {
                    end
                };
                Ok((Content::Complete(bytes), end))
            }
            _ => Err(Failure::Io),
        }
    }

    /// `readDataDescriptor`: where a data descriptor starting at `at` ends, its sizes four bytes
    /// each when a signature follows them, eight otherwise.
    fn data_descriptor_end(&self, at: usize) -> Result<usize, Failure> {
        let at = if self.bytes(at, 4)? == DD_SIG {
            at + 8
        } else {
            at + 4
        };
        let sizes = self.bytes(at, 16)?;
        if sizes[8..12] == CFH_SIG || sizes[8..12] == LFH_SIG {
            Ok(at + 8)
        } else {
            Ok(at + 16)
        }
    }

    /// `readStoredEntry`: the data up to the next signature, checked against the sizes its data
    /// descriptor gives.
    fn read_stored_with_descriptor(&self, current: &Current) -> Result<(Content, usize), Failure> {
        let descriptor_length = if current.zip64 { 20 } else { 12 };
        let start = current.data_start;
        let mut at = start;
        let descriptor = loop {
            let window = self.bytes(at, 4)?;
            if window[..2] == *b"PK" {
                if (at - start >= descriptor_length && window[2..] == LFH_SIG[2..])
                    || window[2..] == CFH_SIG[2..]
                {
                    break at.checked_sub(descriptor_length).ok_or(Failure::Io)?;
                }
                if window[2..] == DD_SIG[2..] {
                    break at;
                }
            }
            at += 1;
        };
        let end = self.data_descriptor_end(descriptor)?;
        let sizes_at = if self.bytes(descriptor, 4)? == DD_SIG {
            descriptor + 8
        } else {
            descriptor + 4
        };
        let sizes = self.bytes(sizes_at, 16)?;
        let (compressed, size) = if end - sizes_at == 8 {
            (u32_at(sizes, 0), u32_at(sizes, 4))
        } else {
            let eight =
                |at: usize| i64::from_le_bytes(sizes[at..at + 8].try_into().expect("eight bytes"));
            (eight(0), eight(8))
        };
        let content = self.data.get(start..descriptor).ok_or(Failure::Io)?;
        if compressed != size || content.len() as i64 != size {
            return Err(Failure::Io);
        }
        Ok((Content::Complete(content.to_vec()), end))
    }

    /// `closeEntry`: where the next local header starts.
    fn close(&self, current: &Current) -> Result<usize, Failure> {
        if !current.has_data_descriptor() {
            let end = current.data_start as i64 + current.compressed_size;
            return if end > self.data.len() as i64 {
                Err(Failure::Io)
            } else {
                Ok(end as usize)
            };
        }
        match self.read(current)? {
            (_, TRUNCATED) => Err(Failure::Io),
            (_, end) => Ok(end),
        }
    }
}

/// The length of the complete deflate stream at the start of `raw`.
fn deflate_length(raw: &[u8]) -> usize {
    let mut inflater = flate2::Decompress::new(false);
    let mut sink = vec![0u8; 64 * 1024];
    loop {
        let before = inflater.total_in();
        let consumed = usize::try_from(before)
            .expect("the input fits in memory")
            .min(raw.len());
        match inflater.decompress(&raw[consumed..], &mut sink, flate2::FlushDecompress::None) {
            Ok(flate2::Status::StreamEnd) | Err(_) => break,
            Ok(_) if inflater.total_in() == before && consumed == raw.len() => break,
            Ok(_) => {}
        }
    }
    usize::try_from(inflater.total_in()).expect("the input fits in memory")
}
