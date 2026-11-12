//! Rock Ridge (RRIP, IEEE P1282) over the System Use Sharing Protocol (SUSP, IEEE P1281): the
//! entries in the system use field of a directory record, and in the continuation areas `CE`
//! chains to, that give a file its POSIX name (`NM`), its times (`TF`), the target of a
//! symbolic link (`SL`), where a deep directory was relocated (`CL`, `RE`) and whether its data
//! is zisofs-compressed (`ZF`).

use super::dates;
use super::directory::extent;

/// The most continuation areas the entries of one record are read from, past which a chain is
/// taken for a loop.
const MAX_CONTINUATIONS: usize = 32;
/// The identifiers an `ER` entry names Rock Ridge by, in its versions 1.09, 1.10 and 1.12.
const RRIP_IDENTIFIERS: [&[u8]; 3] = [b"RRIP_1991A", b"IEEE_P1282", b"IEEE_1282"];
const NM_CURRENT: u8 = 0x02;
const NM_PARENT: u8 = 0x04;
const SL_CONTINUE: u8 = 0x01;
const SL_CURRENT: u8 = 0x02;
const SL_PARENT: u8 = 0x04;
const SL_ROOT: u8 = 0x08;
const TF_LONG_FORM: u8 = 0x80;
const TF_CREATION: u8 = 0;
const TF_MODIFY: u8 = 1;
const TF_STAMPS: u8 = 7;

/// What the Rock Ridge entries of a directory record say of its file.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RockRidge {
    pub(super) name: Option<String>,
    /// The target of a symbolic link, as it is recorded.
    pub(super) link: Option<String>,
    pub(super) created: Option<i64>,
    pub(super) modified: Option<i64>,
    /// The logical block of the directory a record of a relocated directory stands for.
    pub(super) child: Option<u64>,
    /// Whether the record is a relocated directory, read where its `CL` record stands.
    pub(super) relocated: bool,
    /// Whether the data is zisofs-compressed.
    pub(super) compressed: bool,
}

/// The bytes to skip at the start of each system use field but the root's own, if the system
/// use field `root` of the root directory's own record starts with an `SP` entry and its
/// entries name Rock Ridge, by an `ER` entry or, before `ER` was defined, by an `RR` entry.
pub(super) fn detect(input: &[u8], block_size: u64, root: &[u8]) -> Result<Option<usize>, String> {
    let [b'S', b'P', 7, 1, 0xBE, 0xEF, skip, ..] = *root else {
        return Ok(None);
    };
    let mut rock_ridge = false;
    each(input, block_size, root, |signature, data| {
        rock_ridge |= match signature {
            b"RR" => true,
            b"ER" => {
                let identifier_length = usize::from(*data.first().unwrap_or(&0));
                let identifier = data.get(4..4 + identifier_length);
                identifier.is_some_and(|identifier| RRIP_IDENTIFIERS.contains(&identifier))
            }
            _ => false,
        };
        Ok(())
    })?;
    Ok(rock_ridge.then_some(usize::from(skip)))
}

/// The Rock Ridge entries of the system use field `area`, past the bytes to skip.
pub(super) fn read(input: &[u8], block_size: u64, area: &[u8]) -> Result<RockRidge, String> {
    let mut rock_ridge = RockRidge::default();
    let mut name: Option<Vec<u8>> = None;
    let mut link = Link::default();
    each(input, block_size, area, |signature, data| {
        match signature {
            b"NM" => {
                let (&flags, part) = data.split_first().ok_or("an NM entry has no flags")?;
                if flags & (NM_CURRENT | NM_PARENT) == 0 {
                    name.get_or_insert_with(Vec::new).extend_from_slice(part);
                }
            }
            b"SL" => link.extend(data)?,
            b"TF" => times(data, &mut rock_ridge)?,
            b"CL" => {
                let block = data.get(..4).ok_or("a CL entry has no location")?;
                rock_ridge.child = Some(u64::from(u32::from_le_bytes(block.try_into().unwrap())));
            }
            b"RE" => rock_ridge.relocated = true,
            b"ZF" => rock_ridge.compressed = true,
            _ => {}
        }
        Ok(())
    })?;
    rock_ridge.name = name.map(|name| String::from_utf8_lossy(&name).into_owned());
    rock_ridge.link = link.target;
    Ok(rock_ridge)
}

/// Hands `each` the signature and data of each entry of the system use field `area` and of the
/// continuation areas its `CE` entries chain to, up to an `ST` entry or the padding at an
/// area's end.
fn each(
    input: &[u8],
    block_size: u64,
    area: &[u8],
    mut each: impl FnMut(&[u8; 2], &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let mut area = area;
    for _ in 0..=MAX_CONTINUATIONS {
        let mut continuation = None;
        let mut rest = area;
        while rest.len() >= 4 && rest[0] != 0 {
            let length = usize::from(rest[2]);
            if length < 4 || length > rest.len() {
                return Err(format!(
                    "a system use entry {:?} is {length} bytes long, with {} left in its area",
                    String::from_utf8_lossy(&rest[..2]),
                    rest.len()
                ));
            }
            let (entry, after) = rest.split_at(length);
            let signature = [entry[0], entry[1]];
            match &signature {
                b"ST" => break,
                b"CE" => continuation = Some(continued(input, block_size, &entry[4..])?),
                _ => each(&signature, &entry[4..])?,
            }
            rest = after;
        }
        match continuation {
            Some(next) => area = next,
            None => return Ok(()),
        }
    }
    Err(format!(
        "its system use entries chain to more than {MAX_CONTINUATIONS} continuation areas"
    ))
}

/// The continuation area a `CE` entry of data `data` points to, within one logical block.
fn continued<'a>(input: &'a [u8], block_size: u64, data: &[u8]) -> Result<&'a [u8], String> {
    let word = |at: usize| {
        data.get(at..at + 4)
            .map(|bytes| u64::from(u32::from_le_bytes(bytes.try_into().unwrap())))
            .ok_or("a CE entry is cut short")
    };
    let (block, offset, length) = (word(0)?, word(8)?, word(16)?);
    if offset + length > block_size {
        return Err(format!(
            "a continuation area of {length} bytes at offset {offset} of block {block} crosses \
             the block's end"
        ));
    }
    let area = extent(input, block, block_size, offset + length)?;
    Ok(&area[offset as usize..])
}

/// The creation and modification times of a `TF` entry of data `data`.
fn times(data: &[u8], rock_ridge: &mut RockRidge) -> Result<(), String> {
    let (&flags, mut stamps) = data.split_first().ok_or("a TF entry has no flags")?;
    let size = if flags & TF_LONG_FORM != 0 { 17 } else { 7 };
    for stamp in (0..TF_STAMPS).filter(|stamp| flags & (1 << stamp) != 0) {
        if stamps.len() < size {
            return Err(format!("a TF entry with flags {flags:#04x} is cut short"));
        }
        let (bytes, rest) = stamps.split_at(size);
        stamps = rest;
        let millis = if size == 17 {
            dates::long(bytes.try_into().unwrap())
        } else {
            dates::short(bytes.try_into().unwrap())
        };
        match stamp {
            TF_CREATION => rock_ridge.created = millis,
            TF_MODIFY => rock_ridge.modified = millis,
            _ => {}
        }
    }
    Ok(())
}

/// The target of a symbolic link, as its `SL` entries give it component by component.
#[derive(Debug, Default)]
struct Link {
    target: Option<String>,
    /// Whether the last component goes on in the next.
    continuing: bool,
}

impl Link {
    fn extend(&mut self, data: &[u8]) -> Result<(), String> {
        let mut components = data.get(1..).ok_or("an SL entry has no flags")?;
        while let [flags, length, rest @ ..] = components {
            let length = usize::from(*length);
            let content = rest.get(..length).ok_or("an SL component is cut short")?;
            components = &rest[length..];
            let target = self.target.get_or_insert_with(String::new);
            if !self.continuing && !target.is_empty() && !target.ends_with('/') {
                target.push('/');
            }
            match *flags {
                flags if flags & SL_ROOT != 0 => target.push('/'),
                flags if flags & SL_CURRENT != 0 => target.push('.'),
                flags if flags & SL_PARENT != 0 => target.push_str(".."),
                _ => target.push_str(&String::from_utf8_lossy(content)),
            }
            self.continuing = flags & SL_CONTINUE != 0;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A system use entry of `signature` holding `data`.
    pub(crate) fn entry(signature: &[u8; 2], data: &[u8]) -> Vec<u8> {
        let mut bytes = signature.to_vec();
        bytes.push(u8::try_from(4 + data.len()).unwrap());
        bytes.push(1);
        bytes.extend_from_slice(data);
        bytes
    }

    /// The `SP` entry that starts the root's own system use field.
    pub(crate) fn sharing() -> Vec<u8> {
        entry(b"SP", &[0xBE, 0xEF, 0])
    }

    /// An `ER` entry naming Rock Ridge 1.12.
    pub(crate) fn extension() -> Vec<u8> {
        let mut data = vec![9, 0, 0, 1];
        data.extend_from_slice(b"IEEE_1282");
        entry(b"ER", &data)
    }

    pub(crate) fn name(flags: u8, name: &str) -> Vec<u8> {
        let mut data = vec![flags];
        data.extend_from_slice(name.as_bytes());
        entry(b"NM", &data)
    }

    pub(crate) fn continuation(block: u32, offset: u32, length: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for value in [block, offset, length] {
            data.extend_from_slice(&value.to_le_bytes());
            data.extend_from_slice(&value.to_be_bytes());
        }
        entry(b"CE", &data)
    }

    #[test]
    fn detects_rock_ridge_by_its_extension_or_rr_entry() {
        let mut root = sharing();
        root.extend(extension());
        assert_eq!(detect(&[], 2048, &root), Ok(Some(0)));
        let mut old = sharing();
        old.extend(entry(b"RR", &[0x81]));
        assert_eq!(detect(&[], 2048, &old), Ok(Some(0)));
        assert_eq!(detect(&[], 2048, &sharing()), Ok(None));
        assert_eq!(detect(&[], 2048, &extension()), Ok(None));
    }

    #[test]
    fn reads_names_links_and_times_across_continuation_areas() {
        let mut input = vec![0u8; 2 * 2048];
        let mut continued = name(0, "-name.txt");
        let mut times = vec![0x03];
        times.extend_from_slice(&[100, 1, 2, 3, 4, 5, 0]);
        times.extend_from_slice(&[126, 11, 12, 0, 0, 0, 0]);
        continued.extend(entry(b"TF", &times));
        continued.extend(entry(b"ST", &[]));
        continued.extend(name(0, "ignored"));
        input[2048 + 100..2048 + 100 + continued.len()].copy_from_slice(&continued);
        let mut area = name(1, "a-long");
        area.extend(continuation(1, 100, continued.len() as u32));
        area.extend(entry(
            b"SL",
            &[0, 0x08, 0, 0, 3, b'u', b's', b'r', 0x01, 1, b'b'],
        ));
        area.extend(entry(b"SL", &[0, 0, 2, b'i', b'n', 0x04, 0]));
        let read = read(&input, 2048, &area).unwrap();
        assert_eq!(read.name.as_deref(), Some("a-long-name.txt"));
        assert_eq!(read.link.as_deref(), Some("/usr/bin/.."));
        assert_eq!(
            read.created
                .map(crate::java::time::format_instant)
                .as_deref(),
            Some("2000-01-02T03:04:05Z")
        );
        assert!(read.modified.is_some());
    }

    #[test]
    fn refuses_continuation_loops_and_areas_past_a_block() {
        let mut input = vec![0u8; 2 * 2048];
        let looping = continuation(1, 0, 28);
        input[2048..2048 + looping.len()].copy_from_slice(&looping);
        let failed = read(&input, 2048, &looping).unwrap_err();
        assert!(failed.contains("more than 32"), "{failed}");
        let crossing = continuation(1, 2000, 100);
        assert!(read(&input, 2048, &crossing)
            .unwrap_err()
            .contains("crosses"));
        let past = continuation(9, 0, 10);
        assert!(read(&input, 2048, &past).unwrap_err().contains("past"));
        let cut = entry(b"NM", &[]);
        assert!(read(&input, 2048, &cut).is_err());
        assert!(read(&input, 2048, &[b'N', b'M', 40, 1]).is_err());
    }
}
