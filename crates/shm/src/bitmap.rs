//! Update Bitmap — per-source 128-byte aligned blocks.
//!
//! Each source has a 128-byte block (1024 bits = MAX_SYMBOLS).
//! Feed sets bit when it writes a price update.
//! Engine atomically swaps entire u64 words to consume updates.
//!
//! Layout: NUM_SOURCES * 128 bytes = 1 KB

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use memmap2::MmapMut;

use common::types::NUM_SOURCES;

use crate::mmap;

/// 128 bytes per source = 16 × u64 = 1024 bits.
const BLOCK_SIZE: usize = 128;
const WORDS_PER_BLOCK: usize = BLOCK_SIZE / 8;
const TOTAL_SIZE: usize = NUM_SOURCES as usize * BLOCK_SIZE;

pub struct UpdateBitmap {
    mmap: MmapMut,
}

impl UpdateBitmap {
    pub fn create(shm_name: &str) -> Result<Self> {
        let mmap = mmap::create_shm(shm_name, TOTAL_SIZE)?;
        Ok(Self { mmap })
    }

    pub fn open(shm_name: &str) -> Result<Self> {
        let mmap = mmap::open_shm(shm_name, TOTAL_SIZE)?;
        Ok(Self { mmap })
    }

    fn word(&self, source_id: u8, word_idx: usize) -> &AtomicU64 {
        let offset = source_id as usize * BLOCK_SIZE + word_idx * 8;
        unsafe { &*(self.mmap.as_ptr().add(offset) as *const AtomicU64) }
    }

    /// Set bit for symbol_id on source_id (called by feed after writing price).
    pub fn set(&self, source_id: u8, symbol_id: u16) {
        let word_idx = symbol_id as usize / 64;
        let bit_idx = symbol_id as usize % 64;
        let mask = 1u64 << bit_idx;
        self.word(source_id, word_idx).fetch_or(mask, Ordering::Release);
    }

    /// Atomically swap a word to zero and return the old value.
    /// Used by engine to consume all pending updates in bulk.
    pub fn swap_word(&self, source_id: u8, word_idx: usize) -> u64 {
        self.word(source_id, word_idx).swap(0, Ordering::AcqRel)
    }

    /// Check if any bit is set for a source (quick check before scanning words).
    pub fn has_updates(&self, source_id: u8) -> bool {
        for w in 0..WORDS_PER_BLOCK {
            if self.word(source_id, w).load(Ordering::Relaxed) != 0 {
                return true;
            }
        }
        false
    }

    /// Number of u64 words per source block.
    pub fn words_per_block(&self) -> usize {
        WORDS_PER_BLOCK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitmap_set_and_swap() {
        let name = "test-bitmap-basic";
        let _ = mmap::remove_shm(name);

        let bm = UpdateBitmap::create(name).unwrap();

        // Initially no updates
        assert!(!bm.has_updates(0));

        // Set symbol 0 on source 0
        bm.set(0, 0);
        assert!(bm.has_updates(0));

        // Set symbol 63 (last bit of first word) and symbol 64 (first bit of second word)
        bm.set(0, 63);
        bm.set(0, 64);

        // Swap first word — should get bits 0 and 63
        let w0 = bm.swap_word(0, 0);
        assert_eq!(w0, (1u64 << 0) | (1u64 << 63));

        // Swap second word — should get bit 0 (= symbol 64)
        let w1 = bm.swap_word(0, 1);
        assert_eq!(w1, 1u64 << 0);

        // After swap, no more updates
        assert!(!bm.has_updates(0));

        // Different sources are independent
        bm.set(3, 100);
        assert!(!bm.has_updates(0));
        assert!(bm.has_updates(3));

        mmap::remove_shm(name).unwrap();
    }
}
