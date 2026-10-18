//! junrar 8.1.1's unpackers of RAR 1.5 to 4 data (`Unpack15`, `Unpack20` and `Unpack`): one
//! state carried from entry to entry of an archive, as junrar's one `Unpack` carries it, so a
//! solid archive continues its window, tables and model. A stored entry is copied out and
//! leaves that state alone, as unrar copies it, where junrar runs the unpacker of its version
//! over the stale buffer after it (#122).
//!
//! Where Java would throw (an index out of an array's bounds, a null array, a division by
//! zero) the port returns [`Thrown`], and the entry's data ends there.

mod bits;
mod filters;
mod huffman;
mod io;
mod ppm;
mod v15;
mod v20;
mod v29;
mod vm;

use bits::BitInput;
use huffman::Decode;
use io::{DataIo, Input};
use ppm::Model;
use vm::{Filter, Vm, VM_FIXEDGLOBALSIZE, VM_GLOBALMEMADDR, VM_GLOBALMEMSIZE};

/// A Java exception junrar's extraction throws, which ends the entry's stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Thrown(pub(super) &'static str);

const OUT_OF_BOUNDS: Thrown = Thrown("an ArrayIndexOutOfBoundsException");
const NULL: Thrown = Thrown("a NullPointerException, an unpacker state junrar has not made yet");

const MAX_WIN_SIZE: i32 = 0x40_0000;
const MAX_WIN_MASK: i32 = MAX_WIN_SIZE - 1;
const LOW_DIST_REP_COUNT: i32 = 16;
const NC: usize = 299;
const DC: usize = 60;
const LDC: usize = 17;
const RC: usize = 28;
const HUFF_TABLE_SIZE: usize = NC + DC + RC + LDC;
const BC: usize = 20;
const NC20: usize = 298;
const DC20: usize = 48;
const RC20: usize = 28;
const BC20: usize = 19;
const MC20: usize = 257;
const MAX3_UNPACK_FILTERS: usize = 8192;

/// `Unpack20.LDecode` and `LBits`: the lengths of the length slots.
const L_DECODE: [i32; 28] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128,
    160, 192, 224,
];
const L_BITS: [i32; 28] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5,
];
/// `Unpack20.SDDecode` and `SDBits`: the short distances.
const SD_DECODE: [i32; 8] = [0, 4, 8, 16, 32, 64, 128, 192];
const SD_BITS: [i32; 8] = [2, 2, 3, 4, 5, 6, 6, 6];
/// `Unpack.DBitLengthCounts`: how many of `unpack29`'s distance slots have each bit length.
const D_BIT_LENGTH_COUNTS: [i32; 19] = [4, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 14, 0, 12];

/// `unpack29`'s distance slots: their first distances and extra bits.
const DISTANCES: ([i32; DC], [i32; DC]) = {
    let mut decode = [0; DC];
    let mut bits = [0; DC];
    let (mut dist, mut slot, mut bit_length) = (0, 0, 0);
    while bit_length < D_BIT_LENGTH_COUNTS.len() {
        let mut j = 0;
        while j < D_BIT_LENGTH_COUNTS[bit_length] {
            decode[slot] = dist;
            bits[slot] = bit_length as i32;
            slot += 1;
            dist += 1 << bit_length;
            j += 1;
        }
        bit_length += 1;
    }
    (decode, bits)
};

/// `table[index]`, failing as Java does outside the table.
fn at(table: &[i32], index: i32) -> Result<i32, Thrown> {
    usize::try_from(index)
        .ok()
        .and_then(|index| table.get(index))
        .copied()
        .ok_or(OUT_OF_BOUNDS)
}

/// Which of the audio predictors' summed differences is the least, the first of them on a
/// tie, all of them cleared, as the audio decoders pick the coefficient to adapt.
fn least_dif<T: Copy + Default + PartialOrd>(dif: &mut [T]) -> usize {
    let mut least = 0;
    let mut min_dif = dif[0];
    for (i, value) in dif.iter_mut().enumerate().skip(1) {
        if *value < min_dif {
            min_dif = *value;
            least = i;
        }
        *value = T::default();
    }
    dif[0] = T::default();
    least
}

/// The unpacker of the RAR version an entry is compressed for, as unrar's `DoUnpack` picks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Method {
    /// RAR 1.3 to 1.5.
    V15,
    /// RAR 2, and 2.6 for files larger than 2 GB.
    V20,
    /// RAR 2.9 to 4.
    V29,
}

impl Method {
    /// The unpacker of the version `version`, none for one unrar does not unpack: junrar writes
    /// nothing for one it does not know, and throws for version 36.
    pub(super) fn of(version: i32) -> Option<Self> {
        match version {
            13..=15 => Some(Method::V15),
            20 | 26 => Some(Method::V20),
            29 => Some(Method::V29),
            _ => None,
        }
    }
}

/// `BlockTypes`: how the current block of RAR 3 data is coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockType {
    Lz,
    Ppm,
}

/// `AudioVariables`: the state of one channel of RAR 2's audio coding.
#[derive(Debug, Clone, Copy, Default)]
struct AudioVariables {
    k1: i32,
    k2: i32,
    k3: i32,
    k4: i32,
    k5: i32,
    d1: i32,
    d2: i32,
    d3: i32,
    d4: i32,
    last_delta: i32,
    dif: [i32; 11],
    byte_count: i32,
    last_char: i32,
}

/// junrar's `Unpack`, with the state of the `Unpack15` and `Unpack20` it extends.
pub(super) struct Unpack<'a> {
    input: Input<'a>,
    window: Option<Box<[u8]>>,
    dest_unp_size: i64,
    old_dist: [i32; 4],
    unp_ptr: i32,
    wr_ptr: i32,
    first_win_done: bool,
    prev_ptr: i32,
    old_dist_ptr: i32,
    last_dist: i32,
    last_length: i32,
    v15: v15::State,
    md: [Option<Decode>; 4],
    unp_old_table20: [u8; MC20 * 4],
    unp_audio_block: i32,
    unp_channels: i32,
    unp_cur_channel: i32,
    unp_channel_delta: i32,
    aud_v: [Option<AudioVariables>; 4],
    ld: Decode,
    dd: Decode,
    ldd: Decode,
    rd: Decode,
    bd: Decode,
    ppm: Model,
    ppm_esc_char: i32,
    vm: Vm,
    filters: Vec<Filter>,
    prg_stack: Vec<Option<Filter>>,
    old_filter_lengths: Vec<i32>,
    last_filter: i32,
    tables_read: bool,
    unp_old_table: [u8; HUFF_TABLE_SIZE],
    unp_block_type: BlockType,
    written_file_size: i64,
    ppm_error: bool,
    prev_low_dist: i32,
    low_dist_rep_count: i32,
}

impl<'a> Unpack<'a> {
    pub(super) fn new(archive: &'a [u8]) -> Self {
        Unpack {
            input: Input {
                bits: BitInput::new(),
                read_top: 0,
                read_border: 0,
                io: DataIo::new(archive),
            },
            window: None,
            dest_unp_size: 0,
            old_dist: [0; 4],
            unp_ptr: 0,
            wr_ptr: 0,
            first_win_done: false,
            prev_ptr: 0,
            old_dist_ptr: 0,
            last_dist: 0,
            last_length: 0,
            v15: v15::State::default(),
            md: [None, None, None, None],
            unp_old_table20: [0; MC20 * 4],
            unp_audio_block: 0,
            unp_channels: 0,
            unp_cur_channel: 0,
            unp_channel_delta: 0,
            aud_v: [None; 4],
            ld: Decode::new(NC),
            dd: Decode::new(DC),
            ldd: Decode::new(LDC),
            rd: Decode::new(RC),
            bd: Decode::new(BC),
            ppm: Model::new(),
            ppm_esc_char: 0,
            vm: Vm::new(),
            filters: Vec::new(),
            prg_stack: Vec::new(),
            old_filter_lengths: Vec::new(),
            last_filter: 0,
            tables_read: false,
            unp_old_table: [0; HUFF_TABLE_SIZE],
            unp_block_type: BlockType::Lz,
            written_file_size: 0,
            ppm_error: false,
            prev_low_dist: 0,
            low_dist_rep_count: 0,
        }
    }

    /// `init(null)`: a fresh window, before an entry that does not continue a solid stream.
    pub(super) fn init(&mut self) {
        self.window = Some(vec![0; MAX_WIN_SIZE as usize].into_boxed_slice());
        self.input.bits.in_addr = 0;
        self.unp_init_data(false);
    }

    /// `setDestSize`.
    pub(super) fn set_dest_size(&mut self, size: i64) {
        self.dest_unp_size = size;
    }

    /// `ComprDataIO.init`: an entry's packed data of `packed` bytes from `start` on.
    pub(super) fn start_entry(&mut self, start: u64, packed: i64) {
        self.input.io.start(start, packed);
    }

    /// The CRC-32 of what the entry wrote.
    pub(super) fn crc(&self) -> u32 {
        self.input.io.crc()
    }

    /// What the entry unpacked into.
    pub(super) fn take_data(&mut self) -> Result<crate::input::Input<'static>, Thrown> {
        self.input.io.take_data()
    }

    /// `doUnpack` of the data of an entry compressed by `method`.
    pub(super) fn do_unpack(&mut self, method: Method, solid: bool) -> Result<(), Thrown> {
        match method {
            Method::V15 => self.unpack15(solid),
            Method::V20 => self.unpack20(solid),
            Method::V29 => self.unpack29(solid),
        }
    }

    /// `UnstoreFile`: the `size` bytes of a stored entry copied out.
    pub(super) fn unstore(&mut self, size: i64) -> Result<(), Thrown> {
        self.dest_unp_size = size;
        self.unstore_file()
    }

    fn unstore_file(&mut self) -> Result<(), Thrown> {
        let mut buffer = vec![0u8; 0x10000];
        loop {
            let count = (buffer.len() as i64).min(self.dest_unp_size) as i32;
            let mut code = self.input.io.read(&mut buffer, 0, count)?;
            if code == 0 || code == -1 {
                return Ok(());
            }
            if i64::from(code) >= self.dest_unp_size {
                code = self.dest_unp_size as i32;
            }
            self.input.io.write(&buffer, 0, code)?;
            if self.dest_unp_size >= 0 {
                self.dest_unp_size -= i64::from(code);
            }
        }
    }

    fn window(&mut self) -> Result<&mut [u8], Thrown> {
        self.window.as_deref_mut().ok_or(NULL)
    }

    fn getbits(&self) -> Result<i32, Thrown> {
        self.input.bits.getbits()
    }

    fn addbits(&mut self, bits: i32) {
        self.input.bits.addbits(bits);
    }

    fn unp_init_data(&mut self, solid: bool) {
        if !solid {
            self.tables_read = false;
            self.old_dist = [0; 4];
            self.old_dist_ptr = 0;
            self.last_dist = 0;
            self.last_length = 0;
            self.unp_old_table = [0; HUFF_TABLE_SIZE];
            self.unp_ptr = 0;
            self.wr_ptr = 0;
            self.ppm_esc_char = 2;
            self.first_win_done = false;
            self.prev_ptr = 0;
            self.init_filters();
        }
        self.input.bits.init();
        self.ppm_error = false;
        self.written_file_size = 0;
        self.input.read_top = 0;
        self.input.read_border = 0;
        self.unp_init_data20(solid);
    }

    fn init_filters(&mut self) {
        self.old_filter_lengths.clear();
        self.last_filter = 0;
        self.filters.clear();
        self.prg_stack.clear();
    }
}
