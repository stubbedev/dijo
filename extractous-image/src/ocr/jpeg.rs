//! JPEG decoded as the JDK's ImageIO decodes it for our Java `PreparingOcrParser`: by the IJG's
//! libjpeg 6b that the JDK carries, with its accurate integer inverse DCT, its fancy upsampling,
//! its block smoothing of progressive images and its YCbCr conversion, so that the samples are
//! the JDK's to the bit, in the colour space the JDK's reader settles on. Sequential and
//! progressive Huffman-coded JPEG of 8-bit samples is read, damaged data as libjpeg reads it;
//! what libjpeg 6b does not read (arithmetic coding, lossless and 12-bit JPEG) is an error, as
//! it is there. CMYK and YCCK images, and images of three components that the JDK's reader
//! refuses for their Adobe transform, are decoded as libjpeg decodes them for tesseract, which
//! reads the file where Java cannot: YCCK converted to CMYK, and the transform taken for YCbCr.

/// A decoded JPEG: its size, its samples and the ICC profile it embeds, if any.
#[derive(Debug)]
pub(crate) struct Jpeg {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) samples: Samples,
    pub(crate) icc_profile: Option<Vec<u8>>,
}

/// The samples of a decoded JPEG, row by row.
#[derive(Debug)]
pub(crate) enum Samples {
    /// One grey sample a pixel.
    Gray(Vec<u8>),
    /// A red, a green and a blue sample a pixel.
    Rgb(Vec<u8>),
    /// A cyan, a magenta, a yellow and a black sample a pixel, each the amount of ink from 0,
    /// none, to 255: as libjpeg decodes them, inverted for an Adobe file, which codes 255 for
    /// no ink as Photoshop writes them.
    Cmyk(Vec<u8>),
}

/// Decodes a JPEG, or tells why ImageIO's libjpeg cannot.
pub(crate) fn decode(input: &[u8]) -> Result<Jpeg, String> {
    Decoder::new(input)
        .decode()
        .map_err(|e| format!("cannot decode the JPEG: {e}"))
}

const DCTSIZE: usize = 8;
const DCTSIZE2: usize = 64;
const MAX_COMPONENTS: usize = 10;
const MAX_COMPS_IN_SCAN: usize = 4;
const MAX_BLOCKS_IN_MCU: usize = 10;
const MAX_DIMENSION: usize = 65500;
const MAX_SAMPLING: usize = 4;
const ICC_OVERHEAD: usize = 14;
const MAX_ICC_MARKER_BYTES: usize = 65533;

const SOF0: u8 = 0xC0;
const DHT: u8 = 0xC4;
const RST0: u8 = 0xD0;
const SOI: u8 = 0xD8;
const EOI: u8 = 0xD9;
const SOS: u8 = 0xDA;
const DQT: u8 = 0xDB;
const DNL: u8 = 0xDC;
const DRI: u8 = 0xDD;
const APP0: u8 = 0xE0;
const APP2: u8 = 0xE2;
const APP14: u8 = 0xEE;
const COM: u8 = 0xFE;

/// `jpeg_natural_order`: the natural position of each coefficient in zigzag order, with the
/// sixteen extra entries that keep a damaged run length inside the block.
const NATURAL_ORDER: [usize; DCTSIZE2 + 16] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63,
    63, 63, 63, 63, 63, 63,
];

/// A Huffman table as a DHT segment defines it: the count of codes of each length and the
/// symbols.
#[derive(Clone)]
struct TableSpec {
    bits: [u8; 17],
    values: [u8; 256],
}

/// A Huffman table derived for decoding a code bit by bit (`d_derived_tbl`).
struct Huffman {
    max_code: [i32; 18],
    value_offset: [i32; 17],
    values: [u8; 256],
}

impl Huffman {
    /// `jpeg_make_d_derived_tbl`.
    fn derive(spec: &TableSpec, dc: bool) -> Result<Self, String> {
        let bad = || "Bogus Huffman table definition".to_owned();
        let mut sizes = Vec::with_capacity(257);
        for length in 1..=16u8 {
            let count = usize::from(spec.bits[usize::from(length)]);
            if sizes.len() + count > 256 {
                return Err(bad());
            }
            sizes.extend(std::iter::repeat_n(length, count));
        }
        let symbols = sizes.len();
        let mut codes = vec![0i32; symbols];
        let mut code = 0i32;
        let mut size = sizes.first().copied().unwrap_or(0);
        let mut p = 0;
        while p < symbols {
            while p < symbols && sizes[p] == size {
                codes[p] = code;
                p += 1;
                code += 1;
            }
            if code >= 1 << size {
                return Err(bad());
            }
            code <<= 1;
            size += 1;
        }
        let mut max_code = [-1i32; 18];
        let mut value_offset = [0i32; 17];
        let mut p = 0usize;
        for length in 1..=16 {
            let count = usize::from(spec.bits[length]);
            if count > 0 {
                value_offset[length] = p as i32 - codes[p];
                p += count;
                max_code[length] = codes[p - 1];
            }
        }
        max_code[17] = 0xFFFFF;
        if dc && spec.values[..symbols].iter().any(|&symbol| symbol > 15) {
            return Err(bad());
        }
        Ok(Self {
            max_code,
            value_offset,
            values: spec.values,
        })
    }
}

/// A component of the frame, with its coefficients.
struct Component {
    id: u8,
    h: usize,
    v: usize,
    quant_table: usize,
    width_in_blocks: usize,
    height_in_blocks: usize,
    downsampled_width: usize,
    downsampled_height: usize,
    blocks_per_row: usize,
    /// The quantization table latched by the first scan of the component, in natural order.
    quant: Option<[u16; DCTSIZE2]>,
    coefficients: Vec<[i16; DCTSIZE2]>,
    dc_table: usize,
    ac_table: usize,
    /// For each coefficient, the bit position progressive scans have coded it down to, or -1.
    coef_bits: [i32; DCTSIZE2],
}

impl Component {
    fn block(&mut self, row: usize, column: usize) -> &mut [i16; DCTSIZE2] {
        &mut self.coefficients[row * self.blocks_per_row + column]
    }
}

/// The colour space of the samples, as libjpeg and the JDK's reader settle on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorSpace {
    Grayscale,
    Rgb,
    YCbCr,
    Cmyk,
    Ycck,
    Unknown,
}

struct Frame {
    progressive: bool,
    width: usize,
    height: usize,
    max_h: usize,
    max_v: usize,
    components: Vec<Component>,
}

/// One scan's parameters.
struct Scan {
    components: Vec<usize>,
    ss: usize,
    se: usize,
    ah: u8,
    al: u8,
}

/// The coding of a scan: sequential, or one of the four kinds of progressive scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coding {
    Sequential,
    DcFirst,
    DcRefine,
    AcFirst,
    AcRefine,
}

struct Decoder<'a> {
    data: &'a [u8],
    position: usize,
    unread_marker: Option<u8>,
    quant_tables: [Option<[u16; DCTSIZE2]>; 4],
    dc_specs: [Option<TableSpec>; 4],
    ac_specs: [Option<TableSpec>; 4],
    restart_interval: usize,
    saw_jfif: bool,
    adobe_transform: Option<u8>,
    icc_markers: Vec<Vec<u8>>,
    in_headers: bool,
    frame: Option<Frame>,
    bits: u32,
    bits_left: u32,
    insufficient: bool,
    next_restart: u8,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            unread_marker: None,
            quant_tables: [None; 4],
            dc_specs: [None, None, None, None],
            ac_specs: [None, None, None, None],
            restart_interval: 0,
            saw_jfif: false,
            adobe_transform: None,
            icc_markers: Vec::new(),
            in_headers: true,
            frame: None,
            bits: 0,
            bits_left: 0,
            insufficient: false,
            next_restart: 0,
        }
    }

    fn decode(mut self) -> Result<Jpeg, String> {
        if self.data.len() < 2 || self.data[0] != 0xFF || self.data[1] != SOI {
            return Err(format!(
                "Not a JPEG file: starts with {:#04x} {:#04x}",
                self.data.first().copied().unwrap_or(0),
                self.data.get(1).copied().unwrap_or(0)
            ));
        }
        self.position = 2;
        let first = self.read_headers()?;
        self.in_headers = false;
        let icc_profile = self.icc_profile()?;
        let frame = self.frame.as_ref().expect("a scan follows a frame");
        let space = match self.color_space() {
            ColorSpace::Unknown if frame.components.len() == 3 => ColorSpace::YCbCr,
            ColorSpace::Unknown => {
                return Err(format!(
                    "a JPEG of {} components, which neither the JDK's reader nor tesseract reads",
                    frame.components.len()
                ))
            }
            known => known,
        };
        let multiple_scans = frame.progressive || first.components.len() < frame.components.len();
        if frame.components.iter().any(|component| {
            !frame.max_h.is_multiple_of(component.h) || !frame.max_v.is_multiple_of(component.v)
        }) {
            return Err("Fractional sampling not implemented yet".to_owned());
        }
        self.decode_scan(&first)?;
        if multiple_scans {
            while let Some(scan) = self.next_scan()? {
                self.decode_scan(&scan)?;
            }
        }
        let frame = self.frame.take().expect("a decoded image has a frame");
        let planes = frame.planes();
        let samples = frame.converted(&planes, space, self.adobe_transform.is_some());
        Ok(Jpeg {
            width: frame.width,
            height: frame.height,
            samples,
            icc_profile,
        })
    }

    fn read_byte(&mut self) -> Result<u8, String> {
        let byte = *self
            .data
            .get(self.position)
            .ok_or("Premature end of JPEG file")?;
        self.position += 1;
        Ok(byte)
    }

    fn read_u16(&mut self) -> Result<usize, String> {
        Ok((usize::from(self.read_byte()?) << 8) | usize::from(self.read_byte()?))
    }

    /// `next_marker`, where the JDK's source inserts an EOI at the end of the data.
    fn next_marker(&mut self) -> u8 {
        loop {
            let Some(mut byte) = self.data.get(self.position).copied() else {
                return EOI;
            };
            self.position += 1;
            while byte != 0xFF {
                let Some(next) = self.data.get(self.position).copied() else {
                    return EOI;
                };
                self.position += 1;
                byte = next;
            }
            loop {
                let Some(next) = self.data.get(self.position).copied() else {
                    return EOI;
                };
                self.position += 1;
                byte = next;
                if byte != 0xFF {
                    break;
                }
            }
            if byte != 0 {
                return byte;
            }
        }
    }

    fn take_marker(&mut self) -> u8 {
        match self.unread_marker.take() {
            Some(marker) => marker,
            None => self.next_marker(),
        }
    }

    /// The markers up to the first scan (`jpeg_read_header`), and that scan.
    fn read_headers(&mut self) -> Result<Scan, String> {
        loop {
            match self.take_marker() {
                SOS => {
                    if self.frame.is_none() {
                        return Err("Invalid JPEG file structure: SOS before SOF".to_owned());
                    }
                    return self.start_of_scan();
                }
                EOI => return Err("JPEG datastream contains no image".to_owned()),
                marker => self.read_marker(marker)?,
            }
        }
    }

    /// The markers between scans, and the next scan, if any comes before the image ends.
    fn next_scan(&mut self) -> Result<Option<Scan>, String> {
        loop {
            match self.take_marker() {
                SOS => return self.start_of_scan().map(Some),
                EOI => return Ok(None),
                marker => self.read_marker(marker)?,
            }
        }
    }

    /// A marker other than SOS and EOI, as `read_markers` handles it.
    fn read_marker(&mut self, marker: u8) -> Result<(), String> {
        match marker {
            0xC0 | 0xC1 => self.start_of_frame(false),
            0xC2 => self.start_of_frame(true),
            0xC9 | 0xCA => {
                Err("Sorry, there are legal restrictions on arithmetic coding".to_owned())
            }
            0xC3 | 0xC5..=0xC8 | 0xCB | 0xCD..=0xCF => {
                Err(format!("Unsupported JPEG process: SOF type {marker:#04x}"))
            }
            SOI => Err("Invalid JPEG file structure: two SOI markers".to_owned()),
            DHT => self.huffman_tables(),
            DQT => self.quantization_tables(),
            DRI => self.restart_interval(),
            APP0 | APP14 => self.interesting_app(marker),
            APP2 => self.saved_app(),
            0xE1 | 0xE3..=0xED | 0xEF | COM | 0xCC | DNL => self.skip_variable(),
            0xD0..=0xD7 | 0x01 => Ok(()),
            other => Err(format!("Unsupported marker type {other:#04x}")),
        }
    }

    fn skip_variable(&mut self) -> Result<(), String> {
        let length = self.read_u16()?;
        let skip = length.saturating_sub(2);
        if self.position + skip > self.data.len() {
            return Err("Premature end of JPEG file".to_owned());
        }
        self.position += skip;
        Ok(())
    }

    /// `get_interesting_appn`: JFIF in APP0 and Adobe in APP14.
    fn interesting_app(&mut self, marker: u8) -> Result<(), String> {
        let length = self.read_u16()?;
        let data_length = length.saturating_sub(2);
        let read = data_length.min(ICC_OVERHEAD);
        let mut data = [0u8; ICC_OVERHEAD];
        for byte in &mut data[..read] {
            *byte = self.read_byte()?;
        }
        if marker == APP0 && data_length >= 14 && data[..5] == *b"JFIF\0" {
            self.saw_jfif = true;
        }
        if marker == APP14 && data_length >= 12 && data[..5] == *b"Adobe" {
            self.adobe_transform = Some(data[11]);
        }
        let rest = data_length - read;
        if self.position + rest > self.data.len() {
            return Err("Premature end of JPEG file".to_owned());
        }
        self.position += rest;
        Ok(())
    }

    /// An APP2 marker, which the JDK saves for the ICC profile it may hold.
    fn saved_app(&mut self) -> Result<(), String> {
        let length = self.read_u16()?;
        let data_length = length.saturating_sub(2);
        let end = self.position + data_length;
        let data = self
            .data
            .get(self.position..end)
            .ok_or("Premature end of JPEG file")?;
        self.position = end;
        if self.in_headers {
            self.icc_markers.push(data.to_vec());
        }
        Ok(())
    }

    fn quantization_tables(&mut self) -> Result<(), String> {
        let mut length = self.read_u16()? as isize - 2;
        while length > 0 {
            let byte = self.read_byte()?;
            let precision = byte >> 4;
            let index = usize::from(byte & 0x0F);
            if index >= 4 {
                return Err(format!("Bogus DQT index {index}"));
            }
            let mut table = [0u16; DCTSIZE2];
            for &position in &NATURAL_ORDER[..DCTSIZE2] {
                table[position] = if precision != 0 {
                    self.read_u16()? as u16
                } else {
                    u16::from(self.read_byte()?)
                };
            }
            self.quant_tables[index] = Some(table);
            length -= DCTSIZE2 as isize + 1;
            if precision != 0 {
                length -= DCTSIZE2 as isize;
            }
        }
        if length != 0 {
            return Err("Bogus marker length".to_owned());
        }
        Ok(())
    }

    fn huffman_tables(&mut self) -> Result<(), String> {
        let mut length = self.read_u16()? as isize - 2;
        while length > 16 {
            let index = self.read_byte()?;
            let mut bits = [0u8; 17];
            let mut count = 0usize;
            for bit in &mut bits[1..] {
                *bit = self.read_byte()?;
                count += usize::from(*bit);
            }
            length -= 17;
            if count > 256 || count as isize > length {
                return Err("Bogus Huffman table definition".to_owned());
            }
            let mut values = [0u8; 256];
            for value in &mut values[..count] {
                *value = self.read_byte()?;
            }
            length -= count as isize;
            let (specs, slot) = if index & 0x10 != 0 {
                (&mut self.ac_specs, usize::from(index - 0x10))
            } else {
                (&mut self.dc_specs, usize::from(index))
            };
            if slot >= 4 {
                return Err(format!("Bogus DHT index {slot}"));
            }
            specs[slot] = Some(TableSpec { bits, values });
        }
        if length != 0 {
            return Err("Bogus marker length".to_owned());
        }
        Ok(())
    }

    fn restart_interval(&mut self) -> Result<(), String> {
        if self.read_u16()? != 4 {
            return Err("Bogus marker length".to_owned());
        }
        self.restart_interval = self.read_u16()?;
        Ok(())
    }

    /// `get_sof`, with the checks of `initial_setup`.
    fn start_of_frame(&mut self, progressive: bool) -> Result<(), String> {
        let length = self.read_u16()?;
        let precision = self.read_byte()?;
        let height = self.read_u16()?;
        let width = self.read_u16()?;
        let count = usize::from(self.read_byte()?);
        if self.frame.is_some() {
            return Err("Invalid JPEG file structure: two SOF markers".to_owned());
        }
        if height == 0 || width == 0 || count == 0 {
            return Err("Empty JPEG image (DNL not supported)".to_owned());
        }
        if length != count * 3 + 8 {
            return Err("Bogus marker length".to_owned());
        }
        let mut components = Vec::with_capacity(count);
        for _ in 0..count {
            let id = self.read_byte()?;
            let sampling = self.read_byte()?;
            let quant_table = usize::from(self.read_byte()?);
            components.push((
                id,
                usize::from(sampling >> 4),
                usize::from(sampling & 15),
                quant_table,
            ));
        }
        if width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(format!(
                "Maximum supported image dimension is {MAX_DIMENSION} pixels"
            ));
        }
        if precision != 8 {
            return Err(format!("Unsupported JPEG data precision {precision}"));
        }
        if count > MAX_COMPONENTS {
            return Err(format!(
                "Too many color components: {count}, max {MAX_COMPONENTS}"
            ));
        }
        if components.iter().any(|&(_, h, v, _)| {
            !(1..=MAX_SAMPLING).contains(&h) || !(1..=MAX_SAMPLING).contains(&v)
        }) {
            return Err("Bogus sampling factors".to_owned());
        }
        let max_h = components.iter().map(|&(_, h, _, _)| h).max().unwrap_or(1);
        let max_v = components.iter().map(|&(_, _, v, _)| v).max().unwrap_or(1);
        let components = components
            .into_iter()
            .map(|(id, h, v, quant_table)| {
                let width_in_blocks = (width * h).div_ceil(max_h * DCTSIZE);
                let height_in_blocks = (height * v).div_ceil(max_v * DCTSIZE);
                let blocks_per_row = width_in_blocks.next_multiple_of(h);
                let block_rows = height_in_blocks.next_multiple_of(v);
                Component {
                    id,
                    h,
                    v,
                    quant_table,
                    width_in_blocks,
                    height_in_blocks,
                    downsampled_width: (width * h).div_ceil(max_h),
                    downsampled_height: (height * v).div_ceil(max_v),
                    blocks_per_row,
                    quant: None,
                    coefficients: vec![[0; DCTSIZE2]; blocks_per_row * block_rows],
                    dc_table: 0,
                    ac_table: 0,
                    coef_bits: [-1; DCTSIZE2],
                }
            })
            .collect();
        self.frame = Some(Frame {
            progressive,
            width,
            height,
            max_h,
            max_v,
            components,
        });
        Ok(())
    }

    /// `get_sos`, and the checks the scan's decoder makes as it starts.
    fn start_of_scan(&mut self) -> Result<Scan, String> {
        let length = self.read_u16()?;
        let count = usize::from(self.read_byte()?);
        if length != count * 2 + 6 || count == 0 || count > MAX_COMPS_IN_SCAN {
            return Err("Bogus marker length".to_owned());
        }
        let frame = self.frame.as_mut().expect("a scan follows a frame");
        let mut components = Vec::with_capacity(count);
        for _ in 0..count {
            let id = self.data.get(self.position).copied();
            let tables = self.data.get(self.position + 1).copied();
            self.position += 2;
            let (Some(id), Some(tables)) = (id, tables) else {
                return Err("Premature end of JPEG file".to_owned());
            };
            let index = frame
                .components
                .iter()
                .position(|component| component.id == id)
                .ok_or_else(|| format!("Invalid component ID {id} in SOS"))?;
            frame.components[index].dc_table = usize::from(tables >> 4);
            frame.components[index].ac_table = usize::from(tables & 15);
            components.push(index);
        }
        let ss = usize::from(self.read_byte()?);
        let se = usize::from(self.read_byte()?);
        let approximation = self.read_byte()?;
        let scan = Scan {
            components,
            ss,
            se,
            ah: approximation >> 4,
            al: approximation & 15,
        };
        let frame = self.frame.as_mut().expect("a scan follows a frame");
        if scan.components.len() > 1 {
            let blocks: usize = scan
                .components
                .iter()
                .map(|&index| frame.components[index].h * frame.components[index].v)
                .sum();
            if blocks > MAX_BLOCKS_IN_MCU {
                return Err("Sampling factors too large for interleaved scan".to_owned());
            }
        }
        for &index in &scan.components {
            let component = &mut frame.components[index];
            if component.quant.is_none() {
                let table = self
                    .quant_tables
                    .get(component.quant_table)
                    .copied()
                    .flatten()
                    .ok_or_else(|| {
                        format!(
                            "Quantization table {:#04x} was not defined",
                            component.quant_table
                        )
                    })?;
                component.quant = Some(table);
            }
        }
        if frame.progressive {
            let dc = scan.ss == 0;
            let bad = if dc {
                scan.se != 0
            } else {
                scan.ss > scan.se || scan.se >= DCTSIZE2 || scan.components.len() != 1
            } || (scan.ah != 0 && scan.al != scan.ah - 1)
                || scan.al > 13;
            if bad {
                return Err(format!(
                    "Invalid progressive parameters Ss={} Se={} Ah={} Al={}",
                    scan.ss, scan.se, scan.ah, scan.al
                ));
            }
            for &index in &scan.components {
                let bits = &mut frame.components[index].coef_bits;
                for bit in &mut bits[scan.ss..=scan.se] {
                    *bit = i32::from(scan.al);
                }
            }
        }
        self.next_restart = 0;
        Ok(scan)
    }

    /// The ICC profile of the APP2 markers before the image, as the JDK's `read_icc_profile`
    /// assembles it, or none.
    fn icc_profile(&self) -> Result<Option<Vec<u8>>, String> {
        let markers: Vec<&Vec<u8>> = self
            .icc_markers
            .iter()
            .filter(|marker| marker.len() >= ICC_OVERHEAD && marker[..12] == *b"ICC_PROFILE\0")
            .collect();
        let Some(first) = markers.first() else {
            return Ok(None);
        };
        let count = usize::from(first[13]);
        let mut by_number: [Option<&Vec<u8>>; 256] = [None; 256];
        for marker in &markers {
            if usize::from(marker[13]) != count {
                return Err("Invalid icc profile: inconsistent num_markers fields".to_owned());
            }
            let number = usize::from(marker[12]);
            if number > count {
                return Err("Invalid icc profile: bad sequence number".to_owned());
            }
            if by_number[number].is_some() {
                return Err("Invalid icc profile: duplicate sequence numbers".to_owned());
            }
            by_number[number] = Some(marker);
        }
        if count == 0 {
            return Ok(None);
        }
        if count != markers.len() {
            return Err("Invalid icc profile: invalid number of icc markers".to_owned());
        }
        let first = usize::from(by_number[0].is_none());
        let mut profile = Vec::new();
        for marker in &by_number[first..first + count] {
            let marker = marker.ok_or("Invalid icc profile: missing sequence number")?;
            if marker.len() > MAX_ICC_MARKER_BYTES {
                return Err("Invalid icc profile: invalid data length".to_owned());
            }
            profile.extend_from_slice(&marker[ICC_OVERHEAD..]);
        }
        if profile.is_empty() {
            return Err("Invalid icc profile: found only empty markers".to_owned());
        }
        Ok(Some(profile))
    }

    /// The colour space of the samples: libjpeg's `default_decompress_parms`, adjusted as the
    /// JDK's `readImageHeader` adjusts it.
    fn color_space(&self) -> ColorSpace {
        let frame = self.frame.as_ref().expect("a scan follows a frame");
        let components = &frame.components;
        match components.len() {
            1 => ColorSpace::Grayscale,
            3 => {
                let ids = (components[0].id, components[1].id, components[2].id);
                let ijg = if self.saw_jfif {
                    ColorSpace::YCbCr
                } else if let Some(transform) = self.adobe_transform {
                    if transform == 0 {
                        ColorSpace::Rgb
                    } else {
                        ColorSpace::YCbCr
                    }
                } else if ids == (82, 71, 66) {
                    ColorSpace::Rgb
                } else {
                    ColorSpace::YCbCr
                };
                if ijg != ColorSpace::YCbCr {
                    return ijg;
                }
                if let Some(transform) = self.adobe_transform {
                    return if transform == 1 {
                        ColorSpace::YCbCr
                    } else {
                        ColorSpace::Unknown
                    };
                }
                let same_sampling = components[1..].iter().all(|component| {
                    component.h == components[0].h && component.v == components[0].v
                });
                if !self.saw_jfif && ids != (1, 2, 3) && same_sampling {
                    ColorSpace::Rgb
                } else {
                    ColorSpace::YCbCr
                }
            }
            4 => match self.adobe_transform {
                Some(0) | None => ColorSpace::Cmyk,
                Some(_) => ColorSpace::Ycck,
            },
            _ => ColorSpace::Unknown,
        }
    }

    /// Decodes a scan's entropy-coded data into the coefficients.
    fn decode_scan(&mut self, scan: &Scan) -> Result<(), String> {
        let mut frame = self.frame.take().expect("a scan follows a frame");
        let result = self.decode_scan_of(&mut frame, scan);
        self.frame = Some(frame);
        result
    }

    fn decode_scan_of(&mut self, frame: &mut Frame, scan: &Scan) -> Result<(), String> {
        let coding = if !frame.progressive {
            Coding::Sequential
        } else if scan.ss == 0 {
            if scan.ah == 0 {
                Coding::DcFirst
            } else {
                Coding::DcRefine
            }
        } else if scan.ah == 0 {
            Coding::AcFirst
        } else {
            Coding::AcRefine
        };
        let mut dc_tables = Vec::with_capacity(scan.components.len());
        let mut ac_tables = Vec::with_capacity(scan.components.len());
        for &index in &scan.components {
            let component = &frame.components[index];
            let derive = |specs: &[Option<TableSpec>; 4], number: usize, dc: bool| {
                specs
                    .get(number)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| format!("Huffman table {number:#04x} was not defined"))
                    .and_then(|spec| Huffman::derive(spec, dc))
            };
            let needs_dc = matches!(coding, Coding::Sequential | Coding::DcFirst);
            let needs_ac = matches!(
                coding,
                Coding::Sequential | Coding::AcFirst | Coding::AcRefine
            );
            dc_tables.push(if needs_dc {
                Some(derive(&self.dc_specs, component.dc_table, true)?)
            } else {
                None
            });
            ac_tables.push(if needs_ac {
                Some(derive(&self.ac_specs, component.ac_table, false)?)
            } else {
                None
            });
        }
        self.bits = 0;
        self.bits_left = 0;
        self.insufficient = false;
        let mut last_dc = vec![0i32; scan.components.len()];
        let mut eob_run = 0u32;
        let mut restarts_to_go = self.restart_interval;
        let (per_row, rows) = mcu_grid(frame, scan);
        let mut mcu = Vec::with_capacity(MAX_BLOCKS_IN_MCU);
        for (mcu_row, mcu_column) in
            (0..rows).flat_map(|row| (0..per_row).map(move |column| (row, column)))
        {
            mcu_blocks(frame, scan, mcu_row, mcu_column, &mut mcu);
            if self.restart_interval > 0 && restarts_to_go == 0 {
                self.process_restart();
                last_dc.iter_mut().for_each(|dc| *dc = 0);
                eob_run = 0;
                restarts_to_go = self.restart_interval;
            }
            if !self.insufficient {
                for &(slot, row, column) in &mcu {
                    let index = scan.components[slot];
                    let block = frame.components[index].block(row, column);
                    match coding {
                        Coding::Sequential => self.sequential_block(
                            block,
                            dc_tables[slot]
                                .as_ref()
                                .expect("a sequential scan has DC tables"),
                            ac_tables[slot]
                                .as_ref()
                                .expect("a sequential scan has AC tables"),
                            &mut last_dc[slot],
                        ),
                        Coding::DcFirst => self.dc_first(
                            block,
                            dc_tables[slot].as_ref().expect("a DC scan has DC tables"),
                            &mut last_dc[slot],
                            scan.al,
                        ),
                        Coding::DcRefine => self.dc_refine(block, scan.al),
                        Coding::AcFirst => self.ac_first(
                            block,
                            ac_tables[slot].as_ref().expect("an AC scan has AC tables"),
                            &mut eob_run,
                            scan,
                        ),
                        Coding::AcRefine => self.ac_refine(
                            block,
                            ac_tables[slot].as_ref().expect("an AC scan has AC tables"),
                            &mut eob_run,
                            scan,
                        ),
                    }
                }
            }
            restarts_to_go = restarts_to_go.wrapping_sub(1);
        }
        Ok(())
    }

    /// The next bit of entropy-coded data, or a zero where the data has run out before a
    /// marker, which marks the rest of the restart interval as insufficient.
    fn bit(&mut self) -> u32 {
        if self.bits_left == 0 {
            match self.data_byte() {
                Some(byte) => {
                    self.bits = u32::from(byte);
                    self.bits_left = 8;
                }
                None => {
                    self.insufficient = true;
                    return 0;
                }
            }
        }
        self.bits_left -= 1;
        (self.bits >> self.bits_left) & 1
    }

    fn get_bits(&mut self, count: u32) -> i32 {
        (0..count).fold(0, |value, _| (value << 1) | self.bit() as i32)
    }

    /// The next byte of entropy-coded data, unstuffed, or none at a marker, which is kept.
    fn data_byte(&mut self) -> Option<u8> {
        if self.unread_marker.is_some() {
            return None;
        }
        let Some(byte) = self.data.get(self.position).copied() else {
            self.unread_marker = Some(EOI);
            return None;
        };
        self.position += 1;
        if byte != 0xFF {
            return Some(byte);
        }
        loop {
            let Some(next) = self.data.get(self.position).copied() else {
                self.unread_marker = Some(EOI);
                return None;
            };
            self.position += 1;
            if next == 0 {
                return Some(0xFF);
            }
            if next != 0xFF {
                self.unread_marker = Some(next);
                return None;
            }
        }
    }

    /// `jpeg_huff_decode`, bit by bit.
    fn huffman(&mut self, table: &Huffman) -> i32 {
        let mut code = 0i32;
        let mut length = 0;
        loop {
            code = (code << 1) | self.bit() as i32;
            length += 1;
            if length > 16 {
                return 0;
            }
            if code <= table.max_code[length] {
                return i32::from(
                    table.values[(code + table.value_offset[length]) as usize & 0xFF],
                );
            }
        }
    }

    fn extended(&mut self, size: u32) -> i32 {
        if size == 0 {
            return 0;
        }
        let value = self.get_bits(size);
        if value < 1 << (size - 1) {
            value + ((-1i32) << size) + 1
        } else {
            value
        }
    }

    /// `process_restart`: the RST marker, resynchronized as `jpeg_resync_to_restart` does.
    fn process_restart(&mut self) {
        self.bits_left = 0;
        let marker = self.take_marker();
        let desired = self.next_restart;
        self.unread_marker = Some(marker);
        if marker == RST0 + desired {
            self.unread_marker = None;
        } else {
            self.resync(desired);
        }
        self.next_restart = (desired + 1) & 7;
        if self.unread_marker.is_none() {
            self.insufficient = false;
        }
    }

    /// `jpeg_resync_to_restart`: discards the marker when it is the RST wanted or too far from
    /// it, keeps it for the next segment when it is not an RST or is one of the next two, and
    /// looks for the next marker past an invalid one or one of the two before.
    fn resync(&mut self, desired: u8) {
        let restart = |offset: u8| RST0 + (desired.wrapping_add(offset) & 7);
        let mut marker = self.unread_marker.expect("a resync starts at a marker");
        loop {
            let advance = marker < SOF0 || marker == restart(7) || marker == restart(6);
            let keep = !(RST0..=RST0 + 7).contains(&marker)
                || marker == restart(1)
                || marker == restart(2);
            if advance && (marker < SOF0 || !keep) {
                marker = self.next_marker();
                self.unread_marker = Some(marker);
            } else if keep {
                return;
            } else {
                self.unread_marker = None;
                return;
            }
        }
    }

    /// A block of a sequential scan (`decode_mcu` of jdhuff.c).
    fn sequential_block(
        &mut self,
        block: &mut [i16; DCTSIZE2],
        dc_table: &Huffman,
        ac_table: &Huffman,
        last_dc: &mut i32,
    ) {
        let size = self.huffman(dc_table) as u32;
        let difference = self.extended(size);
        *last_dc = last_dc.wrapping_add(difference);
        block[0] = *last_dc as i16;
        let mut k = 1;
        while k < DCTSIZE2 {
            let symbol = self.huffman(ac_table);
            let run = (symbol >> 4) as usize;
            let size = (symbol & 15) as u32;
            if size != 0 {
                k += run;
                let value = self.extended(size);
                block[NATURAL_ORDER[k]] = value as i16;
            } else {
                if run != 15 {
                    break;
                }
                k += 15;
            }
            k += 1;
        }
    }

    fn dc_first(
        &mut self,
        block: &mut [i16; DCTSIZE2],
        table: &Huffman,
        last_dc: &mut i32,
        al: u8,
    ) {
        let size = self.huffman(table) as u32;
        let difference = self.extended(size);
        *last_dc = last_dc.wrapping_add(difference);
        block[0] = last_dc.wrapping_shl(u32::from(al)) as i16;
    }

    fn dc_refine(&mut self, block: &mut [i16; DCTSIZE2], al: u8) {
        if self.bit() != 0 {
            block[0] |= 1 << al;
        }
    }

    fn ac_first(
        &mut self,
        block: &mut [i16; DCTSIZE2],
        table: &Huffman,
        eob_run: &mut u32,
        scan: &Scan,
    ) {
        if *eob_run > 0 {
            *eob_run -= 1;
            return;
        }
        let mut k = scan.ss;
        while k <= scan.se {
            let symbol = self.huffman(table);
            let run = (symbol >> 4) as u32;
            let size = (symbol & 15) as u32;
            if size != 0 {
                k += run as usize;
                let value = self.extended(size);
                block[NATURAL_ORDER[k]] = value.wrapping_shl(u32::from(scan.al)) as i16;
            } else if run == 15 {
                k += 15;
            } else {
                *eob_run = 1 << run;
                if run != 0 {
                    *eob_run += self.get_bits(run) as u32;
                }
                *eob_run -= 1;
                break;
            }
            k += 1;
        }
    }

    fn ac_refine(
        &mut self,
        block: &mut [i16; DCTSIZE2],
        table: &Huffman,
        eob_run: &mut u32,
        scan: &Scan,
    ) {
        let p1 = 1i16 << scan.al;
        let m1 = (-1i16) << scan.al;
        let mut k = scan.ss;
        if *eob_run == 0 {
            while k <= scan.se {
                let symbol = self.huffman(table);
                let mut run = symbol >> 4;
                let size = symbol & 15;
                let mut value = 0i16;
                if size != 0 {
                    value = if self.bit() != 0 { p1 } else { m1 };
                } else if run != 15 {
                    *eob_run = 1 << run;
                    if run != 0 {
                        *eob_run += self.get_bits(run as u32) as u32;
                    }
                    break;
                }
                loop {
                    let coefficient = &mut block[NATURAL_ORDER[k]];
                    if *coefficient != 0 {
                        if self.bit() != 0 && (*coefficient & p1) == 0 {
                            *coefficient = if *coefficient >= 0 {
                                coefficient.wrapping_add(p1)
                            } else {
                                coefficient.wrapping_add(m1)
                            };
                        }
                    } else {
                        run -= 1;
                        if run < 0 {
                            break;
                        }
                    }
                    k += 1;
                    if k > scan.se {
                        break;
                    }
                }
                if value != 0 {
                    block[NATURAL_ORDER[k]] = value;
                }
                k += 1;
            }
        }
        if *eob_run > 0 {
            while k <= scan.se {
                let coefficient = &mut block[NATURAL_ORDER[k]];
                if *coefficient != 0 && self.bit() != 0 && (*coefficient & p1) == 0 {
                    *coefficient = if *coefficient >= 0 {
                        coefficient.wrapping_add(p1)
                    } else {
                        coefficient.wrapping_add(m1)
                    };
                }
                k += 1;
            }
            *eob_run -= 1;
        }
    }
}

/// The MCUs of a scan (`per_scan_setup`): a row's count and the count of rows. A scan of one
/// component codes its blocks one by one, an interleaved scan the blocks of every component
/// that cover the same part of the image.
fn mcu_grid(frame: &Frame, scan: &Scan) -> (usize, usize) {
    if let [index] = scan.components[..] {
        let component = &frame.components[index];
        return (component.width_in_blocks, component.height_in_blocks);
    }
    (
        frame.width.div_ceil(frame.max_h * DCTSIZE),
        frame.height.div_ceil(frame.max_v * DCTSIZE),
    )
}

/// The blocks of an MCU of a scan, in the order they are coded: the scan's component slot, the
/// block row and the block column.
fn mcu_blocks(
    frame: &Frame,
    scan: &Scan,
    mcu_row: usize,
    mcu_column: usize,
    blocks: &mut Vec<(usize, usize, usize)>,
) {
    blocks.clear();
    if scan.components.len() == 1 {
        blocks.push((0, mcu_row, mcu_column));
        return;
    }
    for (slot, &index) in scan.components.iter().enumerate() {
        let component = &frame.components[index];
        for y in 0..component.v {
            for x in 0..component.h {
                blocks.push((
                    slot,
                    mcu_row * component.v + y,
                    mcu_column * component.h + x,
                ));
            }
        }
    }
}

/// A component's samples after the inverse DCT: `stride` samples a row, of which the first
/// `width` of the first `height` rows are the image's.
struct Plane {
    samples: Vec<u8>,
    stride: usize,
    width: usize,
    height: usize,
}

impl Plane {
    fn at(&self, row: usize, column: usize) -> u8 {
        self.samples[row * self.stride + column]
    }
}

impl Frame {
    /// Each component's samples, inverse transformed block by block, smoothed first as libjpeg
    /// smooths a progressive image whose low coefficients are not fully known.
    fn planes(&self) -> Vec<Plane> {
        let smoothing = self.progressive && self.smoothing_useful();
        self.components
            .iter()
            .map(|component| {
                let quant = component.quant.unwrap_or([0; DCTSIZE2]);
                let stride = component.width_in_blocks * DCTSIZE;
                let mut samples = vec![0u8; stride * component.height_in_blocks * DCTSIZE];
                for row in 0..component.height_in_blocks {
                    for column in 0..component.width_in_blocks {
                        let mut block =
                            component.coefficients[row * component.blocks_per_row + column];
                        if smoothing {
                            smooth(component, &quant, row, column, &mut block);
                        }
                        idct_islow(
                            &block,
                            &quant,
                            &mut samples,
                            stride,
                            row * DCTSIZE,
                            column * DCTSIZE,
                        );
                    }
                }
                Plane {
                    samples,
                    stride,
                    width: component.downsampled_width,
                    height: component.downsampled_height,
                }
            })
            .collect()
    }

    /// `smoothing_ok`: whether block smoothing applies.
    fn smoothing_useful(&self) -> bool {
        let mut useful = false;
        for component in &self.components {
            let Some(quant) = component.quant else {
                return false;
            };
            if [0, 1, 8, 16, 9, 2]
                .iter()
                .any(|&position| quant[position] == 0)
            {
                return false;
            }
            if component.coef_bits[0] < 0 {
                return false;
            }
            if component.coef_bits[1..=5].iter().any(|&bits| bits != 0) {
                useful = true;
            }
        }
        useful
    }

    /// The image's samples in the colour space the JDK reads them in.
    /// The samples of the colour space libjpeg converts the image to, CMYK as the amounts of
    /// ink, inverted when an `adobe` marker says Photoshop wrote them.
    fn converted(&self, planes: &[Plane], space: ColorSpace, adobe: bool) -> Samples {
        let upsampled: Vec<Vec<u8>> = self
            .components
            .iter()
            .zip(planes)
            .map(|(component, plane)| self.upsampled(component, plane))
            .collect();
        let pixels = 0..self.width * self.height;
        let ink = |sample: u8| if adobe { 255 - sample } else { sample };
        match space {
            ColorSpace::Grayscale => {
                Samples::Gray(upsampled.into_iter().next().unwrap_or_default())
            }
            ColorSpace::Rgb => Samples::Rgb(
                pixels
                    .flat_map(|i| [upsampled[0][i], upsampled[1][i], upsampled[2][i]])
                    .collect(),
            ),
            ColorSpace::YCbCr | ColorSpace::Unknown => {
                let table = YccTable::new();
                Samples::Rgb(
                    pixels
                        .flat_map(|i| table.rgb(upsampled[0][i], upsampled[1][i], upsampled[2][i]))
                        .collect(),
                )
            }
            ColorSpace::Cmyk => Samples::Cmyk(
                pixels
                    .flat_map(|i| [0, 1, 2, 3].map(|component| ink(upsampled[component][i])))
                    .collect(),
            ),
            ColorSpace::Ycck => {
                let table = YccTable::new();
                Samples::Cmyk(
                    pixels
                        .flat_map(|i| {
                            let [c, m, y] = table
                                .rgb(upsampled[0][i], upsampled[1][i], upsampled[2][i])
                                .map(|level| ink(255 - level));
                            [c, m, y, ink(upsampled[3][i])]
                        })
                        .collect(),
                )
            }
        }
    }

    /// A component's samples at the image's size, as `jinit_upsampler`'s method for its
    /// sampling makes them.
    fn upsampled(&self, component: &Component, plane: &Plane) -> Vec<u8> {
        let (width, height) = (self.width, self.height);
        let mut out = vec![0u8; width * height];
        let h_expand = self.max_h / component.h;
        let v_expand = self.max_v / component.v;
        let fancy_width = plane.width > 2;
        if component.h == self.max_h && component.v == self.max_v {
            for y in 0..height {
                out[y * width..(y + 1) * width]
                    .copy_from_slice(&plane.samples[y * plane.stride..y * plane.stride + width]);
            }
        } else if component.h * 2 == self.max_h && component.v == self.max_v && fancy_width {
            for y in 0..height {
                let row = h2v1_fancy(plane, y);
                out[y * width..(y + 1) * width].copy_from_slice(&row[..width]);
            }
        } else if component.h * 2 == self.max_h && component.v * 2 == self.max_v && fancy_width {
            for y in 0..height {
                let nearest = y / 2;
                let further = if y % 2 == 0 {
                    nearest.saturating_sub(1)
                } else {
                    (nearest + 1).min(plane.height - 1)
                };
                let row = h2v2_fancy(plane, nearest, further);
                out[y * width..(y + 1) * width].copy_from_slice(&row[..width]);
            }
        } else {
            for y in 0..height {
                for x in 0..width {
                    out[y * width + x] = plane.at(y / v_expand, x / h_expand);
                }
            }
        }
        out
    }
}

/// `h2v1_fancy_upsample` of one row.
fn h2v1_fancy(plane: &Plane, row: usize) -> Vec<u8> {
    let input = &plane.samples[row * plane.stride..row * plane.stride + plane.width];
    let mut out = Vec::with_capacity(plane.width * 2);
    let first = i32::from(input[0]);
    out.push(input[0]);
    out.push(((first * 3 + i32::from(input[1]) + 2) >> 2) as u8);
    for x in 1..plane.width - 1 {
        let value = i32::from(input[x]) * 3;
        out.push(((value + i32::from(input[x - 1]) + 1) >> 2) as u8);
        out.push(((value + i32::from(input[x + 1]) + 2) >> 2) as u8);
    }
    let last = i32::from(input[plane.width - 1]);
    out.push(((last * 3 + i32::from(input[plane.width - 2]) + 1) >> 2) as u8);
    out.push(input[plane.width - 1]);
    out
}

/// `h2v2_fancy_upsample` of one output row: from the nearest input row and the further one,
/// above it for an even output row and below it for an odd one.
fn h2v2_fancy(plane: &Plane, nearest: usize, further: usize) -> Vec<u8> {
    const ROUND_LEFT: i32 = 8;
    const ROUND_RIGHT: i32 = 7;
    let column_sum =
        |x: usize| i32::from(plane.at(nearest, x)) * 3 + i32::from(plane.at(further, x));
    let mut out = Vec::with_capacity(plane.width * 2);
    let mut this = column_sum(0);
    let mut next = column_sum(1);
    out.push(((this * 4 + ROUND_LEFT) >> 4) as u8);
    out.push(((this * 3 + next + ROUND_RIGHT) >> 4) as u8);
    let mut last = this;
    this = next;
    for x in 2..plane.width {
        next = column_sum(x);
        out.push(((this * 3 + last + ROUND_LEFT) >> 4) as u8);
        out.push(((this * 3 + next + ROUND_RIGHT) >> 4) as u8);
        last = this;
        this = next;
    }
    out.push(((this * 3 + last + ROUND_LEFT) >> 4) as u8);
    out.push(((this * 4 + ROUND_RIGHT) >> 4) as u8);
    out
}

/// `build_ycc_rgb_table`.
struct YccTable {
    cr_r: [i32; 256],
    cb_b: [i32; 256],
    cr_g: [i32; 256],
    cb_g: [i32; 256],
}

impl YccTable {
    fn new() -> Self {
        const SCALEBITS: u32 = 16;
        const ONE_HALF: i32 = 1 << (SCALEBITS - 1);
        let fix = |x: f64| (x * f64::from(1u32 << SCALEBITS) + 0.5) as i32;
        let mut table = Self {
            cr_r: [0; 256],
            cb_b: [0; 256],
            cr_g: [0; 256],
            cb_g: [0; 256],
        };
        for i in 0..256 {
            let x = i as i32 - 128;
            table.cr_r[i] = (fix(1.40200) * x + ONE_HALF) >> SCALEBITS;
            table.cb_b[i] = (fix(1.77200) * x + ONE_HALF) >> SCALEBITS;
            table.cr_g[i] = -fix(0.71414) * x;
            table.cb_g[i] = -fix(0.34414) * x + ONE_HALF;
        }
        table
    }

    /// `ycc_rgb_convert` of one pixel.
    fn rgb(&self, y: u8, cb: u8, cr: u8) -> [u8; 3] {
        let y = i32::from(y);
        let (cb, cr) = (usize::from(cb), usize::from(cr));
        let limit = |value: i32| value.clamp(0, 255) as u8;
        [
            limit(y + self.cr_r[cr]),
            limit(y + ((self.cb_g[cb] + self.cr_g[cr]) >> 16)),
            limit(y + self.cb_b[cb]),
        ]
    }
}

/// libjpeg's post-IDCT range limit: the sample a descaled value stands for, wrapped as its
/// `RANGE_MASK` wraps it.
fn idct_limit(value: i32) -> u8 {
    let masked = (value & 1023) as usize;
    match masked {
        0..=127 => (masked + 128) as u8,
        128..=511 => 255,
        512..=895 => 0,
        _ => (masked - 896) as u8,
    }
}

/// `jpeg_idct_islow`: the accurate integer inverse DCT of a block, its 32-bit arithmetic
/// wrapping as the JDK's does.
fn idct_islow(
    block: &[i16; DCTSIZE2],
    quant: &[u16; DCTSIZE2],
    out: &mut [u8],
    stride: usize,
    top: usize,
    left: usize,
) {
    const CONST_BITS: u32 = 13;
    const PASS1_BITS: u32 = 2;
    const FIX_0_298631336: i32 = 2446;
    const FIX_0_390180644: i32 = 3196;
    const FIX_0_541196100: i32 = 4433;
    const FIX_0_765366865: i32 = 6270;
    const FIX_0_899976223: i32 = 7373;
    const FIX_1_175875602: i32 = 9633;
    const FIX_1_501321110: i32 = 12299;
    const FIX_1_847759065: i32 = 15137;
    const FIX_1_961570560: i32 = 16069;
    const FIX_2_053119869: i32 = 16819;
    const FIX_2_562915447: i32 = 20995;
    const FIX_3_072711026: i32 = 25172;
    let descale = |x: i32, n: u32| x.wrapping_add(1 << (n - 1)) >> n;
    let mul = |a: i32, b: i32| a.wrapping_mul(b);
    let mut workspace = [0i32; DCTSIZE2];
    for column in 0..DCTSIZE {
        let input = |row: usize| {
            mul(
                i32::from(block[row * DCTSIZE + column]),
                i32::from(quant[row * DCTSIZE + column]),
            )
        };
        if (1..DCTSIZE).all(|row| block[row * DCTSIZE + column] == 0) {
            let dc = input(0).wrapping_shl(PASS1_BITS);
            for row in 0..DCTSIZE {
                workspace[row * DCTSIZE + column] = dc;
            }
            continue;
        }
        let (z2, z3) = (input(2), input(6));
        let z1 = mul(z2.wrapping_add(z3), FIX_0_541196100);
        let tmp2 = z1.wrapping_add(mul(z3, -FIX_1_847759065));
        let tmp3 = z1.wrapping_add(mul(z2, FIX_0_765366865));
        let (z2, z3) = (input(0), input(4));
        let tmp0 = z2.wrapping_add(z3).wrapping_shl(CONST_BITS);
        let tmp1 = z2.wrapping_sub(z3).wrapping_shl(CONST_BITS);
        let even = [
            tmp0.wrapping_add(tmp3),
            tmp1.wrapping_add(tmp2),
            tmp1.wrapping_sub(tmp2),
            tmp0.wrapping_sub(tmp3),
        ];
        let odd = odd_part(input(7), input(5), input(3), input(1));
        for (i, (&e, &o)) in even.iter().zip(&odd).enumerate() {
            workspace[i * DCTSIZE + column] = descale(e.wrapping_add(o), CONST_BITS - PASS1_BITS);
            workspace[(7 - i) * DCTSIZE + column] =
                descale(e.wrapping_sub(o), CONST_BITS - PASS1_BITS);
        }
    }
    for row in 0..DCTSIZE {
        let w = &workspace[row * DCTSIZE..(row + 1) * DCTSIZE];
        let out_row = &mut out[(top + row) * stride + left..(top + row) * stride + left + DCTSIZE];
        if w[1..].iter().all(|&value| value == 0) {
            out_row.fill(idct_limit(descale(w[0], PASS1_BITS + 3)));
            continue;
        }
        let (z2, z3) = (w[2], w[6]);
        let z1 = mul(z2.wrapping_add(z3), FIX_0_541196100);
        let tmp2 = z1.wrapping_add(mul(z3, -FIX_1_847759065));
        let tmp3 = z1.wrapping_add(mul(z2, FIX_0_765366865));
        let tmp0 = w[0].wrapping_add(w[4]).wrapping_shl(CONST_BITS);
        let tmp1 = w[0].wrapping_sub(w[4]).wrapping_shl(CONST_BITS);
        let even = [
            tmp0.wrapping_add(tmp3),
            tmp1.wrapping_add(tmp2),
            tmp1.wrapping_sub(tmp2),
            tmp0.wrapping_sub(tmp3),
        ];
        let odd = odd_part(w[7], w[5], w[3], w[1]);
        for (i, (&e, &o)) in even.iter().zip(&odd).enumerate() {
            out_row[i] = idct_limit(descale(e.wrapping_add(o), CONST_BITS + PASS1_BITS + 3));
            out_row[7 - i] = idct_limit(descale(e.wrapping_sub(o), CONST_BITS + PASS1_BITS + 3));
        }
    }

    /// The odd part of a pass, from the inputs at positions 7, 5, 3 and 1: the terms each even
    /// term at positions 0 to 3 is combined with.
    fn odd_part(tmp0: i32, tmp1: i32, tmp2: i32, tmp3: i32) -> [i32; 4] {
        let mul = |a: i32, b: i32| a.wrapping_mul(b);
        let z1 = tmp0.wrapping_add(tmp3);
        let z2 = tmp1.wrapping_add(tmp2);
        let z3 = tmp0.wrapping_add(tmp2);
        let z4 = tmp1.wrapping_add(tmp3);
        let z5 = mul(z3.wrapping_add(z4), FIX_1_175875602);
        let tmp0 = mul(tmp0, FIX_0_298631336);
        let tmp1 = mul(tmp1, FIX_2_053119869);
        let tmp2 = mul(tmp2, FIX_3_072711026);
        let tmp3 = mul(tmp3, FIX_1_501321110);
        let z1 = mul(z1, -FIX_0_899976223);
        let z2 = mul(z2, -FIX_2_562915447);
        let z3 = mul(z3, -FIX_1_961570560).wrapping_add(z5);
        let z4 = mul(z4, -FIX_0_390180644).wrapping_add(z5);
        [
            tmp3.wrapping_add(z1).wrapping_add(z4),
            tmp2.wrapping_add(z2).wrapping_add(z3),
            tmp1.wrapping_add(z2).wrapping_add(z4),
            tmp0.wrapping_add(z1).wrapping_add(z3),
        ]
    }
}

/// `decompress_smooth_data`'s estimate of the low coefficients of a block of a progressive
/// image not yet fully known, from the DC values of the blocks around it.
fn smooth(
    component: &Component,
    quant: &[u16; DCTSIZE2],
    row: usize,
    column: usize,
    block: &mut [i16; DCTSIZE2],
) {
    let dc =
        |r: usize, c: usize| i32::from(component.coefficients[r * component.blocks_per_row + c][0]);
    let above = row.saturating_sub(1);
    let below = (row + 1).min(component.height_in_blocks - 1);
    let left = column.saturating_sub(1);
    let right = (column + 1).min(component.width_in_blocks - 1);
    let (dc1, dc2, dc3) = (dc(above, left), dc(above, column), dc(above, right));
    let (dc4, dc5, dc6) = (dc(row, left), dc(row, column), dc(row, right));
    let (dc7, dc8, dc9) = (dc(below, left), dc(below, column), dc(below, right));
    let q00 = i32::from(quant[0]);
    let estimates = [
        (1, 1, 36 * q00 * (dc4 - dc6)),
        (2, 8, 36 * q00 * (dc2 - dc8)),
        (3, 16, 9 * q00 * (dc2 + dc8 - 2 * dc5)),
        (4, 9, 5 * q00 * (dc1 - dc3 - dc7 + dc9)),
        (5, 2, 9 * q00 * (dc4 + dc6 - 2 * dc5)),
    ];
    for (bits_index, position, numerator) in estimates {
        let al = component.coef_bits[bits_index];
        if al == 0 || block[position] != 0 {
            continue;
        }
        let q = i32::from(quant[position]);
        let mut prediction = ((q << 7) + numerator.abs()) / (q << 8);
        if al > 0 && prediction >= 1 << al {
            prediction = (1 << al) - 1;
        }
        if numerator < 0 {
            prediction = -prediction;
        }
        block[position] = prediction as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_descaled_samples_as_libjpeg_does() {
        assert_eq!(idct_limit(0), 128);
        assert_eq!(idct_limit(127), 255);
        assert_eq!(idct_limit(300), 255);
        assert_eq!(idct_limit(-1), 127);
        assert_eq!(idct_limit(-128), 0);
        assert_eq!(idct_limit(-300), 0);
    }

    #[test]
    fn converts_ycbcr_as_libjpeg_does() {
        let table = YccTable::new();
        assert_eq!(table.rgb(128, 128, 128), [128, 128, 128]);
        assert_eq!(table.rgb(0, 0, 255), [178, 0, 0]);
    }

    /// Photoshop-style CMYK and YCCK JPEGs, inverted, decode to the ink libjpeg-turbo decodes
    /// for ImageMagick, written by `tests/fixtures/jpeg/generate.sh`.
    #[test]
    fn decodes_cmyk_and_ycck_to_their_ink() {
        for name in ["cmyk", "ycck"] {
            let input = std::fs::read(format!("tests/fixtures/jpeg/{name}.jpg")).unwrap();
            let expected = std::fs::read(format!("tests/fixtures/jpeg/{name}.ink")).unwrap();
            let decoded = decode(&input).unwrap();
            assert_eq!((decoded.width, decoded.height), (96, 80), "{name}");
            let Samples::Cmyk(ink) = decoded.samples else {
                panic!("{name} decodes to CMYK");
            };
            assert_eq!(ink, expected, "{name}");
        }
    }

    /// Three components the JDK's reader refuses for an Adobe transform other than 0 and 1
    /// decode as YCbCr, as libjpeg decodes them.
    #[test]
    fn decodes_three_components_of_another_transform_as_ycbcr() {
        let ycbcr = std::fs::read("tests/fixtures/jpeg/ycbcr.jpg").unwrap();
        let mut transformed = ycbcr[..2].to_vec();
        transformed.extend_from_slice(b"\xFF\xEE\x00\x0EAdobe\x00\x64\x00\x00\x00\x00\x02");
        transformed.extend_from_slice(&ycbcr[2..]);
        let samples = |input: &[u8]| match decode(input).unwrap().samples {
            Samples::Rgb(samples) => samples,
            other => panic!("decoded to {other:?}"),
        };
        assert_eq!(samples(&transformed), samples(&ycbcr));
    }
}
