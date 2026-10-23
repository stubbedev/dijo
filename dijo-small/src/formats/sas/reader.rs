//! parso's `SasFileParser`: the header of a SAS7BDAT file read field by field from its stream,
//! then its pages one at a time, their subheaders read into the file's properties and columns
//! until the first page with data, and the rows read on demand.
//!
//! Its failures are parso's: an `IOException` while the metadata is read ends the metadata,
//! which parso logs, a failure while a subheader is read skips the subheader, and anything else
//! fails the file.

use super::decompress::{copy_of_range, head_of_range, Compression};
use super::values::{self, Style, Value};
use crate::formats::thrown::{Exception, Thrown};
use crate::input::{Pages, Paging};
use crate::text::charset::Charset;

const ALIGN_1_OFFSET: i64 = 32;
const ALIGN_2_OFFSET: i64 = 35;
const ALIGN_CHECKER_VALUE: u8 = 51;
const ALIGN_VALUE: i64 = 4;
const ENDIANNESS_OFFSET: i64 = 37;
pub(super) const ENCODING_OFFSET: usize = 70;
const DATASET_OFFSET: i64 = 92;
const DATASET_LENGTH: usize = 64;
const FILE_TYPE_OFFSET: i64 = 156;
const DATE_CREATED_OFFSET: i64 = 164;
const DATE_MODIFIED_OFFSET: i64 = 172;
const HEADER_SIZE_OFFSET: i64 = 196;
const PAGE_SIZE_OFFSET: i64 = 200;
const PAGE_COUNT_OFFSET: i64 = 204;
const SAS_RELEASE_OFFSET: i64 = 216;
const SAS_SERVER_TYPE_OFFSET: i64 = 224;
const OS_VERSION_NUMBER_OFFSET: i64 = 240;
const OS_MAKER_OFFSET: i64 = 256;
const OS_NAME_OFFSET: i64 = 272;
const LITTLE_ENDIAN: i32 = 1;
const BIG_ENDIAN: i32 = 0;
/// `MAX_PAGE_LENGTH`.
const MAX_PAGE_LENGTH: i32 = 10_000_000;

const PAGE_BIT_OFFSET_X86: i64 = 16;
const PAGE_BIT_OFFSET_X64: i64 = 32;
const SUBHEADER_POINTER_LENGTH_X86: i64 = 12;
const SUBHEADER_POINTER_LENGTH_X64: i64 = 24;
const SUBHEADER_POINTERS_OFFSET: i64 = 8;
const PAGE_DELETED_POINTER_OFFSET_X64: i64 = 24;
const PAGE_DELETED_POINTER_OFFSET_X86: i64 = 12;

const PAGE_META_TYPE_1: i32 = 0;
const PAGE_META_TYPE_2: i32 = 16384;
const PAGE_CMETA_TYPE: i32 = 128;
const PAGE_DATA_TYPE: i32 = 256;
const PAGE_DATA_TYPE_2: i32 = 384;
const PAGE_MIX_TYPE_1: i32 = 512;
const PAGE_MIX_TYPE_2: i32 = 640;
const PAGE_AMD_TYPE: i32 = 1024;

const TRUNCATED_SUBHEADER_ID: u8 = 1;
const COMPRESSED_SUBHEADER_ID: u8 = 4;
const COMPRESSED_SUBHEADER_TYPE: u8 = 1;

/// `EMPTY_INPUT_STREAM`.
const EMPTY_INPUT_STREAM: &str = "There are no available bytes in the input stream.";
/// `FILE_NOT_VALID`.
const FILE_NOT_VALID: &str = "Can not read metadata from sas7bdat file.";

fn is_meta(page_type: i32) -> bool {
    matches!(
        page_type,
        PAGE_META_TYPE_1 | PAGE_META_TYPE_2 | PAGE_CMETA_TYPE
    )
}

fn is_mix(page_type: i32) -> bool {
    matches!(page_type, PAGE_MIX_TYPE_1 | PAGE_MIX_TYPE_2)
}

fn is_data(page_type: i32) -> bool {
    matches!(page_type, PAGE_DATA_TYPE | PAGE_DATA_TYPE_2)
}

/// `SAS_CHARACTER_ENCODINGS`: the charset the encoding byte of a file names, if it names one.
pub(super) fn encoding_name(byte: u8) -> Option<&'static str> {
    Some(match byte {
        0x46 => "x-MacArabic",
        0xF5 => "x-MacCroatian",
        0xF6 => "x-MacCyrillic",
        0x48 => "x-MacGreek",
        0x47 => "x-MacHebrew",
        0xA3 => "x-MacIceland",
        0x22 => "ISO-8859-6",
        0x45 => "x-MacRoman",
        0xF7 => "x-MacRomania",
        0x49 => "x-MacThai",
        0x4B => "x-MacTurkish",
        0x4C => "x-MacUkraine",
        0x7B => "Big5",
        0x21 => "ISO-8859-5",
        0x4E | 0xAD => "IBM037",
        0x5F | 0x6C => "x-IBM1025",
        0xCF | 0xD0 => "x-IBM1097",
        0x62 | 0x6F => "x-IBM1112",
        0x63 | 0x70 => "x-IBM1122",
        0xB7 | 0xC0 => "IBM01140",
        0xB8 | 0xC1 => "IBM01141",
        0xB9 | 0xC2 => "IBM01142",
        0xBA | 0xC3 => "IBM01143",
        0xBB | 0xC4 => "IBM01144",
        0xBC | 0xC5 => "IBM01145",
        0xBD | 0xC6 => "IBM01146",
        0xBE | 0xC7 => "IBM01147",
        0xBF | 0xC8 => "IBM01148",
        0xD3 | 0xD4 => "IBM01149",
        0x57 | 0x66 => "IBM424",
        0x58 => "IBM500",
        0x59 | 0x67 => "IBM-Thai",
        0x5A | 0x68 => "IBM870",
        0x5B | 0x69 => "x-IBM875",
        0x7D => "GBK",
        0x86 => "EUC-JP",
        0x8C => "EUC-KR",
        0x77 => "x-EUC-TW",
        0xCD => "GB18030",
        0x23 => "ISO-8859-7",
        0x24 => "ISO-8859-8",
        0x80 => "x-IBM1381",
        0x82 | 0xEA => "x-IBM930",
        0x8B | 0xEB => "x-IBM933",
        0x7C | 0xEC => "x-IBM935",
        0x75 | 0xED => "x-IBM937",
        0x81 | 0xEE => "x-IBM939",
        0x89 => "x-IBM942",
        0x8E => "x-IBM949",
        0xAC => "x-ISO2022-CN-CNS",
        0xA9 => "x-ISO2022-CN-GB",
        0xA7 => "ISO-2022-JP",
        0xA8 => "ISO-2022-KR",
        0x1D => "ISO-8859-1",
        0x1E => "ISO-8859-2",
        0x1F => "ISO-8859-3",
        0x20 => "ISO-8859-4",
        0x25 => "ISO-8859-9",
        0xF2 => "ISO-8859-13",
        0x28 => "ISO-8859-15",
        0x88 => "x-windows-iso2022jp",
        0x7E => "x-mswin-936",
        0x8D => "x-windows-949",
        0x76 => "x-windows-950",
        0x6D => "IBM1026",
        0x6E => "IBM1047",
        0x2B => "IBM437",
        0x2C => "IBM850",
        0x2D => "IBM852",
        0x3A => "IBM857",
        0x2E => "IBM00858",
        0x2F => "IBM862",
        0x33 => "IBM866",
        0x8A => "Shift_JIS",
        0xF8 => "JIS_X0201",
        0x27 => "x-iso-8859-11",
        0x1C => "US-ASCII",
        0x14 => "UTF-8",
        0x42 => "windows-1256",
        0x43 => "windows-1257",
        0x3D => "windows-1251",
        0x3F => "windows-1253",
        0x41 => "windows-1255",
        0x3E => "windows-1252",
        0x3C => "windows-1250",
        0x40 => "windows-1254",
        0x44 => "windows-1258",
        _ => return None,
    })
}

/// A charset this port decodes, as `new String(bytes, charset)` decodes.
#[derive(Debug, Clone, Copy)]
pub(super) struct Decoder(Charset);

impl Decoder {
    /// The decoder of the charset Java names `name`, if this port decodes it.
    pub(super) fn named(name: &str) -> Option<Self> {
        Charset::for_name(name)
            .filter(|charset| charset.decode(b"").is_some())
            .map(Self)
    }

    fn decode(self, bytes: &[u8]) -> String {
        self.0
            .decode(bytes)
            .expect("a charset that decodes one input decodes any")
    }
}

fn io(message: impl Into<String>) -> Thrown {
    Thrown::Io(Exception::new("java.io.IOException", message))
}

fn runtime(class: &'static str, message: impl Into<String>) -> Thrown {
    Thrown::Runtime(Exception::new(class, message))
}

fn underflow() -> Thrown {
    Thrown::Runtime(Exception::bare("java.nio.BufferUnderflowException"))
}

fn index_out_of_bounds(index: impl std::fmt::Display, length: usize) -> Thrown {
    runtime(
        "java.lang.IndexOutOfBoundsException",
        format!("Index {index} out of bounds for length {length}"),
    )
}

fn negative_size(size: impl std::fmt::Display) -> Thrown {
    runtime("java.lang.NegativeArraySizeException", size.to_string())
}

/// The kind of a column: numbers, or text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Number,
    Text,
}

/// `ColumnFormat`, with the [`Style`] of its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Format {
    pub(super) name: String,
    pub(super) precision: i32,
    pub(super) style: Style,
}

impl Format {
    fn new(name: String, precision: i32) -> Self {
        let style = Style::of(&name);
        Self {
            name,
            precision,
            style,
        }
    }
}

/// parso's `Column`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Column {
    pub(super) id: i32,
    pub(super) name: String,
    pub(super) label: String,
    pub(super) format: Format,
    pub(super) kind: Kind,
}

/// What [`Reader::read_next`] reads: a row, its cells read into the buffer it is given, a row
/// the file marks deleted, which parso reads as none and Tika's parser ends the table at
/// (#155), or the end of the rows.
#[derive(Debug)]
pub(super) enum Next {
    Row,
    Deleted,
    End,
}

/// `SasFileProperties`, as far as the reader and Tika's parser read them.
#[derive(Debug, Default)]
pub(super) struct Properties {
    pub(super) u64: bool,
    pub(super) endianness: i32,
    pub(super) encoding: Option<&'static str>,
    pub(super) name: Option<String>,
    pub(super) created: Option<i64>,
    pub(super) modified: Option<i64>,
    header_length: i32,
    page_length: i32,
    pub(super) page_count: i64,
    pub(super) sas_release: Option<String>,
    pub(super) server_type: Option<String>,
    row_length: i64,
    pub(super) row_count: i64,
    mix_page_row_count: i64,
    deleted_row_count: i64,
    pub(super) columns_count: i64,
    compression: Option<Compression>,
}

/// What a missing piece of a column's information is: its name, format or label, in a text
/// block that comes on a later page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Missing {
    Name,
    Format,
    Label,
}

/// `ColumnMissingInfo`.
#[derive(Debug, Clone, Copy)]
struct MissingInfo {
    column: i32,
    text: i32,
    offset: i32,
    length: i32,
    missing: Missing,
}

/// `SubheaderPointer`.
#[derive(Debug, Clone, Copy)]
struct Pointer {
    offset: i64,
    length: i64,
    compression: u8,
    kind: u8,
}

/// The subheaders parso reads, by their signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subheader {
    RowSize,
    ColumnSize,
    Counts,
    ColumnText,
    ColumnName,
    ColumnAttributes,
    FormatAndLabel,
    ColumnList,
    Data,
}

/// `SUBHEADER_SIGNATURE_TO_INDEX`.
fn subheader(signature: i64) -> Option<Subheader> {
    Some(match signature.cast_unsigned() {
        0xFFFF_FFFF_F7F7_F7F7
        | 0x0000_0000_F7F7_F7F7
        | 0xF7F7_F7F7_0000_0000
        | 0xF7F7_F7F7_FFFF_FBFE => Subheader::RowSize,
        0xFFFF_FFFF_F6F6_F6F6
        | 0x0000_0000_F6F6_F6F6
        | 0xF6F6_F6F6_0000_0000
        | 0xF6F6_F6F6_FFFF_FBFE => Subheader::ColumnSize,
        0xFFFF_FFFF_FFFF_FC00 | 0x00FC_FFFF_FFFF_FFFF => Subheader::Counts,
        0xFFFF_FFFF_FFFF_FFFD | 0xFDFF_FFFF_FFFF_FFFF => Subheader::ColumnText,
        0xFFFF_FFFF_FFFF_FFFF => Subheader::ColumnName,
        0xFFFF_FFFF_FFFF_FFFC | 0xFCFF_FFFF_FFFF_FFFF => Subheader::ColumnAttributes,
        0xFFFF_FFFF_FFFF_FBFE | 0xFEFB_FFFF_FFFF_FFFF => Subheader::FormatAndLabel,
        0xFFFF_FFFF_FFFF_FFFE | 0xFEFF_FFFF_FFFF_FFFF => Subheader::ColumnList,
        _ => return None,
    })
}

/// The `DataInputStream` parso reads the file through, whose pages it releases as it reads on.
struct Stream<'a> {
    input: &'a [u8],
    at: usize,
    paging: Paging<'a>,
}

impl Stream<'_> {
    /// `readFully`: `buffer` filled from the stream, or as much of it as the stream has left,
    /// which ends in an `EOFException`.
    fn read_fully(&mut self, buffer: &mut [u8]) -> Result<(), EndOfFile> {
        let available = (self.input.len() - self.at).min(buffer.len());
        buffer[..available].copy_from_slice(&self.input[self.at..self.at + available]);
        self.at += available;
        self.paging.read(available);
        if available < buffer.len() {
            return Err(EndOfFile);
        }
        Ok(())
    }

    /// `skipBytes`: `count` bytes read and dropped, which throws where fewer are left, or where
    /// `count` is negative.
    fn skip(&mut self, count: i64) -> Result<(), Thrown> {
        let available = i64::try_from(self.input.len() - self.at).expect("a length fits");
        let skipped = count.clamp(0, available);
        let skipped_bytes = usize::try_from(skipped).expect("not negative");
        self.at += skipped_bytes;
        self.paging.read(skipped_bytes);
        if skipped != count {
            return Err(io(format!(
                "Expected to skip {count} to the end of the header, but skipped {skipped} instead."
            )));
        }
        Ok(())
    }
}

/// The `EOFException` of a read past the end of the file, which parso takes as its end.
struct EndOfFile;

/// parso's `SasFileParser`.
pub(super) struct Reader<'a> {
    stream: Stream<'a>,
    file_decoder: Option<Decoder>,
    decoder: Decoder,
    pub(super) properties: Properties,
    page: Option<Vec<u8>>,
    page_type: i32,
    block_count: i32,
    subheaders_count: i32,
    file_position: i32,
    column_number: i32,
    row_in_file: i32,
    row_on_page: i32,
    eof: bool,
    file_label_offset: i32,
    file_label_length: i32,
    compression_offset: i32,
    compression_length: i32,
    data_pointers: Vec<Pointer>,
    texts: Vec<Vec<u8>>,
    names: Vec<String>,
    kinds: Vec<Kind>,
    offsets: Vec<i64>,
    lengths: Vec<i32>,
    pub(super) columns: Vec<Column>,
    missing: Vec<MissingInfo>,
    deleted_markers: String,
}

impl<'a> Reader<'a> {
    /// `new SasFileParser`: the metadata read up to the first page with data, an `IOException`
    /// ending it there, or what else parso throws on it. `file_decoder` decodes the charset the
    /// file's encoding byte names, where it names one. The `pages` of `input` are released as
    /// the reader reads on.
    pub(super) fn open(
        input: &'a [u8],
        pages: Pages<'a>,
        file_decoder: Option<Decoder>,
    ) -> Result<Self, Thrown> {
        let mut reader = Self {
            stream: Stream {
                input,
                at: 0,
                paging: Paging::new(pages),
            },
            file_decoder,
            decoder: Decoder::named("US-ASCII").expect("the port decodes US-ASCII"),
            properties: Properties::default(),
            page: None,
            page_type: 0,
            block_count: 0,
            subheaders_count: 0,
            file_position: 0,
            column_number: 0,
            row_in_file: 0,
            row_on_page: 0,
            eof: false,
            file_label_offset: 0,
            file_label_length: 0,
            compression_offset: 0,
            compression_length: 0,
            data_pointers: Vec::new(),
            texts: Vec::new(),
            names: Vec::new(),
            kinds: Vec::new(),
            offsets: Vec::new(),
            lengths: Vec::new(),
            columns: Vec::new(),
            missing: Vec::new(),
            deleted_markers: String::new(),
        };
        match reader.read_metadata() {
            Ok(()) | Err(Thrown::Io(_)) => Ok(reader),
            Err(thrown) => Err(thrown),
        }
    }

    fn int_or_long(&self) -> i64 {
        if self.properties.u64 {
            8
        } else {
            4
        }
    }

    fn bit_offset(&self) -> i64 {
        if self.properties.u64 {
            PAGE_BIT_OFFSET_X64
        } else {
            PAGE_BIT_OFFSET_X86
        }
    }

    fn pointer_length(&self) -> i64 {
        if self.properties.u64 {
            SUBHEADER_POINTER_LENGTH_X64
        } else {
            SUBHEADER_POINTER_LENGTH_X86
        }
    }

    fn little_endian(&self) -> bool {
        self.properties.endianness != BIG_ENDIAN
    }

    /// The first `N` bytes of `bytes`, which a `ByteBuffer` read throws on where there are fewer.
    fn first<const N: usize>(bytes: &[u8]) -> Result<[u8; N], Thrown> {
        bytes
            .get(..N)
            .map(|first| first.try_into().expect("N bytes"))
            .ok_or_else(underflow)
    }

    /// `bytesToShort`.
    fn short(&self, bytes: &[u8]) -> Result<i32, Thrown> {
        let first = Self::first(bytes)?;
        Ok(i32::from(if self.little_endian() {
            i16::from_le_bytes(first)
        } else {
            i16::from_be_bytes(first)
        }))
    }

    /// `bytesToInt`.
    fn int(&self, bytes: &[u8]) -> Result<i32, Thrown> {
        let first = Self::first(bytes)?;
        Ok(if self.little_endian() {
            i32::from_le_bytes(first)
        } else {
            i32::from_be_bytes(first)
        })
    }

    /// `bytesToLong`: eight bytes in a 64-bit file, four in a 32-bit one.
    fn long(&self, bytes: &[u8]) -> Result<i64, Thrown> {
        if !self.properties.u64 {
            return Ok(i64::from(self.int(bytes)?));
        }
        let first = Self::first(bytes)?;
        Ok(if self.little_endian() {
            i64::from_le_bytes(first)
        } else {
            i64::from_be_bytes(first)
        })
    }

    /// `bytesToDouble`: a number of fewer than eight bytes, its low bytes dropped, padded.
    fn double(&self, bytes: &[u8]) -> Result<f64, Thrown> {
        let mut eight = [0u8; 8];
        if bytes.len() < 8 {
            let start = if self.properties.endianness == LITTLE_ENDIAN {
                8 - bytes.len()
            } else {
                0
            };
            eight[start..start + bytes.len()].copy_from_slice(bytes);
        } else {
            eight.copy_from_slice(&bytes[..8]);
        }
        Ok(f64::from_bits(if self.little_endian() {
            u64::from_le_bytes(eight)
        } else {
            u64::from_be_bytes(eight)
        }))
    }

    /// `bytesToString`.
    fn string(&self, bytes: &[u8]) -> String {
        self.decoder.decode(bytes)
    }

    /// `new String(bytes, offset, length, encoding)`, which throws where the range does not lie
    /// within `bytes`.
    fn string_in(&self, bytes: &[u8], offset: i32, length: i32) -> Result<String, Thrown> {
        let range = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(length).ok())
            .and_then(|(offset, length)| bytes.get(offset..offset.checked_add(length)?))
            .ok_or_else(|| {
                runtime(
                    "java.lang.StringIndexOutOfBoundsException",
                    format!("offset {offset}, count {length}, length {}", bytes.len()),
                )
            })?;
        Ok(self.string(range))
    }

    /// `getBytesFromFile`: the fields at `fields`, offsets and lengths, read from the stream
    /// before the first page, or from the page.
    fn fields(&mut self, fields: &[(i64, i32)]) -> Result<Vec<Vec<u8>>, Thrown> {
        let mut read = Vec::with_capacity(fields.len());
        for &(offset, length) in fields {
            let Some(page) = &self.page else {
                let size = usize::try_from(length).map_err(|_| negative_size(length))?;
                let mut field = vec![0; size];
                self.stream.skip(offset - i64::from(self.file_position))?;
                if self.stream.read_fully(&mut field).is_err() {
                    self.eof = true;
                }
                self.file_position = (offset as i32).wrapping_add(length);
                read.push(field);
                continue;
            };
            if to_i64(page.len()) < offset {
                return Err(io(EMPTY_INPUT_STREAM));
            }
            let from = i64::from(offset as i32);
            read.push(copy_of_range(page, from, from + i64::from(length))?);
        }
        Ok(read)
    }

    /// `getMetadataFromSasFile`: the header, then the pages up to the first with data.
    fn read_metadata(&mut self) -> Result<(), Thrown> {
        self.read_header()?;
        let length = self.properties.page_length;
        let size = usize::try_from(length).map_err(|_| negative_size(length))?;
        self.page = Some(vec![0; size]);
        loop {
            if !self.read_page() {
                self.eof = true;
                break;
            }
            if self.read_page_metadata()? {
                break;
            }
        }
        Ok(())
    }

    /// `readFully` of the next page into the page held, false where the file ends first.
    fn read_page(&mut self) -> bool {
        let page = self
            .page
            .as_mut()
            .expect("the page is allocated before pages are read");
        self.stream.read_fully(page).is_ok()
    }

    /// `processSasFileHeader`.
    fn read_header(&mut self) -> Result<(), Thrown> {
        let aligns = self.fields(&[(ALIGN_1_OFFSET, 1), (ALIGN_2_OFFSET, 1)])?;
        let mut align_1 = 0;
        let mut align_2 = 0;
        if aligns[0][0] == ALIGN_CHECKER_VALUE {
            align_2 = ALIGN_VALUE;
            self.properties.u64 = true;
        }
        if aligns[1][0] == ALIGN_CHECKER_VALUE {
            align_1 = ALIGN_VALUE;
        }
        let total = align_1 + align_2;
        let page_count_length = 4 + i32::try_from(align_2).expect("small");
        let fields = self.fields(&[
            (ENDIANNESS_OFFSET, 1),
            (ENCODING_OFFSET as i64, 1),
            (DATASET_OFFSET, DATASET_LENGTH as i32),
            (FILE_TYPE_OFFSET, 8),
            (DATE_CREATED_OFFSET + align_1, 8),
            (DATE_MODIFIED_OFFSET + align_1, 8),
            (HEADER_SIZE_OFFSET + align_1, 4),
            (PAGE_SIZE_OFFSET + align_1, 4),
            (PAGE_COUNT_OFFSET + align_1, page_count_length),
            (SAS_RELEASE_OFFSET + total, 8),
            (SAS_SERVER_TYPE_OFFSET + total, 16),
            (OS_VERSION_NUMBER_OFFSET + total, 16),
            (OS_MAKER_OFFSET + total, 16),
            (OS_NAME_OFFSET + total, 16),
        ])?;
        self.properties.endianness = i32::from(fields[0][0].cast_signed());
        if !matches!(self.properties.endianness, LITTLE_ENDIAN | BIG_ENDIAN) {
            return Err(io(FILE_NOT_VALID));
        }
        let encoding = encoding_name(fields[1][0]);
        if encoding.is_some() {
            self.decoder = self
                .file_decoder
                .expect("a file whose charset this port does not decode is left unread");
        }
        self.properties.encoding = encoding;
        self.properties.name = Some(java_trim(&self.string(&fields[2])));
        self.properties.created = self.date_time(&fields[4])?;
        self.properties.modified = self.date_time(&fields[5])?;
        self.properties.header_length = self.int(&fields[6])?;
        let page_length = self.int(&fields[7])?;
        if page_length > MAX_PAGE_LENGTH {
            return Err(io(format!(
                "Page limit ({page_length}) exceeds maximum: {MAX_PAGE_LENGTH}"
            )));
        }
        self.properties.page_length = page_length;
        self.properties.page_count = self.long(&fields[8])?;
        self.properties.sas_release = Some(java_trim(&self.string(&fields[9])));
        self.properties.server_type = Some(java_trim(&self.string(&fields[10])));
        let skip = i64::from(self.properties.header_length) - i64::from(self.file_position);
        self.stream.skip(skip)?;
        self.file_position = 0;
        Ok(())
    }

    /// `bytesToDateTime`: the date of the SAS date and time, none for NaN.
    fn date_time(&self, bytes: &[u8]) -> Result<Option<i64>, Thrown> {
        let seconds = self.double(bytes)?;
        Ok((!seconds.is_nan()).then(|| values::sas_date_millis(seconds)))
    }

    /// `processSasFilePageMeta`: the subheaders of a page of metadata, and whether the page
    /// has data.
    fn read_page_metadata(&mut self) -> Result<bool, Thrown> {
        self.read_page_header()?;
        if is_meta(self.page_type) || is_mix(self.page_type) {
            self.read_subheaders();
        }
        Ok(is_data(self.page_type) || is_mix(self.page_type) || !self.data_pointers.is_empty())
    }

    /// `readPageHeader`.
    fn read_page_header(&mut self) -> Result<(), Thrown> {
        let bit_offset = self.bit_offset();
        let fields = self.fields(&[(bit_offset, 2), (bit_offset + 2, 2), (bit_offset + 4, 2)])?;
        self.page_type = self.short(&fields[0])?;
        self.block_count = self.short(&fields[1])?;
        self.subheaders_count = self.short(&fields[2])?;
        Ok(())
    }

    /// `processPageMetadata`: each subheader of the page read, one that fails skipped, and those
    /// of data kept to read rows from.
    fn read_subheaders(&mut self) {
        let mut pointers = Vec::new();
        for index in 0..self.subheaders_count.max(0) {
            let _skipped = self.read_subheader(&mut pointers, index);
        }
    }

    fn read_subheader(&mut self, pointers: &mut Vec<Pointer>, index: i32) -> Result<(), Thrown> {
        let pointer = self.read_pointer(index)?;
        pointers.push(pointer);
        if pointer.compression == TRUNCATED_SUBHEADER_ID {
            return Ok(());
        }
        let signature = {
            let length = i32::try_from(self.int_or_long()).expect("small");
            let fields = self.fields(&[(pointer.offset, length)])?;
            self.long(&fields[0])?
        };
        let Some(kind) = self.subheader_kind(signature, pointer) else {
            return Ok(());
        };
        let indexed = usize::try_from(index).expect("not negative");
        let pointer = *pointers
            .get(indexed)
            .ok_or_else(|| index_out_of_bounds(index, pointers.len()))?;
        match kind {
            Subheader::Data => self.data_pointers.push(pointer),
            Subheader::RowSize => self.read_row_size(pointer.offset)?,
            Subheader::ColumnSize => self.read_column_size(pointer.offset)?,
            Subheader::Counts | Subheader::ColumnList => {}
            Subheader::ColumnText => self.read_column_text(pointer.offset)?,
            Subheader::ColumnName => self.read_column_names(pointer.offset, pointer.length)?,
            Subheader::ColumnAttributes => {
                self.read_column_attributes(pointer.offset, pointer.length)?;
            }
            Subheader::FormatAndLabel => self.read_format_and_label(pointer.offset)?,
        }
        Ok(())
    }

    /// `chooseSubheaderClass`: the subheader of a signature, or, in a compressed file, data.
    fn subheader_kind(&self, signature: i64, pointer: Pointer) -> Option<Subheader> {
        subheader(signature).or_else(|| {
            (self.properties.compression.is_some()
                && matches!(pointer.compression, COMPRESSED_SUBHEADER_ID | 0)
                && pointer.kind == COMPRESSED_SUBHEADER_TYPE)
                .then_some(Subheader::Data)
        })
    }

    /// `processSubheaderPointers`.
    fn read_pointer(&mut self, index: i32) -> Result<Pointer, Thrown> {
        let length = self.int_or_long();
        let total = self.bit_offset()
            + SUBHEADER_POINTERS_OFFSET
            + self.pointer_length() * i64::from(index);
        let width = i32::try_from(length).expect("small");
        let fields = self.fields(&[
            (total, width),
            (total + length, width),
            (total + 2 * length, 1),
            (total + 2 * length + 1, 1),
        ])?;
        Ok(Pointer {
            offset: self.long(&fields[0])?,
            length: self.long(&fields[1])?,
            compression: fields[2][0],
            kind: fields[3][0],
        })
    }

    /// `RowSizeSubheader`.
    fn read_row_size(&mut self, offset: i64) -> Result<(), Thrown> {
        let length = self.int_or_long();
        let width = i32::try_from(length).expect("small");
        let fields = self.fields(&[
            (offset + 5 * length, width),
            (offset + 6 * length, width),
            (offset + 15 * length, width),
            (offset + 24 + 82 * length, 2),
            (offset + 26 + 82 * length, 2),
            (offset + 8 * length, width),
            (offset + 36 + 82 * length, 2),
            (offset + 38 + 82 * length, 2),
        ])?;
        if self.properties.row_length == 0 {
            self.properties.row_length = self.long(&fields[0])?;
        }
        if self.properties.row_count == 0 {
            self.properties.row_count = self.long(&fields[1])?;
        }
        if self.properties.mix_page_row_count == 0 {
            self.properties.mix_page_row_count = self.long(&fields[2])?;
        }
        self.file_label_offset = self.short(&fields[3])?;
        self.file_label_length = self.short(&fields[4])?;
        if self.properties.deleted_row_count == 0 {
            self.properties.deleted_row_count = self.long(&fields[5])?;
        }
        self.compression_offset = self.short(&fields[6])?;
        self.compression_length = self.short(&fields[7])?;
        Ok(())
    }

    /// `ColumnSizeSubheader`.
    fn read_column_size(&mut self, offset: i64) -> Result<(), Thrown> {
        let length = self.int_or_long();
        let fields = self.fields(&[(offset + length, i32::try_from(length).expect("small"))])?;
        self.properties.columns_count = self.long(&fields[0])?;
        Ok(())
    }

    /// `ColumnTextSubheader`: a block of text, the first one naming the compression.
    fn read_column_text(&mut self, offset: i64) -> Result<(), Thrown> {
        let start = offset + self.int_or_long();
        let size = self.fields(&[(start, 2)])?;
        let size = self.short(&size[0])?;
        let text = self.fields(&[(start, size)])?.remove(0);
        self.texts.push(text);
        if self.texts.len() == 1 {
            let first = &self.texts[0];
            let compression =
                self.string_in(first, self.compression_offset, self.compression_length)?;
            if let Some(compression) = Compression::named(&compression) {
                self.properties.compression = Some(compression);
            }
            self.string_in(first, self.file_label_offset, self.file_label_length)?;
        }
        Ok(())
    }

    /// `ColumnNameSubheader`: each column's name, or a name of its length's NULs where its text
    /// block comes later.
    fn read_column_names(&mut self, offset: i64, length: i64) -> Result<(), Thrown> {
        let word = self.int_or_long();
        let count = (length - 2 * word - 12) / 8;
        let mut index = 0i32;
        while i64::from(index) < count {
            let base = offset + word + 8 * (i64::from(index) + 1);
            let fields = self.fields(&[(base, 2), (base + 2, 2), (base + 4, 2)])?;
            let text = self.short(&fields[0])?;
            let name_offset = self.short(&fields[1])?;
            let name_length = self.short(&fields[2])?;
            if to_i64(self.texts.len()) > i64::from(text) {
                let block = self.text_block(text)?;
                let name = self.string_in(block, name_offset, name_length)?;
                self.names.push(name);
            } else {
                let size = usize::try_from(name_length).map_err(|_| negative_size(name_length))?;
                self.names.push("\0".repeat(size));
                self.missing.push(MissingInfo {
                    column: index,
                    text,
                    offset: name_offset,
                    length: name_length,
                    missing: Missing::Name,
                });
            }
            index += 1;
        }
        Ok(())
    }

    /// The text block at `index`, which `List.get` throws on outside the list.
    fn text_block(&self, index: i32) -> Result<&[u8], Thrown> {
        usize::try_from(index)
            .ok()
            .and_then(|at| self.texts.get(at))
            .map(Vec::as_slice)
            .ok_or_else(|| index_out_of_bounds(index, self.texts.len()))
    }

    /// `ColumnAttributesSubheader`: each column's offset in a row, length and kind.
    fn read_column_attributes(&mut self, offset: i64, length: i64) -> Result<(), Thrown> {
        let word = self.int_or_long();
        let count = (length - 2 * word - 12) / (word + 8);
        let mut index = 0i64;
        while index < count {
            let step = index * (word + 8);
            let fields = self.fields(&[
                (
                    offset + word + 8 + step,
                    i32::try_from(word).expect("small"),
                ),
                (offset + 2 * word + 8 + step, 4),
                (offset + 2 * word + 14 + step, 1),
            ])?;
            self.offsets.push(self.long(&fields[0])?);
            self.lengths.push(self.int(&fields[1])?);
            self.kinds.push(if fields[2][0] == 1 {
                Kind::Number
            } else {
                Kind::Text
            });
            index += 1;
        }
        Ok(())
    }

    /// `FormatAndLabelSubheader`: a column, with its format and label, or with the place of
    /// the text block that holds them on a later page.
    fn read_format_and_label(&mut self, offset: i64) -> Result<(), Thrown> {
        let start = offset + 3 * self.int_or_long();
        let fields = self.fields(&[
            (start, 2),
            (start + 2, 2),
            (start + 22, 2),
            (start + 24, 2),
            (start + 26, 2),
            (start + 28, 2),
            (start + 30, 2),
            (start + 32, 2),
        ])?;
        let numbers = fields
            .iter()
            .map(|field| self.short(field))
            .collect::<Result<Vec<i32>, Thrown>>()?;
        let [_width, precision, format_text, format_offset, format_length, label_text, label_offset, label_length] =
            numbers[..]
        else {
            unreachable!("eight fields were read");
        };
        let column = i32::try_from(self.columns.len()).expect("columns fit");
        let mut label = String::new();
        let mut format = String::new();
        if to_i64(self.texts.len()) > i64::from(label_text) {
            label = self.string_in(self.text_block(label_text)?, label_offset, label_length)?;
        } else {
            self.missing.push(MissingInfo {
                column,
                text: label_text,
                offset: label_offset,
                length: label_length,
                missing: Missing::Label,
            });
        }
        if to_i64(self.texts.len()) > i64::from(format_text) {
            format = self.string_in(self.text_block(format_text)?, format_offset, format_length)?;
        } else {
            self.missing.push(MissingInfo {
                column,
                text: format_text,
                offset: format_offset,
                length: format_length,
                missing: Missing::Format,
            });
        }
        let at = self.columns.len();
        let name = self
            .names
            .get(at)
            .cloned()
            .ok_or_else(|| index_out_of_bounds(at, self.names.len()))?;
        let kind = *self
            .kinds
            .get(at)
            .ok_or_else(|| index_out_of_bounds(at, self.kinds.len()))?;
        let number = self.column_number;
        self.column_number += 1;
        usize::try_from(number)
            .ok()
            .and_then(|number| self.lengths.get(number))
            .ok_or_else(|| index_out_of_bounds(number, self.lengths.len()))?;
        self.columns.push(Column {
            id: number + 1,
            name,
            label,
            format: Format::new(format, precision),
            kind,
        });
        Ok(())
    }

    /// `readNext`: the next row, its cells read into `cells`, a deleted one, or the end of the
    /// rows.
    pub(super) fn read_next(&mut self, cells: &mut Vec<Option<Value>>) -> Result<Next, Thrown> {
        let index = self.row_in_file;
        self.row_in_file = self.row_in_file.wrapping_add(1);
        if i64::from(index) >= self.properties.row_count || self.eof {
            return Ok(Next::End);
        }
        let bit_offset = self.bit_offset();
        let row_length = self.properties.row_length;
        let mut row = Next::Deleted;
        match self.page_type {
            PAGE_META_TYPE_1 | PAGE_META_TYPE_2 | PAGE_CMETA_TYPE => {
                if self.data_pointers.is_empty() && self.page_type == PAGE_CMETA_TYPE {
                    self.read_next_page()?;
                    self.row_on_page = 0;
                }
                let at = self.row_on_page;
                self.row_on_page += 1;
                let pointer = *usize::try_from(at)
                    .ok()
                    .and_then(|at| self.data_pointers.get(at))
                    .ok_or_else(|| index_out_of_bounds(at, self.data_pointers.len()))?;
                self.row(pointer.offset, pointer.length, cells)?;
                row = Next::Row;
                if to_i64(self.data_pointers.len()) == i64::from(self.row_on_page) {
                    self.read_next_page()?;
                    self.row_on_page = 0;
                }
            }
            PAGE_MIX_TYPE_1 | PAGE_MIX_TYPE_2 => {
                if self.page_type == PAGE_MIX_TYPE_2 && self.deleted_markers.is_empty() {
                    self.read_deleted_markers()?;
                }
                let pointers = i64::from(self.subheaders_count) * self.pointer_length();
                let align = (bit_offset + SUBHEADER_POINTERS_OFFSET + pointers) % 8;
                let at = self.row_on_page;
                if self.page_type == PAGE_MIX_TYPE_1 || self.is_kept(at)? {
                    let offset = bit_offset
                        + SUBHEADER_POINTERS_OFFSET
                        + align
                        + pointers
                        + i64::from(at) * row_length;
                    self.row(offset, row_length, cells)?;
                    row = Next::Row;
                }
                self.row_on_page += 1;
                let rows = self
                    .properties
                    .row_count
                    .min(self.properties.mix_page_row_count);
                if i64::from(self.row_on_page) == rows {
                    self.read_next_page()?;
                    self.row_on_page = 0;
                }
            }
            PAGE_DATA_TYPE | PAGE_DATA_TYPE_2 => {
                if self.page_type == PAGE_DATA_TYPE_2 && self.deleted_markers.is_empty() {
                    self.read_deleted_markers()?;
                }
                let at = self.row_on_page;
                if self.page_type == PAGE_DATA_TYPE || self.is_kept(at)? {
                    let offset =
                        bit_offset + SUBHEADER_POINTERS_OFFSET + i64::from(at) * row_length;
                    self.row(offset, row_length, cells)?;
                    row = Next::Row;
                }
                self.row_on_page += 1;
                if self.row_on_page == self.block_count {
                    self.read_next_page()?;
                    self.row_on_page = 0;
                }
            }
            _ => return Ok(Next::End),
        }
        Ok(row)
    }

    /// Whether the row at `at` of the page is not deleted: `deletedMarkers.charAt(at) == '0'`.
    fn is_kept(&self, at: i32) -> Result<bool, Thrown> {
        let marker = usize::try_from(at)
            .ok()
            .and_then(|at| self.deleted_markers.as_bytes().get(at))
            .ok_or_else(|| {
                runtime(
                    "java.lang.StringIndexOutOfBoundsException",
                    format!(
                        "Index {at} out of bounds for length {}",
                        self.deleted_markers.len()
                    ),
                )
            })?;
        Ok(*marker == b'0')
    }

    /// `readNextPage`: the next page of metadata or data, past any other.
    fn read_next_page(&mut self) -> Result<(), Thrown> {
        self.deleted_markers.clear();
        self.process_next_page()?;
        while !is_meta(self.page_type) && !is_mix(self.page_type) && !is_data(self.page_type) {
            if self.eof {
                return Ok(());
            }
            self.process_next_page()?;
        }
        Ok(())
    }

    /// `processNextPage`.
    fn process_next_page(&mut self) -> Result<(), Thrown> {
        self.data_pointers.clear();
        if !self.read_page() {
            self.eof = true;
            return Ok(());
        }
        self.read_page_header()?;
        if is_meta(self.page_type) || self.page_type == PAGE_AMD_TYPE || is_mix(self.page_type) {
            self.read_subheaders();
            self.read_deleted_markers()?;
            if self.page_type == PAGE_AMD_TYPE {
                self.read_missing_column_information()?;
            }
        }
        Ok(())
    }

    /// `processMissingColumnInfo`: the names, formats and labels whose text blocks have come.
    fn read_missing_column_information(&mut self) -> Result<(), Thrown> {
        for info in self.missing.clone() {
            let text = self.string_in(self.text_block(info.text)?, info.offset, info.length)?;
            let length = self.columns.len();
            let column = usize::try_from(info.column)
                .ok()
                .and_then(|at| self.columns.get_mut(at))
                .ok_or_else(|| index_out_of_bounds(info.column, length))?;
            match info.missing {
                Missing::Name => column.name = text,
                Missing::Format => column.format = Format::new(text, 0),
                Missing::Label => column.label = text,
            }
        }
        Ok(())
    }

    /// `readDeletedInfo`: a bit for each row of the page, set where it is deleted.
    fn read_deleted_markers(&mut self) -> Result<(), Thrown> {
        let (pointer_offset, bit_offset) = if self.properties.u64 {
            (PAGE_DELETED_POINTER_OFFSET_X64, PAGE_BIT_OFFSET_X64 + 8)
        } else {
            (PAGE_DELETED_POINTER_OFFSET_X86, PAGE_BIT_OFFSET_X86 + 8)
        };
        let pointers = i64::from(self.subheaders_count) * self.pointer_length();
        let align = (bit_offset + SUBHEADER_POINTERS_OFFSET + pointers) % 8;
        let pointer = self.fields(&[(pointer_offset, 4)])?;
        let pointer = i64::from(self.int(&pointer[0])?);
        let rows = i64::from(self.block_count - self.subheaders_count);
        let offset = bit_offset + pointer + align + pointers + rows * self.properties.row_length;
        let length = (rows as f64 / 8.0).ceil() as i32;
        let bytes = self.fields(&[(offset, length)])?.remove(0);
        for byte in bytes {
            self.deleted_markers.push_str(&format!("{byte:08b}"));
        }
        Ok(())
    }

    /// `processByteArrayWithData`: the cells of the row of `length` bytes at `offset` read into
    /// `cells`, decompressed where it is shorter than a row, up to the first column of no
    /// length.
    fn row(&self, offset: i64, length: i64, cells: &mut Vec<Option<Value>>) -> Result<(), Thrown> {
        let count = self.properties.columns_count as i32;
        let size = usize::try_from(count).map_err(|_| negative_size(count))?;
        cells.clear();
        cells.resize(size, None);
        let page = self.page.as_deref().expect("rows are read from a page");
        let decompressed;
        let (source, start) = match self.properties.compression {
            Some(compression) if length < self.properties.row_length => {
                decompressed = compression.decompress(
                    page,
                    offset as i32,
                    length as i32,
                    self.properties.row_length as i32,
                )?;
                (decompressed.as_slice(), 0)
            }
            _ => (page, offset as i32),
        };
        let mut index = 0usize;
        while to_i64(index) < self.properties.columns_count {
            let length = *self
                .lengths
                .get(index)
                .ok_or_else(|| index_out_of_bounds(index, self.lengths.len()))?;
            if length == 0 {
                break;
            }
            let cell = self.cell(source, start, index, length)?;
            let size = cells.len();
            *cells
                .get_mut(index)
                .ok_or_else(|| array_out_of_bounds(to_i64(index), size))? = cell;
            index += 1;
        }
        Ok(())
    }

    /// `processElement`: the cell of the column at `index` in the row at `start` of `source`.
    fn cell(
        &self,
        source: &[u8],
        start: i32,
        index: usize,
        length: i32,
    ) -> Result<Option<Value>, Thrown> {
        let column = self
            .columns
            .get(index)
            .ok_or_else(|| index_out_of_bounds(index, self.columns.len()))?;
        let offset = *self
            .offsets
            .get(index)
            .ok_or_else(|| index_out_of_bounds(index, self.offsets.len()))?;
        let at = start.wrapping_add(offset as i32);
        if column.kind == Kind::Number {
            let (head, kept) =
                head_of_range::<8>(source, i64::from(at), i64::from(at.wrapping_add(length)))?;
            let bytes = &head[..kept];
            if length <= 2 {
                return Ok(Some(Value::Short(self.short(bytes)?)));
            }
            let number = self.double(bytes)?;
            return Ok(match column.format.style {
                Style::Temporal(temporal) => temporal.value(number),
                Style::Percent | Style::Plain => values::number(number),
            });
        }
        Ok(trimmed(source, i64::from(at), i64::from(length))?
            .map(|bytes| Value::Text(self.string(bytes))))
    }
}

/// `trimBytesArray`: the bytes without the spaces, NULs and tabs they end with, none where
/// nothing is left; it throws where the bytes do not lie within `source`.
fn trimmed(source: &[u8], offset: i64, length: i64) -> Result<Option<&[u8]>, Thrown> {
    let mut end = offset + length;
    while end > offset {
        let byte = usize::try_from(end - 1)
            .ok()
            .and_then(|at| source.get(at))
            .ok_or_else(|| array_out_of_bounds(end - 1, source.len()))?;
        if !matches!(byte, b' ' | b'\0' | b'\t') {
            break;
        }
        end -= 1;
    }
    if end == offset {
        return Ok(None);
    }
    let start = usize::try_from(offset).map_err(|_| array_out_of_bounds(offset, source.len()))?;
    let end = usize::try_from(end).expect("after a start that is not negative");
    Ok(Some(&source[start..end]))
}

fn array_out_of_bounds(index: i64, length: usize) -> Thrown {
    runtime(
        "java.lang.ArrayIndexOutOfBoundsException",
        format!("Index {index} out of bounds for length {length}"),
    )
}

/// `String.trim`.
fn java_trim(text: &str) -> String {
    crate::java::trim(text).to_owned()
}

fn to_i64(value: usize) -> i64 {
    i64::try_from(value).expect("a length fits")
}
