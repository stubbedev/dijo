//! LZX, as cabinets ([MS-CAB]) and Compiled HTML Help store it ([MS-PATCH] 2 without the LZX
//! DELTA extensions), decoded a frame at a time as libmspack's `lzxd.c` decodes it: E8 call
//! translation as it translates calls, an operand of 0 included, every match offset and length
//! checked against what was decoded, and a window allocated once per decoder, which a reset
//! keeps.

use std::fmt;

/// The bytes a frame decodes into, all but the last frame of a stream.
pub const FRAME: usize = 32 * 1024;

const LITERALS: usize = 256;
const MIN_MATCH: usize = 2;
const PRIMARY_LENGTHS: usize = 7;
const SECONDARY_LENGTHS: usize = 249;
const PRETREE_SYMBOLS: usize = 20;
const ALIGNED_SYMBOLS: usize = 8;
const MAX_CODE: u32 = 16;
const MIN_WINDOW_BITS: u32 = 15;
const MAX_WINDOW_BITS: u32 = 21;
/// The position slots of each window size, from 2^15 bytes.
const POSITION_SLOTS: [usize; 7] = [30, 32, 34, 36, 38, 42, 50];
const MAX_POSITION_SLOTS: usize = 50;
/// The frames whose calls are translated: those of the first GiB of a stream.
const E8_FRAMES: u32 = 32_768;
/// The bytes at the end of a frame in which a call is not translated.
const E8_TAIL: usize = 10;
const E8: u8 = 0xE8;

/// The bits of a match offset that follow its position slot.
const fn extra_bits(slot: usize) -> u32 {
    if slot < 4 {
        0
    } else if slot < 36 {
        (slot / 2 - 1) as u32
    } else {
        17
    }
}

/// The smallest formatted offset of each position slot.
const POSITION_BASE: [u32; MAX_POSITION_SLOTS] = {
    let mut base = [0; MAX_POSITION_SLOTS];
    let mut slot = 1;
    while slot < MAX_POSITION_SLOTS {
        base[slot] = base[slot - 1] + (1 << extra_bits(slot - 1));
        slot += 1;
    }
    base
};

/// The size of an LZX window, a power of two from 2^15 to 2^21 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize(u32);

impl WindowSize {
    /// The window of 2^`bits` bytes, if LZX allows it.
    pub fn from_bits(bits: u32) -> Option<Self> {
        (MIN_WINDOW_BITS..=MAX_WINDOW_BITS)
            .contains(&bits)
            .then_some(Self(bits))
    }

    /// The window of `bytes` bytes, if LZX allows it.
    pub fn from_bytes(bytes: u64) -> Option<Self> {
        if !bytes.is_power_of_two() {
            return None;
        }
        Self::from_bits(bytes.trailing_zeros())
    }

    /// Its size in bytes.
    pub fn bytes(self) -> usize {
        1 << self.0
    }

    fn position_slots(self) -> usize {
        POSITION_SLOTS[(self.0 - MIN_WINDOW_BITS) as usize]
    }
}

/// A Huffman tree of an LZX stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tree {
    Pretree,
    Main,
    Length,
    AlignedOffset,
}

impl fmt::Display for Tree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Tree::Pretree => "pretree",
            Tree::Main => "main tree",
            Tree::Length => "length tree",
            Tree::AlignedOffset => "aligned offset tree",
        })
    }
}

/// Why an LZX stream does not decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Malformed {
    /// A frame's data, of this many bytes, ends before the frame is decoded.
    InputEnded(usize),
    /// A frame of this many bytes, more than [`FRAME`].
    FrameTooLong(usize),
    /// A frame after one of fewer than [`FRAME`] bytes, which only the last frame may be.
    AfterLastFrame,
    /// A block of this type, which LZX does not define.
    BlockType(u32),
    /// The code lengths of this tree make no complete prefix code.
    Incomplete(Tree),
    /// A run of code lengths of this tree runs past its last symbol.
    LengthsOverrun(Tree),
    /// This pretree symbol, where symbol 19 must be followed by a change of 0 to 16.
    RunSymbol(u16),
    /// A match needs the length tree, which its block leaves empty.
    EmptyLengthTree,
    /// An uncompressed block repeats an offset as large as the window or larger.
    RepeatedOffset { offset: u32, window: usize },
    /// A match reaches back `offset` bytes, where only `available` were decoded in the window.
    MatchOffset { offset: u32, available: usize },
    /// A match of `length` bytes, where its block has only `remaining` bytes left.
    PastBlock { length: usize, remaining: u32 },
    /// A match of `length` bytes, where its frame has only `remaining` bytes left.
    PastFrame { length: usize, remaining: usize },
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Malformed::InputEnded(length) => {
                write!(
                    f,
                    "the frame's {length} bytes of data end before the frame does"
                )
            }
            Malformed::FrameTooLong(length) => write!(
                f,
                "a frame of {length} bytes, where a frame holds at most {FRAME}"
            ),
            Malformed::AfterLastFrame => write!(
                f,
                "a frame follows one of fewer than {FRAME} bytes, which only the last may be"
            ),
            Malformed::BlockType(kind) => {
                write!(f, "a block of type {kind}, which LZX does not define")
            }
            Malformed::Incomplete(tree) => write!(
                f,
                "the code lengths of the {tree} make no complete prefix code"
            ),
            Malformed::LengthsOverrun(tree) => write!(
                f,
                "a run of code lengths of the {tree} runs past its last symbol"
            ),
            Malformed::RunSymbol(symbol) => write!(
                f,
                "the pretree symbol {symbol} follows symbol 19, where a change of 0 to 16 must"
            ),
            Malformed::EmptyLengthTree => {
                write!(
                    f,
                    "a match needs the length tree, which its block leaves empty"
                )
            }
            Malformed::RepeatedOffset { offset, window } => write!(
                f,
                "an uncompressed block repeats an offset of {offset} bytes, where the window \
                 holds {window}"
            ),
            Malformed::MatchOffset { offset, available } => write!(
                f,
                "a match reaches {offset} bytes back, where 1 to {available} bytes back are \
                 decoded"
            ),
            Malformed::PastBlock { length, remaining } => write!(
                f,
                "a match of {length} bytes, where its block has {remaining} bytes left"
            ),
            Malformed::PastFrame { length, remaining } => write!(
                f,
                "a match of {length} bytes, where its frame has {remaining} bytes left"
            ),
        }
    }
}

impl std::error::Error for Malformed {}

/// LZX's bitstream: 16-bit little-endian words, each read from its most significant bit, with
/// the bits of up to two words at the top of `buffer`. Past the end of the data it reads zeros,
/// so that a code at the end can be looked up, but fails as soon as one of them is taken.
struct Bits<'a> {
    input: &'a [u8],
    at: usize,
    buffer: u32,
    left: u32,
    padded: u32,
}

impl<'a> Bits<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            at: 0,
            buffer: 0,
            left: 0,
            padded: 0,
        }
    }

    fn ended(&self) -> Malformed {
        Malformed::InputEnded(self.input.len())
    }

    /// Fills the buffer to at least `count` bits, at most 17.
    fn ensure(&mut self, count: u32) {
        while self.left < count {
            let word = match self.input.get(self.at..self.at + 2) {
                Some(&[low, high]) => {
                    self.at += 2;
                    u16::from_le_bytes([low, high])
                }
                _ => {
                    self.padded += 1;
                    0
                }
            };
            self.buffer |= u32::from(word) << (16 - self.left);
            self.left += 16;
        }
    }

    /// The next `count` bits, 1 to 17, which the buffer holds.
    fn peek(&self, count: u32) -> u32 {
        self.buffer >> (32 - count)
    }

    /// Takes `count` bits the buffer holds, failing where they run past the data.
    fn remove(&mut self, count: u32) -> Result<(), Malformed> {
        self.buffer <<= count;
        self.left -= count;
        if self.padded > 0 && self.left < 16 * self.padded {
            return Err(self.ended());
        }
        Ok(())
    }

    /// The next `count` bits, 0 to 17.
    fn read(&mut self, count: u32) -> Result<u32, Malformed> {
        if count == 0 {
            return Ok(0);
        }
        self.ensure(count);
        let bits = self.peek(count);
        self.remove(count)?;
        Ok(bits)
    }

    /// Drops the bits left of the word being read, or skips the next word where none are left,
    /// as an uncompressed block's header does, after which bytes are read whole.
    fn align(&mut self) -> Result<(), Malformed> {
        if self.left == 0 {
            self.raw(2)?;
        }
        self.left = 0;
        self.buffer = 0;
        Ok(())
    }

    /// The next `count` bytes, read whole after [`Bits::align`].
    fn raw(&mut self, count: usize) -> Result<&'a [u8], Malformed> {
        let input = self.input;
        let bytes = input
            .get(self.at..self.at + count)
            .ok_or(Malformed::InputEnded(input.len()))?;
        self.at += count;
        Ok(bytes)
    }

    /// Whether a byte is left to read whole.
    fn has_byte(&self) -> bool {
        self.at < self.input.len()
    }
}

/// An entry of a lookup table that leads to a code longer than the table.
const LONG: u16 = u16::MAX;

/// A canonical Huffman code, by its code lengths: a table looked up by the code's first
/// `table_bits` bits, and for longer codes the symbols in code order.
struct Huffman {
    tree: Tree,
    lengths: Box<[u8]>,
    table_bits: u32,
    /// The symbol and length of each code no longer than the table, `symbol << 5 | length`.
    table: Box<[u16]>,
    sorted: Box<[u16]>,
    count: [u16; MAX_CODE as usize + 1],
    first: [u32; MAX_CODE as usize + 1],
    offset: [u16; MAX_CODE as usize + 1],
    empty: bool,
}

impl Huffman {
    fn new(tree: Tree, symbols: usize, table_bits: u32) -> Self {
        Self {
            tree,
            lengths: vec![0; symbols].into_boxed_slice(),
            table_bits,
            table: vec![LONG; 1 << table_bits].into_boxed_slice(),
            sorted: vec![0; symbols].into_boxed_slice(),
            count: [0; MAX_CODE as usize + 1],
            first: [0; MAX_CODE as usize + 1],
            offset: [0; MAX_CODE as usize + 1],
            empty: true,
        }
    }

    /// Builds the code of the lengths, which must make a complete prefix code, or for the
    /// length tree may all be 0.
    fn build(&mut self) -> Result<(), Malformed> {
        self.count = [0; MAX_CODE as usize + 1];
        for &length in self.lengths.iter() {
            self.count[usize::from(length)] += 1;
        }
        self.empty = usize::from(self.count[0]) == self.lengths.len();
        if self.empty && self.tree == Tree::Length {
            return Ok(());
        }
        let incomplete = Malformed::Incomplete(self.tree);
        let mut left = 1i32;
        for length in 1..=MAX_CODE as usize {
            left = 2 * left - i32::from(self.count[length]);
            if left < 0 {
                return Err(incomplete);
            }
        }
        if left != 0 {
            return Err(incomplete);
        }
        let (mut code, mut index) = (0u32, 0u16);
        for length in 1..=MAX_CODE as usize {
            self.first[length] = code;
            self.offset[length] = index;
            code = (code + u32::from(self.count[length])) << 1;
            index += self.count[length];
        }
        let mut next = self.offset;
        for (symbol, &length) in self.lengths.iter().enumerate() {
            if length > 0 {
                self.sorted[usize::from(next[usize::from(length)])] = symbol as u16;
                next[usize::from(length)] += 1;
            }
        }
        self.table.fill(LONG);
        for length in 1..=self.table_bits {
            let shift = self.table_bits - length;
            let at = length as usize;
            for index in 0..u32::from(self.count[at]) {
                let symbol = self.sorted[usize::from(self.offset[at]) + index as usize];
                let start = ((self.first[at] + index) << shift) as usize;
                self.table[start..start + (1 << shift)].fill(symbol << 5 | length as u16);
            }
        }
        Ok(())
    }

    /// The next symbol of `bits`.
    fn decode(&self, bits: &mut Bits) -> Result<u16, Malformed> {
        bits.ensure(MAX_CODE);
        let entry = self.table[bits.peek(self.table_bits) as usize];
        if entry != LONG {
            bits.remove(u32::from(entry & 0x1F))?;
            return Ok(entry >> 5);
        }
        let code = bits.peek(MAX_CODE);
        for length in self.table_bits + 1..=MAX_CODE {
            let at = length as usize;
            let index = (code >> (MAX_CODE - length)).wrapping_sub(self.first[at]);
            if index < u32::from(self.count[at]) {
                bits.remove(length)?;
                return Ok(self.sorted[usize::from(self.offset[at]) + index as usize]);
            }
        }
        Err(Malformed::Incomplete(self.tree))
    }
}

/// Reads the code lengths `lengths` of `tree` as changes from the lengths before, coded by a
/// pretree read first.
fn read_lengths(
    pretree: &mut Huffman,
    lengths: &mut [u8],
    tree: Tree,
    bits: &mut Bits,
) -> Result<(), Malformed> {
    for length in pretree.lengths.iter_mut() {
        *length = bits.read(4)? as u8;
    }
    pretree.build()?;
    let changed = |previous: u8, change: u16| ((17 + u16::from(previous) - change) % 17) as u8;
    let mut at = 0;
    while at < lengths.len() {
        let (run, length) = match pretree.decode(bits)? {
            17 => (bits.read(4)? + 4, 0),
            18 => (bits.read(5)? + 20, 0),
            19 => {
                let run = bits.read(1)? + 4;
                let change = pretree.decode(bits)?;
                if change > 16 {
                    return Err(Malformed::RunSymbol(change));
                }
                (run, changed(lengths[at], change))
            }
            change => (1, changed(lengths[at], change)),
        };
        let run = run as usize;
        lengths
            .get_mut(at..at + run)
            .ok_or(Malformed::LengthsOverrun(tree))?
            .fill(length);
        at += run;
    }
    Ok(())
}

/// Turns the absolute targets of the E8 calls of `frame`, which starts `offset` bytes into
/// its stream, back into the relative ones they were, as libmspack does: an operand from
/// `-offset` up to `translation`, 0 included, is translated, and no call in the last
/// [`E8_TAIL`] bytes.
fn translate_calls(frame: &mut [u8], offset: i32, translation: i32) {
    let end = frame.len().saturating_sub(E8_TAIL);
    let mut at = 0;
    while at < end {
        if frame[at] != E8 {
            at += 1;
            continue;
        }
        let current = offset + at as i32;
        let operand = &mut frame[at + 1..at + 5];
        let absolute = i32::from_le_bytes([operand[0], operand[1], operand[2], operand[3]]);
        if absolute >= -current && absolute < translation {
            let relative = if absolute >= 0 {
                absolute.wrapping_sub(current)
            } else {
                absolute.wrapping_add(translation)
            };
            operand.copy_from_slice(&relative.to_le_bytes());
        }
        at += 5;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Verbatim,
    AlignedOffset,
    /// An uncompressed block, `odd` where it holds an odd number of bytes.
    Uncompressed {
        odd: bool,
    },
}

/// The block being decoded, which ends with its last byte decoded.
#[derive(Debug, Clone, Copy)]
struct Block {
    kind: Kind,
    remaining: u32,
}

/// Where a decoded frame is.
enum Decoded {
    /// In the window, from here.
    Window(usize),
    /// In the buffer of translated calls.
    Translated,
}

/// The decoder of an LZX stream, which decodes its frames in order.
pub struct Lzx {
    window: Box<[u8]>,
    main: Huffman,
    length: Huffman,
    aligned: Huffman,
    pretree: Huffman,
    /// The last frame with its calls translated, where the stream translates them.
    translated: Box<[u8]>,
    state: State,
}

/// What a decoder holds of the stream it decodes, besides its window and trees.
struct State {
    repeated: [u32; 3],
    block: Block,
    /// Whether an uncompressed block of an odd number of bytes ended, which a byte pads.
    pad: bool,
    header_read: bool,
    /// The E8 translation size, 0 where the stream translates no calls.
    translation: i32,
    position: usize,
    frame_start: usize,
    frames: u32,
    /// The bytes of the frames decoded.
    decoded: u64,
    /// Whether the last frame decoded was shorter than [`FRAME`], so the stream ended.
    ended: bool,
    /// Why the stream failed to decode, which every frame after fails with.
    failed: Option<Malformed>,
}

impl State {
    /// The state at the start of a stream.
    fn start() -> Self {
        Self {
            repeated: [1; 3],
            block: Block {
                kind: Kind::Verbatim,
                remaining: 0,
            },
            pad: false,
            header_read: false,
            translation: 0,
            position: 0,
            frame_start: 0,
            frames: 0,
            decoded: 0,
            ended: false,
            failed: None,
        }
    }
}

impl Lzx {
    /// The decoder of a stream of `window`.
    pub fn new(window: WindowSize) -> Self {
        Self {
            window: vec![0; window.bytes()].into_boxed_slice(),
            main: Huffman::new(Tree::Main, LITERALS + 8 * window.position_slots(), 12),
            length: Huffman::new(Tree::Length, SECONDARY_LENGTHS, 12),
            aligned: Huffman::new(Tree::AlignedOffset, ALIGNED_SYMBOLS, 7),
            pretree: Huffman::new(Tree::Pretree, PRETREE_SYMBOLS, 6),
            translated: vec![0; FRAME].into_boxed_slice(),
            state: State::start(),
        }
    }

    /// Starts a new stream, as at a reset point of a Compiled HTML Help file, keeping the
    /// window's memory.
    pub fn reset(&mut self) {
        self.main.lengths.fill(0);
        self.length.lengths.fill(0);
        self.state = State::start();
    }

    /// Decodes the next frame, of `length` bytes, from its data `input`: a cabinet's
    /// `CFDATA` block, or the bytes between two entries of a CHM reset table.
    pub fn decode_frame(&mut self, input: &[u8], length: usize) -> Result<&[u8], Malformed> {
        if let Some(failed) = &self.state.failed {
            return Err(failed.clone());
        }
        if length == 0 {
            return Ok(&[]);
        }
        match self.decode(input, length) {
            Ok(Decoded::Window(start)) => Ok(&self.window[start..start + length]),
            Ok(Decoded::Translated) => Ok(&self.translated[..length]),
            Err(malformed) => {
                self.state.failed = Some(malformed.clone());
                Err(malformed)
            }
        }
    }

    fn decode(&mut self, input: &[u8], length: usize) -> Result<Decoded, Malformed> {
        if length > FRAME {
            return Err(Malformed::FrameTooLong(length));
        }
        if self.state.ended {
            return Err(Malformed::AfterLastFrame);
        }
        let mut bits = Bits::new(input);
        if !self.state.header_read {
            self.state.translation = if bits.read(1)? == 1 {
                let high = bits.read(16)?;
                let low = bits.read(16)?;
                (high << 16 | low) as i32
            } else {
                0
            };
            self.state.header_read = true;
        }
        let start = self.state.frame_start;
        let end = start + length;
        while self.state.position < end {
            if self.state.block.remaining == 0 {
                self.read_block(&mut bits)?;
                continue;
            }
            match self.state.block.kind {
                Kind::Uncompressed { odd } => {
                    let run = (self.state.block.remaining as usize).min(end - self.state.position);
                    self.window[self.state.position..self.state.position + run]
                        .copy_from_slice(bits.raw(run)?);
                    self.state.position += run;
                    self.state.block.remaining -= run as u32;
                    self.state.pad = odd && self.state.block.remaining == 0;
                }
                Kind::Verbatim | Kind::AlignedOffset => self.decode_codes(&mut bits, end)?,
            }
        }
        if self.state.pad && bits.has_byte() {
            bits.raw(1)?;
            self.state.pad = false;
        }
        let translate =
            self.state.translation != 0 && self.state.frames < E8_FRAMES && length > E8_TAIL;
        let decoded = if translate {
            let frame = &mut self.translated[..length];
            frame.copy_from_slice(&self.window[start..end]);
            translate_calls(frame, self.state.decoded as i32, self.state.translation);
            Decoded::Translated
        } else {
            Decoded::Window(start)
        };
        self.state.decoded += length as u64;
        self.state.frames += 1;
        self.state.frame_start = end;
        if end == self.window.len() {
            self.state.position = 0;
            self.state.frame_start = 0;
        }
        self.state.ended = length < FRAME;
        Ok(decoded)
    }

    /// Reads the header of the next block, after the byte that pads an uncompressed block of an
    /// odd number of bytes before it.
    fn read_block(&mut self, bits: &mut Bits) -> Result<(), Malformed> {
        if self.state.pad {
            bits.raw(1)?;
            self.state.pad = false;
        }
        let kind = bits.read(3)?;
        let high = bits.read(16)?;
        let low = bits.read(8)?;
        let size = high << 8 | low;
        let kind = match kind {
            1 => {
                self.read_main_and_length_trees(bits)?;
                Kind::Verbatim
            }
            2 => {
                for length in self.aligned.lengths.iter_mut() {
                    *length = bits.read(3)? as u8;
                }
                self.aligned.build()?;
                self.read_main_and_length_trees(bits)?;
                Kind::AlignedOffset
            }
            3 => {
                bits.align()?;
                let header = bits.raw(12)?;
                for (repeated, offset) in self.state.repeated.iter_mut().zip(header.chunks_exact(4))
                {
                    let offset = u32::from_le_bytes([offset[0], offset[1], offset[2], offset[3]]);
                    if offset as usize >= self.window.len() {
                        return Err(Malformed::RepeatedOffset {
                            offset,
                            window: self.window.len(),
                        });
                    }
                    *repeated = offset;
                }
                Kind::Uncompressed { odd: size % 2 == 1 }
            }
            other => return Err(Malformed::BlockType(other)),
        };
        self.state.block = Block {
            kind,
            remaining: size,
        };
        Ok(())
    }

    fn read_main_and_length_trees(&mut self, bits: &mut Bits) -> Result<(), Malformed> {
        let (literals, matches) = self.main.lengths.split_at_mut(LITERALS);
        read_lengths(&mut self.pretree, literals, Tree::Main, bits)?;
        read_lengths(&mut self.pretree, matches, Tree::Main, bits)?;
        self.main.build()?;
        read_lengths(
            &mut self.pretree,
            &mut self.length.lengths,
            Tree::Length,
            bits,
        )?;
        self.length.build()
    }

    /// Decodes the literals and matches of a verbatim or aligned offset block up to its end or
    /// the frame's, `end`.
    fn decode_codes(&mut self, bits: &mut Bits, end: usize) -> Result<(), Malformed> {
        let aligned = self.state.block.kind == Kind::AlignedOffset;
        while self.state.block.remaining > 0 && self.state.position < end {
            let symbol = usize::from(self.main.decode(bits)?);
            if symbol < LITERALS {
                self.window[self.state.position] = symbol as u8;
                self.state.position += 1;
                self.state.block.remaining -= 1;
                continue;
            }
            let element = symbol - LITERALS;
            let mut length = element & PRIMARY_LENGTHS;
            if length == PRIMARY_LENGTHS {
                if self.length.empty {
                    return Err(Malformed::EmptyLengthTree);
                }
                length += usize::from(self.length.decode(bits)?);
            }
            length += MIN_MATCH;
            let offset = match element >> 3 {
                0 => self.state.repeated[0],
                1 => {
                    self.state.repeated.swap(0, 1);
                    self.state.repeated[0]
                }
                2 => {
                    self.state.repeated.swap(0, 2);
                    self.state.repeated[0]
                }
                slot => {
                    let extra = extra_bits(slot);
                    let mut offset = POSITION_BASE[slot] - 2;
                    if aligned && extra >= 3 {
                        offset += bits.read(extra - 3)? << 3;
                        offset += u32::from(self.aligned.decode(bits)?);
                    } else {
                        offset += bits.read(extra)?;
                    }
                    self.state.repeated = [offset, self.state.repeated[0], self.state.repeated[1]];
                    offset
                }
            };
            self.copy_match(offset, length, end)?;
        }
        Ok(())
    }

    /// Copies the `length` bytes `offset` bytes back to the window, within the block and the
    /// frame, which ends at `end`.
    fn copy_match(&mut self, offset: u32, length: usize, end: usize) -> Result<(), Malformed> {
        let remaining = self.state.block.remaining;
        if length > remaining as usize {
            return Err(Malformed::PastBlock { length, remaining });
        }
        if length > end - self.state.position {
            return Err(Malformed::PastFrame {
                length,
                remaining: end - self.state.position,
            });
        }
        let size = self.window.len();
        let decoded = self.state.decoded + (self.state.position - self.state.frame_start) as u64;
        let available = decoded.min(size as u64) as usize;
        let back = offset as usize;
        if back == 0 || back > available {
            return Err(Malformed::MatchOffset { offset, available });
        }
        let destination = self.state.position;
        let source = (destination + size - back) & (size - 1);
        if source + length <= destination {
            self.window
                .copy_within(source..source + length, destination);
        } else {
            for index in 0..length {
                self.window[destination + index] = self.window[(source + index) & (size - 1)];
            }
        }
        self.state.position += length;
        self.state.block.remaining -= length as u32;
        Ok(())
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod tests;
