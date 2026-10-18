//! Jackcess's `UsageMap`: the pages a table, an index or the database owns, as a bitmap kept
//! inline in a row of a page or on usage map pages that row refers to.

use super::buffer::{Buffer, Thrown};
use super::pages::{Pages, PAGE_TYPE_USAGE_MAP};
use super::table::{row_end, row_start};

const MAP_TYPE_INLINE: u8 = 0x0;
const MAP_TYPE_REFERENCE: u8 = 0x1;

/// A usage map: the pages from `start_page` whose bits are set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UsageMap {
    start_page: i32,
    end_page: i32,
    /// The set bits, ascending: pages counted from `start_page`.
    bits: Vec<i32>,
}

impl UsageMap {
    /// `UsageMap.read(database, buffer)`: the row and page number of a map, then the map.
    pub(super) fn read_at(pages: &Pages<'_>, buffer: &mut Buffer<'_>) -> Result<Self, Thrown> {
        let row = i32::from(buffer.u8()? as i8);
        let page = buffer.u24()?;
        Self::read(pages, page, row)
    }

    /// `UsageMap.read(database, pageNum, rowNum, isGlobal)`.
    pub(super) fn read(pages: &Pages<'_>, page_number: i32, row: i32) -> Result<Self, Thrown> {
        if page_number <= 0 {
            return Err(Thrown::illegal_state(format!(
                "Invalid usage map page number {page_number}"
            )));
        }
        let format = pages.format;
        let page = pages.page(page_number)?;
        let mut buffer = Buffer::new(&page);
        let start = row_start(&buffer, row, format)?;
        let end = row_end(&buffer, row, format)?;
        buffer.set_limit(i64::from(end))?;
        let map_type = buffer.u8_at(i64::from(start))?;
        let start = i64::from(start);
        buffer.set_position(start + format.offset_usage_map_start as i64)?;
        match map_type {
            MAP_TYPE_INLINE => {
                let data_start = buffer.position() as i64;
                let max_inline_pages = (buffer.limit() as i64 - data_start) as i32 * 8;
                let start_page = buffer.i32_at(start + 1)?;
                let mut map = Self {
                    start_page,
                    end_page: start_page.wrapping_add(max_inline_pages),
                    bits: Vec::new(),
                };
                let data = &buffer.bytes()[buffer.position()..buffer.limit()];
                map.process(data, 0)?;
                Ok(map)
            }
            MAP_TYPE_REFERENCE => {
                let max_per_page =
                    ((format.page_size - format.offset_usage_map_page_data) * 8) as i32;
                let usage_pages = ((buffer.limit() as i64 - start - 1) / 4) as i32;
                let mut map = Self {
                    start_page: 0,
                    end_page: usage_pages.wrapping_mul(max_per_page),
                    bits: Vec::new(),
                };
                for i in 0..usage_pages {
                    let pointer =
                        start + format.offset_reference_map_page_numbers as i64 + i64::from(i) * 4;
                    let map_page_number = buffer.i32_at(pointer)?;
                    if map_page_number <= 0 {
                        continue;
                    }
                    let map_page = pages.page(map_page_number)?;
                    let page_type = map_page[0] as i8;
                    if map_page[0] != PAGE_TYPE_USAGE_MAP {
                        return Err(Thrown::io(format!(
                            "Looking for usage map at page {map_page_number}, but page type is \
                             {page_type}"
                        )));
                    }
                    map.process(
                        &map_page[format.offset_usage_map_page_data..],
                        max_per_page.wrapping_mul(i),
                    )?;
                }
                Ok(map)
            }
            other => Err(Thrown::io(format!(
                "Unrecognized map type: {}",
                other as i8
            ))),
        }
    }

    /// `processMap`: the set bits of `data`, the first counted from `first`.
    fn process(&mut self, data: &[u8], first: i32) -> Result<(), Thrown> {
        for (index, &byte) in data.iter().enumerate() {
            if byte == 0 {
                continue;
            }
            for bit in 0..8 {
                if byte & (1 << bit) == 0 {
                    continue;
                }
                let offset = (index as i32)
                    .wrapping_mul(8)
                    .wrapping_add(bit)
                    .wrapping_add(first);
                let page = if offset >= 0 {
                    self.start_page.wrapping_add(offset)
                } else {
                    -1
                };
                if page < self.start_page || page >= self.end_page {
                    return Err(Thrown::illegal_state(format!(
                        "found page number {page} in usage map outside of expected range {} to {}",
                        self.start_page, self.end_page
                    )));
                }
                self.bits.push(offset);
            }
        }
        self.bits.sort_unstable();
        self.bits.dedup();
        Ok(())
    }

    /// `containsPageNumber`.
    pub(super) fn contains(&self, page: i32) -> bool {
        page >= self.start_page
            && page < self.end_page
            && self
                .bits
                .binary_search(&page.wrapping_sub(self.start_page))
                .is_ok()
    }

    /// The pages a forward `PageCursor` gives, in order.
    pub(super) fn pages(&self) -> impl Iterator<Item = i32> + '_ {
        self.bits
            .iter()
            .map(|&offset| self.start_page.wrapping_add(offset))
    }
}
