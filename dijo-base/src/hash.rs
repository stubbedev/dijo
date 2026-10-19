//! A fast hasher for maps keyed by the program's own tables or by codes a document defines, where
//! SipHash's resistance to chosen keys buys nothing: FxHash's rotate, xor and multiply, a word at
//! a time. The hash is rotated before use, as rustc-hash 2 rotates it: a product's low bits
//! depend only on the key's low bits, and the table picks buckets by the low bits, so keys that
//! differ only above them, such as codes that are multiples of a power of two, would share a
//! bucket.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// 2^64 divided by the golden ratio.
const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// FxHash.
#[derive(Debug, Default)]
pub struct FastHasher(u64);

impl FastHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FastHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_le_bytes(*word));
        }
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word));
        }
    }

    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    fn write_i32(&mut self, value: i32) {
        self.write_u32(value as u32);
    }

    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    fn finish(&self) -> u64 {
        self.0.rotate_left(26)
    }
}

/// A map hashed with [`FastHasher`].
pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

/// A set hashed with [`FastHasher`].
pub type FastSet<T> = HashSet<T, BuildHasherDefault<FastHasher>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreads_keys_that_differ_only_in_high_bits_over_the_low_bits() {
        let low_bits: FastSet<u64> = (0..1024u32)
            .map(|key| {
                let mut hasher = FastHasher::default();
                hasher.write_u32(key << 20);
                hasher.finish() & 1023
            })
            .collect();
        assert!(low_bits.len() > 600, "{} buckets", low_bits.len());
    }
}
