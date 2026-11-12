//! The structures of an RPM package before its payload: the 96-byte lead, of which only the
//! magic counts, as rpm reads it; the signature header, padded to a multiple of eight bytes;
//! and the main header, an index of tagged entries over a store of their data. Format 4 and
//! format 6 packages share this layout.
//!
//! The header is read in place and its entries' data is checked as each is read: offsets and
//! counts within the store, strings terminated within it, so that a hostile header can neither
//! read past it nor make the reader allocate more than its own size.

use std::collections::HashMap;

/// The bytes of the lead.
const LEAD: usize = 96;
const LEAD_MAGIC: [u8; 4] = [0xED, 0xAB, 0xEE, 0xDB];
/// The magic and version a header starts with, before four reserved bytes.
const HEADER_MAGIC: [u8; 4] = [0x8E, 0xAD, 0xE8, 0x01];
/// The bytes of a header's preamble: magic, reserved bytes, entry count and store size.
const PREAMBLE: usize = 16;
/// The bytes of an index entry: tag, type, offset and count.
const INDEX_ENTRY: usize = 16;
/// rpm's `HEADER_TAGS_MAX`, the most entries a header has.
const MAX_ENTRIES: u32 = 0xFFFF;
/// rpm's `HEADER_DATA_MAX`, the most bytes a header's store has.
const MAX_STORE: u32 = 0x0FFF_FFFF;

/// The tag of the header's encoding, which rpm requires to be UTF-8 where it is given.
const ENCODING: u32 = 5062;

/// The type of an entry's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Type {
    Null,
    Char,
    Int8,
    Int16,
    Int32,
    Int64,
    String,
    Bin,
    StringArray,
    /// One string per locale of the header's `HEADERI18NTABLE`, the C locale's first.
    I18nString,
}

impl Type {
    fn of(code: u32) -> Option<Type> {
        Some(match code {
            0 => Type::Null,
            1 => Type::Char,
            2 => Type::Int8,
            3 => Type::Int16,
            4 => Type::Int32,
            5 => Type::Int64,
            6 => Type::String,
            7 => Type::Bin,
            8 => Type::StringArray,
            9 => Type::I18nString,
            _ => return None,
        })
    }

    /// The bytes of each of an entry's numbers, for the integer types.
    fn width(self) -> Option<usize> {
        match self {
            Type::Char | Type::Int8 => Some(1),
            Type::Int16 => Some(2),
            Type::Int32 => Some(4),
            Type::Int64 => Some(8),
            Type::Null | Type::String | Type::Bin | Type::StringArray | Type::I18nString => None,
        }
    }
}

/// An entry of the index: its data's type, where it starts in the store, and how many values
/// it holds.
#[derive(Debug, Clone, Copy)]
struct Entry {
    kind: Type,
    offset: usize,
    count: usize,
}

/// How the header's strings are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// UTF-8, as the header's `ENCODING` tag says.
    Utf8,
    /// Unsaid, as in packages built before rpm 4.13 wrote the tag: UTF-8 where the bytes are,
    /// else ISO 8859-1, which the packages of that time were mostly written in.
    Legacy,
}

/// A header: its index by tag, over its store.
#[derive(Debug)]
pub(super) struct Header<'a> {
    entries: HashMap<u32, Entry>,
    store: &'a [u8],
    encoding: Encoding,
}

/// A package's main header, and its payload, what follows it.
#[derive(Debug)]
pub(super) struct Package<'a> {
    pub(super) header: Header<'a>,
    pub(super) payload: &'a [u8],
}

/// The main header and the payload of the package `input`, or why they cannot be read.
pub(super) fn read(input: &[u8]) -> Result<Package<'_>, String> {
    if input.len() < LEAD || !input.starts_with(&LEAD_MAGIC) {
        return Err(format!(
            "it does not start with the {LEAD}-byte lead of an RPM package"
        ));
    }
    let (_, length) = Header::parse(&input[LEAD..])
        .map_err(|reason| format!("its signature header: {reason}"))?;
    let start = LEAD + length.next_multiple_of(8);
    let rest = input.get(start..).ok_or_else(|| {
        format!("it ends inside the padding of its signature header, at byte {start}")
    })?;
    let (header, length) =
        Header::parse(rest).map_err(|reason| format!("its main header: {reason}"))?;
    Ok(Package {
        header,
        payload: &rest[length..],
    })
}

impl<'a> Header<'a> {
    /// The header at the start of `input`, and its length, or why it cannot be read.
    fn parse(input: &'a [u8]) -> Result<(Header<'a>, usize), String> {
        let preamble = input
            .get(..PREAMBLE)
            .ok_or("the input ends inside its preamble")?;
        if preamble[..4] != HEADER_MAGIC {
            return Err(format!(
                "it starts with {:02X?}, not the magic of a header",
                &preamble[..4]
            ));
        }
        let count = be32(&preamble[8..12]);
        let size = be32(&preamble[12..16]);
        if count > MAX_ENTRIES {
            return Err(format!(
                "it has {count} entries, more than the {MAX_ENTRIES} rpm allows"
            ));
        }
        if size > MAX_STORE {
            return Err(format!(
                "its store has {size} bytes, more than the {MAX_STORE} rpm allows"
            ));
        }
        let index_length = count as usize * INDEX_ENTRY;
        let length = PREAMBLE + index_length + size as usize;
        let index = input
            .get(PREAMBLE..PREAMBLE + index_length)
            .ok_or_else(|| format!("the input ends inside its index of {count} entries"))?;
        let store = input
            .get(PREAMBLE + index_length..length)
            .ok_or_else(|| format!("the input ends inside its store of {size} bytes"))?;
        let mut entries = HashMap::with_capacity(count as usize);
        for raw in index.as_chunks::<INDEX_ENTRY>().0 {
            let tag = be32(&raw[0..4]);
            let code = be32(&raw[4..8]);
            let kind = Type::of(code)
                .ok_or_else(|| format!("the entry of tag {tag} has the unknown type {code}"))?;
            let offset = be32(&raw[8..12]) as usize;
            if offset > store.len() {
                return Err(format!(
                    "the entry of tag {tag} starts at byte {offset}, past its store of {} bytes",
                    store.len()
                ));
            }
            let entry = Entry {
                kind,
                offset,
                count: be32(&raw[12..16]) as usize,
            };
            if entries.insert(tag, entry).is_some() {
                return Err(format!("it has more than one entry of tag {tag}"));
            }
        }
        let mut header = Header {
            entries,
            store,
            encoding: Encoding::Legacy,
        };
        header.encoding = match header.raw_strings(ENCODING)? {
            None => Encoding::Legacy,
            Some(mut values) => match values.next().transpose()? {
                Some(b"utf-8") => Encoding::Utf8,
                other => {
                    return Err(format!(
                        "its encoding is {:?}, not the utf-8 rpm allows",
                        other.map(String::from_utf8_lossy)
                    ))
                }
            },
        };
        Ok((header, length))
    }

    /// The first string of the entry of `tag`, a string or the C locale's of an I18N string,
    /// if the header has one.
    pub(super) fn string(&self, tag: u32) -> Result<Option<String>, String> {
        let Some(entry) = self.entries.get(&tag) else {
            return Ok(None);
        };
        if !matches!(entry.kind, Type::String | Type::I18nString) {
            return Err(format!(
                "the entry of tag {tag} is of type {:?}, not a string",
                entry.kind
            ));
        }
        self.strings(tag)
            .map(|strings| strings.and_then(|strings| strings.into_iter().next()))
    }

    /// The strings of the entry of `tag`, of any of the string types, if the header has one.
    pub(super) fn strings(&self, tag: u32) -> Result<Option<Vec<String>>, String> {
        let Some(raw) = self.raw_strings(tag)? else {
            return Ok(None);
        };
        raw.map(|bytes| bytes.and_then(|bytes| self.decode(tag, bytes)))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// The raw strings of the entry of `tag`, each checked to end within the store as it is
    /// read, if the header has one.
    fn raw_strings(
        &self,
        tag: u32,
    ) -> Result<Option<impl Iterator<Item = Result<&'a [u8], String>> + 'a>, String> {
        let Some(&entry) = self.entries.get(&tag) else {
            return Ok(None);
        };
        let count = match entry.kind {
            Type::String => 1,
            Type::StringArray | Type::I18nString => entry.count,
            other => {
                return Err(format!(
                    "the entry of tag {tag} is of type {other:?}, not strings"
                ))
            }
        };
        let mut rest = &self.store[entry.offset..];
        if count > rest.len() {
            return Err(format!(
                "the entry of tag {tag} has {count} strings, more than the {} bytes after it",
                rest.len()
            ));
        }
        Ok(Some((0..count).map(move |index| {
            let end = memchr::memchr(0, rest).ok_or_else(|| {
                format!("string {index} of the entry of tag {tag} does not end within the store")
            })?;
            let (string, after) = rest.split_at(end);
            rest = &after[1..];
            Ok(string)
        })))
    }

    fn decode(&self, tag: u32, bytes: &[u8]) -> Result<String, String> {
        match (std::str::from_utf8(bytes), self.encoding) {
            (Ok(text), _) => Ok(text.to_owned()),
            (Err(error), Encoding::Utf8) => Err(format!(
                "a string of the entry of tag {tag} is not the UTF-8 the header says: {error}"
            )),
            (Err(_), Encoding::Legacy) => Ok(bytes.iter().map(|&b| char::from(b)).collect()),
        }
    }

    /// The numbers of the entry of `tag`, of any of the integer types, if the header has one.
    pub(super) fn numbers(&self, tag: u32) -> Result<Option<Vec<u64>>, String> {
        let Some(&entry) = self.entries.get(&tag) else {
            return Ok(None);
        };
        let width = entry.kind.width().ok_or_else(|| {
            format!(
                "the entry of tag {tag} is of type {:?}, not numbers",
                entry.kind
            )
        })?;
        let data = entry
            .count
            .checked_mul(width)
            .and_then(|length| {
                self.store
                    .get(entry.offset..entry.offset.checked_add(length)?)
            })
            .ok_or_else(|| {
                format!(
                    "the {} numbers of the entry of tag {tag} end past its store",
                    entry.count
                )
            })?;
        Ok(Some(
            data.chunks_exact(width)
                .map(|number| {
                    number
                        .iter()
                        .fold(0u64, |value, &byte| value << 8 | u64::from(byte))
                })
                .collect(),
        ))
    }

    /// The first number of the entry of `tag`, if the header has one with a number.
    pub(super) fn number(&self, tag: u32) -> Result<Option<u64>, String> {
        Ok(self
            .numbers(tag)?
            .and_then(|numbers| numbers.into_iter().next()))
    }
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().expect("four bytes"))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A value of a header entry, as tests write it.
    pub(in super::super) enum Value<'v> {
        String(&'v str),
        I18n(&'v [&'v str]),
        Strings(&'v [&'v str]),
        Int16(&'v [u16]),
        Int32(&'v [u32]),
        Int64(&'v [u64]),
    }

    /// A header of `entries`, as rpm writes one, each value aligned to its type.
    pub(in super::super) fn header(entries: &[(u32, Value)]) -> Vec<u8> {
        let mut index = Vec::new();
        let mut store = Vec::new();
        for (tag, value) in entries {
            let (code, count, align) = match value {
                Value::String(_) => (6, 1, 1),
                Value::I18n(strings) => (9, strings.len(), 1),
                Value::Strings(strings) => (8, strings.len(), 1),
                Value::Int16(numbers) => (3, numbers.len(), 2),
                Value::Int32(numbers) => (4, numbers.len(), 4),
                Value::Int64(numbers) => (5, numbers.len(), 8),
            };
            store.resize(store.len().next_multiple_of(align), 0);
            for field in [*tag, code, store.len() as u32, count as u32] {
                index.extend_from_slice(&field.to_be_bytes());
            }
            let mut strings = |strings: &[&str]| {
                for string in strings {
                    store.extend_from_slice(string.as_bytes());
                    store.push(0);
                }
            };
            match value {
                Value::String(string) => strings(&[string]),
                Value::I18n(list) | Value::Strings(list) => strings(list),
                Value::Int16(numbers) => {
                    store.extend(numbers.iter().flat_map(|number| number.to_be_bytes()))
                }
                Value::Int32(numbers) => {
                    store.extend(numbers.iter().flat_map(|number| number.to_be_bytes()))
                }
                Value::Int64(numbers) => {
                    store.extend(numbers.iter().flat_map(|number| number.to_be_bytes()))
                }
            }
        }
        let mut out = HEADER_MAGIC.to_vec();
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        out.extend_from_slice(&(store.len() as u32).to_be_bytes());
        out.extend_from_slice(&index);
        out.extend_from_slice(&store);
        out
    }

    /// A package of the main header `main` and `payload`, after a lead and an empty signature
    /// header, which rpm pads to eight bytes.
    pub(in super::super) fn package(main: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = LEAD_MAGIC.to_vec();
        out.extend_from_slice(&[3, 0]);
        out.resize(LEAD, 0);
        out.extend_from_slice(&header(&[(62, Value::Int32(&[0, 0, 0, 0]))]));
        out.resize(out.len().next_multiple_of(8), 0);
        out.extend_from_slice(main);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn reads_the_entries_of_each_type_after_the_signature() {
        let main = header(&[
            (1000, Value::String("lighthouse")),
            (1004, Value::I18n(&["The log", "Le journal"])),
            (1117, Value::Strings(&["a.txt", "b.txt"])),
            (1030, Value::Int16(&[0o100644, 0o040755])),
            (1006, Value::Int32(&[1_794_484_800])),
            (5008, Value::Int64(&[5_000_000_000])),
        ]);
        let package = package(&main, b"payload");
        let Package { header, payload } = read(&package).unwrap();
        assert_eq!(payload, b"payload");
        assert_eq!(header.string(1000).unwrap().as_deref(), Some("lighthouse"));
        assert_eq!(header.string(1004).unwrap().as_deref(), Some("The log"));
        assert_eq!(header.strings(1117).unwrap().unwrap(), ["a.txt", "b.txt"]);
        assert_eq!(header.numbers(1030).unwrap().unwrap(), [0o100644, 0o040755]);
        assert_eq!(header.number(1006).unwrap(), Some(1_794_484_800));
        assert_eq!(header.number(5008).unwrap(), Some(5_000_000_000));
        assert_eq!(header.string(1001).unwrap(), None);
        assert!(header.string(1006).unwrap_err().contains("not a string"));
        assert!(header.numbers(1000).unwrap_err().contains("not numbers"));
    }

    #[test]
    fn decodes_strings_as_the_header_says_they_are_encoded() {
        let legacy = header(&[(1015, Value::String("K\u{e6}per"))]);
        let mut latin1 = legacy.clone();
        let at = latin1.windows(2).position(|w| w == "æ".as_bytes()).unwrap();
        latin1.splice(at..at + 2, [0xE6, b'_']);
        let (parsed, _) = Header::parse(&latin1).unwrap();
        assert_eq!(parsed.string(1015).unwrap().as_deref(), Some("K\u{e6}_per"));
        let (parsed, _) = Header::parse(&legacy).unwrap();
        assert_eq!(parsed.string(1015).unwrap().as_deref(), Some("K\u{e6}per"));
        let mut strict = header(&[
            (1015, Value::String("K\u{e6}per")),
            (ENCODING, Value::String("utf-8")),
        ]);
        let at = strict.windows(2).position(|w| w == "æ".as_bytes()).unwrap();
        strict.splice(at..at + 2, [0xE6, b'_']);
        let (parsed, _) = Header::parse(&strict).unwrap();
        assert!(parsed.string(1015).unwrap_err().contains("not the UTF-8"));
        let other = header(&[(ENCODING, Value::String("latin1"))]);
        assert!(Header::parse(&other).unwrap_err().contains("latin1"));
    }

    #[test]
    fn refuses_hostile_headers_with_what_is_wrong() {
        let reason = |data: &[u8]| Header::parse(data).unwrap_err();
        let valid = header(&[(1000, Value::String("x"))]);
        let mut many = valid.clone();
        many[8..12].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        assert!(reason(&many).contains("65536 entries"), "{}", reason(&many));
        let mut large = valid.clone();
        large[12..16].copy_from_slice(&0x1000_0000u32.to_be_bytes());
        assert!(reason(&large).contains("more than"), "{}", reason(&large));
        assert!(reason(&valid[..valid.len() - 1]).contains("ends inside its store"));
        let mut past = valid.clone();
        past[24..28].copy_from_slice(&100u32.to_be_bytes());
        assert!(
            reason(&past).contains("past its store"),
            "{}",
            reason(&past)
        );
        let mut typed = valid.clone();
        typed[20..24].copy_from_slice(&17u32.to_be_bytes());
        assert!(reason(&typed).contains("unknown type 17"));
        let unterminated = [&valid[..valid.len() - 1], b"y"].concat();
        let (parsed, _) = Header::parse(&unterminated).unwrap();
        assert!(parsed.string(1000).unwrap_err().contains("does not end"));
        let mut counted = header(&[(1117, Value::Strings(&["a"]))]);
        counted[28..32].copy_from_slice(&u32::MAX.to_be_bytes());
        let (parsed, _) = Header::parse(&counted).unwrap();
        assert!(parsed
            .strings(1117)
            .unwrap_err()
            .contains("more than the 2 bytes"));
        let mut numbers = header(&[(1028, Value::Int32(&[1]))]);
        numbers[28..32].copy_from_slice(&u32::MAX.to_be_bytes());
        let (parsed, _) = Header::parse(&numbers).unwrap();
        assert!(parsed.numbers(1028).unwrap_err().contains("past its store"));
        let twice = header(&[(1000, Value::String("x")), (1000, Value::String("y"))]);
        assert!(reason(&twice).contains("more than one entry of tag 1000"));
        assert!(reason(b"\x8E\xAD\xE8\x01").contains("preamble"));
        assert!(reason(&[0; 16]).contains("not the magic"));
        assert!(read(b"\xED\xAB\xEE\xDB").unwrap_err().contains("lead"));
    }
}
