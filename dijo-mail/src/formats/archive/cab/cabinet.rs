//! The headers of a cabinet ([MS-CAB] 2.1 to 2.3): the `CFHEADER`, with the names of the
//! cabinets before and after it in a set, its `CFFOLDER`s and its `CFFILE`s, read and checked
//! against the cabinet's length before anything is decoded.

use std::fmt;

use crate::compress::dos_millis;
use crate::java;
use crate::text::charset::Charset;

const SIGNATURE: &[u8; 4] = b"MSCF";

/// `cfhdrPREV_CABINET`: the cabinet continues one before it in its set.
const PREV_CABINET: u16 = 0x0001;
/// `cfhdrNEXT_CABINET`: the cabinet is continued by one after it in its set.
const NEXT_CABINET: u16 = 0x0002;
/// `cfhdrRESERVE_PRESENT`: the headers carry reserved areas, whose sizes the header gives.
const RESERVE_PRESENT: u16 = 0x0004;

/// `_A_NAME_IS_UTF`: the file's name is UTF-8, else in the code page of the system it was
/// made on.
const NAME_IS_UTF: u16 = 0x0080;

/// `ifoldCONTINUED_FROM_PREV`: the file starts in the cabinet before.
const CONTINUED_FROM_PREV: u16 = 0xFFFD;
/// `ifoldCONTINUED_TO_NEXT`: the file ends in the cabinet after.
const CONTINUED_TO_NEXT: u16 = 0xFFFE;
/// `ifoldCONTINUED_PREV_AND_NEXT`: the file starts in the cabinet before and ends in the one
/// after.
const CONTINUED_PREV_AND_NEXT: u16 = 0xFFFF;

/// The longest string the headers hold, `CB_MAX_FILENAME` and `CB_MAX_CABINET_NAME`, without its
/// terminating NUL.
const MAX_STRING: usize = 255;

/// The bytes of a `CFHEADER` before its optional fields.
const HEADER: usize = 36;
/// The bytes of a `CFFOLDER` before its reserved area.
const FOLDER: usize = 8;
/// The bytes of a `CFFILE` before its name.
const FILE: usize = 16;

/// The code page a name without `_A_NAME_IS_UTF` is read in. The format names none: the tools
/// that write cabinets store the name as the system's ANSI code page has it, which is
/// windows-1252 on the Western systems most cabinets come from.
const ANSI_NAMES: &str = "windows-1252";

/// Why the headers of a cabinet cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Malformed(pub(super) String);

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a folder's data is compressed, as `typeCompress` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Method {
    Stored,
    Mszip,
    Lzx(lzxd::WindowSize),
    Quantum,
    /// A method the format does not define, of this number, or LZX with a window of a size it
    /// does not allow, `typeCompress` whole.
    Undefined(u16),
}

impl Method {
    fn of(type_compress: u16) -> Self {
        use lzxd::WindowSize;
        match (type_compress & 0x000F, type_compress >> 8 & 0x1F) {
            (0, _) => Method::Stored,
            (1, _) => Method::Mszip,
            (2, _) => Method::Quantum,
            (3, 15) => Method::Lzx(WindowSize::KB32),
            (3, 16) => Method::Lzx(WindowSize::KB64),
            (3, 17) => Method::Lzx(WindowSize::KB128),
            (3, 18) => Method::Lzx(WindowSize::KB256),
            (3, 19) => Method::Lzx(WindowSize::KB512),
            (3, 20) => Method::Lzx(WindowSize::MB1),
            (3, 21) => Method::Lzx(WindowSize::MB2),
            _ => Method::Undefined(type_compress),
        }
    }
}

/// A `CFFOLDER`: where its first `CFDATA` block is and how many blocks of it the cabinet holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Folder {
    pub(super) first_block: usize,
    pub(super) blocks: u16,
    pub(super) method: Method,
}

/// Where a file's data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Place {
    /// In the folder of this index, which the cabinet holds.
    Folder(u16),
    /// In the cabinet's first folder, which continues the last of the cabinet before.
    ContinuedFromPrevious,
    /// In the cabinet's last folder, which the cabinet after continues.
    ContinuedToNext,
    /// In the cabinet's only folder, which continues the cabinet before and is continued by the
    /// one after.
    ContinuedBoth,
}

/// A `CFFILE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct File {
    /// Its name, its path separated by slashes.
    pub(super) name: String,
    pub(super) size: u32,
    /// Where its data starts in its folder's decoded data.
    pub(super) offset: u32,
    pub(super) place: Place,
    /// Its DOS date and time as a wall-clock time in UTC, in milliseconds since the epoch, if
    /// its fields are in range.
    pub(super) modified_millis: Option<i64>,
}

/// A cabinet's headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Cabinet {
    pub(super) folders: Vec<Folder>,
    pub(super) files: Vec<File>,
    /// The bytes of the reserved area of each `CFDATA` block.
    pub(super) block_reserve: usize,
    /// The file name of the cabinet before it in its set, if it continues one.
    pub(super) previous: Option<String>,
    /// The file name of the cabinet after it in its set, if one continues it.
    pub(super) next: Option<String>,
}

impl Cabinet {
    /// Whether the first folder continues the last one of the cabinet before, as a file that
    /// starts there says.
    pub(super) fn continues_previous(&self) -> bool {
        self.files.iter().any(|file| {
            matches!(
                file.place,
                Place::ContinuedFromPrevious | Place::ContinuedBoth
            )
        })
    }
}

/// Little-endian fields read in order from a position in the cabinet.
struct Fields<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    /// Where `length` bytes from the position end, if the cabinet holds them.
    fn fit(&self, length: usize, what: &str) -> Result<usize, Malformed> {
        self.at
            .checked_add(length)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| {
                Malformed(format!(
                    "the cabinet, {} bytes long, is too short for {what} at offset {}",
                    self.data.len(),
                    self.at
                ))
            })
    }

    fn bytes(&mut self, length: usize, what: &str) -> Result<&'a [u8], Malformed> {
        let end = self.fit(length, what)?;
        let bytes = &self.data[self.at..end];
        self.at = end;
        Ok(bytes)
    }

    fn u8(&mut self, what: &str) -> Result<u8, Malformed> {
        Ok(self.bytes(1, what)?[0])
    }

    fn u16(&mut self, what: &str) -> Result<u16, Malformed> {
        let bytes = self.bytes(2, what)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self, what: &str) -> Result<u32, Malformed> {
        let bytes = self.bytes(4, what)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A string ended by a NUL, of at most [`MAX_STRING`] bytes before it.
    fn string(&mut self, what: &str) -> Result<&'a [u8], Malformed> {
        let rest = &self.data[self.at.min(self.data.len())..];
        let window = &rest[..rest.len().min(MAX_STRING + 1)];
        let length = window.iter().position(|&byte| byte == 0).ok_or_else(|| {
            Malformed(format!(
                "{what} at offset {} is not ended by a NUL within {MAX_STRING} bytes",
                self.at
            ))
        })?;
        let string = &rest[..length];
        self.at += length + 1;
        Ok(string)
    }
}

/// A name of the headers that has no `_A_NAME_IS_UTF` attribute, read as [`ANSI_NAMES`].
fn ansi(bytes: &[u8]) -> String {
    Charset::named(ANSI_NAMES)
        .decode(bytes)
        .expect("windows-1252 decodes every byte")
}

/// The file name of the cabinet `which` of the set, before the name of the disk it is on.
fn neighbour(fields: &mut Fields<'_>, which: &str) -> Result<String, Malformed> {
    let cabinet = ansi(fields.string(&format!("the name of the {which} cabinet"))?);
    fields.string(&format!("the disk name of the {which} cabinet"))?;
    Ok(cabinet)
}

/// Reads the headers of the cabinet `data`.
pub(super) fn read(data: &[u8]) -> Result<Cabinet, Malformed> {
    if !data.starts_with(SIGNATURE) {
        return Err(Malformed("the cabinet does not start with MSCF".to_owned()));
    }
    let mut fields = Fields { data, at: 0 };
    let header = fields.bytes(HEADER, "the CFHEADER")?;
    let field16 = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
    let files_at = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
    let (folder_count, file_count, flags) = (field16(26), field16(28), field16(30));
    let (header_reserve, folder_reserve, block_reserve) = if flags & RESERVE_PRESENT != 0 {
        (
            fields.u16("cbCFHeader")?,
            fields.u8("cbCFFolder")?,
            fields.u8("cbCFData")?,
        )
    } else {
        (0, 0, 0)
    };
    fields.bytes(usize::from(header_reserve), "the header's reserved area")?;
    let previous = (flags & PREV_CABINET != 0)
        .then(|| neighbour(&mut fields, "previous"))
        .transpose()?;
    let next = (flags & NEXT_CABINET != 0)
        .then(|| neighbour(&mut fields, "next"))
        .transpose()?;
    let folder_length = FOLDER + usize::from(folder_reserve);
    fields.fit(
        usize::from(folder_count) * folder_length,
        &format!("the {folder_count} CFFOLDERs"),
    )?;
    let mut folders = Vec::with_capacity(usize::from(folder_count));
    for index in 0..folder_count {
        let what = |field: &str| format!("{field} of CFFOLDER {index}");
        let first_block = fields.u32(&what("coffCabStart"))?;
        let blocks = fields.u16(&what("cCFData"))?;
        let method = Method::of(fields.u16(&what("typeCompress"))?);
        fields.bytes(usize::from(folder_reserve), &what("the reserved area"))?;
        folders.push(Folder {
            first_block: first_block as usize,
            blocks,
            method,
        });
    }
    let mut fields = Fields {
        data,
        at: files_at as usize,
    };
    fields.fit(
        usize::from(file_count) * (FILE + 1),
        &format!("the {file_count} CFFILEs"),
    )?;
    let mut files = Vec::with_capacity(usize::from(file_count));
    for index in 0..file_count {
        files.push(file(&mut fields, index, folder_count)?);
    }
    Ok(Cabinet {
        folders,
        files,
        block_reserve: usize::from(block_reserve),
        previous,
        next,
    })
}

/// The `CFFILE` of this `index` at the position of `fields`, in a cabinet of `folder_count`
/// folders.
fn file(fields: &mut Fields<'_>, index: u16, folder_count: u16) -> Result<File, Malformed> {
    let what = |field: &str| format!("{field} of CFFILE {index}");
    let size = fields.u32(&what("cbFile"))?;
    let offset = fields.u32(&what("uoffFolderStart"))?;
    let folder = fields.u16(&what("iFolder"))?;
    let date = fields.u16(&what("date"))?;
    let time = fields.u16(&what("time"))?;
    let attributes = fields.u16(&what("attribs"))?;
    let name = fields.string(&what("szName"))?;
    let name = if attributes & NAME_IS_UTF != 0 {
        java::utf8(name)
    } else {
        ansi(name)
    };
    let place = match folder {
        CONTINUED_FROM_PREV => Place::ContinuedFromPrevious,
        CONTINUED_TO_NEXT => Place::ContinuedToNext,
        CONTINUED_PREV_AND_NEXT => Place::ContinuedBoth,
        folder if folder < folder_count => Place::Folder(folder),
        folder => {
            return Err(Malformed(format!(
                "CFFILE {index} ({name}) is in folder {folder}, of a cabinet of {folder_count} \
                 folders"
            )))
        }
    };
    Ok(File {
        name: name.replace('\\', "/"),
        size,
        offset,
        place,
        modified_millis: dos_millis(i64::from(date) << 16 | i64::from(time)),
    })
}
