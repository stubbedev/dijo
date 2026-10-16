//! JBIG2 files (ITU-T T.88, Annex D): their structure as jbig2-imageio's `JBIG2Document` maps it
//! for ImageIO, the pages and the information of each, and the image of a file of one page,
//! which hayro-jbig2 decodes.

use std::collections::BTreeMap;

use image::GrayImage;

/// `JBIG2Document.FILE_HEADER_ID`.
const FILE_HEADER_ID: [u8; 8] = [0x97, 0x4A, 0x42, 0x32, 0x0D, 0x0A, 0x1A, 0x0A];
/// The segment type of a page's information (7.4.8).
const PAGE_INFORMATION: u8 = 48;
/// The segment type of the end of a file (7.4.11).
const END_OF_FILE: u8 = 51;
/// The bytes of a page information segment's data that `PageInformation` reads.
const PAGE_INFORMATION_LENGTH: usize = 19;

/// How a file lays its segments out (D.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Organisation {
    /// Each segment's header followed by its data.
    Sequential,
    /// Every header, then the data of every segment in the same order.
    RandomAccess,
}

/// A segment's header, as far as the structure needs it.
#[derive(Debug, Clone, Copy)]
struct Segment {
    kind: u8,
    data_length: u64,
    data_start: u64,
}

/// A page's information segment (7.4.8): its size, `0xffffffff` high when it is striped and of
/// a height known only once decoded, and its resolution in pixels a metre, 0 when unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageInformation {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) resolution_x: i32,
    pub(crate) resolution_y: i32,
}

/// A JBIG2 file as `JBIG2Document` maps it: the number of pages its header declares, and the
/// segments of each page by page number, each page's by segment number.
#[derive(Debug)]
pub(crate) struct Jbig2File {
    declared_pages: Option<i32>,
    pages: BTreeMap<i32, BTreeMap<i32, Segment>>,
}

/// Reads past the end of the data fail, as `EOFException` does.
struct Bits<'a> {
    data: &'a [u8],
    position: u64,
}

impl Bits<'_> {
    fn bytes(&mut self, count: usize) -> Result<&[u8], String> {
        let start = usize::try_from(self.position).map_err(|_| "a position past the file")?;
        let bytes = start
            .checked_add(count)
            .and_then(|end| self.data.get(start..end))
            .ok_or_else(|| format!("the file ends inside a segment header at {start}"))?;
        self.position += count as u64;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// `reachedEndOfStream`: whether fewer than four bytes follow `offset`.
    fn ends_at(&self, offset: u64) -> bool {
        offset.saturating_add(4) > self.data.len() as u64
    }
}

impl Jbig2File {
    /// `JBIG2Document.mapStream`: the file header, when there is one, then the segment headers
    /// up to the end of the file or of its data, each segment of a page kept under the page.
    /// Fails where jbig2-imageio throws.
    pub(crate) fn read(data: &[u8]) -> Result<Self, String> {
        let mut bits = Bits { data, position: 0 };
        let mut organisation = Organisation::Sequential;
        let mut declared_pages = None;
        if data.starts_with(&FILE_HEADER_ID) {
            bits.position = FILE_HEADER_ID.len() as u64;
            let flags = bits.u8()?;
            organisation = if flags & 1 == 1 {
                Organisation::Sequential
            } else {
                Organisation::RandomAccess
            };
            if flags & 2 == 0 {
                declared_pages = Some(bits.u32()? as i32);
            }
        }
        let mut offset = bits.position;
        let mut segments = Vec::new();
        let mut pages: BTreeMap<i32, BTreeMap<i32, usize>> = BTreeMap::new();
        let mut kind = 0;
        while kind != END_OF_FILE && !bits.ends_at(offset) {
            bits.position = offset;
            let (number, page, segment) = segment_header(&mut bits, organisation)?;
            kind = segment.kind;
            if page != 0 {
                pages
                    .entry(page)
                    .or_default()
                    .insert(number, segments.len());
            }
            segments.push(segment);
            offset = bits.position;
            if organisation == Organisation::Sequential {
                offset = offset.saturating_add(segment.data_length);
            }
        }
        if !pages.is_empty() && !pages.contains_key(&1) {
            return Err("Page 1 missing".to_owned());
        }
        if organisation == Organisation::RandomAccess {
            for segment in &mut segments {
                segment.data_start = offset;
                offset = offset.saturating_add(segment.data_length);
            }
        }
        let pages = pages
            .into_iter()
            .map(|(page, numbers)| {
                let segments = numbers
                    .into_iter()
                    .map(|(number, index)| (number, segments[index]))
                    .collect();
                (page, segments)
            })
            .collect();
        Ok(Self {
            declared_pages,
            pages,
        })
    }

    /// `getAmountOfPages`: the count the header declares, unless it declares none or 0.
    pub(crate) fn page_count(&self) -> i32 {
        match self.declared_pages {
            Some(count) if count != 0 => count,
            _ => self.pages.len() as i32,
        }
    }

    /// The information segment of the first page, the first by segment number, as
    /// `JBIG2Page.getPageInformationSegment` finds it.
    pub(crate) fn first_page(&self, data: &[u8]) -> Result<PageInformation, String> {
        let segment = self
            .pages
            .get(&1)
            .ok_or("Requested page at index=0 does not exist.")?
            .values()
            .find(|segment| segment.kind == PAGE_INFORMATION)
            .ok_or("the first page has no page information segment")?;
        if segment.data_length < PAGE_INFORMATION_LENGTH as u64 {
            return Err(format!(
                "a page information segment of {} bytes",
                segment.data_length
            ));
        }
        let mut bits = Bits {
            data,
            position: segment.data_start,
        };
        let width = bits.u32()? as i32;
        let height = bits.u32()? as i32;
        let resolution_x = bits.u32()? as i32;
        let resolution_y = bits.u32()? as i32;
        bits.u8()?;
        bits.u16()?;
        Ok(PageInformation {
            width,
            height,
            resolution_x,
            resolution_y,
        })
    }
}

/// `SegmentHeader.parse` (7.2): the segment's number and page, and its type and data.
fn segment_header(
    bits: &mut Bits,
    organisation: Organisation,
) -> Result<(i32, i32, Segment), String> {
    let number = bits.u32()?;
    let flags = bits.u8()?;
    let long_page = flags & 0x40 != 0;
    let kind = flags & 0x3F;
    let first = bits.u8()?;
    let mut referred = u32::from(first >> 5);
    if referred > 4 {
        let rest = bits.bytes(3)?;
        referred = u32::from_be_bytes([first & 0x1F, rest[0], rest[1], rest[2]]);
        let retain_bytes = ((referred as usize + 8) >> 3).saturating_sub(1);
        bits.bytes(retain_bytes)?;
    }
    let size = if number > 65536 {
        4
    } else if number > 256 {
        2
    } else {
        1
    };
    bits.bytes(
        (referred as usize)
            .checked_mul(size)
            .ok_or("too many referred-to segments")?,
    )?;
    let page = if long_page {
        bits.u32()? as i32
    } else {
        i32::from(bits.u8()?)
    };
    let data_length = u64::from(bits.u32()?);
    let data_start = match organisation {
        Organisation::Sequential => bits.position,
        Organisation::RandomAccess => 0,
    };
    Ok((
        number as i32,
        page,
        Segment {
            kind,
            data_length,
            data_start,
        },
    ))
}

/// The image of a JBIG2 file of one page, black on white, or why it cannot be decoded.
pub(crate) fn decode(data: &[u8]) -> Result<GrayImage, String> {
    let file = Jbig2File::read(data)?;
    if file.page_count() != 1 {
        return Err(format!(
            "a JBIG2 file of {} pages, whose first the decoder cannot decode alone",
            file.page_count()
        ));
    }
    let image = hayro_jbig2::Image::new(data).map_err(|e| format!("cannot read the JBIG2: {e}"))?;
    super::decode::within_limits(image.width(), image.height())?;
    let mut gray = Gray {
        pixels: Vec::with_capacity(image.width() as usize * image.height() as usize),
    };
    image
        .decode(&mut gray)
        .map_err(|e| format!("cannot decode the JBIG2: {e}"))?;
    GrayImage::from_raw(image.width(), image.height(), gray.pixels)
        .ok_or_else(|| "the JBIG2 decoded to fewer pixels than its size".to_owned())
}

/// Collects decoded pixels in grey: black for set pixels, white for the others.
struct Gray {
    pixels: Vec<u8>,
}

impl hayro_jbig2::Decoder for Gray {
    fn push_pixel(&mut self, black: bool) {
        self.pixels.push(if black { 0 } else { 255 });
    }

    fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
        let level = if black { 0 } else { 255 };
        self.pixels
            .extend(std::iter::repeat_n(level, chunk_count as usize * 8));
    }

    fn next_line(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of `pages` pages, each of one page information segment, sequential, with the
    /// header declaring their count when `declared`.
    fn file(pages: u8, declared: bool) -> Vec<u8> {
        let mut file = FILE_HEADER_ID.to_vec();
        file.push(if declared { 0x01 } else { 0x03 });
        if declared {
            file.extend_from_slice(&u32::from(pages).to_be_bytes());
        }
        for page in 1..=pages {
            file.extend_from_slice(&u32::from(page).to_be_bytes());
            file.extend_from_slice(&[PAGE_INFORMATION, 0, page]);
            file.extend_from_slice(&(PAGE_INFORMATION_LENGTH as u32).to_be_bytes());
            for value in [40u32 * u32::from(page), 30, 11811, 0] {
                file.extend_from_slice(&value.to_be_bytes());
            }
            file.extend_from_slice(&[0, 0, 0]);
        }
        file
    }

    #[test]
    fn maps_pages_as_jbig2_imageio_does() {
        let two = file(2, false);
        let mapped = Jbig2File::read(&two).unwrap();
        assert_eq!(mapped.page_count(), 2);
        assert_eq!(
            mapped.first_page(&two).unwrap(),
            PageInformation {
                width: 40,
                height: 30,
                resolution_x: 11811,
                resolution_y: 0,
            }
        );
        let mut declared = file(1, true);
        declared[9..13].copy_from_slice(&7u32.to_be_bytes());
        assert_eq!(Jbig2File::read(&declared).unwrap().page_count(), 7);
        let mut second_only = file(2, false);
        second_only[9 + 6] = 2;
        assert_eq!(Jbig2File::read(&second_only).unwrap_err(), "Page 1 missing");
    }

    #[test]
    fn decodes_files_of_one_page_alone() {
        assert!(decode(&file(2, true)).unwrap_err().contains("of 2 pages"));
    }

    /// jbig2enc coded the corpus's bitmap without loss.
    #[test]
    fn decodes_the_corpus_scan_as_its_bitmap() {
        let decoded = decode(&std::fs::read("../quality/corpus/prose-scan.jb2").unwrap()).unwrap();
        let bitmap = image::open("../quality/corpus/prose-scan.pbm")
            .unwrap()
            .to_luma8();
        assert_eq!(decoded, bitmap);
    }
}
