//! Jackcess's `JetFormat`: where the Jet 3 (Access 97), Jet 4 (Access 2000 to 2003) and ACE
//! (Access 2007 and later) formats keep what the reader reads, as the version byte of the file
//! header picks the format.

use super::buffer::Thrown;
use super::column::SortOrder;
use crate::formats::ole2::codepage_encoding;
use crate::text::charset::Charset as JdkCharset;

const OFFSET_VERSION: usize = 20;
const HEADER_LENGTH: usize = 21;
const OFFSET_ENGINE_NAME: usize = 4;
const MSISAM_ENGINE: &[u8] = b"MSISAM Database";

/// `BASE_HEADER_MASK`: what the bytes from `OFFSET_MASKED_HEADER` of the first page are XORed
/// with.
const BASE_HEADER_MASK: [u8; 128] = [
    0xB5, 0x6F, 0x03, 0x62, 0x61, 0x08, 0xC2, 0x55, 0xEB, 0xA9, 0x67, 0x72, 0x43, 0x3F, 0x00, 0x9C,
    0x7A, 0x9F, 0x90, 0xFF, 0x80, 0x9A, 0x31, 0xC5, 0x79, 0xBA, 0xED, 0x30, 0xBC, 0xDF, 0xCC, 0x9D,
    0x63, 0xD9, 0xE4, 0xC3, 0x7B, 0x42, 0xFB, 0x8A, 0xBC, 0x4E, 0x86, 0xFB, 0xEC, 0x37, 0x5D, 0x44,
    0x9C, 0xFA, 0xC6, 0x5E, 0x28, 0xE6, 0x13, 0xB6, 0x8A, 0x60, 0x54, 0x94, 0x7B, 0x36, 0xF5, 0x72,
    0xDF, 0xB1, 0x77, 0xF4, 0x13, 0x43, 0xCF, 0xAF, 0xB1, 0x33, 0x34, 0x61, 0x79, 0x5B, 0x92, 0xB5,
    0x7C, 0x2A, 0x05, 0xF1, 0x7C, 0x99, 0x01, 0x1B, 0x98, 0xFD, 0x12, 0x4F, 0x4A, 0x94, 0x6C, 0x3E,
    0x60, 0x26, 0x5F, 0x95, 0xF8, 0xD0, 0x89, 0x24, 0x85, 0x67, 0xC6, 0x1F, 0x27, 0x44, 0xD2, 0xEE,
    0xCF, 0x65, 0xED, 0xFF, 0x07, 0xC7, 0x46, 0xA1, 0x78, 0x16, 0x0C, 0xED, 0xE9, 0x2D, 0x62, 0xD4,
];

/// How the pages of a format are encoded, as `CryptCodecProvider` reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodecType {
    /// RC4 keyed by the encoding key and the page number, where the key is not blank.
    Jet,
    /// Office encryption, which needs a password, where the encoding key is not blank.
    Office,
}

/// How a format encodes its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Encoding {
    /// In the code page the database's header names: Jet 3.
    CodePage,
    /// UTF-16LE, a text value possibly compressed: Jet 4 and ACE.
    Unicode,
}

/// The charset a database's text is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Charset {
    /// A code page, as a JDK charset this port decodes.
    CodePage(JdkCharset),
    Unicode,
}

impl Charset {
    /// The charset of the Windows code page `code_page`, where it names one the port decodes.
    pub(super) fn of_code_page(code_page: u16) -> Option<Charset> {
        let charset =
            codepage_encoding(i32::from(code_page)).and_then(|name| JdkCharset::for_name(&name))?;
        charset.decode(&[]).map(|_| Charset::CodePage(charset))
    }

    /// The text of `bytes`, malformed input replaced as the JDK replaces it.
    pub(super) fn decode(self, bytes: &[u8]) -> String {
        match self {
            Charset::CodePage(charset) => charset
                .decode(bytes)
                .expect("a code page charset only where the port decodes it"),
            Charset::Unicode => crate::java::utf16le(bytes),
        }
    }
}

/// A `JetFormat`'s offsets and sizes.
#[derive(Debug)]
pub(super) struct Format {
    pub(super) codec: CodecType,
    pub(super) page_size: usize,
    pub(super) offset_masked_header: usize,
    pub(super) header_mask: &'static [u8],
    pub(super) offset_encoding_key: usize,
    pub(super) offset_next_table_def_page: usize,
    pub(super) offset_num_rows: usize,
    pub(super) offset_next_auto_number: usize,
    pub(super) offset_next_complex_auto_number: Option<usize>,
    pub(super) offset_table_type: usize,
    pub(super) offset_max_cols: usize,
    pub(super) offset_num_var_cols: usize,
    pub(super) offset_num_cols: usize,
    pub(super) offset_num_index_slots: usize,
    pub(super) offset_num_indexes: usize,
    pub(super) offset_owned_pages: usize,
    pub(super) offset_index_def_block: usize,
    pub(super) offset_column_type: usize,
    pub(super) offset_column_number: usize,
    pub(super) offset_column_scale: usize,
    pub(super) offset_column_sort_order: usize,
    pub(super) size_sort_order: usize,
    pub(super) offset_column_flags: usize,
    pub(super) offset_column_ext_flags: Option<usize>,
    pub(super) offset_column_complex_id: Option<usize>,
    pub(super) offset_column_length: usize,
    pub(super) offset_column_variable_table_index: usize,
    pub(super) offset_column_fixed_data_offset: usize,
    pub(super) offset_column_fixed_data_row_offset: usize,
    pub(super) offset_row_start: usize,
    pub(super) offset_usage_map_start: usize,
    pub(super) offset_usage_map_page_data: usize,
    pub(super) offset_reference_map_page_numbers: usize,
    pub(super) offset_num_rows_on_data_page: usize,
    pub(super) size_index_definition: usize,
    pub(super) size_column_header: usize,
    pub(super) size_long_value_def: usize,
    pub(super) skip_before_index_flags: usize,
    pub(super) skip_after_index_flags: usize,
    pub(super) skip_before_index_slot: usize,
    pub(super) skip_after_index_slot: usize,
    pub(super) skip_before_index: usize,
    pub(super) size_name_length: usize,
    pub(super) size_row_column_count: usize,
    pub(super) size_row_var_col_offset: usize,
    pub(super) encoding: Encoding,
    pub(super) default_sort_order: SortOrder,
}

const JET3: Format = Format {
    codec: CodecType::Jet,
    page_size: 2048,
    offset_masked_header: 24,
    header_mask: BASE_HEADER_MASK.split_at(BASE_HEADER_MASK.len() - 2).0,
    offset_encoding_key: 62,
    offset_next_table_def_page: 4,
    offset_num_rows: 12,
    offset_next_auto_number: 20,
    offset_next_complex_auto_number: None,
    offset_table_type: 20,
    offset_max_cols: 21,
    offset_num_var_cols: 23,
    offset_num_cols: 25,
    offset_num_index_slots: 27,
    offset_num_indexes: 31,
    offset_owned_pages: 35,
    offset_index_def_block: 43,
    offset_column_type: 0,
    offset_column_number: 1,
    offset_column_scale: 12,
    offset_column_sort_order: 9,
    size_sort_order: 2,
    offset_column_flags: 13,
    offset_column_ext_flags: None,
    offset_column_complex_id: None,
    offset_column_length: 16,
    offset_column_variable_table_index: 3,
    offset_column_fixed_data_offset: 14,
    offset_column_fixed_data_row_offset: 1,
    offset_row_start: 10,
    offset_usage_map_start: 5,
    offset_usage_map_page_data: 4,
    offset_reference_map_page_numbers: 1,
    offset_num_rows_on_data_page: 8,
    size_index_definition: 8,
    size_column_header: 18,
    size_long_value_def: 12,
    skip_before_index_flags: 0,
    skip_after_index_flags: 0,
    skip_before_index_slot: 0,
    skip_after_index_slot: 0,
    skip_before_index: 0,
    size_name_length: 1,
    size_row_column_count: 1,
    size_row_var_col_offset: 1,
    encoding: Encoding::CodePage,
    default_sort_order: SortOrder::GENERAL_97,
};

const JET4: Format = Format {
    codec: CodecType::Jet,
    page_size: 4096,
    offset_masked_header: 24,
    header_mask: &BASE_HEADER_MASK,
    offset_encoding_key: 62,
    offset_next_table_def_page: 4,
    offset_num_rows: 16,
    offset_next_auto_number: 20,
    offset_next_complex_auto_number: None,
    offset_table_type: 40,
    offset_max_cols: 41,
    offset_num_var_cols: 43,
    offset_num_cols: 45,
    offset_num_index_slots: 47,
    offset_num_indexes: 51,
    offset_owned_pages: 55,
    offset_index_def_block: 63,
    offset_column_type: 0,
    offset_column_number: 5,
    offset_column_scale: 12,
    offset_column_sort_order: 11,
    size_sort_order: 4,
    offset_column_flags: 15,
    offset_column_ext_flags: Some(16),
    offset_column_complex_id: None,
    offset_column_length: 23,
    offset_column_variable_table_index: 7,
    offset_column_fixed_data_offset: 21,
    offset_column_fixed_data_row_offset: 2,
    offset_row_start: 14,
    offset_usage_map_start: 5,
    offset_usage_map_page_data: 4,
    offset_reference_map_page_numbers: 1,
    offset_num_rows_on_data_page: 12,
    size_index_definition: 12,
    size_column_header: 25,
    size_long_value_def: 12,
    skip_before_index_flags: 4,
    skip_after_index_flags: 5,
    skip_before_index_slot: 4,
    skip_after_index_slot: 4,
    skip_before_index: 4,
    size_name_length: 2,
    size_row_column_count: 2,
    size_row_var_col_offset: 2,
    encoding: Encoding::Unicode,
    default_sort_order: SortOrder::GENERAL_LEGACY,
};

/// The ACE formats, which differ from Jet 4 in their codec, their complex columns and, from
/// Access 2010 on, their default sort order.
const fn ace(default_sort_order: SortOrder) -> Format {
    Format {
        codec: CodecType::Office,
        offset_next_complex_auto_number: Some(28),
        offset_column_complex_id: Some(11),
        default_sort_order,
        ..JET4
    }
}

const ACE12: Format = ace(SortOrder::GENERAL_LEGACY);
const ACE14: Format = ace(SortOrder::GENERAL);

/// What `JetFormat.getFormat` makes of a file: its format, or why it throws, or that it is an
/// MSISAM (Microsoft Money) database, which the port does not read.
pub(super) enum Detected {
    Format(&'static Format),
    Msisam,
}

/// `JetFormat.getFormat`.
pub(super) fn detect(file: &[u8]) -> Result<Detected, Thrown> {
    if file.len() < HEADER_LENGTH {
        return Err(Thrown::io("Empty database file"));
    }
    let version = file[OFFSET_VERSION] as i8;
    Ok(Detected::Format(match version {
        0 => &JET3,
        1 if file[OFFSET_ENGINE_NAME..].starts_with(MSISAM_ENGINE) => return Ok(Detected::Msisam),
        1 => &JET4,
        2 => &ACE12,
        3 | 5 | 6 => &ACE14,
        _ => {
            return Err(Thrown::io(format!(
                "Unsupported {} version: {version}",
                if version < 0 { "older" } else { "newer" }
            )))
        }
    }))
}
