//! What the ports of POI's readers share: the Java exception POI throws reading a document,
//! which fails Tika's parse, and little-endian reads that fail where Java's array bounds do.

/// A Java exception POI throws reading a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Thrown(pub(crate) String);

impl Thrown {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Thrown(reason.into())
    }
}

pub(crate) type Read<T> = Result<T, Thrown>;

pub(crate) fn thrown<T>(reason: impl Into<String>) -> Read<T> {
    Err(Thrown(reason.into()))
}

fn slice(data: &[u8], at: usize, length: usize) -> Read<&[u8]> {
    at.checked_add(length)
        .and_then(|end| data.get(at..end))
        .ok_or_else(|| {
            Thrown(format!(
                "Index {at} out of bounds for length {}",
                data.len()
            ))
        })
}

pub(crate) fn u8_at(data: &[u8], at: usize) -> Read<u8> {
    Ok(slice(data, at, 1)?[0])
}

/// `LittleEndian.getShort`.
pub(crate) fn i16_at(data: &[u8], at: usize) -> Read<i16> {
    let bytes = slice(data, at, 2)?;
    Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
}

/// `LittleEndian.getUShort`.
pub(crate) fn u16_at(data: &[u8], at: usize) -> Read<u16> {
    Ok(i16_at(data, at)? as u16)
}

/// `LittleEndian.getInt`.
pub(crate) fn i32_at(data: &[u8], at: usize) -> Read<i32> {
    let bytes = slice(data, at, 4)?;
    Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// `IOUtils.safelyClone(data, offset, length, max)`: the bytes from `offset`, as many as there
/// are up to `length`.
pub(crate) fn safely_clone(data: &[u8], offset: i64, length: i64, max: i64) -> Read<Vec<u8>> {
    if offset < 0 || length < 0 {
        return thrown(format!(
            "Invalid offset/length specified: offset: {offset}, length: {length}"
        ));
    }
    let real = (data.len() as i64 - offset).min(length);
    if real < 0 || real > max {
        return thrown(format!("Tried to allocate an array of length {real}"));
    }
    let start = offset as usize;
    Ok(data[start..start + real as usize].to_vec())
}
