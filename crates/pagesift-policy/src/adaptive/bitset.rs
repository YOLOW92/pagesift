//! Lock-free dense atomic bitset for high-throughput zero-lock visited tracking.

use std::sync::atomic::{AtomicU64, Ordering};

/// A lock-free bitset backed by contiguous `AtomicU64` words.
/// Provides atomic test-and-set and lookup operations without mutex contention.
pub struct AtomicBitSet {
    words: Box<[AtomicU64]>,
    capacity: usize,
}

impl AtomicBitSet {
    /// Creates a new `AtomicBitSet` capable of storing at least `capacity` bits.
    pub fn new(capacity: usize) -> Self {
        let num_words = capacity.div_ceil(64);
        let mut words_vec = Vec::with_capacity(num_words);
        for _ in 0..num_words {
            words_vec.push(AtomicU64::new(0));
        }
        Self {
            words: words_vec.into_boxed_slice(),
            capacity,
        }
    }

    /// Returns the capacity (in bits).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Atomically tests if the bit at `index` is set, and sets it if not.
    /// Returns `true` if the bit was ALREADY set, `false` if it was newly set.
    #[inline]
    pub fn test_and_set(&self, index: usize) -> bool {
        let word_idx = index / 64;
        if word_idx >= self.words.len() {
            return false;
        }
        let bit_idx = index % 64;
        let mask = 1u64 << bit_idx;
        let prev = self.words[word_idx].fetch_or(mask, Ordering::AcqRel);
        (prev & mask) != 0
    }

    /// Atomically sets the bit at `index`.
    #[inline]
    pub fn set(&self, index: usize) {
        let word_idx = index / 64;
        if word_idx < self.words.len() {
            let bit_idx = index % 64;
            let mask = 1u64 << bit_idx;
            self.words[word_idx].fetch_or(mask, Ordering::Release);
        }
    }

    /// Checks if the bit at `index` is set.
    #[inline]
    pub fn contains(&self, index: usize) -> bool {
        let word_idx = index / 64;
        if word_idx >= self.words.len() {
            return false;
        }
        let bit_idx = index % 64;
        let mask = 1u64 << bit_idx;
        (self.words[word_idx].load(Ordering::Acquire) & mask) != 0
    }

    /// Counts total number of set bits (relaxed approximate or consistent if quiesced).
    pub fn count_ones(&self) -> usize {
        self.words
            .iter()
            .map(|w| w.load(Ordering::Relaxed).count_ones() as usize)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_atomic_bitset_basic() {
        let bs = AtomicBitSet::new(1000);
        assert!(!bs.contains(42));
        assert!(!bs.test_and_set(42));
        assert!(bs.contains(42));
        assert!(bs.test_and_set(42));
        assert_eq!(bs.count_ones(), 1);

        assert!(!bs.test_and_set(999));
        assert!(bs.contains(999));
        assert_eq!(bs.count_ones(), 2);
    }

    #[test]
    fn test_atomic_bitset_concurrent() {
        let bs = Arc::new(AtomicBitSet::new(10_000));
        let mut handles = Vec::new();
        for t in 0..8 {
            let bs = bs.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..1000 {
                    let id = t * 1000 + i;
                    assert!(!bs.test_and_set(id));
                    assert!(bs.contains(id));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(bs.count_ones(), 8000);
    }
}
