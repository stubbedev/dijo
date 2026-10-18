//! TAR archives read as commons-compress's `TarArchiveInputStream` gives Tika's `PackageParser`
//! their files: a record at a time, GNU long names and links, local and global PAX headers, old
//! GNU sparse files and the GNU (0.0, 0.1, 1.0) and star PAX sparse files reassembled. Names are
//! decoded as UTF-8 with `?` for what is malformed; a name from a GNU long name or a PAX path
//! loses its leading slashes; a directory is an entry of the directory type or a name ending in
//! a slash.
//!
//! The stream's own order of work is kept, quirks included: an entry right after a global PAX
//! header has the global headers applied again over its own, and an old GNU sparse file after a
//! long name or a global header has its extension records read again.
//!
//! Where commons-compress reads the next header from the data of a directory, which its stream
//! never skips, the port skips that data as it skips a file's.

use std::collections::BTreeMap;

use super::tar_header::{normalize, sparse_structs, Header, RECORD};
use super::zip::decode_name;
use super::{throws, ArchiveFile, Decoding, Limited, Undecoded};
use crate::input::{Input, Pages};
use crate::java;

const LF_DIR: u8 = b'5';
const LF_GNU_LONG_LINK: u8 = b'K';
const LF_GNU_LONG_NAME: u8 = b'L';
const LF_OLD_GNU_SPARSE: u8 = b'S';
const LF_PAX_GLOBAL: u8 = b'g';
const LF_PAX_LOCAL: [u8; 2] = *b"xX";
/// The sparse structs of an old GNU extension record, and where its flag that another follows
/// stands.
const SPARSE_STRUCTS_IN_EXTENSION: usize = 21;
const EXTENSION_IS_EXTENDED: usize = 504;

/// `TarGnuSparseKeys` and the other PAX keys the stream reads.
const GNU_SPARSE_MAP: &str = "GNU.sparse.map";
const GNU_SPARSE_NAME: &str = "GNU.sparse.name";
const GNU_SPARSE_OFFSET: &str = "GNU.sparse.offset";
const GNU_SPARSE_NUMBYTES: &str = "GNU.sparse.numbytes";
const GNU_SPARSE_SIZE: &str = "GNU.sparse.size";
const GNU_SPARSE_REALSIZE: &str = "GNU.sparse.realsize";
const SCHILY_FILETYPE: &str = "SCHILY.filetype";
const SCHILY_REALSIZE: &str = "SCHILY.realsize";
const SCHILY_ARCHTYPE: &str = "SCHILY.archtype";
/// The keys whose processing reads the other headers of their map, `fill...Data`, last, as
/// they come after the others in the order Java's `HashMap` of them iterates in.
const FILLED_LAST: [&str; 3] = [SCHILY_FILETYPE, GNU_SPARSE_REALSIZE, GNU_SPARSE_SIZE];

/// Instant.MIN and Instant.MAX, in seconds.
const MIN_SECOND: i64 = -31_557_014_167_219_200;
const MAX_SECOND: i64 = 31_556_889_864_403_199;
const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// The PAX headers of an entry, by key.
type Pax = BTreeMap<String, String>;

/// `TarArchiveStructSparse`: where a stretch of a sparse file's data stands, and how long it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sparse {
    offset: i64,
    numbytes: i64,
}

/// One of the streams a sparse file's data is read from in turn.
#[derive(Debug, Clone, Copy)]
enum Piece {
    /// A hole of this many zeros.
    Zeros(u64),
    /// This many bytes of the archive, read on from where the stream stands.
    Data(u64),
}

/// What reading an entry's data hands on: bytes of the archive, or zeros.
enum Read<'a> {
    Bytes(&'a [u8]),
    Zeros(u64),
}

/// `TarArchiveEntry` as the stream makes it of a header and the PAX headers before it, and
/// what of its data has been read.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    link_flag: u8,
    size: i64,
    real_size: i64,
    modified_millis: i64,
    sparse: Vec<Sparse>,
    extended: bool,
    pax_gnu_sparse: bool,
    pax_gnu_1x_sparse: bool,
    star_sparse: bool,
    /// `entryOffset`: how many bytes of its data have been read.
    offset: u64,
    /// The streams its sparse data is read from, and how far into them it is.
    pieces: Vec<Piece>,
}

impl Entry {
    fn of(header: Header) -> Self {
        Entry {
            name: header.name,
            link_flag: header.link_flag,
            size: header.size,
            real_size: header.real_size,
            modified_millis: header.mtime.saturating_mul(1000),
            sparse: header
                .sparse
                .into_iter()
                .map(|(offset, numbytes)| Sparse { offset, numbytes })
                .collect(),
            extended: header.extended,
            pax_gnu_sparse: false,
            pax_gnu_1x_sparse: false,
            star_sparse: false,
            offset: 0,
            pieces: Vec::new(),
        }
    }

    fn is_pax_header(&self) -> bool {
        LF_PAX_LOCAL.contains(&self.link_flag)
    }

    fn is_directory(&self) -> bool {
        self.link_flag == LF_DIR
            || (!self.is_pax_header()
                && self.link_flag != LF_PAX_GLOBAL
                && self.name.ends_with('/'))
    }

    fn is_sparse(&self) -> bool {
        self.link_flag == LF_OLD_GNU_SPARSE || self.pax_gnu_sparse || self.star_sparse
    }

    /// `getRealSize`: the size of the file a sparse entry's data makes, else its size.
    fn real_size(&self) -> i64 {
        if self.is_sparse() {
            self.real_size
        } else {
            self.size
        }
    }

    /// `setName`: the name without its leading slashes.
    fn set_name(&mut self, name: &str) {
        self.name = normalize(name);
    }

    /// `updateEntryFromPaxHeaders` and `setSparseHeaders`.
    fn apply_pax(&mut self, headers: &Pax, sparse: Vec<Sparse>) -> Result<(), Undecoded> {
        let filled_last = |key: &str| FILLED_LAST.contains(&key);
        for (key, value) in headers.iter().filter(|(key, _)| !filled_last(key)) {
            self.process_pax_header(key, value, headers)?;
        }
        for key in FILLED_LAST {
            if let Some(value) = headers.get(key) {
                self.process_pax_header(key, value, headers)?;
            }
        }
        self.sparse = sparse;
        Ok(())
    }

    /// `processPaxHeader` of `key` and `value`, from `headers`, as far as Tika's parser reads
    /// the entry: its name, size, modification time and sparse layout; the other numbers and
    /// times are checked as they are parsed.
    fn process_pax_header(
        &mut self,
        key: &str,
        value: &str,
        headers: &Pax,
    ) -> Result<(), Undecoded> {
        match key {
            "path" => self.set_name(value),
            "gid" | "uid" => {
                parse_long(value)?;
            }
            "size" => {
                let size = parse_long(value)?;
                if size < 0 {
                    return Err(throws("Corrupted TAR archive. Entry size is negative"));
                }
                self.size = size;
            }
            "mtime" => self.modified_millis = decimal_seconds_millis(value)?,
            "atime" | "ctime" | "LIBARCHIVE.creationtime" => {
                decimal_seconds_millis(value)?;
            }
            "SCHILY.devminor" | "SCHILY.devmajor" => {
                if parse_int(value)? < 0 {
                    return Err(throws(format!("Corrupted TAR archive. {key} is negative")));
                }
            }
            GNU_SPARSE_SIZE => {
                self.pax_gnu_sparse = true;
                self.real_size = i64::from(parse_int(value)?);
                if let Some(name) = headers.get(GNU_SPARSE_NAME) {
                    self.name = name.clone();
                }
            }
            GNU_SPARSE_REALSIZE => {
                self.pax_gnu_sparse = true;
                self.pax_gnu_1x_sparse = true;
                if let Some(name) = headers.get(GNU_SPARSE_NAME) {
                    self.name = name.clone();
                }
                self.real_size = i64::from(parse_int(value)?);
            }
            SCHILY_FILETYPE if value == "sparse" => {
                self.star_sparse = true;
                if let Some(real_size) = headers.get(SCHILY_REALSIZE) {
                    self.real_size = parse_long(real_size)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `getOrderedSparseHeaders`: the stretches of data in order, those at the start without
    /// data left out, which may neither overlap nor pass the real size.
    fn ordered_sparse(&self) -> Result<Vec<Sparse>, Undecoded> {
        let mut ordered: Vec<Sparse> = self
            .sparse
            .iter()
            .copied()
            .filter(|sparse| sparse.offset > 0 || sparse.numbytes > 0)
            .collect();
        ordered.sort_by_key(|sparse| sparse.offset);
        for (index, sparse) in ordered.iter().enumerate() {
            let end = sparse.offset.checked_add(sparse.numbytes);
            if let (Some(end), Some(next)) = (end, ordered.get(index + 1)) {
                if end > next.offset {
                    return Err(throws(format!(
                        "Corrupted TAR archive. Sparse blocks for {} overlap each other.",
                        self.name
                    )));
                }
            }
            if end.is_none_or(|end| end < 0) {
                return Err(throws(format!(
                    "Unreadable TAR archive. Offset and numbytes for sparse block in {} too large.",
                    self.name
                )));
            }
        }
        if let Some(last) = ordered.last() {
            if last.offset + last.numbytes > self.real_size() {
                return Err(throws(
                    "Corrupted TAR archive. Sparse block extends beyond real size of the entry",
                ));
            }
        }
        Ok(ordered)
    }

    /// `buildSparseInputStreams`: a hole before each stretch of data, and the stretch, but
    /// those of no bytes.
    fn build_sparse_streams(&mut self) -> Result<(), Undecoded> {
        let mut pieces = Vec::new();
        let mut offset = 0i64;
        for sparse in self.ordered_sparse()? {
            let zeros = sparse.offset - offset;
            if zeros < 0 {
                return Err(throws("Corrupted struct sparse detected"));
            }
            if zeros > 0 {
                pieces.push(Piece::Zeros(zeros as u64));
            }
            if sparse.numbytes > 0 {
                pieces.push(Piece::Data(sparse.numbytes as u64));
            }
            offset = sparse.offset + sparse.numbytes;
        }
        self.pieces = pieces;
        Ok(())
    }
}

/// `Long.parseLong` of a PAX value, failing as `ParsingUtils.parseLongValue` does.
fn parse_long(value: &str) -> Result<i64, Undecoded> {
    java::parse_long(value)
        .ok_or_else(|| throws(format!("Unable to parse long from string value: {value}")))
}

/// `Integer.parseInt` of a PAX value, failing as `ParsingUtils.parseIntValue` does.
fn parse_int(value: &str) -> Result<i32, Undecoded> {
    java::parse_int(value)
        .ok_or_else(|| throws(format!("Unable to parse int from string value: {value}")))
}

/// `parseInstantFromDecimalSeconds` of a PAX time, `-?\d{1,19}(\.\d{1,19})?`, in milliseconds
/// since the epoch, as `FileTime.toMillis` gives them: the whole seconds as `BigDecimal.longValue`
/// takes them, wrapped to 64 bits, and the fraction's nanoseconds truncated.
fn decimal_seconds_millis(value: &str) -> Result<i64, Undecoded> {
    let invalid = || {
        throws(format!(
            "Corrupted PAX header. Time field value is invalid '{value}'"
        ))
    };
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };
    let digits = |text: &str| {
        (1..=19).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit())
    };
    if !digits(whole) || fraction.is_some_and(|fraction| !digits(fraction)) {
        return Err(invalid());
    }
    let sign = if negative { -1 } else { 1 };
    let whole: i128 = whole.parse().map_err(|_| invalid())?;
    let seconds = (sign * whole) as i64;
    let nanos = fraction.map_or(0, |fraction| {
        let nine: String = fraction
            .chars()
            .chain(std::iter::repeat('0'))
            .take(9)
            .collect();
        sign as i64 * nine.parse::<i64>().expect("nine ASCII digits")
    });
    let seconds = seconds
        .checked_add(nanos.div_euclid(NANOS_PER_SECOND))
        .ok_or_else(invalid)?;
    if !(MIN_SECOND..=MAX_SECOND).contains(&seconds) {
        return Err(invalid());
    }
    let total = i128::from(seconds) * i128::from(NANOS_PER_SECOND)
        + i128::from(nanos.rem_euclid(NANOS_PER_SECOND));
    Ok(
        i64::try_from(total.div_euclid(1_000_000)).unwrap_or(if total < 0 {
            i64::MIN
        } else {
            i64::MAX
        }),
    )
}

/// `TarArchiveInputStream` over the archive's bytes.
struct Stream<'a> {
    data: &'a [u8],
    /// Where the underlying stream stands.
    at: usize,
    eof: bool,
    global: Pax,
    global_sparse: Vec<Sparse>,
    current: Option<Entry>,
}

impl<'a> Stream<'a> {
    fn new(data: &'a [u8]) -> Self {
        Stream {
            data,
            at: 0,
            eof: false,
            global: Pax::new(),
            global_sparse: Vec::new(),
            current: None,
        }
    }

    fn current(&self) -> &Entry {
        self.current.as_ref().expect("the stream is in an entry")
    }

    fn current_mut(&mut self) -> &mut Entry {
        self.current.as_mut().expect("the stream is in an entry")
    }

    /// `getRecord`: the next record, none, the end of the archive, where fewer than a record's
    /// bytes are left or the record is all zeros.
    fn record(&mut self) -> Option<&'a [u8]> {
        let record = self.data.get(self.at..self.at + RECORD);
        self.at = self.data.len().min(self.at + RECORD);
        match record {
            Some(record) if record.iter().any(|&byte| byte != 0) => Some(record),
            _ => {
                self.eof = true;
                None
            }
        }
    }

    /// `count` bytes of the underlying stream, which must hold them.
    fn take(&mut self, count: u64) -> Result<&'a [u8], Undecoded> {
        let bytes = usize::try_from(count)
            .ok()
            .and_then(|count| self.data.get(self.at..self.at.checked_add(count)?))
            .ok_or_else(truncated)?;
        self.at += bytes.len();
        Ok(bytes)
    }

    /// `getNextTarEntry`: on to the next entry, `false` at the end of the archive. The entry
    /// left is read to its end and its last record's padding skipped.
    fn next_entry(&mut self) -> Result<bool, Undecoded> {
        if self.eof {
            return Ok(false);
        }
        if self.current.is_some() {
            self.skip_entry()?;
        }
        let Some(record) = self.record() else {
            self.current = None;
            return Ok(false);
        };
        let archtype = self
            .global
            .get(SCHILY_ARCHTYPE)
            .map(|archtype| archtype == "xustar" || archtype == "exustar");
        let header = Header::parse(record, archtype)
            .map_err(|_| throws("Error detected parsing the header"))?;
        self.current = Some(Entry::of(header));
        if self.current().link_flag == LF_GNU_LONG_LINK && self.long_name_data()?.is_none() {
            return Ok(false);
        }
        if self.current().link_flag == LF_GNU_LONG_NAME {
            let Some(data) = self.long_name_data()? else {
                return Ok(false);
            };
            let name = decode_name(&data);
            let entry = self.current_mut();
            entry.set_name(&name);
            if entry.is_directory() && !name.ends_with('/') {
                entry.set_name(&format!("{name}/"));
            }
        }
        if self.current().link_flag == LF_PAX_GLOBAL {
            self.read_global_pax_headers()?;
        }
        if self.current().is_pax_header() {
            self.pax_headers()?;
        } else if !self.global.is_empty() {
            let (global, sparse) = (self.global.clone(), self.global_sparse.clone());
            self.current_mut().apply_pax(&global, sparse)?;
        }
        if self.current().link_flag == LF_OLD_GNU_SPARSE {
            self.read_old_gnu_sparse()?;
        }
        Ok(true)
    }

    /// The rest of the current entry's data read, and the padding of its last record skipped;
    /// a directory's data skipped whole, where commons-compress reads none of it.
    fn skip_entry(&mut self) -> Result<(), Undecoded> {
        let entry = self.current();
        let size = u64::try_from(entry.size).map_err(|_| truncated())?;
        if entry.is_directory() {
            self.take(size.next_multiple_of(RECORD as u64))?;
            return Ok(());
        }
        self.read_data(|_| Ok(()))?;
        let padding = (RECORD as u64 - size % RECORD as u64) % RECORD as u64;
        self.take(padding)?;
        Ok(())
    }

    /// Reads the rest of the current entry's data, handing each piece to `each`: a sparse
    /// file's holes and stretches of data in turn, else its bytes, up to its real size; an
    /// archive that ends first fails as the stream's reads do.
    fn read_data(
        &mut self,
        mut each: impl FnMut(Read<'a>) -> Result<(), Undecoded>,
    ) -> Result<(), Undecoded> {
        let entry = self.current();
        let real_size = u64::try_from(entry.real_size()).map_err(|_| truncated())?;
        let mut available = real_size.saturating_sub(entry.offset);
        if entry.is_directory() || available == 0 {
            return Ok(());
        }
        let pieces = if entry.is_sparse() && !entry.pieces.is_empty() {
            std::mem::take(&mut self.current_mut().pieces)
        } else {
            vec![Piece::Data(available)]
        };
        let mut read = 0;
        for piece in pieces {
            if available == 0 {
                break;
            }
            match piece {
                Piece::Zeros(zeros) => {
                    let zeros = zeros.min(available);
                    each(Read::Zeros(zeros))?;
                    available -= zeros;
                    read += zeros;
                }
                Piece::Data(length) => {
                    let bytes = self.take(length.min(available))?;
                    each(Read::Bytes(bytes))?;
                    available -= bytes.len() as u64;
                    read += bytes.len() as u64;
                }
            }
        }
        self.current_mut().offset += read;
        if available > 0 {
            return Err(truncated());
        }
        Ok(())
    }

    /// The current entry's data, which `getLongNameData` and the PAX parser read whole.
    fn entry_data(&mut self) -> Result<Vec<u8>, Undecoded> {
        let mut data = Vec::new();
        self.read_data(|read| {
            match read {
                Read::Bytes(bytes) => data.extend_from_slice(bytes),
                Read::Zeros(zeros) => data.resize(data.len() + zeros as usize, 0),
            }
            Ok(())
        })?;
        Ok(data)
    }

    /// `getLongNameData`: the current entry's data without its trailing NULs, once the stream
    /// is on the entry it names; none where the archive ends first.
    fn long_name_data(&mut self) -> Result<Option<Vec<u8>>, Undecoded> {
        let mut data = self.entry_data()?;
        if !self.next_entry()? {
            return Ok(None);
        }
        while data.last() == Some(&0) {
            data.pop();
        }
        Ok(Some(data))
    }

    /// `readGlobalPaxHeaders`: the global headers, read over those before, then the entry they
    /// precede.
    fn read_global_pax_headers(&mut self) -> Result<(), Undecoded> {
        let size = self.current().size;
        let data = self.entry_data()?;
        let mut sparse = std::mem::take(&mut self.global_sparse);
        self.global = parse_pax_headers(&data, &mut sparse, &self.global, size)?;
        self.global_sparse = sparse;
        if !self.next_entry()? {
            return Err(throws("Error detected parsing the pax header"));
        }
        Ok(())
    }

    /// `paxHeaders`: the local headers over the global ones, applied to the entry they
    /// precede, with its sparse layout from a 0.0 or 0.1 map, or a 1.0 one before its data.
    fn pax_headers(&mut self) -> Result<(), Undecoded> {
        let size = self.current().size;
        let data = self.entry_data()?;
        let mut sparse = Vec::new();
        let headers = parse_pax_headers(&data, &mut sparse, &self.global, size)?;
        if let Some(map) = headers.get(GNU_SPARSE_MAP) {
            sparse = parse_pax01_sparse_headers(map)?;
        }
        if !self.next_entry()? {
            return Err(throws(
                "premature end of tar archive. Didn't find any entry after PAX header.",
            ));
        }
        self.current_mut().apply_pax(&headers, sparse)?;
        if self.current().pax_gnu_1x_sparse {
            let sparse = self.parse_pax1x_sparse_headers()?;
            self.current_mut().sparse = sparse;
        }
        self.current_mut().build_sparse_streams()
    }

    /// `readOldGNUSparse`: the sparse structs of the extension records that follow an old GNU
    /// sparse file's header, while each says another follows.
    fn read_old_gnu_sparse(&mut self) -> Result<(), Undecoded> {
        if self.current().extended {
            loop {
                let record = self.record().ok_or_else(|| {
                    throws(
                        "premature end of tar archive. Didn't find extended_header after header \
                         with extended flag.",
                    )
                })?;
                let structs = sparse_structs(record, SPARSE_STRUCTS_IN_EXTENSION)
                    .map_err(|_| throws("Corrupted TAR archive, sparse entry is invalid"))?;
                self.current_mut().sparse.extend(
                    structs
                        .into_iter()
                        .map(|(offset, numbytes)| Sparse { offset, numbytes }),
                );
                if record[EXTENSION_IS_EXTENDED] != 1 {
                    break;
                }
            }
        }
        self.current_mut().build_sparse_streams()
    }

    /// `TarUtils.parsePAX1XSparseHeaders`: the 1.0 sparse map before a file's data, decimal
    /// numbers on lines of their own, the count then an offset and a length for each, read from
    /// the underlying stream, which then skips to the end of the record, or past a whole one
    /// where the map fills its records.
    fn parse_pax1x_sparse_headers(&mut self) -> Result<Vec<Sparse>, Undecoded> {
        let mut read = 0u64;
        let count = self.number_line(&mut read)?;
        if count < 0 {
            return Err(throws(
                "Corrupted TAR archive. Negative value in sparse headers block",
            ));
        }
        let mut sparse = Vec::new();
        for _ in 0..count {
            let offset = self.number_line(&mut read)?;
            if offset < 0 {
                return Err(throws(
                    "Corrupted TAR archive. Sparse header block offset contains negative value",
                ));
            }
            let numbytes = self.number_line(&mut read)?;
            if numbytes < 0 {
                return Err(throws(
                    "Corrupted TAR archive. Sparse header block numbytes contains negative value",
                ));
            }
            sparse.push(Sparse { offset, numbytes });
        }
        let skip = RECORD as u64 - read % RECORD as u64;
        self.at = self.data.len().min(self.at.saturating_add(skip as usize));
        Ok(sparse)
    }

    /// `readLineOfNumberForPax1x`: a decimal number ended by a line feed, the bytes it takes
    /// added to `read`; Java's `long` arithmetic wraps.
    fn number_line(&mut self, read: &mut u64) -> Result<i64, Undecoded> {
        let mut number = 0i64;
        loop {
            let byte = *self.data.get(self.at).ok_or_else(|| {
                throws("Unexpected EOF when reading parse information of 1.X PAX format")
            })?;
            self.at += 1;
            *read += 1;
            if byte == b'\n' {
                return Ok(number);
            }
            if !byte.is_ascii_digit() {
                return Err(throws(
                    "Corrupted TAR archive. Non-numeric value in sparse headers block",
                ));
            }
            number = number.wrapping_mul(10).wrapping_add(i64::from(byte - b'0'));
        }
    }
}

/// `TarUtils.parsePaxHeaders` of a PAX entry's `data`, `length keyword=value\n` records, over a
/// copy of `global`: a value of no more than its line feed removes the keyword; the offsets and
/// lengths of a 0.0 sparse map are added to `sparse` as they come.
fn parse_pax_headers(
    data: &[u8],
    sparse: &mut Vec<Sparse>,
    global: &Pax,
    header_size: i64,
) -> Result<Pax, Undecoded> {
    let mut headers = global.clone();
    let mut offset: Option<i64> = None;
    let mut total_read: i32 = 0;
    let mut bytes = data.iter().copied();
    'records: loop {
        let mut length: i32 = 0;
        let mut read: i32 = 0;
        loop {
            let Some(byte) = bytes.next() else {
                break 'records;
            };
            read = read.wrapping_add(1);
            total_read = total_read.wrapping_add(1);
            if byte == b'\n' {
                break;
            }
            if byte == b' ' {
                let mut keyword = Vec::new();
                loop {
                    let Some(byte) = bytes.next() else {
                        break 'records;
                    };
                    read = read.wrapping_add(1);
                    total_read = total_read.wrapping_add(1);
                    if total_read < 0 || (header_size >= 0 && i64::from(total_read) >= header_size)
                    {
                        break;
                    }
                    if byte == b'=' {
                        let keyword = String::from_utf8_lossy(&keyword).into_owned();
                        let rest_length = length.wrapping_sub(read);
                        if rest_length <= 1 {
                            headers.remove(&keyword);
                        } else if header_size >= 0
                            && i64::from(rest_length) > header_size - i64::from(total_read)
                        {
                            return Err(throws(format!(
                                "Paxheader value size {rest_length} exceeds size of header record"
                            )));
                        } else {
                            let rest: Vec<u8> = bytes.by_ref().take(rest_length as usize).collect();
                            if rest.len() != rest_length as usize {
                                return Err(throws(format!(
                                    "Failed to read Paxheader. Expected {rest_length} bytes, read {}",
                                    rest.len()
                                )));
                            }
                            total_read = total_read.wrapping_add(rest_length);
                            if rest.last() != Some(&b'\n') {
                                return Err(throws(
                                    "Failed to read Paxheader.Value should end with a newline",
                                ));
                            }
                            let value =
                                String::from_utf8_lossy(&rest[..rest.len() - 1]).into_owned();
                            sparse_0_0(&keyword, &value, &mut offset, sparse)?;
                            headers.insert(keyword, value);
                        }
                        break;
                    }
                    keyword.push(byte);
                }
                break;
            }
            if !byte.is_ascii_digit() {
                return Err(throws(
                    "Failed to read Paxheader. Encountered a non-number while reading length",
                ));
            }
            length = length.wrapping_mul(10).wrapping_add(i32::from(byte - b'0'));
        }
    }
    if let Some(offset) = offset {
        sparse.push(Sparse {
            offset,
            numbytes: 0,
        });
    }
    Ok(headers)
}

/// The 0.0 sparse map's `GNU.sparse.offset` and `GNU.sparse.numbytes`, which pair up in
/// `sparse` as they come: an offset without its length takes none.
fn sparse_0_0(
    keyword: &str,
    value: &str,
    offset: &mut Option<i64>,
    sparse: &mut Vec<Sparse>,
) -> Result<(), Undecoded> {
    if keyword == GNU_SPARSE_OFFSET {
        if let Some(previous) = offset.take() {
            sparse.push(Sparse {
                offset: previous,
                numbytes: 0,
            });
        }
        let parsed = java::parse_long(value).ok_or_else(|| {
            throws(format!(
                "Failed to read Paxheader.{GNU_SPARSE_OFFSET} contains a non-numeric value"
            ))
        })?;
        if parsed < 0 {
            return Err(throws(format!(
                "Failed to read Paxheader.{GNU_SPARSE_OFFSET} contains negative value"
            )));
        }
        *offset = Some(parsed);
    }
    if keyword == GNU_SPARSE_NUMBYTES {
        let Some(start) = offset.take() else {
            return Err(throws(format!(
                "Failed to read Paxheader.{GNU_SPARSE_OFFSET} is expected before \
                 GNU.sparse.numbytes shows up."
            )));
        };
        let numbytes = parse_long(value)?;
        if numbytes < 0 {
            return Err(throws(format!(
                "Failed to read Paxheader.{GNU_SPARSE_NUMBYTES} contains negative value"
            )));
        }
        sparse.push(Sparse {
            offset: start,
            numbytes,
        });
    }
    Ok(())
}

/// `TarUtils.parseFromPAX01SparseHeaders`: the 0.1 sparse map, offsets and lengths separated by
/// commas, split as Java's `String.split` splits it, its trailing empty fields dropped.
fn parse_pax01_sparse_headers(map: &str) -> Result<Vec<Sparse>, Undecoded> {
    let mut fields: Vec<&str> = map.split(',').collect();
    if !map.is_empty() {
        while fields.last() == Some(&"") {
            fields.pop();
        }
    }
    if fields.len() % 2 == 1 {
        return Err(throws(
            "Corrupted TAR archive. Bad format in GNU.sparse.map PAX Header",
        ));
    }
    fields
        .chunks(2)
        .map(|pair| {
            let offset = parse_long(pair[0])?;
            if offset < 0 {
                return Err(throws(
                    "Corrupted TAR archive. Sparse struct offset contains negative value",
                ));
            }
            let numbytes = parse_long(pair[1])?;
            if numbytes < 0 {
                return Err(throws(
                    "Corrupted TAR archive. Sparse struct numbytes contains negative value",
                ));
            }
            Ok(Sparse { offset, numbytes })
        })
        .collect()
}

fn truncated() -> Undecoded {
    throws("Truncated TAR archive")
}

/// The files of the TAR archive `data`, whose pages are `pages`, or why they are not read: a
/// file's data in place, a sparse file's reassembled to a spool, up to the
/// [`decode_limit`](super::decode_limit) of the archive, at which the files end.
pub fn files<'a>(
    data: &'a [u8],
    pages: Pages<'a>,
) -> Result<Limited<Vec<ArchiveFile<'a>>>, Undecoded> {
    let mut stream = Stream::new(data);
    let mut files = Vec::new();
    while stream.next_entry()? {
        let entry = stream.current().clone();
        let directory = entry.is_directory();
        let content = if directory {
            Limited::Whole(Input::Borrowed(&[]))
        } else if entry.is_sparse() {
            let mut decoding = Decoding::spooled(data.len(), u64::try_from(entry.real_size()).ok());
            let zeros = [0u8; 8192];
            stream.read_data(|read| match read {
                Read::Bytes(bytes) => decoding.write(bytes),
                Read::Zeros(mut count) => {
                    while count > 0 && !decoding.is_cut() {
                        let chunk = count.min(zeros.len() as u64) as usize;
                        decoding.write(&zeros[..chunk])?;
                        count -= chunk as u64;
                    }
                    Ok(())
                }
            })?;
            decoding.finish()?
        } else {
            let mut stored = &[][..];
            stream.read_data(|read| {
                if let Read::Bytes(bytes) = read {
                    stored = bytes;
                }
                Ok(())
            })?;
            Limited::Whole(Input::Within(stored, pages))
        };
        let file = content.map(|content| ArchiveFile {
            name: entry.name,
            size: entry.size,
            modified_millis: Some(entry.modified_millis),
            created_millis: None,
            directory,
            content,
        });
        if let Some(cut) = file.add_to(&mut files) {
            return Ok(cut);
        }
    }
    Ok(Limited::Whole(files))
}

#[cfg(test)]
mod tests {
    use super::super::tar_header::tests::{header, pax, tar};
    use super::*;

    fn names_and_contents(data: &[u8]) -> Vec<(String, bool, Vec<u8>)> {
        files(data, Pages::IN_MEMORY)
            .and_then(Limited::whole)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.directory, file.content.to_vec()))
            .collect()
    }

    #[test]
    fn reads_long_names_and_pax_paths() {
        let long = "d/".repeat(60) + "long.txt";
        let data = tar(&[
            (b"dir/", b'5', b""),
            (b"././@PaxHeader", b'x', &pax(&[("path", "/abs/pax.txt")])),
            (b"short", b'0', b"pax data"),
            (b"././@LongLink", b'L', format!("{long}\0").as_bytes()),
            (b"cut", b'0', b"long data"),
            (b"global", b'g', &pax(&[("comment", "a commit")])),
            (b"/kept", b'0', b"absolute"),
        ]);
        assert_eq!(
            names_and_contents(&data),
            [
                ("dir/".to_owned(), true, Vec::new()),
                ("abs/pax.txt".to_owned(), false, b"pax data".to_vec()),
                (long, false, b"long data".to_vec()),
                ("/kept".to_owned(), false, b"absolute".to_vec()),
            ]
        );
    }

    /// commons-compress does not skip a directory's data, and reads the next header from it.
    #[test]
    fn skips_a_directory_s_data_where_commons_compress_reads_on_in_it() {
        let mut data = header(b"dir/", b'5', 512);
        data.extend_from_slice(&header(b"inside.txt", b'0', 0));
        data.extend_from_slice(&tar(&[(b"after.txt", b'0', b"after")]));
        let names: Vec<String> = files(&data, Pages::IN_MEMORY)
            .and_then(Limited::whole)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(names, ["dir/", "after.txt"]);
    }

    #[test]
    fn reads_files_in_place_and_fails_on_an_archive_cut_short() {
        let data = tar(&[(b"a.txt", b'0', b"content"), (b"b.txt", b'0', b"more")]);
        assert!(files(&data[..512 + 3], Pages::IN_MEMORY).is_err());
        let read = files(&data, Pages::IN_MEMORY)
            .and_then(Limited::whole)
            .unwrap();
        let Input::Within(stored, _) = read[1].content else {
            panic!("a stored file is not read in place");
        };
        assert_eq!(stored.as_ptr(), data[1536..].as_ptr());
    }

    /// The name and modification time of each file of `data`.
    fn names_and_times(data: &[u8]) -> Vec<(String, Option<i64>)> {
        files(data, Pages::IN_MEMORY)
            .and_then(Limited::whole)
            .unwrap()
            .into_iter()
            .map(|file| (file.name, file.modified_millis))
            .collect()
    }

    #[test]
    fn applies_global_pax_headers_to_every_entry_after_them() {
        let data = tar(&[
            (
                b"pax_global_header",
                b'g',
                &pax(&[("mtime", "1700000123.25"), ("comment", "a commit")]),
            ),
            (b"a.txt", b'0', b"a"),
            (b"././@PaxHeader", b'x', &pax(&[("path", "local.txt")])),
            (b"b.txt", b'0', b"b"),
            (
                b"pax_global_header",
                b'g',
                &pax(&[("mtime", "-1.5"), ("path", "global.txt")]),
            ),
            (b"c.txt", b'0', b"c"),
            (b"d.txt", b'0', b"d"),
        ]);
        assert_eq!(
            names_and_times(&data),
            [
                ("a.txt".to_owned(), Some(1_700_000_123_250)),
                ("local.txt".to_owned(), Some(1_700_000_123_250)),
                ("global.txt".to_owned(), Some(-1_500)),
                ("global.txt".to_owned(), Some(-1_500)),
            ]
        );
    }

    /// commons-compress applies the global headers again to the entry right after them, over
    /// that entry's own PAX headers.
    #[test]
    fn applies_global_pax_headers_again_over_the_next_entry_s_own() {
        let data = tar(&[
            (b"pax_global_header", b'g', &pax(&[("path", "global.txt")])),
            (b"././@PaxHeader", b'x', &pax(&[("path", "local.txt")])),
            (b"a.txt", b'0', b"a"),
            (b"././@PaxHeader", b'x', &pax(&[("path", "later.txt")])),
            (b"b.txt", b'0', b"b"),
        ]);
        let names: Vec<String> = names_and_times(&data)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, ["global.txt", "later.txt"]);
    }

    /// The content of the sparse file every sparse test archive holds: `ab`, four zeros, `cd`
    /// and two zeros.
    const SPARSE: &[u8] = b"ab\0\0\0\0cd\0\0";

    #[test]
    fn reassembles_pax_sparse_files() {
        let map_1_0 = {
            let mut map = b"3\n0\n2\n6\n2\n10\n0\n".to_vec();
            map.resize(512, 0);
            map.extend_from_slice(b"abcd");
            map
        };
        let data = tar(&[
            (
                b"././@PaxHeader",
                b'x',
                &pax(&[
                    ("path", "sparse-0.0.txt"),
                    ("GNU.sparse.size", "10"),
                    ("GNU.sparse.numblocks", "3"),
                    ("GNU.sparse.offset", "0"),
                    ("GNU.sparse.numbytes", "2"),
                    ("GNU.sparse.offset", "6"),
                    ("GNU.sparse.numbytes", "2"),
                    ("GNU.sparse.offset", "10"),
                    ("GNU.sparse.numbytes", "0"),
                ]),
            ),
            (b"./GNUSparseFile.1/sparse-0.0.txt", b'0', b"abcd"),
            (
                b"././@PaxHeader",
                b'x',
                &pax(&[
                    ("GNU.sparse.size", "10"),
                    ("GNU.sparse.map", "0,2,6,2,10,0"),
                    ("GNU.sparse.name", "sparse-0.1.txt"),
                ]),
            ),
            (b"./GNUSparseFile.1/sparse-0.1.txt", b'0', b"abcd"),
            (
                b"././@PaxHeader",
                b'x',
                &pax(&[
                    ("path", "./GNUSparseFile.1/sparse-1.0.txt"),
                    ("GNU.sparse.major", "1"),
                    ("GNU.sparse.minor", "0"),
                    ("GNU.sparse.name", "sparse-1.0.txt"),
                    ("GNU.sparse.realsize", "10"),
                ]),
            ),
            (b"./GNUSparseFile.1/sparse-1.0.txt", b'0', &map_1_0),
            (b"after.txt", b'0', b"after"),
        ]);
        let read = names_and_contents(&data);
        let sparse: Vec<(&str, &[u8])> = read
            .iter()
            .map(|(name, _, content)| (name.as_str(), content.as_slice()))
            .collect();
        assert_eq!(
            sparse,
            [
                ("sparse-0.0.txt", SPARSE),
                ("sparse-0.1.txt", SPARSE),
                ("sparse-1.0.txt", SPARSE),
                ("after.txt", &b"after"[..]),
            ]
        );
    }

    /// An old GNU header of `name` whose sparse structs are `structs` and real size 10, and
    /// whose flag says whether an extension record follows.
    fn old_gnu_sparse(name: &[u8], size: usize, structs: &[(u64, u64)], extended: bool) -> Vec<u8> {
        let mut record = header(name, b'S', size);
        record[257..265].copy_from_slice(b"ustar  \0");
        for (index, (offset, numbytes)) in structs.iter().enumerate() {
            let at = 386 + index * 24;
            record[at..at + 12].copy_from_slice(format!("{offset:011o}\0").as_bytes());
            record[at + 12..at + 24].copy_from_slice(format!("{numbytes:011o}\0").as_bytes());
        }
        record[482] = u8::from(extended);
        record[483..495].copy_from_slice(format!("{:011o}\0", 10).as_bytes());
        record
    }

    #[test]
    fn reassembles_old_gnu_sparse_files_with_their_extension_records() {
        let mut data = old_gnu_sparse(b"old.txt", 4, &[(0, 2), (6, 2), (10, 0)], false);
        let mut stored = b"abcd".to_vec();
        stored.resize(512, 0);
        data.extend_from_slice(&stored);
        data.extend(old_gnu_sparse(
            b"extended.txt",
            5,
            &[(0, 1), (2, 1), (4, 1), (6, 1)],
            true,
        ));
        let mut extension = vec![0u8; 512];
        for (index, (offset, numbytes)) in [(8u64, 1u64), (10, 0)].iter().enumerate() {
            let at = index * 24;
            extension[at..at + 12].copy_from_slice(format!("{offset:011o}\0").as_bytes());
            extension[at + 12..at + 24].copy_from_slice(format!("{numbytes:011o}\0").as_bytes());
        }
        data.extend_from_slice(&extension);
        let mut stored = b"abcde".to_vec();
        stored.resize(512, 0);
        data.extend_from_slice(&stored);
        data.resize(data.len() + 1024, 0);
        assert_eq!(
            names_and_contents(&data),
            [
                ("old.txt".to_owned(), false, SPARSE.to_vec()),
                (
                    "extended.txt".to_owned(),
                    false,
                    b"a\0b\0c\0d\0e\0".to_vec()
                ),
            ]
        );
    }

    #[test]
    fn fails_where_commons_compress_fails() {
        let overlapping = tar(&[
            (
                b"././@PaxHeader",
                b'x',
                &pax(&[("GNU.sparse.size", "10"), ("GNU.sparse.map", "0,4,2,2")]),
            ),
            (b"sparse", b'0', b"abcdef"),
        ]);
        assert!(files(&overlapping, Pages::IN_MEMORY).is_err());
        let bad_time = tar(&[
            (b"pax_global_header", b'g', &pax(&[("mtime", "1e9")])),
            (b"a.txt", b'0', b"a"),
        ]);
        assert!(files(&bad_time, Pages::IN_MEMORY).is_err());
        let alone = tar(&[(b"././@PaxHeader", b'x', &pax(&[("path", "x")]))]);
        assert!(files(&alone, Pages::IN_MEMORY).is_err());
    }
}
