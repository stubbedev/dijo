//! `java.util.zip.Deflater` at its best compression, as the JDK runs it: zlib's `deflate`, whose
//! output Java's image writers carry byte for byte. This is zlib 1.3's lazy-matching
//! compressor (`deflate_slow`) and its Huffman coding (`trees.c`), ported with its window, hash
//! chains and block decisions, so that the stream is zlib's to the bit. zlib-rs, the deflate
//! extractous otherwise uses, follows zlib-ng's matching, whose streams differ.

/// `Deflater.BEST_COMPRESSION`'s row of zlib's `configuration_table`.
const GOOD_LENGTH: usize = 32;
const MAX_LAZY: usize = 258;
const NICE_LENGTH: usize = 258;
const MAX_CHAIN: usize = 4096;
/// The level the zlib header records.
const LEVEL_FLAGS: u16 = 3;

const W_BITS: usize = 15;
const W_SIZE: usize = 1 << W_BITS;
const W_MASK: usize = W_SIZE - 1;
const WINDOW_SIZE: usize = 2 * W_SIZE;
/// `DEF_MEM_LEVEL`, from which the hash table and the symbol buffer are sized.
const MEM_LEVEL: usize = 8;
const HASH_BITS: usize = MEM_LEVEL + 7;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: usize = HASH_SIZE - 1;
const HASH_SHIFT: usize = HASH_BITS.div_ceil(MIN_MATCH);
const LIT_BUFSIZE: usize = 1 << (MEM_LEVEL + 6);
const SYM_END: usize = (LIT_BUFSIZE - 1) * 3;

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;
const WIN_INIT: usize = MAX_MATCH;
const TOO_FAR: usize = 4096;
const NIL: u16 = 0;

const LENGTH_CODES: usize = 29;
const LITERALS: usize = 256;
const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
const D_CODES: usize = 30;
const BL_CODES: usize = 19;
const HEAP_SIZE: usize = 2 * L_CODES + 1;
const MAX_BITS: usize = 15;
const MAX_BL_BITS: usize = 7;
const END_BLOCK: usize = 256;
const REP_3_6: usize = 16;
const REPZ_3_10: usize = 17;
const REPZ_11_138: usize = 18;
const BUF_SIZE: i32 = 16;
const STORED_BLOCK: u16 = 0;
const STATIC_TREES: u16 = 1;
const DYN_TREES: u16 = 2;

const EXTRA_LBITS: [u8; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const EXTRA_DBITS: [u8; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const EXTRA_BLBITS: [u8; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
const BL_ORDER: [usize; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// `ct_data`: a frequency that becomes a code, and a father that becomes a length.
#[derive(Debug, Clone, Copy, Default)]
struct Node {
    /// `fc`: `Freq`, then `Code`.
    fc: u16,
    /// `dl`: `Dad`, then `Len`.
    dl: u16,
}

/// zlib's tables, which `tr_static_init` computes.
struct Tables {
    static_ltree: [Node; L_CODES + 2],
    static_dtree: [Node; D_CODES],
    dist_code: [u8; 512],
    length_code: [u8; MAX_MATCH - MIN_MATCH + 1],
    base_length: [u16; LENGTH_CODES],
    base_dist: [u16; D_CODES],
}

impl Tables {
    fn new() -> Self {
        let mut tables = Tables {
            static_ltree: [Node::default(); L_CODES + 2],
            static_dtree: [Node::default(); D_CODES],
            dist_code: [0; 512],
            length_code: [0; MAX_MATCH - MIN_MATCH + 1],
            base_length: [0; LENGTH_CODES],
            base_dist: [0; D_CODES],
        };
        let mut length = 0usize;
        for (code, &extra) in EXTRA_LBITS.iter().enumerate().take(LENGTH_CODES - 1) {
            tables.base_length[code] = length as u16;
            for _ in 0..1 << extra {
                tables.length_code[length] = code as u8;
                length += 1;
            }
        }
        tables.length_code[length - 1] = (LENGTH_CODES - 1) as u8;
        let mut dist = 0usize;
        for (code, &extra) in EXTRA_DBITS.iter().enumerate().take(16) {
            tables.base_dist[code] = dist as u16;
            for _ in 0..1 << extra {
                tables.dist_code[dist] = code as u8;
                dist += 1;
            }
        }
        dist >>= 7;
        for (code, &extra) in EXTRA_DBITS.iter().enumerate().skip(16) {
            tables.base_dist[code] = (dist << 7) as u16;
            for _ in 0..1 << (extra - 7) {
                tables.dist_code[256 + dist] = code as u8;
                dist += 1;
            }
        }
        let mut bl_count = [0u16; MAX_BITS + 1];
        for (n, node) in tables.static_ltree.iter_mut().enumerate() {
            let len = match n {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
            node.dl = len;
            bl_count[len as usize] += 1;
        }
        gen_codes(&mut tables.static_ltree, L_CODES + 1, &bl_count);
        for (n, node) in tables.static_dtree.iter_mut().enumerate() {
            node.dl = 5;
            node.fc = bi_reverse(n as u16, 5);
        }
        tables
    }

    /// `d_code`.
    fn d_code(&self, dist: usize) -> usize {
        usize::from(if dist < 256 {
            self.dist_code[dist]
        } else {
            self.dist_code[256 + (dist >> 7)]
        })
    }
}

fn bi_reverse(mut code: u16, len: u16) -> u16 {
    let mut res = 0u16;
    for _ in 0..len {
        res = (res << 1) | (code & 1);
        code >>= 1;
    }
    res
}

/// `gen_codes`: the codes of a tree whose lengths are set, from the count of each length.
fn gen_codes(tree: &mut [Node], max_code: usize, bl_count: &[u16; MAX_BITS + 1]) {
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code = 0u32;
    for bits in 1..=MAX_BITS {
        code = (code + u32::from(bl_count[bits - 1])) << 1;
        next_code[bits] = code as u16;
    }
    for node in tree.iter_mut().take(max_code + 1) {
        let len = node.dl;
        if len == 0 {
            continue;
        }
        node.fc = bi_reverse(next_code[usize::from(len)], len);
        next_code[usize::from(len)] = next_code[usize::from(len)].wrapping_add(1);
    }
}

/// Which tree `build_tree` builds, with what `static_tree_desc` says of it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TreeKind {
    Literal,
    Distance,
    BitLength,
}

impl TreeKind {
    fn elems(self) -> usize {
        match self {
            Self::Literal => L_CODES,
            Self::Distance => D_CODES,
            Self::BitLength => BL_CODES,
        }
    }

    fn max_length(self) -> usize {
        match self {
            Self::Literal | Self::Distance => MAX_BITS,
            Self::BitLength => MAX_BL_BITS,
        }
    }

    fn extra_base(self) -> usize {
        match self {
            Self::Literal => LITERALS + 1,
            Self::Distance | Self::BitLength => 0,
        }
    }

    fn extra(self, index: usize) -> usize {
        usize::from(match self {
            Self::Literal => EXTRA_LBITS[index],
            Self::Distance => EXTRA_DBITS[index],
            Self::BitLength => EXTRA_BLBITS[index],
        })
    }
}

/// A `Deflater` at `BEST_COMPRESSION` that has been given input and not yet finished: zlib's
/// `deflate_state`, writing a zlib stream.
pub(crate) struct Deflater {
    tables: Tables,
    out: Vec<u8>,
    window: Vec<u8>,
    prev: Vec<u16>,
    head: Vec<u16>,
    ins_h: usize,
    block_start: isize,
    match_length: usize,
    prev_match: usize,
    match_available: bool,
    strstart: usize,
    match_start: usize,
    lookahead: usize,
    prev_length: usize,
    insert: usize,
    high_water: usize,
    dyn_ltree: [Node; HEAP_SIZE],
    dyn_dtree: [Node; 2 * D_CODES + 1],
    bl_tree: [Node; 2 * BL_CODES + 1],
    l_max_code: usize,
    d_max_code: usize,
    bl_count: [u16; MAX_BITS + 1],
    heap: [usize; 2 * L_CODES + 1],
    heap_len: usize,
    heap_max: usize,
    depth: [u8; 2 * L_CODES + 1],
    sym_buf: Vec<u8>,
    opt_len: u64,
    static_len: u64,
    bi_buf: u16,
    bi_valid: i32,
    adler: Adler32,
}

/// Input not yet read into the window.
struct Input<'a> {
    data: &'a [u8],
}

/// zlib's `adler32`.
struct Adler32 {
    a: u32,
    b: u32,
}

impl Adler32 {
    const BASE: u32 = 65521;
    const NMAX: usize = 5552;

    fn update(&mut self, data: &[u8]) {
        for chunk in data.chunks(Self::NMAX) {
            for &byte in chunk {
                self.a += u32::from(byte);
                self.b += self.a;
            }
            self.a %= Self::BASE;
            self.b %= Self::BASE;
        }
    }

    fn value(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

/// What `deflate_slow` returns.
#[derive(PartialEq, Eq)]
enum BlockState {
    NeedMore,
    FinishDone,
}

impl Deflater {
    /// `new Deflater(Deflater.BEST_COMPRESSION)`, its zlib header written.
    pub(crate) fn best_compression() -> Self {
        let mut deflater = Self {
            tables: Tables::new(),
            out: Vec::new(),
            window: vec![0; WINDOW_SIZE],
            prev: vec![0; W_SIZE],
            head: vec![NIL; HASH_SIZE],
            ins_h: 0,
            block_start: 0,
            match_length: MIN_MATCH - 1,
            prev_match: 0,
            match_available: false,
            strstart: 0,
            match_start: 0,
            lookahead: 0,
            prev_length: MIN_MATCH - 1,
            insert: 0,
            high_water: 0,
            dyn_ltree: [Node::default(); HEAP_SIZE],
            dyn_dtree: [Node::default(); 2 * D_CODES + 1],
            bl_tree: [Node::default(); 2 * BL_CODES + 1],
            l_max_code: 0,
            d_max_code: 0,
            bl_count: [0; MAX_BITS + 1],
            heap: [0; 2 * L_CODES + 1],
            heap_len: 0,
            heap_max: 0,
            depth: [0; 2 * L_CODES + 1],
            sym_buf: Vec::with_capacity(SYM_END),
            opt_len: 0,
            static_len: 0,
            bi_buf: 0,
            bi_valid: 0,
            adler: Adler32 { a: 1, b: 0 },
        };
        deflater.init_block();
        let mut header = (8 + (((W_BITS - 8) as u16) << 4)) << 8;
        header |= LEVEL_FLAGS << 6;
        header += 31 - (header % 31);
        deflater.out.extend_from_slice(&header.to_be_bytes());
        deflater
    }

    /// `setInput` and `deflate` until the input is taken, without flushing.
    pub(crate) fn write(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut input = Input { data };
        self.deflate_slow(&mut input, false);
    }

    /// `finish` and `deflate` until `finished`: the whole zlib stream.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        let mut input = Input { data: &[] };
        let state = self.deflate_slow(&mut input, true);
        debug_assert!(state == BlockState::FinishDone);
        let adler = self.adler.value();
        self.out.extend_from_slice(&adler.to_be_bytes());
        self.out
    }

    /// `read_buf`.
    fn read_buf(&mut self, input: &mut Input, at: usize, size: usize) -> usize {
        let len = input.data.len().min(size);
        if len == 0 {
            return 0;
        }
        let (taken, rest) = input.data.split_at(len);
        self.window[at..at + len].copy_from_slice(taken);
        self.adler.update(taken);
        input.data = rest;
        len
    }

    fn update_hash(&self, h: usize, c: u8) -> usize {
        ((h << HASH_SHIFT) ^ usize::from(c)) & HASH_MASK
    }

    /// `INSERT_STRING`: the previous head of the string's hash chain.
    fn insert_string(&mut self, at: usize) -> u16 {
        self.ins_h = self.update_hash(self.ins_h, self.window[at + MIN_MATCH - 1]);
        let head = self.head[self.ins_h];
        self.prev[at & W_MASK] = head;
        self.head[self.ins_h] = at as u16;
        head
    }

    /// `slide_hash`.
    fn slide_hash(&mut self) {
        for entry in self.head.iter_mut().chain(self.prev.iter_mut()) {
            let m = usize::from(*entry);
            *entry = if m >= W_SIZE {
                (m - W_SIZE) as u16
            } else {
                NIL
            };
        }
    }

    /// `fill_window`.
    fn fill_window(&mut self, input: &mut Input) {
        loop {
            let mut more = WINDOW_SIZE - self.lookahead - self.strstart;
            if self.strstart >= W_SIZE + MAX_DIST {
                self.window.copy_within(W_SIZE..W_SIZE + (W_SIZE - more), 0);
                self.match_start = self.match_start.wrapping_sub(W_SIZE);
                self.strstart -= W_SIZE;
                self.block_start -= W_SIZE as isize;
                if self.insert > self.strstart {
                    self.insert = self.strstart;
                }
                self.slide_hash();
                more += W_SIZE;
            }
            if input.data.is_empty() {
                break;
            }
            let at = self.strstart + self.lookahead;
            let n = self.read_buf(input, at, more);
            self.lookahead += n;
            if self.lookahead + self.insert >= MIN_MATCH {
                let mut str = self.strstart - self.insert;
                self.ins_h = usize::from(self.window[str]);
                self.ins_h = self.update_hash(self.ins_h, self.window[str + 1]);
                while self.insert > 0 {
                    self.ins_h = self.update_hash(self.ins_h, self.window[str + MIN_MATCH - 1]);
                    self.prev[str & W_MASK] = self.head[self.ins_h];
                    self.head[self.ins_h] = str as u16;
                    str += 1;
                    self.insert -= 1;
                    if self.lookahead + self.insert < MIN_MATCH {
                        break;
                    }
                }
            }
            if !(self.lookahead < MIN_LOOKAHEAD && !input.data.is_empty()) {
                break;
            }
        }
        if self.high_water < WINDOW_SIZE {
            let curr = self.strstart + self.lookahead;
            if self.high_water < curr {
                let init = (WINDOW_SIZE - curr).min(WIN_INIT);
                self.window[curr..curr + init].fill(0);
                self.high_water = curr + init;
            } else if self.high_water < curr + WIN_INIT {
                let init = (curr + WIN_INIT - self.high_water).min(WINDOW_SIZE - self.high_water);
                self.window[self.high_water..self.high_water + init].fill(0);
                self.high_water += init;
            }
        }
    }

    /// `longest_match`.
    fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let mut chain_length = MAX_CHAIN;
        let scan = self.strstart;
        let mut best_len = self.prev_length;
        let mut nice_match = NICE_LENGTH;
        let limit = if self.strstart > MAX_DIST {
            self.strstart - MAX_DIST
        } else {
            usize::from(NIL)
        };
        let strend = scan + MAX_MATCH;
        let window = &self.window;
        let mut scan_end1 = window[scan + best_len - 1];
        let mut scan_end = window[scan + best_len];
        if self.prev_length >= GOOD_LENGTH {
            chain_length >>= 2;
        }
        if nice_match > self.lookahead {
            nice_match = self.lookahead;
        }
        loop {
            let m = cur_match;
            if window[m + best_len] == scan_end
                && window[m + best_len - 1] == scan_end1
                && window[m] == window[scan]
                && window[m + 1] == window[scan + 1]
            {
                let mut s = scan + 2;
                let mut mm = m + 2;
                loop {
                    let mut stop = false;
                    for _ in 0..8 {
                        s += 1;
                        mm += 1;
                        if window[s] != window[mm] {
                            stop = true;
                            break;
                        }
                    }
                    if stop || s >= strend {
                        break;
                    }
                }
                let len = MAX_MATCH - (strend - s);
                if len > best_len {
                    self.match_start = cur_match;
                    best_len = len;
                    if len >= nice_match {
                        break;
                    }
                    scan_end1 = window[scan + best_len - 1];
                    scan_end = window[scan + best_len];
                }
            }
            cur_match = usize::from(self.prev[cur_match & W_MASK]);
            if cur_match <= limit {
                break;
            }
            chain_length -= 1;
            if chain_length == 0 {
                break;
            }
        }
        best_len.min(self.lookahead)
    }

    /// `_tr_tally`'s literal.
    fn tally_lit(&mut self, c: u8) -> bool {
        self.sym_buf.extend_from_slice(&[0, 0, c]);
        self.dyn_ltree[usize::from(c)].fc += 1;
        self.sym_buf.len() == SYM_END
    }

    /// `_tr_tally`'s match of `length` (less `MIN_MATCH`) at `distance`.
    fn tally_dist(&mut self, distance: usize, length: usize) -> bool {
        let dist = distance as u16;
        self.sym_buf
            .extend_from_slice(&[dist as u8, (dist >> 8) as u8, length as u8]);
        let dist = usize::from(dist) - 1;
        let lcode = usize::from(self.tables.length_code[length]);
        self.dyn_ltree[lcode + LITERALS + 1].fc += 1;
        let dcode = self.tables.d_code(dist);
        self.dyn_dtree[dcode].fc += 1;
        self.sym_buf.len() == SYM_END
    }

    /// `FLUSH_BLOCK_ONLY`.
    fn flush_block(&mut self, last: bool) {
        let stored = (self.block_start >= 0).then_some(self.block_start as usize);
        let stored_len = (self.strstart as isize - self.block_start) as usize;
        self.tr_flush_block(stored, stored_len, last);
        self.block_start = self.strstart as isize;
        self.bi_flush();
    }

    /// `deflate_slow`, with `deflate`'s finishing of the stream when `finish`.
    fn deflate_slow(&mut self, input: &mut Input, finish: bool) -> BlockState {
        loop {
            if self.lookahead < MIN_LOOKAHEAD {
                self.fill_window(input);
                if self.lookahead < MIN_LOOKAHEAD && !finish {
                    return BlockState::NeedMore;
                }
                if self.lookahead == 0 {
                    break;
                }
            }
            let mut hash_head = NIL;
            if self.lookahead >= MIN_MATCH {
                hash_head = self.insert_string(self.strstart);
            }
            self.prev_length = self.match_length;
            self.prev_match = self.match_start;
            self.match_length = MIN_MATCH - 1;
            let head = usize::from(hash_head);
            if hash_head != NIL && self.prev_length < MAX_LAZY && self.strstart - head <= MAX_DIST {
                self.match_length = self.longest_match(head);
                if self.match_length <= 5
                    && self.match_length == MIN_MATCH
                    && self.strstart.wrapping_sub(self.match_start) > TOO_FAR
                {
                    self.match_length = MIN_MATCH - 1;
                }
            }
            if self.prev_length >= MIN_MATCH && self.match_length <= self.prev_length {
                let max_insert = self.strstart + self.lookahead - MIN_MATCH;
                let bflush = self.tally_dist(
                    self.strstart - 1 - self.prev_match,
                    self.prev_length - MIN_MATCH,
                );
                self.lookahead -= self.prev_length - 1;
                self.prev_length -= 2;
                loop {
                    self.strstart += 1;
                    if self.strstart <= max_insert {
                        self.insert_string(self.strstart);
                    }
                    self.prev_length -= 1;
                    if self.prev_length == 0 {
                        break;
                    }
                }
                self.match_available = false;
                self.match_length = MIN_MATCH - 1;
                self.strstart += 1;
                if bflush {
                    self.flush_block(false);
                }
            } else if self.match_available {
                let bflush = self.tally_lit(self.window[self.strstart - 1]);
                if bflush {
                    self.flush_block(false);
                }
                self.strstart += 1;
                self.lookahead -= 1;
            } else {
                self.match_available = true;
                self.strstart += 1;
                self.lookahead -= 1;
            }
        }
        if self.match_available {
            self.tally_lit(self.window[self.strstart - 1]);
            self.match_available = false;
        }
        self.insert = self.strstart.min(MIN_MATCH - 1);
        self.flush_block(true);
        BlockState::FinishDone
    }

    /// `init_block`.
    fn init_block(&mut self) {
        for node in &mut self.dyn_ltree[..L_CODES] {
            node.fc = 0;
        }
        for node in &mut self.dyn_dtree[..D_CODES] {
            node.fc = 0;
        }
        for node in &mut self.bl_tree[..BL_CODES] {
            node.fc = 0;
        }
        self.dyn_ltree[END_BLOCK].fc = 1;
        self.opt_len = 0;
        self.static_len = 0;
        self.sym_buf.clear();
    }

    fn tree(&mut self, kind: TreeKind) -> &mut [Node] {
        match kind {
            TreeKind::Literal => &mut self.dyn_ltree,
            TreeKind::Distance => &mut self.dyn_dtree,
            TreeKind::BitLength => &mut self.bl_tree,
        }
    }

    fn static_len_of(&self, kind: TreeKind, n: usize) -> Option<u16> {
        match kind {
            TreeKind::Literal => Some(self.tables.static_ltree[n].dl),
            TreeKind::Distance => Some(self.tables.static_dtree[n].dl),
            TreeKind::BitLength => None,
        }
    }

    /// `smaller`.
    fn smaller(tree: &[Node], depth: &[u8], n: usize, m: usize) -> bool {
        tree[n].fc < tree[m].fc || (tree[n].fc == tree[m].fc && depth[n] <= depth[m])
    }

    /// `pqdownheap`.
    fn pqdownheap(&mut self, kind: TreeKind, mut k: usize) {
        let tree: &[Node] = match kind {
            TreeKind::Literal => &self.dyn_ltree,
            TreeKind::Distance => &self.dyn_dtree,
            TreeKind::BitLength => &self.bl_tree,
        };
        let heap = &mut self.heap;
        let depth = &self.depth;
        let heap_len = self.heap_len;
        let v = heap[k];
        let mut j = k << 1;
        while j <= heap_len {
            if j < heap_len && Self::smaller(tree, depth, heap[j + 1], heap[j]) {
                j += 1;
            }
            if Self::smaller(tree, depth, v, heap[j]) {
                break;
            }
            heap[k] = heap[j];
            k = j;
            j <<= 1;
        }
        heap[k] = v;
    }

    /// `gen_bitlen`.
    fn gen_bitlen(&mut self, kind: TreeKind, max_code: usize) {
        let max_length = kind.max_length();
        let base = kind.extra_base();
        self.bl_count = [0; MAX_BITS + 1];
        let root = self.heap[self.heap_max];
        self.tree(kind)[root].dl = 0;
        let mut overflow = 0i32;
        for h in self.heap_max + 1..HEAP_SIZE {
            let n = self.heap[h];
            let tree = self.tree(kind);
            let mut bits = usize::from(tree[usize::from(tree[n].dl)].dl) + 1;
            if bits > max_length {
                bits = max_length;
                overflow += 1;
            }
            tree[n].dl = bits as u16;
            if n > max_code {
                continue;
            }
            let f = u64::from(tree[n].fc);
            self.bl_count[bits] += 1;
            let xbits = if n >= base { kind.extra(n - base) } else { 0 };
            self.opt_len = self.opt_len.wrapping_add(f * (bits + xbits) as u64);
            if let Some(len) = self.static_len_of(kind, n) {
                let added = f * (usize::from(len) + xbits) as u64;
                self.static_len = self.static_len.wrapping_add(added);
            }
        }
        if overflow == 0 {
            return;
        }
        loop {
            let mut bits = max_length - 1;
            while self.bl_count[bits] == 0 {
                bits -= 1;
            }
            self.bl_count[bits] -= 1;
            self.bl_count[bits + 1] += 2;
            self.bl_count[max_length] -= 1;
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }
        let mut h = HEAP_SIZE;
        for bits in (1..=max_length).rev() {
            let mut n = self.bl_count[bits];
            while n != 0 {
                h -= 1;
                let m = self.heap[h];
                if m > max_code {
                    continue;
                }
                let tree = self.tree(kind);
                if usize::from(tree[m].dl) != bits {
                    let freq = u64::from(tree[m].fc);
                    let old = u64::from(tree[m].dl);
                    tree[m].dl = bits as u16;
                    self.opt_len = self
                        .opt_len
                        .wrapping_add((bits as u64).wrapping_sub(old).wrapping_mul(freq));
                }
                n -= 1;
            }
        }
    }

    /// `build_tree`: the tree's lengths and codes, and its largest code with a frequency.
    fn build_tree(&mut self, kind: TreeKind) -> usize {
        let elems = kind.elems();
        let mut max_code: isize = -1;
        self.heap_len = 0;
        self.heap_max = HEAP_SIZE;
        for n in 0..elems {
            if self.tree(kind)[n].fc != 0 {
                self.heap_len += 1;
                self.heap[self.heap_len] = n;
                max_code = n as isize;
                self.depth[n] = 0;
            } else {
                self.tree(kind)[n].dl = 0;
            }
        }
        while self.heap_len < 2 {
            let node = if max_code < 2 {
                max_code += 1;
                max_code as usize
            } else {
                0
            };
            self.heap_len += 1;
            self.heap[self.heap_len] = node;
            self.tree(kind)[node].fc = 1;
            self.depth[node] = 0;
            self.opt_len = self.opt_len.wrapping_sub(1);
            if let Some(len) = self.static_len_of(kind, node) {
                self.static_len = self.static_len.wrapping_sub(u64::from(len));
            }
        }
        let max_code = max_code as usize;
        for n in (1..=self.heap_len / 2).rev() {
            self.pqdownheap(kind, n);
        }
        let mut node = elems;
        loop {
            let n = self.heap[1];
            self.heap[1] = self.heap[self.heap_len];
            self.heap_len -= 1;
            self.pqdownheap(kind, 1);
            let m = self.heap[1];
            self.heap_max -= 1;
            self.heap[self.heap_max] = n;
            self.heap_max -= 1;
            self.heap[self.heap_max] = m;
            let depth = self.depth[n].max(self.depth[m]) + 1;
            let tree = self.tree(kind);
            tree[node].fc = tree[n].fc.wrapping_add(tree[m].fc);
            tree[n].dl = node as u16;
            tree[m].dl = node as u16;
            self.depth[node] = depth;
            self.heap[1] = node;
            node += 1;
            self.pqdownheap(kind, 1);
            if self.heap_len < 2 {
                break;
            }
        }
        self.heap_max -= 1;
        self.heap[self.heap_max] = self.heap[1];
        self.gen_bitlen(kind, max_code);
        let bl_count = self.bl_count;
        gen_codes(self.tree(kind), max_code, &bl_count);
        max_code
    }

    /// `scan_tree`.
    fn scan_tree(&mut self, kind: TreeKind, max_code: usize) {
        let tree = self.tree(kind);
        let mut prevlen: i32 = -1;
        let mut nextlen = i32::from(tree[0].dl);
        let mut count = 0;
        let (mut max_count, mut min_count) = if nextlen == 0 { (138, 3) } else { (7, 4) };
        tree[max_code + 1].dl = 0xffff;
        for n in 0..=max_code {
            let tree = self.tree(kind);
            let curlen = nextlen;
            nextlen = i32::from(tree[n + 1].dl);
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                self.bl_tree[curlen as usize].fc += count as u16;
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.bl_tree[curlen as usize].fc += 1;
                }
                self.bl_tree[REP_3_6].fc += 1;
            } else if count <= 10 {
                self.bl_tree[REPZ_3_10].fc += 1;
            } else {
                self.bl_tree[REPZ_11_138].fc += 1;
            }
            count = 0;
            prevlen = curlen;
            (max_count, min_count) = if nextlen == 0 {
                (138, 3)
            } else if curlen == nextlen {
                (6, 3)
            } else {
                (7, 4)
            };
        }
    }

    /// `send_tree`.
    fn send_tree(&mut self, kind: TreeKind, max_code: usize) {
        let mut prevlen: i32 = -1;
        let mut nextlen = i32::from(self.tree(kind)[0].dl);
        let mut count = 0;
        let (mut max_count, mut min_count) = if nextlen == 0 { (138, 3) } else { (7, 4) };
        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = i32::from(self.tree(kind)[n + 1].dl);
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                while count != 0 {
                    self.send_bl_code(curlen as usize);
                    count -= 1;
                }
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.send_bl_code(curlen as usize);
                    count -= 1;
                }
                self.send_bl_code(REP_3_6);
                self.send_bits((count - 3) as u16, 2);
            } else if count <= 10 {
                self.send_bl_code(REPZ_3_10);
                self.send_bits((count - 3) as u16, 3);
            } else {
                self.send_bl_code(REPZ_11_138);
                self.send_bits((count - 11) as u16, 7);
            }
            count = 0;
            prevlen = curlen;
            (max_count, min_count) = if nextlen == 0 {
                (138, 3)
            } else if curlen == nextlen {
                (6, 3)
            } else {
                (7, 4)
            };
        }
    }

    fn send_bl_code(&mut self, c: usize) {
        let node = self.bl_tree[c];
        self.send_bits(node.fc, node.dl as i32);
    }

    /// `build_bl_tree`: the index of the last bit length code with a frequency.
    fn build_bl_tree(&mut self) -> usize {
        self.scan_tree(TreeKind::Literal, self.l_max_code);
        self.scan_tree(TreeKind::Distance, self.d_max_code);
        self.build_tree(TreeKind::BitLength);
        let mut max_blindex = BL_CODES - 1;
        while max_blindex >= 3 {
            if self.bl_tree[BL_ORDER[max_blindex]].dl != 0 {
                break;
            }
            max_blindex -= 1;
        }
        self.opt_len = self
            .opt_len
            .wrapping_add(3 * (max_blindex as u64 + 1) + 5 + 5 + 4);
        max_blindex
    }

    /// `send_all_trees`.
    fn send_all_trees(&mut self, lcodes: usize, dcodes: usize, blcodes: usize) {
        self.send_bits((lcodes - 257) as u16, 5);
        self.send_bits((dcodes - 1) as u16, 5);
        self.send_bits((blcodes - 4) as u16, 4);
        for &order in BL_ORDER.iter().take(blcodes) {
            self.send_bits(self.bl_tree[order].dl, 3);
        }
        self.send_tree(TreeKind::Literal, lcodes - 1);
        self.send_tree(TreeKind::Distance, dcodes - 1);
    }

    /// `_tr_flush_block`, the block's data at `stored` in the window, if it is still there.
    fn tr_flush_block(&mut self, stored: Option<usize>, stored_len: usize, last: bool) {
        self.l_max_code = self.build_tree(TreeKind::Literal);
        self.d_max_code = self.build_tree(TreeKind::Distance);
        let max_blindex = self.build_bl_tree();
        let mut opt_lenb = self.opt_len.wrapping_add(3 + 7) >> 3;
        let static_lenb = self.static_len.wrapping_add(3 + 7) >> 3;
        if static_lenb <= opt_lenb {
            opt_lenb = static_lenb;
        }
        let last_bit = u16::from(last);
        match stored {
            Some(at) if stored_len as u64 + 4 <= opt_lenb => {
                self.send_bits((STORED_BLOCK << 1) + last_bit, 3);
                self.bi_windup();
                self.out
                    .extend_from_slice(&(stored_len as u16).to_le_bytes());
                self.out
                    .extend_from_slice(&(!(stored_len as u16)).to_le_bytes());
                let data = self.window[at..at + stored_len].to_vec();
                self.out.extend_from_slice(&data);
            }
            _ if static_lenb == opt_lenb => {
                self.send_bits((STATIC_TREES << 1) + last_bit, 3);
                self.compress_block(true);
            }
            _ => {
                self.send_bits((DYN_TREES << 1) + last_bit, 3);
                self.send_all_trees(self.l_max_code + 1, self.d_max_code + 1, max_blindex + 1);
                self.compress_block(false);
            }
        }
        self.init_block();
        if last {
            self.bi_windup();
        }
    }

    fn literal_code(&self, fixed: bool, c: usize) -> Node {
        if fixed {
            self.tables.static_ltree[c]
        } else {
            self.dyn_ltree[c]
        }
    }

    fn distance_code(&self, fixed: bool, c: usize) -> Node {
        if fixed {
            self.tables.static_dtree[c]
        } else {
            self.dyn_dtree[c]
        }
    }

    /// `compress_block`, with the static trees when `fixed`.
    fn compress_block(&mut self, fixed: bool) {
        let symbols = std::mem::take(&mut self.sym_buf);
        for symbol in symbols.as_chunks::<3>().0 {
            let mut dist = usize::from(symbol[0]) | (usize::from(symbol[1]) << 8);
            let mut lc = usize::from(symbol[2]);
            if dist == 0 {
                let node = self.literal_code(fixed, lc);
                self.send_bits(node.fc, node.dl as i32);
                continue;
            }
            let code = usize::from(self.tables.length_code[lc]);
            let node = self.literal_code(fixed, code + LITERALS + 1);
            self.send_bits(node.fc, node.dl as i32);
            let extra = EXTRA_LBITS[code];
            if extra != 0 {
                lc -= usize::from(self.tables.base_length[code]);
                self.send_bits(lc as u16, i32::from(extra));
            }
            dist -= 1;
            let code = self.tables.d_code(dist);
            let node = self.distance_code(fixed, code);
            self.send_bits(node.fc, node.dl as i32);
            let extra = EXTRA_DBITS[code];
            if extra != 0 {
                dist -= usize::from(self.tables.base_dist[code]);
                self.send_bits(dist as u16, i32::from(extra));
            }
        }
        self.sym_buf = symbols;
        let node = self.literal_code(fixed, END_BLOCK);
        self.send_bits(node.fc, node.dl as i32);
    }

    /// `send_bits`.
    fn send_bits(&mut self, value: u16, length: i32) {
        let shifted = (u32::from(value) << self.bi_valid) as u16;
        if self.bi_valid > BUF_SIZE - length {
            self.bi_buf |= shifted;
            self.out.extend_from_slice(&self.bi_buf.to_le_bytes());
            self.bi_buf = value >> (BUF_SIZE - self.bi_valid);
            self.bi_valid += length - BUF_SIZE;
        } else {
            self.bi_buf |= shifted;
            self.bi_valid += length;
        }
    }

    /// `bi_flush`.
    fn bi_flush(&mut self) {
        if self.bi_valid == 16 {
            self.out.extend_from_slice(&self.bi_buf.to_le_bytes());
            self.bi_buf = 0;
            self.bi_valid = 0;
        } else if self.bi_valid >= 8 {
            self.out.push(self.bi_buf as u8);
            self.bi_buf >>= 8;
            self.bi_valid -= 8;
        }
    }

    /// `bi_windup`.
    fn bi_windup(&mut self) {
        if self.bi_valid > 8 {
            self.out.extend_from_slice(&self.bi_buf.to_le_bytes());
        } else if self.bi_valid > 0 {
            self.out.push(self.bi_buf as u8);
        }
        self.bi_buf = 0;
        self.bi_valid = 0;
    }
}
