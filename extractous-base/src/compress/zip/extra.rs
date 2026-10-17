//! A ZIP entry's extra fields as commons-compress parses and merges them, best effort, and the
//! times `ZipArchiveEntry` and `java.util.zip.ZipEntry` read from them.

use super::Corrupt;

pub(crate) const ZIP64: u16 = 0x0001;
pub(crate) const NTFS: u16 = 0x000A;
pub(crate) const EXTENDED_TIMESTAMP: u16 = 0x5455;
pub(crate) const UNICODE_PATH: u16 = 0x7075;
pub(crate) const UNICODE_COMMENT: u16 = 0x6375;
/// Seconds between 1601-01-01, where NTFS times count from, and 1970-01-01.
const WINDOWS_EPOCH_SECONDS: i64 = 11_644_473_600;
/// The hundreds of nanoseconds between the NTFS epoch and Java's, `FileTimes.WINDOWS_EPOCH_OFFSET`.
const WINDOWS_EPOCH_OFFSET: i64 = -116_444_736_000_000_000;

/// An instant of a `FileTime`, in seconds and nanoseconds since 1970.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant {
    pub seconds: i64,
    pub nanos: u32,
}

impl Instant {
    pub(crate) fn from_millis(millis: i64) -> Self {
        Instant {
            seconds: millis.div_euclid(1000),
            nanos: (millis.rem_euclid(1000) * 1_000_000) as u32,
        }
    }

    fn from_micros(micros: i64) -> Self {
        Instant {
            seconds: micros.div_euclid(1_000_000),
            nanos: (micros.rem_euclid(1_000_000) * 1000) as u32,
        }
    }

    fn from_seconds(seconds: i64) -> Self {
        Instant { seconds, nanos: 0 }
    }
}

/// A parsed extra field, of the kinds whose content plays a part here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Field {
    /// `UnicodePathExtraField` or `UnicodeCommentExtraField`: the CRC-32 of the original bytes
    /// and the UTF-8 replacement.
    Unicode { id: u16, crc: u32, text: Vec<u8> },
    /// `X5455_ExtendedTimestamp`: the times its flags say are present, in Unix seconds.
    ExtendedTimestamp {
        modify: Option<i32>,
        access: Option<i32>,
        create: Option<i32>,
    },
    /// `X000A_NTFS`: times in hundreds of nanoseconds since 1601, zero for absent.
    Ntfs {
        modify: u64,
        access: u64,
        create: u64,
    },
    /// Any other field, or one that failed to parse, by its local and central data.
    Other {
        id: u16,
        local: Vec<u8>,
        central: Vec<u8>,
    },
    /// Data after the last field that does not form one.
    Unparseable(Vec<u8>),
}

impl Field {
    fn id(&self) -> Option<u16> {
        match self {
            Field::Unicode { id, .. } | Field::Other { id, .. } => Some(*id),
            Field::ExtendedTimestamp { .. } => Some(EXTENDED_TIMESTAMP),
            Field::Ntfs { .. } => Some(NTFS),
            Field::Unparseable(_) => None,
        }
    }

    /// `getLocalFileDataData`, prefixed with its header as `mergeLocalFileDataData` writes it.
    fn local_record(&self, out: &mut Vec<u8>) {
        let mut data = Vec::new();
        match self {
            Field::Unicode { crc, text, .. } => {
                data.push(1);
                data.extend_from_slice(&crc.to_le_bytes());
                data.extend_from_slice(text);
            }
            Field::ExtendedTimestamp {
                modify,
                access,
                create,
                ..
            } => {
                let present = [modify, access, create];
                let flags = present
                    .iter()
                    .enumerate()
                    .filter(|(_, time)| time.is_some())
                    .fold(0u8, |flags, (bit, _)| flags | (1 << bit));
                data.push(flags);
                for time in present.into_iter().flatten() {
                    data.extend_from_slice(&time.to_le_bytes());
                }
            }
            Field::Ntfs {
                modify,
                access,
                create,
            } => {
                data.extend_from_slice(&[0, 0, 0, 0, 1, 0, 24, 0]);
                for time in [modify, access, create] {
                    data.extend_from_slice(&time.to_le_bytes());
                }
            }
            Field::Other { local, .. } => data.extend_from_slice(local),
            Field::Unparseable(bytes) => {
                out.extend_from_slice(bytes);
                return;
            }
        }
        out.extend_from_slice(&self.id().expect("a field has an id").to_le_bytes());
        out.extend_from_slice(&(data.len() as u16).to_le_bytes());
        out.extend_from_slice(&data);
    }
}

fn u16_le(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32_le(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().expect("four bytes"))
}

fn u64_le(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().expect("eight bytes"))
}

/// `X5455_ExtendedTimestamp.parseFromLocalFileData`: the flags, and each time they flag that the
/// data holds.
fn extended_timestamp(data: &[u8]) -> Option<Field> {
    let (&flags, mut rest) = data.split_first()?;
    let mut take = |bit: u8| {
        if flags & bit == 0 || rest.len() < 4 {
            return None;
        }
        let value = u32_le(rest, 0) as i32;
        rest = &rest[4..];
        Some(value)
    };
    let modify = take(1);
    let access = take(2);
    let create = take(4);
    Some(Field::ExtendedTimestamp {
        modify,
        access,
        create,
    })
}

/// `X000A_NTFS.parseFromLocalFileData` onto the times it holds: the time attribute, when one of
/// the right size follows the reserved bytes, replaces them.
fn ntfs_times(data: &[u8], times: [u64; 3]) -> [u64; 3] {
    let mut at = 4;
    while at + 4 <= data.len() {
        let tag = u16_le(data, at);
        at += 2;
        if tag == 1 {
            if data.len() - at >= 26 && u16_le(data, at) == 24 {
                return [
                    u64_le(data, at + 2),
                    u64_le(data, at + 10),
                    u64_le(data, at + 18),
                ];
            }
            return times;
        }
        at += 2 + usize::from(u16_le(data, at));
    }
    times
}

/// `AbstractUnicodeExtraField.parseFromLocalFileData`.
fn unicode(id: u16, data: &[u8]) -> Option<Field> {
    (data.len() >= 5 && data[0] == 1).then(|| Field::Unicode {
        id,
        crc: u32_le(data, 1),
        text: data[5..].to_vec(),
    })
}

/// A field `ExtraFieldUtils.createExtraField` makes and fills from `data`, unrecognized when
/// filling it fails.
fn fill(id: u16, data: &[u8], local: bool) -> Field {
    let parsed = match id {
        UNICODE_PATH | UNICODE_COMMENT => unicode(id, data),
        EXTENDED_TIMESTAMP => extended_timestamp(data),
        NTFS => {
            let [modify, access, create] = ntfs_times(data, [0; 3]);
            Some(Field::Ntfs {
                modify,
                access,
                create,
            })
        }
        _ => None,
    };
    parsed.unwrap_or_else(|| Field::Other {
        id,
        local: if local { data.to_vec() } else { Vec::new() },
        central: if local { Vec::new() } else { data.to_vec() },
    })
}

/// `ExtraFieldUtils.parse` in best-effort mode.
pub(crate) fn parse(data: &[u8], local: bool) -> Vec<Field> {
    let mut fields = Vec::new();
    let mut start = 0;
    while start + 4 <= data.len() {
        let id = u16_le(data, start);
        let length = usize::from(u16_le(data, start + 2));
        if start + 4 + length > data.len() {
            fields.push(Field::Unparseable(data[start..].to_vec()));
            break;
        }
        fields.push(fill(id, &data[start + 4..start + 4 + length], local));
        start += 4 + length;
    }
    fields
}

/// What `java.util.zip.ZipEntry` and `ZipArchiveEntry` know of an entry's times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Times {
    pub(crate) modified: Option<Instant>,
    pub(crate) created: Option<Instant>,
}

/// An entry's extra fields, merged as `ZipArchiveEntry.mergeExtraFields` merges them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExtraFields {
    fields: Vec<Field>,
}

impl ExtraFields {
    /// `getExtraField`: the first field of `id`.
    pub(crate) fn get(&self, id: u16) -> Option<&Field> {
        self.fields.iter().find(|field| field.id() == Some(id))
    }

    /// `setCentralDirectoryExtra` onto an entry without fields, then the times it sets.
    pub(crate) fn from_central(data: &[u8], times: &mut Times) -> Result<Self, Corrupt> {
        let fields = ExtraFields {
            fields: parse(data, false),
        };
        fields.update_times(times)?;
        Ok(fields)
    }

    /// `setExtra(localData)`: the local fields merged in, then the times they set.
    pub(crate) fn merge_local(&mut self, data: &[u8], times: &mut Times) -> Result<(), Corrupt> {
        for field in parse(data, true) {
            let existing = match &field {
                Field::Unparseable(_) => self
                    .fields
                    .iter()
                    .position(|f| matches!(f, Field::Unparseable(_))),
                field => self.fields.iter().position(|f| f.id() == field.id()),
            };
            let Some(index) = existing else {
                self.fields.push(field);
                continue;
            };
            let mut record = Vec::new();
            field.local_record(&mut record);
            let local_data = match &field {
                Field::Unparseable(bytes) => bytes.clone(),
                _ => record[4..].to_vec(),
            };
            self.fields[index] = reparse_local(&self.fields[index], &local_data);
        }
        self.update_times(times)
    }

    /// `ZipArchiveEntry.setExtra()`: `ZipEntry` reads times from the local data of the fields,
    /// then the timestamp and NTFS fields overwrite them.
    fn update_times(&self, times: &mut Times) -> Result<(), Corrupt> {
        let mut local = Vec::new();
        for field in &self.fields {
            field.local_record(&mut local);
        }
        zip_entry_times(&local, times);
        if let Some(Field::ExtendedTimestamp { modify, create, .. }) = self.get(EXTENDED_TIMESTAMP)
        {
            if let Some(seconds) = modify {
                times.modified = Some(Instant::from_seconds(i64::from(*seconds)));
            }
            if let Some(seconds) = create {
                times.created = Some(Instant::from_seconds(i64::from(*seconds)));
            }
        }
        if let Some(Field::Ntfs { modify, create, .. }) = self.get(NTFS) {
            if *modify != 0 {
                times.modified = Some(ntfs_instant(*modify)?);
            }
            if *create != 0 {
                times.created = Some(ntfs_instant(*create)?);
            }
        }
        Ok(())
    }
}

/// `parseFromLocalFileData` of an existing field with a local field's data, a failure turning
/// it unrecognized with that data as its local data.
fn reparse_local(existing: &Field, data: &[u8]) -> Field {
    let failed = |central: Vec<u8>, id: u16| Field::Other {
        id,
        local: data.to_vec(),
        central,
    };
    match existing {
        Field::Unicode { id, .. } => unicode(*id, data).unwrap_or_else(|| failed(Vec::new(), *id)),
        Field::ExtendedTimestamp { .. } => {
            extended_timestamp(data).unwrap_or_else(|| failed(Vec::new(), EXTENDED_TIMESTAMP))
        }
        Field::Ntfs {
            modify,
            access,
            create,
        } => {
            let [modify, access, create] = ntfs_times(data, [*modify, *access, *create]);
            Field::Ntfs {
                modify,
                access,
                create,
            }
        }
        Field::Other { id, central, .. } => Field::Other {
            id: *id,
            local: data.to_vec(),
            central: central.clone(),
        },
        Field::Unparseable(_) => Field::Unparseable(data.to_vec()),
    }
}

/// `FileTimes.ntfsTimeToFileTime`, whose exact addition throws past `long`'s range.
fn ntfs_instant(ntfs: u64) -> Result<Instant, Corrupt> {
    let hundreds = (ntfs as i64)
        .checked_add(WINDOWS_EPOCH_OFFSET)
        .ok_or(Corrupt)?;
    Ok(Instant {
        seconds: hundreds.div_euclid(10_000_000),
        nanos: (hundreds.rem_euclid(10_000_000) * 100) as u32,
    })
}

/// `ZipEntry.setExtra0` over the local data of the fields: the extended timestamp's times and the
/// NTFS field's first attribute, at microsecond precision, a time of zero absent. `ZipEntry`
/// takes only `Long.MIN_VALUE` for absent, so a zero creation time becomes 1601-01-01 (#99).
fn zip_entry_times(extra: &[u8], times: &mut Times) {
    let mut off = 0;
    while off + 4 < extra.len() {
        let tag = u16_le(extra, off);
        let size = usize::from(u16_le(extra, off + 2));
        off += 4;
        if off + size > extra.len() {
            break;
        }
        match tag {
            NTFS if size >= 32 => {
                let pos = off + 4;
                if u16_le(extra, pos) == 1 && u16_le(extra, pos + 2) == 24 {
                    let modify = u64_le(extra, pos + 4) as i64;
                    let create = u64_le(extra, pos + 20) as i64;
                    let windows = |time: i64| {
                        Instant::from_micros(time / 10 - WINDOWS_EPOCH_SECONDS * 1_000_000)
                    };
                    if modify != 0 && modify != i64::MIN {
                        times.modified = Some(windows(modify));
                    }
                    if create != 0 && create != i64::MIN {
                        times.created = Some(windows(create));
                    }
                }
            }
            EXTENDED_TIMESTAMP => {
                let flags = extra[off];
                let mut at = 1;
                let mut take = |bit: u8| {
                    if flags & bit != 0 && at + 4 <= size {
                        let seconds = u32_le(extra, off + at) as i32;
                        at += 4;
                        Some(seconds)
                    } else {
                        None
                    }
                };
                if let Some(seconds) = take(1) {
                    times.modified = Some(Instant::from_seconds(i64::from(seconds)));
                }
                take(2);
                if let Some(seconds) = take(4) {
                    times.created = Some(Instant::from_seconds(i64::from(seconds)));
                }
            }
            _ => {}
        }
        off += size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_times_as_zip_archive_entry_does() {
        let mut times = Times {
            modified: Some(Instant::from_millis(1000)),
            created: None,
        };
        let mut extra = vec![0x55, 0x54, 13, 0, 7];
        extra.extend_from_slice(&100i32.to_le_bytes());
        extra.extend_from_slice(&200i32.to_le_bytes());
        extra.extend_from_slice(&300i32.to_le_bytes());
        ExtraFields::from_central(&extra, &mut times).unwrap();
        assert_eq!(times.modified, Some(Instant::from_seconds(100)));
        assert_eq!(times.created, Some(Instant::from_seconds(300)));
        let mut ntfs = vec![0x0A, 0, 32, 0, 0, 0, 0, 0, 1, 0, 24, 0];
        ntfs.extend_from_slice(&(116_444_736_000_000_005u64).to_le_bytes());
        ntfs.extend_from_slice(&0u64.to_le_bytes());
        ntfs.extend_from_slice(&0u64.to_le_bytes());
        let mut times = Times::default();
        ExtraFields::from_central(&ntfs, &mut times).unwrap();
        assert_eq!(
            times.modified,
            Some(Instant {
                seconds: 0,
                nanos: 500
            })
        );
        assert_eq!(times.created, None);
    }

    #[test]
    fn keeps_unparseable_data_apart() {
        let fields = parse(&[0x75, 0x70, 9, 0, 1, 2], true);
        assert_eq!(fields, [Field::Unparseable(vec![0x75, 0x70, 9, 0, 1, 2])]);
    }
}
