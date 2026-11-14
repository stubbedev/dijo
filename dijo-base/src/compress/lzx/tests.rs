use super::{extra_bits, WindowSize, LITERALS, POSITION_BASE, SECONDARY_LENGTHS};

#[cfg(test)]
use super::*;

/// What an LZX stream decodes into, a token at a time.
#[derive(Debug, Clone, Copy)]
pub enum Token {
    Literal(u8),
    /// A match `offset` bytes back, which it makes the most recent offset.
    Match {
        offset: u32,
        length: usize,
    },
    /// A match at the repeated offset of `slot`, 0 to 2.
    Repeat {
        slot: usize,
        length: usize,
    },
}

impl Token {
    fn length(self) -> usize {
        match self {
            Token::Literal(_) => 1,
            Token::Match { length, .. } | Token::Repeat { length, .. } => length,
        }
    }
}

/// The bytes `tokens` decode into.
fn size(tokens: &[Token]) -> u32 {
    tokens.iter().map(|token| token.length()).sum::<usize>() as u32
}

/// The canonical codes of `lengths`, assigned in the order of their lengths, then symbols.
fn canonical(lengths: &[u8]) -> Vec<u32> {
    let mut codes = vec![0; lengths.len()];
    let mut code = 0;
    for length in 1..=16 {
        for (symbol, _) in lengths.iter().enumerate().filter(|&(_, &l)| l == length) {
            codes[symbol] = code;
            code += 1;
        }
        code <<= 1;
    }
    codes
}

/// The lengths of a complete code of `count` symbols, of two lengths that differ by one.
fn flat(count: usize) -> Vec<u8> {
    let long = count.next_power_of_two().trailing_zeros() as u8;
    let shorter = (1 << long) - count;
    let mut lengths = vec![long - 1; shorter];
    lengths.resize(count, long);
    lengths
}

/// The pretree every block of a [`Stream`] codes its trees with.
const PRETREE: [u8; 20] = [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5];

/// LZX's bitstream as an encoder writes it: 16-bit little-endian words, each filled from its
/// most significant bit.
#[derive(Default)]
pub struct Writer {
    out: Vec<u8>,
    word: u32,
    count: u32,
}

impl Writer {
    pub fn put(&mut self, value: u32, length: u32) {
        for shift in (0..length).rev() {
            self.word = self.word << 1 | (value >> shift & 1);
            self.count += 1;
            if self.count == 16 {
                self.out
                    .extend_from_slice(&(self.word as u16).to_le_bytes());
                self.word = 0;
                self.count = 0;
            }
        }
    }

    /// Pads the word being written, or writes a whole word where none is, as an uncompressed
    /// block's header does.
    fn align(&mut self) {
        self.put(0, 16 - self.count);
    }

    fn code(&mut self, codes: &[u32], lengths: &[u8], symbol: usize) {
        self.put(codes[symbol], u32::from(lengths[symbol]));
    }

    /// The words written, the last padded.
    pub fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.align();
        }
        self.out
    }
}

/// An LZX stream of one frame, written a block at a time with fixed trees.
pub struct Stream {
    window: WindowSize,
    bits: Writer,
    main: Vec<u8>,
    length: Vec<u8>,
    empty_length_tree: bool,
}

impl Stream {
    /// A stream of `window` whose calls are translated with `translation`, if given.
    pub fn new(window: WindowSize, translation: Option<i32>) -> Self {
        let mut bits = Writer::default();
        match translation {
            Some(size) => {
                bits.put(1, 1);
                bits.put(size as u32, 32);
            }
            None => bits.put(0, 1),
        }
        Self {
            window,
            bits,
            main: vec![0; LITERALS + 8 * window.position_slots()],
            length: vec![0; SECONDARY_LENGTHS],
            empty_length_tree: false,
        }
    }

    /// Makes the length trees of the blocks after empty.
    pub fn empty_length_tree(&mut self) -> &mut Self {
        self.empty_length_tree = true;
        self
    }

    /// An uncompressed block of `data` that sets the repeated offsets to `repeated`.
    pub fn uncompressed(&mut self, repeated: [u32; 3], data: &[u8]) -> &mut Self {
        self.bits.put(3, 3);
        self.bits.put(data.len() as u32, 24);
        self.bits.align();
        for offset in repeated {
            self.bits.out.extend_from_slice(&offset.to_le_bytes());
        }
        self.bits.out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            self.bits.out.push(0);
        }
        self
    }

    /// A verbatim block of `tokens`.
    pub fn verbatim(&mut self, tokens: &[Token]) -> &mut Self {
        self.block(1, size(tokens), tokens)
    }

    /// An aligned offset block of `tokens`, whose aligned offset tree gives each symbol 3 bits.
    pub fn aligned(&mut self, tokens: &[Token]) -> &mut Self {
        self.block(2, size(tokens), tokens)
    }

    /// A block of `kind` that says it decodes into `size` bytes, of `tokens`.
    fn block(&mut self, kind: u32, size: u32, tokens: &[Token]) -> &mut Self {
        let aligned = kind == 2;
        self.bits.put(kind, 3);
        self.bits.put(size, 24);
        if aligned {
            for _ in 0..8 {
                self.bits.put(3, 3);
            }
        }
        let main = flat(self.main.len());
        let length = if self.empty_length_tree {
            vec![0; SECONDARY_LENGTHS]
        } else {
            flat(SECONDARY_LENGTHS)
        };
        self.lengths(0..LITERALS, &main, true);
        self.lengths(LITERALS..main.len(), &main, true);
        self.lengths(0..SECONDARY_LENGTHS, &length, false);
        let (main_codes, length_codes) = (canonical(&main), canonical(&length));
        let slots = self.window.position_slots();
        for &token in tokens {
            let (slot, length_header, footer) = match token {
                Token::Literal(byte) => {
                    self.bits.code(&main_codes, &main, usize::from(byte));
                    continue;
                }
                Token::Repeat { slot, length } => (slot, length, None),
                Token::Match { offset, length } => {
                    let formatted = offset + 2;
                    let slot = (3..slots)
                        .rev()
                        .find(|&slot| POSITION_BASE[slot] <= formatted)
                        .expect("an offset within the window");
                    (slot, length, Some(formatted - POSITION_BASE[slot]))
                }
            };
            let header = (length_header - 2).min(7);
            self.bits
                .code(&main_codes, &main, LITERALS + slot * 8 + header);
            if header == 7 {
                self.bits.code(&length_codes, &length, length_header - 9);
            }
            if let Some(footer) = footer {
                let extra = extra_bits(slot);
                if aligned && extra >= 3 {
                    self.bits.put(footer >> 3, extra - 3);
                    self.bits.put(footer & 7, 3);
                } else {
                    self.bits.put(footer, extra);
                }
            }
        }
        self
    }

    /// Writes the code lengths `lengths` of the symbols `range` of the main tree, or else the
    /// length tree, as changes from those before, coded by [`PRETREE`].
    fn lengths(&mut self, range: std::ops::Range<usize>, lengths: &[u8], main: bool) {
        let codes = canonical(&PRETREE);
        for length in PRETREE {
            self.bits.put(u32::from(length), 4);
        }
        let previous = if main {
            &mut self.main
        } else {
            &mut self.length
        };
        for symbol in range {
            let change = (17 + previous[symbol] - lengths[symbol]) % 17;
            self.bits.put(
                codes[usize::from(change)],
                u32::from(PRETREE[usize::from(change)]),
            );
            previous[symbol] = lengths[symbol];
        }
    }

    pub fn finish(self) -> Vec<u8> {
        self.bits.finish()
    }
}

#[cfg(test)]
fn window(bits: u32) -> WindowSize {
    WindowSize::from_bits(bits).unwrap()
}

#[cfg(test)]
fn decode(stream: &[u8], length: usize) -> Result<Vec<u8>, Malformed> {
    Lzx::new(window(15))
        .decode_frame(stream, length)
        .map(<[u8]>::to_vec)
}

#[test]
fn takes_the_window_sizes_lzx_allows() {
    assert_eq!(WindowSize::from_bits(14), None);
    assert_eq!(
        WindowSize::from_bits(15).map(WindowSize::bytes),
        Some(0x8000)
    );
    assert_eq!(
        WindowSize::from_bits(21).map(WindowSize::bytes),
        Some(0x20_0000)
    );
    assert_eq!(WindowSize::from_bits(22), None);
    assert_eq!(WindowSize::from_bytes(0x10_0000), Some(window(20)));
    assert_eq!(WindowSize::from_bytes(0x1_8000), None);
    assert_eq!(WindowSize::from_bytes(0), None);
    assert_eq!(POSITION_BASE[..6], [0, 1, 2, 3, 4, 6]);
    assert_eq!(POSITION_BASE[49], 1_966_080);
}

#[test]
fn decodes_a_verbatim_block() {
    let mut stream = Stream::new(window(15), None);
    stream.verbatim(&[
        Token::Literal(b'a'),
        Token::Literal(b'b'),
        Token::Literal(b'c'),
        Token::Match {
            offset: 3,
            length: 6,
        },
        Token::Literal(b'X'),
        Token::Match {
            offset: 4,
            length: 20,
        },
        Token::Repeat { slot: 0, length: 3 },
        Token::Repeat { slot: 1, length: 2 },
    ]);
    let expected = b"abcabcabcXabcXabcXabcXabcXabcXabcab";
    assert_eq!(
        decode(&stream.finish(), expected.len()),
        Ok(expected.to_vec())
    );
}

#[test]
fn decodes_an_aligned_offset_block() {
    let mut tokens: Vec<Token> = (0..40).map(Token::Literal).collect();
    tokens.extend([
        Token::Match {
            offset: 37,
            length: 5,
        },
        Token::Match {
            offset: 4,
            length: 3,
        },
        Token::Match {
            offset: 18,
            length: 2,
        },
    ]);
    let mut stream = Stream::new(window(16), None);
    stream.aligned(&tokens);
    let mut expected: Vec<u8> = (0..40).collect();
    expected.extend([3, 4, 5, 6, 7, 4, 5, 6, 30, 31]);
    let decoded = Lzx::new(window(16))
        .decode_frame(&stream.finish(), expected.len())
        .map(<[u8]>::to_vec);
    assert_eq!(decoded, Ok(expected));
}

#[test]
fn decodes_uncompressed_blocks_and_the_byte_that_pads_an_odd_one() {
    let mut stream = Stream::new(window(15), None);
    stream
        .uncompressed([3, 1, 1], b"odd")
        .verbatim(&[Token::Repeat { slot: 0, length: 4 }]);
    assert_eq!(decode(&stream.finish(), 7), Ok(b"oddoddo".to_vec()));
}

#[test]
fn decodes_a_stream_frame_by_frame_and_again_after_a_reset() {
    let data: Vec<u8> = (0..FRAME + 5).map(|index| (index % 251) as u8).collect();
    let mut stream = Stream::new(window(15), None);
    stream.uncompressed([1, 1, 1], &data);
    let stream = stream.finish();
    let (first, second) = stream.split_at(16 + FRAME);
    let mut lzx = Lzx::new(window(15));
    for _ in 0..2 {
        assert_eq!(lzx.decode_frame(first, FRAME), Ok(&data[..FRAME]));
        assert_eq!(lzx.decode_frame(second, 5), Ok(&data[FRAME..]));
        assert_eq!(lzx.decode_frame(second, 5), Err(Malformed::AfterLastFrame));
        lzx.reset();
    }
    assert_eq!(lzx.decode_frame(&[], 0), Ok(&[][..]));
    assert_eq!(
        lzx.decode_frame(first, FRAME + 1),
        Err(Malformed::FrameTooLong(FRAME + 1))
    );
}

#[test]
fn translates_calls_back_as_libmspack_does_an_operand_of_0_included() {
    let mut frame = vec![0x90, 0x90];
    frame.extend([E8, 0, 0, 0, 0]);
    frame.extend([E8, 16, 0, 0, 0]);
    frame.extend([E8, 0xFB, 0xFF, 0xFF, 0xFF]);
    frame.extend([E8, 0x9C, 0xFF, 0xFF, 0xFF]);
    frame.extend([E8, 0x40, 0x42, 0x0F, 0]);
    frame.extend([0x90; 5]);
    frame.extend([E8, 0, 0, 0, 0, 0x90]);
    let mut stream = Stream::new(window(15), Some(1_000_000));
    stream.uncompressed([1, 1, 1], &frame);
    let mut expected = vec![0x90, 0x90];
    expected.push(E8);
    expected.extend((-2i32).to_le_bytes());
    expected.push(E8);
    expected.extend((16i32 - 7).to_le_bytes());
    expected.push(E8);
    expected.extend((1_000_000i32 - 5).to_le_bytes());
    expected.extend([E8, 0x9C, 0xFF, 0xFF, 0xFF]);
    expected.extend([E8, 0x40, 0x42, 0x0F, 0]);
    expected.extend([0x90; 5]);
    expected.extend([E8, 0, 0, 0, 0, 0x90]);
    assert_eq!(decode(&stream.finish(), frame.len()), Ok(expected));
    let mut later = [E8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    translate_calls(&mut later, 0x8000, 1_000_000);
    assert_eq!(later[1..5], (-0x8000i32).to_le_bytes());
}

#[test]
fn refuses_repeated_offsets_beyond_the_window() {
    let mut stream = Stream::new(window(15), None);
    stream
        .uncompressed([0xFFFF_FFF0; 3], b"ab")
        .verbatim(&[Token::Repeat { slot: 0, length: 2 }]);
    assert_eq!(
        decode(&stream.finish(), 4),
        Err(Malformed::RepeatedOffset {
            offset: 0xFFFF_FFF0,
            window: 0x8000
        })
    );
}

#[test]
fn refuses_matches_beyond_what_was_decoded() {
    let mut stream = Stream::new(window(15), None);
    stream
        .uncompressed([1000, 1, 1], b"abcd")
        .verbatim(&[Token::Repeat { slot: 0, length: 2 }]);
    assert_eq!(
        decode(&stream.finish(), 6),
        Err(Malformed::MatchOffset {
            offset: 1000,
            available: 4
        })
    );
    let mut stream = Stream::new(window(15), None);
    stream
        .uncompressed([0, 1, 1], b"ab")
        .verbatim(&[Token::Repeat { slot: 0, length: 2 }]);
    assert_eq!(
        decode(&stream.finish(), 4),
        Err(Malformed::MatchOffset {
            offset: 0,
            available: 2
        })
    );
}

#[test]
fn refuses_matches_past_their_frame_and_fails_for_good() {
    let mut tokens = vec![Token::Literal(b'a'); FRAME - 1];
    tokens.push(Token::Match {
        offset: 1,
        length: 3,
    });
    let mut stream = Stream::new(window(15), None);
    stream.verbatim(&tokens);
    let stream = stream.finish();
    let mut lzx = Lzx::new(window(15));
    let past = Err(Malformed::PastFrame {
        length: 3,
        remaining: 1,
    });
    assert_eq!(lzx.decode_frame(&stream, FRAME), past);
    assert_eq!(lzx.decode_frame(&stream, FRAME), past);
}

#[test]
fn refuses_matches_past_their_block() {
    let mut stream = Stream::new(window(15), None);
    stream.block(
        1,
        2,
        &[
            Token::Literal(b'a'),
            Token::Match {
                offset: 1,
                length: 3,
            },
        ],
    );
    assert_eq!(
        decode(&stream.finish(), 4),
        Err(Malformed::PastBlock {
            length: 3,
            remaining: 1
        })
    );
}

#[test]
fn refuses_data_that_ends_before_its_frame() {
    let mut stream = Stream::new(window(15), None);
    stream.verbatim(&[Token::Literal(b'a'); 64]);
    let stream = stream.finish();
    let cut = &stream[..stream.len() - 8];
    assert_eq!(decode(cut, 64), Err(Malformed::InputEnded(cut.len())));
    let mut stream = Stream::new(window(15), None);
    stream.uncompressed([1, 1, 1], b"abcdef");
    let stream = stream.finish();
    let cut = &stream[..stream.len() - 1];
    assert_eq!(decode(cut, 6), Err(Malformed::InputEnded(cut.len())));
}

#[test]
fn refuses_undefined_blocks_and_codes() {
    let mut bits = Writer::default();
    bits.put(0, 1);
    bits.put(5, 3);
    bits.put(1, 24);
    assert_eq!(decode(&bits.finish(), 1), Err(Malformed::BlockType(5)));
    let mut bits = Writer::default();
    bits.put(0, 1);
    bits.put(2, 3);
    bits.put(1, 24);
    for _ in 0..8 {
        bits.put(1, 3);
    }
    assert_eq!(
        decode(&bits.finish(), 1),
        Err(Malformed::Incomplete(Tree::AlignedOffset))
    );
    let mut stream = Stream::new(window(15), None);
    stream.empty_length_tree().verbatim(&[
        Token::Literal(b'a'),
        Token::Match {
            offset: 1,
            length: 9,
        },
    ]);
    assert_eq!(
        decode(&stream.finish(), 10),
        Err(Malformed::EmptyLengthTree)
    );
}

#[test]
fn reads_code_lengths_in_runs() {
    let codes = canonical(&PRETREE);
    let pretree = |bits: &mut Writer| {
        for length in PRETREE {
            bits.put(u32::from(length), 4);
        }
    };
    let symbol =
        |bits: &mut Writer, symbol: usize| bits.put(codes[symbol], u32::from(PRETREE[symbol]));
    let mut bits = Writer::default();
    pretree(&mut bits);
    symbol(&mut bits, 19);
    bits.put(0, 1);
    symbol(&mut bits, 16);
    symbol(&mut bits, 17);
    bits.put(0, 4);
    symbol(&mut bits, 18);
    bits.put(2, 5);
    let data = bits.finish();
    let mut huffman = Huffman::new(Tree::Pretree, PRETREE_SYMBOLS, 6);
    let mut lengths = [7; 30];
    lengths[0] = 0;
    read_lengths(
        &mut huffman,
        &mut lengths,
        Tree::Main,
        &mut Bits::new(&data),
    )
    .unwrap();
    let mut expected = [0; 30];
    expected[..4].fill(1);
    assert_eq!(lengths, expected);

    let mut bits = Writer::default();
    pretree(&mut bits);
    symbol(&mut bits, 18);
    bits.put(0, 5);
    let data = bits.finish();
    assert_eq!(
        read_lengths(
            &mut huffman,
            &mut [0; 10],
            Tree::Length,
            &mut Bits::new(&data)
        ),
        Err(Malformed::LengthsOverrun(Tree::Length))
    );
    let mut bits = Writer::default();
    pretree(&mut bits);
    symbol(&mut bits, 19);
    bits.put(0, 1);
    symbol(&mut bits, 17);
    let data = bits.finish();
    assert_eq!(
        read_lengths(
            &mut huffman,
            &mut [0; 10],
            Tree::Main,
            &mut Bits::new(&data)
        ),
        Err(Malformed::RunSymbol(17))
    );
}

#[test]
fn decodes_codes_longer_than_the_lookup_table() {
    let mut lengths: Vec<u8> = (1..=16).collect();
    lengths.push(16);
    let codes = canonical(&lengths);
    let mut huffman = Huffman::new(Tree::Main, lengths.len(), 6);
    huffman.lengths.copy_from_slice(&lengths);
    huffman.build().unwrap();
    let symbols = [16, 0, 15, 6, 7, 3, 12, 16, 1];
    let mut bits = Writer::default();
    for &symbol in &symbols {
        bits.code(&codes, &lengths, symbol);
    }
    let data = bits.finish();
    let mut reader = Bits::new(&data);
    for &symbol in &symbols {
        assert_eq!(huffman.decode(&mut reader), Ok(symbol as u16));
    }
    huffman.lengths[16] = 0;
    assert_eq!(huffman.build(), Err(Malformed::Incomplete(Tree::Main)));
}
