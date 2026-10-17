//! junrar's PPMd variant H decoder (`ModelPPM`, `PPMContext`, `SEE2Context`), its model kept
//! in the [`SubAllocator`]'s memory: a state is 6 bytes (symbol, frequency, successor), a
//! context 12 (the number of its states, then its one state or its total frequency and the
//! address of its states, then its suffix).

mod alloc;
mod coder;

use alloc::SubAllocator;
use coder::RangeCoder;

use super::io::Input;
use super::{Thrown, OUT_OF_BOUNDS};

const MAX_O: usize = 64;
const INT_BITS: i32 = 7;
const PERIOD_BITS: i32 = 7;
const TOT_BITS: u32 = (INT_BITS + PERIOD_BITS) as u32;
const INTERVAL: i32 = 1 << INT_BITS;
const BIN_SCALE: i32 = 1 << TOT_BITS;
const MAX_FREQ: i32 = 124;
const STATE_SIZE: i32 = 6;
const INIT_BIN_ESC: [i32; 8] = [
    0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051,
];
const EXP_ESCAPE: [i32; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
const DIVIDED_BY_ZERO: Thrown = Thrown("an ArithmeticException, a division by zero");

/// `SEE2Context`: an adaptive escape estimate.
#[derive(Debug, Clone, Copy, Default)]
struct See2 {
    summ: i32,
    shift: i32,
    count: i32,
}

impl See2 {
    fn init(&mut self, init_val: i32) {
        self.shift = (PERIOD_BITS - 4) & 0xff;
        self.summ = (init_val << self.shift) & 0xffff;
        self.count = 4;
    }

    fn mean(&mut self) -> i32 {
        let ret_val = (self.summ as u32 >> self.shift) as i32;
        self.summ -= ret_val;
        ret_val + i32::from(ret_val == 0)
    }

    fn update(&mut self) {
        if self.shift < PERIOD_BITS {
            self.count -= 1;
            if self.count == 0 {
                self.summ += self.summ;
                self.count = 3 << self.shift;
                self.shift += 1;
            }
        }
        self.summ &= 0xffff;
        self.count &= 0xff;
        self.shift &= 0xff;
    }

    fn inc_summ(&mut self, delta: i32) {
        self.summ = (self.summ + delta) & 0xffff;
    }
}

/// Which estimate an escape is coded with: one of the model's, or the dummy of a context of
/// all 256 symbols.
#[derive(Clone, Copy)]
enum See2Slot {
    Context(usize, usize),
    Dummy,
}

/// `StateRef`: a state's fields copied out of the memory.
#[derive(Debug, Clone, Copy, Default)]
struct StateRef {
    symbol: i32,
    freq: i32,
    successor: i32,
}

/// `ModelPPM`.
pub(super) struct Model {
    see2: [[See2; 16]; 25],
    dummy_see2: See2,
    min_context: i32,
    max_context: i32,
    found_state: i32,
    num_masked: i32,
    init_esc: i32,
    order_fall: i32,
    max_order: i32,
    run_length: i32,
    init_rl: i32,
    char_mask: [i32; 256],
    ns2_indx: [i32; 256],
    ns2_bs_indx: [i32; 256],
    hb2_flag: [i32; 256],
    esc_count: i32,
    prev_success: i32,
    hi_bits_flag: i32,
    bin_summ: [[i32; 64]; 128],
    coder: RangeCoder,
    alloc: SubAllocator,
}

impl Model {
    pub(super) fn new() -> Self {
        Model {
            see2: [[See2::default(); 16]; 25],
            dummy_see2: See2::default(),
            min_context: 0,
            max_context: 0,
            found_state: 0,
            num_masked: 0,
            init_esc: 0,
            order_fall: 0,
            max_order: 0,
            run_length: 0,
            init_rl: 0,
            char_mask: [0; 256],
            ns2_indx: [0; 256],
            ns2_bs_indx: [0; 256],
            hb2_flag: [0; 256],
            esc_count: 0,
            prev_success: 0,
            hi_bits_flag: 0,
            bin_summ: [[0; 64]; 128],
            coder: RangeCoder::default(),
            alloc: SubAllocator::new(),
        }
    }

    /// `decodeInit`: the model of a PPMd block started, or continued when the block does not
    /// reset it, from its first bytes; whether there is a model to decode with. The block may
    /// name a new escape character.
    pub(super) fn decode_init(
        &mut self,
        input: &mut Input,
        esc_char: &mut i32,
    ) -> Result<bool, Thrown> {
        let mut max_order = input.get_char()? & 0xff;
        let reset = max_order & 0x20 != 0;
        let mut max_mb = 0;
        if reset {
            max_mb = input.get_char()?;
        } else if self.alloc.allocated() == 0 {
            return Ok(false);
        }
        if max_order & 0x40 != 0 {
            *esc_char = input.get_char()?;
        }
        self.coder.init(input)?;
        if reset {
            max_order = (max_order & 0x1f) + 1;
            if max_order > 16 {
                max_order = 16 + (max_order - 16) * 3;
            }
            if max_order == 1 {
                self.alloc.stop();
                return Ok(false);
            }
            self.alloc.start(max_mb + 1);
            self.min_context = 0;
            self.max_context = 0;
            self.found_state = 0;
            self.dummy_see2 = See2::default();
            self.see2 = [[See2::default(); 16]; 25];
            self.start_model_rare(max_order)?;
        }
        Ok(self.min_context != 0)
    }

    /// `decodeChar`: the next symbol, or -1 where the model is broken.
    pub(super) fn decode_char(&mut self, input: &mut Input) -> Result<i32, Thrown> {
        if !self.in_heap(self.min_context) {
            return Ok(-1);
        }
        if self.num_stats(self.min_context)? != 1 {
            let stats = self.stats(self.min_context)?;
            if !self.in_heap(stats) || !self.decode_symbol1(self.min_context)? {
                return Ok(-1);
            }
        } else {
            self.decode_bin_symbol(self.min_context)?;
        }
        self.coder.decode();
        while self.found_state == 0 {
            self.coder.normalize(input)?;
            loop {
                self.order_fall += 1;
                self.min_context = self.suffix(self.min_context)?;
                if !self.in_heap(self.min_context) {
                    return Ok(-1);
                }
                if self.num_stats(self.min_context)? != self.num_masked {
                    break;
                }
            }
            if !self.decode_symbol2(self.min_context)? {
                return Ok(-1);
            }
            self.coder.decode();
        }
        let symbol = self.symbol(self.found_state)?;
        let successor = match self.order_fall {
            0 => Some(self.successor(self.found_state)?),
            _ => None,
        };
        if let Some(successor) = successor.filter(|&successor| successor > self.alloc.p_text) {
            self.min_context = successor;
            self.max_context = successor;
        } else {
            self.update_model()?;
            if self.esc_count == 0 {
                self.clear_mask();
            }
        }
        self.coder.normalize(input)?;
        Ok(symbol)
    }

    fn in_heap(&self, address: i32) -> bool {
        address > self.alloc.p_text && address <= self.alloc.heap_end
    }

    fn clear_mask(&mut self) {
        self.esc_count = 1;
        self.char_mask = [0; 256];
    }

    fn set_esc_count(&mut self, count: i32) {
        self.esc_count = count & 0xff;
    }

    fn restart_model_rare(&mut self) -> Result<(), Thrown> {
        self.char_mask = [0; 256];
        self.alloc.init()?;
        self.init_rl = -self.max_order.min(12) - 1;
        let context = self.alloc.alloc_context()?;
        self.min_context = context;
        self.max_context = context;
        self.set_suffix(context, 0)?;
        self.order_fall = self.max_order;
        self.set_num_stats(context, 256)?;
        self.set_summ_freq(context, 257)?;
        let stats = self.alloc.alloc_units(256 / 2)?;
        self.found_state = stats;
        self.set_stats(context, stats)?;
        self.run_length = self.init_rl;
        self.prev_success = 0;
        for i in 0..256 {
            let state = stats + i * STATE_SIZE;
            self.set_symbol(state, i)?;
            self.set_freq(state, 1)?;
            self.set_successor(state, 0)?;
        }
        for (i, row) in self.bin_summ.iter_mut().enumerate() {
            for (k, esc) in INIT_BIN_ESC.iter().enumerate() {
                for m in (0..64).step_by(8) {
                    row[k + m] = BIN_SCALE - esc / (i as i32 + 2);
                }
            }
        }
        for (i, row) in self.see2.iter_mut().enumerate() {
            for see2 in row.iter_mut() {
                see2.init(5 * i as i32 + 10);
            }
        }
        Ok(())
    }

    fn start_model_rare(&mut self, max_order: i32) -> Result<(), Thrown> {
        self.esc_count = 1;
        self.max_order = max_order;
        self.restart_model_rare()?;
        self.ns2_bs_indx[0] = 0;
        self.ns2_bs_indx[1] = 2;
        self.ns2_bs_indx[2..11].fill(4);
        self.ns2_bs_indx[11..].fill(6);
        for i in 0..3 {
            self.ns2_indx[i] = i as i32;
        }
        let (mut m, mut k, mut step) = (3, 1, 1);
        for i in 3..256 {
            self.ns2_indx[i] = m;
            k -= 1;
            if k == 0 {
                step += 1;
                k = step;
                m += 1;
            }
        }
        self.hb2_flag[..0x40].fill(0);
        self.hb2_flag[0x40..].fill(0x08);
        self.dummy_see2.shift = PERIOD_BITS;
        Ok(())
    }

    fn update_model_restart(&mut self) -> Result<(), Thrown> {
        self.restart_model_rare()?;
        self.esc_count = 0;
        Ok(())
    }

    fn create_successors(&mut self, skip: bool, p1: i32) -> Result<i32, Thrown> {
        let mut pc = self.min_context;
        let up_branch = self.successor(self.found_state)?;
        let found_symbol = self.symbol(self.found_state)?;
        let mut p = 0;
        let mut ps = [0i32; MAX_O];
        let mut pps = 0;
        let mut no_loop = false;
        if !skip {
            ps[pps] = self.found_state;
            pps += 1;
            if self.suffix(pc)? == 0 {
                no_loop = true;
            }
        }
        if !no_loop {
            let mut loop_entry = false;
            if p1 != 0 {
                p = p1;
                pc = self.suffix(pc)?;
                loop_entry = true;
            }
            loop {
                if !loop_entry {
                    pc = self.suffix(pc)?;
                    if self.num_stats(pc)? != 1 {
                        p = self.stats(pc)?;
                        while self.symbol(p)? != found_symbol {
                            p += STATE_SIZE;
                        }
                    } else {
                        p = pc + 2;
                    }
                }
                loop_entry = false;
                if self.successor(p)? != up_branch {
                    pc = self.successor(p)?;
                    break;
                }
                if pps >= MAX_O {
                    return Ok(0);
                }
                ps[pps] = p;
                pps += 1;
                if self.suffix(pc)? == 0 {
                    break;
                }
            }
        }
        if pps == 0 {
            return Ok(pc);
        }
        let mut up_state = StateRef {
            symbol: self.alloc.u8_at(up_branch)?,
            successor: up_branch + 1,
            freq: 0,
        };
        if self.num_stats(pc)? != 1 {
            if pc <= self.alloc.p_text {
                return Ok(0);
            }
            p = self.stats(pc)?;
            while self.symbol(p)? != up_state.symbol {
                p += STATE_SIZE;
            }
            let cf = self.freq(p)? - 1;
            let s0 = self.summ_freq(pc)? - self.num_stats(pc)? - cf;
            let extra = if 2 * cf <= s0 {
                i32::from(5 * cf > s0)
            } else {
                let divisor = 2 * s0;
                if divisor == 0 {
                    return Err(DIVIDED_BY_ZERO);
                }
                (2 * cf + 3 * s0 - 1) / divisor
            };
            up_state.freq = (1 + extra) & 0xff;
        } else {
            up_state.freq = self.freq(pc + 2)?;
        }
        while pps != 0 {
            pps -= 1;
            pc = self.create_child(pc, ps[pps], up_state)?;
            if pc == 0 {
                return Ok(0);
            }
        }
        Ok(pc)
    }

    /// `PPMContext.createChild`: a context of the one state `first` under `context`, which
    /// `state` leads to; written even where no unit is left, at address 0.
    fn create_child(&mut self, context: i32, state: i32, first: StateRef) -> Result<i32, Thrown> {
        let child = self.alloc.alloc_context()?;
        self.set_num_stats(child, 1)?;
        self.set_state(child + 2, first)?;
        self.set_suffix(child, context)?;
        self.set_successor(state, child)?;
        Ok(child)
    }

    fn update_model(&mut self) -> Result<(), Thrown> {
        let mut fs = self.state_ref(self.found_state)?;
        let mut p = 0;
        let pc = self.suffix(self.min_context)?;
        if fs.freq < MAX_FREQ / 4 && pc != 0 {
            if self.num_stats(pc)? != 1 {
                p = self.stats(pc)?;
                if self.symbol(p)? != fs.symbol {
                    loop {
                        p += STATE_SIZE;
                        if self.symbol(p)? == fs.symbol {
                            break;
                        }
                    }
                    let previous = p - STATE_SIZE;
                    if self.freq(p)? >= self.freq(previous)? {
                        self.swap(p, previous)?;
                        p -= STATE_SIZE;
                    }
                }
                if self.freq(p)? < MAX_FREQ - 9 {
                    self.inc_freq(p, 2)?;
                    self.alloc.inc_u16(pc + 2, 2)?;
                }
            } else {
                p = pc + 2;
                if self.freq(p)? < 32 {
                    self.inc_freq(p, 1)?;
                }
            }
        }
        if self.order_fall == 0 {
            let successor = self.create_successors(true, p)?;
            self.set_successor(self.found_state, successor)?;
            self.min_context = successor;
            self.max_context = successor;
            if successor == 0 {
                self.update_model_restart()?;
            }
            return Ok(());
        }
        self.alloc.set_u8(self.alloc.p_text, fs.symbol)?;
        self.alloc.p_text += 1;
        let mut successor = self.alloc.p_text;
        if self.alloc.p_text >= self.alloc.fake_units_start {
            return self.update_model_restart();
        }
        if fs.successor != 0 {
            if fs.successor <= self.alloc.p_text {
                fs.successor = self.create_successors(false, p)?;
                if fs.successor == 0 {
                    return self.update_model_restart();
                }
            }
            self.order_fall -= 1;
            if self.order_fall == 0 {
                successor = fs.successor;
                if self.max_context != self.min_context {
                    self.alloc.p_text -= 1;
                }
            }
        } else {
            self.set_successor(self.found_state, successor)?;
            fs.successor = self.min_context;
        }
        let ns = self.num_stats(self.min_context)?;
        let s0 = self.summ_freq(self.min_context)? - ns - (fs.freq - 1);
        let mut pc = self.max_context;
        while pc != self.min_context {
            let mut ns1 = self.num_stats(pc)?;
            if ns1 != 1 {
                if ns1 & 1 == 0 {
                    let stats = self.alloc.expand_units(self.stats(pc)?, ns1 >> 1)?;
                    self.set_stats(pc, stats)?;
                    if stats == 0 {
                        return self.update_model_restart();
                    }
                }
                let sum = i32::from(2 * ns1 < ns)
                    + 2 * (i32::from(4 * ns1 <= ns) & i32::from(self.summ_freq(pc)? <= 8 * ns1));
                self.alloc.inc_u16(pc + 2, sum)?;
            } else {
                let stats = self.alloc.alloc_units(1)?;
                if stats == 0 {
                    return self.update_model_restart();
                }
                self.alloc.copy(pc + 2, stats, STATE_SIZE)?;
                self.set_stats(pc, stats)?;
                let freq = self.freq(stats)?;
                if freq < MAX_FREQ / 4 - 1 {
                    self.inc_freq(stats, freq)?;
                } else {
                    self.set_freq(stats, MAX_FREQ - 4)?;
                }
                let summ = self.freq(stats)? + self.init_esc + i32::from(ns > 3);
                self.set_summ_freq(pc, summ)?;
            }
            let mut cf = 2 * fs.freq * (self.summ_freq(pc)? + 6);
            let sf = s0 + self.summ_freq(pc)?;
            if cf < 6 * sf {
                cf = 1 + i32::from(cf > sf) + i32::from(cf >= 4 * sf);
                self.alloc.inc_u16(pc + 2, 3)?;
            } else {
                cf = 4
                    + i32::from(cf >= 9 * sf)
                    + i32::from(cf >= 12 * sf)
                    + i32::from(cf >= 15 * sf);
                self.alloc.inc_u16(pc + 2, cf)?;
            }
            let state = self.stats(pc)? + ns1 * STATE_SIZE;
            self.set_successor(state, successor)?;
            self.set_symbol(state, fs.symbol)?;
            self.set_freq(state, cf)?;
            ns1 += 1;
            self.set_num_stats(pc, ns1)?;
            pc = self.suffix(pc)?;
        }
        self.max_context = fs.successor;
        self.min_context = fs.successor;
        Ok(())
    }

    fn rescale(&mut self, context: i32) -> Result<(), Thrown> {
        let old_ns = self.num_stats(context)?;
        let mut i = old_ns - 1;
        let stats = self.stats(context)?;
        let mut p = self.found_state;
        while p != stats {
            self.swap(p, p - STATE_SIZE)?;
            p -= STATE_SIZE;
        }
        self.inc_freq(stats, 4)?;
        self.alloc.inc_u16(context + 2, 4)?;
        let mut esc_freq = self.summ_freq(context)? - self.freq(p)?;
        let adder = i32::from(self.order_fall != 0);
        self.set_freq(p, (self.freq(p)? + adder) >> 1)?;
        self.set_summ_freq(context, self.freq(p)?)?;
        loop {
            p += STATE_SIZE;
            esc_freq -= self.freq(p)?;
            self.set_freq(p, (self.freq(p)? + adder) >> 1)?;
            self.alloc.inc_u16(context + 2, self.freq(p)?)?;
            if self.freq(p)? > self.freq(p - STATE_SIZE)? {
                let mut p1 = p;
                let moved = self.state_ref(p1)?;
                loop {
                    self.alloc.copy(p1 - STATE_SIZE, p1, STATE_SIZE)?;
                    p1 -= STATE_SIZE;
                    if p1 == stats || moved.freq <= self.freq(p1 - STATE_SIZE)? {
                        break;
                    }
                }
                self.set_state(p1, moved)?;
            }
            i -= 1;
            if i == 0 {
                break;
            }
        }
        if self.freq(p)? == 0 {
            loop {
                i += 1;
                p -= STATE_SIZE;
                if self.freq(p)? != 0 {
                    break;
                }
            }
            esc_freq += i;
            self.set_num_stats(context, self.num_stats(context)? - i)?;
            if self.num_stats(context)? == 1 {
                let mut first = self.state_ref(stats)?;
                loop {
                    first.freq = (first.freq - (first.freq >> 1)) & 0xff;
                    esc_freq = (esc_freq as u32 >> 1) as i32;
                    if esc_freq <= 1 {
                        break;
                    }
                }
                self.alloc.free_units(stats, (old_ns + 1) >> 1)?;
                self.set_state(context + 2, first)?;
                self.found_state = context + 2;
                return Ok(());
            }
        }
        esc_freq -= (esc_freq as u32 >> 1) as i32;
        self.alloc.inc_u16(context + 2, esc_freq)?;
        let n0 = (old_ns + 1) >> 1;
        let n1 = (self.num_stats(context)? + 1) >> 1;
        if n0 != n1 {
            let shrunk = self.alloc.shrink_units(stats, n0, n1)?;
            self.set_stats(context, shrunk)?;
        }
        self.found_state = self.stats(context)?;
        Ok(())
    }

    fn array_index(&self, context: i32, state: i32) -> Result<usize, Thrown> {
        let suffix = self.suffix(context)?;
        let index = self.prev_success
            + table(&self.ns2_bs_indx, self.num_stats(suffix)? - 1)?
            + self.hi_bits_flag
            + 2 * table(&self.hb2_flag, self.symbol(state)?)?
            + ((self.run_length as u32 >> 26) as i32 & 0x20);
        usize::try_from(index).map_err(|_| OUT_OF_BOUNDS)
    }

    fn decode_bin_symbol(&mut self, context: i32) -> Result<(), Thrown> {
        let rs = context + 2;
        self.set_hi_bits_flag(table(&self.hb2_flag, self.symbol(self.found_state)?)?);
        let off1 = usize::try_from(self.freq(rs)? - 1).map_err(|_| OUT_OF_BOUNDS)?;
        let off2 = self.array_index(context, rs)?;
        let mut bs = *self
            .bin_summ
            .get(off1)
            .and_then(|row| row.get(off2))
            .ok_or(OUT_OF_BOUNDS)?;
        if self.coder.current_shift_count(TOT_BITS)? < i64::from(bs) {
            self.found_state = rs;
            let freq = self.freq(rs)?;
            self.inc_freq(rs, i32::from(freq < 128))?;
            self.coder.set_low_count(0);
            self.coder.set_high_count(i64::from(bs));
            bs = (bs + INTERVAL - mean(bs, PERIOD_BITS, 2)) & 0xffff;
            self.bin_summ[off1][off2] = bs;
            self.set_prev_success(1);
            self.run_length += 1;
        } else {
            self.coder.set_low_count(i64::from(bs));
            bs = (bs - mean(bs, PERIOD_BITS, 2)) & 0xffff;
            self.bin_summ[off1][off2] = bs;
            self.coder.set_high_count(i64::from(BIN_SCALE));
            self.init_esc = table(&EXP_ESCAPE, (bs as u32 >> 10) as i32)?;
            self.num_masked = 1;
            let symbol = self.symbol(rs)?;
            self.char_mask[symbol as usize] = self.esc_count;
            self.set_prev_success(0);
            self.found_state = 0;
        }
        Ok(())
    }

    fn update1(&mut self, context: i32, p: i32) -> Result<(), Thrown> {
        self.found_state = p;
        self.inc_freq(p, 4)?;
        self.alloc.inc_u16(context + 2, 4)?;
        let previous = p - STATE_SIZE;
        if self.freq(p)? > self.freq(previous)? {
            self.swap(p, previous)?;
            self.found_state = previous;
            if self.freq(previous)? > MAX_FREQ {
                self.rescale(context)?;
            }
        }
        Ok(())
    }

    fn decode_symbol2(&mut self, context: i32) -> Result<bool, Thrown> {
        let mut i = self.num_stats(context)? - self.num_masked;
        let see2 = self.make_esc_freq2(context, i)?;
        let mut p = self.stats(context)? - STATE_SIZE;
        let mut ps = [0i32; 256];
        let mut pps = 0usize;
        let mut hi_cnt = 0i32;
        loop {
            loop {
                p += STATE_SIZE;
                if self.char_mask[self.symbol(p)? as usize] != self.esc_count {
                    break;
                }
            }
            hi_cnt += self.freq(p)?;
            if pps >= ps.len() {
                return Ok(false);
            }
            ps[pps] = p;
            pps += 1;
            i -= 1;
            if i == 0 {
                break;
            }
        }
        self.coder.set_scale(self.coder.scale + i64::from(hi_cnt));
        let count = self.coder.current_count()?;
        if i64::from(count) >= self.coder.scale {
            return Ok(false);
        }
        pps = 0;
        p = ps[pps];
        if count < hi_cnt {
            hi_cnt = 0;
            loop {
                hi_cnt += self.freq(p)?;
                if hi_cnt > count {
                    break;
                }
                pps += 1;
                if pps >= ps.len() {
                    return Ok(false);
                }
                p = ps[pps];
            }
            self.coder.set_high_count(i64::from(hi_cnt));
            self.coder.set_low_count(i64::from(hi_cnt - self.freq(p)?));
            self.see2_mut(see2).update();
            self.update2(context, p)?;
        } else {
            self.coder.set_low_count(i64::from(hi_cnt));
            self.coder.set_high_count(self.coder.scale);
            i = self.num_stats(context)? - self.num_masked;
            let mut index = pps as isize - 1;
            loop {
                index += 1;
                let Some(&state) = usize::try_from(index).ok().and_then(|at| ps.get(at)) else {
                    return Ok(false);
                };
                let symbol = self.symbol(state)?;
                self.char_mask[symbol as usize] = self.esc_count;
                i -= 1;
                if i == 0 {
                    break;
                }
            }
            let scale = self.coder.scale as i32;
            self.see2_mut(see2).inc_summ(scale);
            self.num_masked = self.num_stats(context)?;
        }
        Ok(true)
    }

    fn update2(&mut self, context: i32, p: i32) -> Result<(), Thrown> {
        self.found_state = p;
        self.inc_freq(p, 4)?;
        self.alloc.inc_u16(context + 2, 4)?;
        if self.freq(p)? > MAX_FREQ {
            self.rescale(context)?;
        }
        self.set_esc_count(self.esc_count + 1);
        self.run_length = self.init_rl;
        Ok(())
    }

    fn make_esc_freq2(&mut self, context: i32, diff: i32) -> Result<See2Slot, Thrown> {
        let num_stats = self.num_stats(context)?;
        if num_stats == 256 {
            self.coder.set_scale(1);
            return Ok(See2Slot::Dummy);
        }
        let suffix = self.suffix(context)?;
        let idx1 = table(&self.ns2_indx, diff - 1)? as usize;
        let idx2 = i32::from(diff < self.num_stats(suffix)? - num_stats)
            + 2 * i32::from(self.summ_freq(context)? < 11 * num_stats)
            + 4 * i32::from(self.num_masked > diff)
            + self.hi_bits_flag;
        let idx2 = usize::try_from(idx2).map_err(|_| OUT_OF_BOUNDS)?;
        let see2 = self
            .see2
            .get_mut(idx1)
            .and_then(|row| row.get_mut(idx2))
            .ok_or(OUT_OF_BOUNDS)?;
        let scale = see2.mean();
        self.coder.set_scale(i64::from(scale));
        Ok(See2Slot::Context(idx1, idx2))
    }

    fn see2_mut(&mut self, slot: See2Slot) -> &mut See2 {
        match slot {
            See2Slot::Context(idx1, idx2) => &mut self.see2[idx1][idx2],
            See2Slot::Dummy => &mut self.dummy_see2,
        }
    }

    fn decode_symbol1(&mut self, context: i32) -> Result<bool, Thrown> {
        let scale = self.summ_freq(context)?;
        self.coder.set_scale(i64::from(scale));
        let mut p = self.stats(context)?;
        let count = self.coder.current_count()?;
        if i64::from(count) >= self.coder.scale {
            return Ok(false);
        }
        let mut hi_cnt = self.freq(p)?;
        if count < hi_cnt {
            self.coder.set_high_count(i64::from(hi_cnt));
            self.set_prev_success(i32::from(i64::from(2 * hi_cnt) > self.coder.scale));
            self.run_length += self.prev_success;
            hi_cnt += 4;
            self.found_state = p;
            self.set_freq(p, hi_cnt)?;
            self.alloc.inc_u16(context + 2, 4)?;
            if hi_cnt > MAX_FREQ {
                self.rescale(context)?;
            }
            self.coder.set_low_count(0);
            return Ok(true);
        }
        if self.found_state == 0 {
            return Ok(false);
        }
        self.set_prev_success(0);
        let num_stats = self.num_stats(context)?;
        let mut i = num_stats - 1;
        loop {
            p += STATE_SIZE;
            hi_cnt += self.freq(p)?;
            if hi_cnt > count {
                break;
            }
            i -= 1;
            if i == 0 {
                self.set_hi_bits_flag(table(&self.hb2_flag, self.symbol(self.found_state)?)?);
                self.coder.set_low_count(i64::from(hi_cnt));
                let symbol = self.symbol(p)?;
                self.char_mask[symbol as usize] = self.esc_count;
                self.num_masked = num_stats;
                i = num_stats - 1;
                self.found_state = 0;
                loop {
                    p -= STATE_SIZE;
                    let symbol = self.symbol(p)?;
                    self.char_mask[symbol as usize] = self.esc_count;
                    i -= 1;
                    if i == 0 {
                        break;
                    }
                }
                self.coder.set_high_count(self.coder.scale);
                return Ok(true);
            }
        }
        self.coder.set_low_count(i64::from(hi_cnt - self.freq(p)?));
        self.coder.set_high_count(i64::from(hi_cnt));
        self.update1(context, p)?;
        Ok(true)
    }

    fn set_prev_success(&mut self, value: i32) {
        self.prev_success = value & 0xff;
    }

    fn set_hi_bits_flag(&mut self, value: i32) {
        self.hi_bits_flag = value & 0xff;
    }

    fn symbol(&self, state: i32) -> Result<i32, Thrown> {
        self.alloc.u8_at(state)
    }

    fn set_symbol(&mut self, state: i32, symbol: i32) -> Result<(), Thrown> {
        self.alloc.set_u8(state, symbol)
    }

    fn freq(&self, state: i32) -> Result<i32, Thrown> {
        self.alloc.u8_at(state + 1)
    }

    fn set_freq(&mut self, state: i32, freq: i32) -> Result<(), Thrown> {
        self.alloc.set_u8(state + 1, freq)
    }

    fn inc_freq(&mut self, state: i32, delta: i32) -> Result<(), Thrown> {
        let freq = self.freq(state)?;
        self.set_freq(state, freq + delta)
    }

    fn successor(&self, state: i32) -> Result<i32, Thrown> {
        self.alloc.i32_at(state + 2)
    }

    fn set_successor(&mut self, state: i32, successor: i32) -> Result<(), Thrown> {
        self.alloc.set_i32(state + 2, successor)
    }

    fn state_ref(&self, state: i32) -> Result<StateRef, Thrown> {
        Ok(StateRef {
            symbol: self.symbol(state)?,
            freq: self.freq(state)?,
            successor: self.successor(state)?,
        })
    }

    fn set_state(&mut self, state: i32, values: StateRef) -> Result<(), Thrown> {
        self.set_symbol(state, values.symbol)?;
        self.set_freq(state, values.freq)?;
        self.set_successor(state, values.successor)
    }

    /// `State.ppmdSwap`.
    fn swap(&mut self, a: i32, b: i32) -> Result<(), Thrown> {
        let first = self.state_ref(a)?;
        let second = self.state_ref(b)?;
        self.set_state(a, second)?;
        self.set_state(b, first)
    }

    fn num_stats(&self, context: i32) -> Result<i32, Thrown> {
        self.alloc.u16_at(context)
    }

    fn set_num_stats(&mut self, context: i32, count: i32) -> Result<(), Thrown> {
        self.alloc.set_u16(context, count)
    }

    fn summ_freq(&self, context: i32) -> Result<i32, Thrown> {
        self.alloc.u16_at(context + 2)
    }

    fn set_summ_freq(&mut self, context: i32, summ: i32) -> Result<(), Thrown> {
        self.alloc.set_u16(context + 2, summ)
    }

    fn stats(&self, context: i32) -> Result<i32, Thrown> {
        self.alloc.i32_at(context + 4)
    }

    fn set_stats(&mut self, context: i32, stats: i32) -> Result<(), Thrown> {
        self.alloc.set_i32(context + 4, stats)
    }

    fn suffix(&self, context: i32) -> Result<i32, Thrown> {
        self.alloc.i32_at(context + 8)
    }

    fn set_suffix(&mut self, context: i32, suffix: i32) -> Result<(), Thrown> {
        self.alloc.set_i32(context + 8, suffix)
    }
}

/// `PPMContext.getMean`.
fn mean(summ: i32, shift: i32, round: i32) -> i32 {
    ((summ + (1 << (shift - round))) as u32 >> shift) as i32
}

/// `table[index]`, failing as Java does outside the table.
fn table(table: &[i32], index: i32) -> Result<i32, Thrown> {
    usize::try_from(index)
        .ok()
        .and_then(|index| table.get(index))
        .copied()
        .ok_or(OUT_OF_BOUNDS)
}
