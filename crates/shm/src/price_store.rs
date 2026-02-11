//! Price Store — split seq/data shared memory regions.
//!
//! Layout per region:
//!   - Header (64 bytes): magic, version, num_symbols
//!   - Entries: MAX_SYMBOLS * NUM_SOURCES * 64 bytes
//!
//! Index: symbol_id * NUM_SOURCES + source_id

use anyhow::Result;
use memmap2::MmapMut;

use common::types::{
    PriceDataEntry, PriceSeqEntry, PriceSnapshot,
    MAX_SYMBOLS, NUM_SOURCES,
};

use crate::mmap;
use crate::seqlock;

const HEADER_SIZE: usize = 64;
const MAGIC_SEQS: u32 = 0x53455153; // "SEQS"
const MAGIC_DATA: u32 = 0x44415441; // "DATA"
const VERSION: u32 = 1;

fn entries_size() -> usize {
    MAX_SYMBOLS as usize * NUM_SOURCES as usize * 64
}

fn total_size() -> usize {
    HEADER_SIZE + entries_size()
}

/// Shared memory header (same layout for both seq and data regions).
#[repr(C)]
struct ShmHeader {
    magic: u32,
    version: u32,
    num_symbols: u16,
    _reserved: [u8; 54],
}

/// Price Store handle — provides read/write access to split seq/data regions.
pub struct PriceStore {
    seqs: MmapMut,
    data: MmapMut,
}

impl PriceStore {
    /// Create new Price Store (used by shm-init).
    pub fn create(shm_seqs: &str, shm_data: &str, num_symbols: u16) -> Result<Self> {
        let size = total_size();

        let mut seqs = mmap::create_shm(shm_seqs, size)?;
        let mut data = mmap::create_shm(shm_data, size)?;

        // Write headers
        unsafe {
            let seq_hdr = seqs.as_mut_ptr() as *mut ShmHeader;
            (*seq_hdr).magic = MAGIC_SEQS;
            (*seq_hdr).version = VERSION;
            (*seq_hdr).num_symbols = num_symbols;

            let data_hdr = data.as_mut_ptr() as *mut ShmHeader;
            (*data_hdr).magic = MAGIC_DATA;
            (*data_hdr).version = VERSION;
            (*data_hdr).num_symbols = num_symbols;
        }

        Ok(Self { seqs, data })
    }

    /// Open existing Price Store.
    pub fn open(shm_seqs: &str, shm_data: &str) -> Result<Self> {
        let size = total_size();
        let seqs = mmap::open_shm(shm_seqs, size)?;
        let data = mmap::open_shm(shm_data, size)?;

        // Validate headers
        unsafe {
            let seq_hdr = seqs.as_ptr() as *const ShmHeader;
            anyhow::ensure!((*seq_hdr).magic == MAGIC_SEQS, "seqs magic mismatch");
            anyhow::ensure!((*seq_hdr).version == VERSION, "seqs version mismatch");

            let data_hdr = data.as_ptr() as *const ShmHeader;
            anyhow::ensure!((*data_hdr).magic == MAGIC_DATA, "data magic mismatch");
            anyhow::ensure!((*data_hdr).version == VERSION, "data version mismatch");
        }

        Ok(Self { seqs, data })
    }

    /// Read num_symbols from header.
    pub fn num_symbols(&self) -> u16 {
        unsafe {
            let hdr = self.seqs.as_ptr() as *const ShmHeader;
            (*hdr).num_symbols
        }
    }

    fn slot_offset(symbol_id: u16, source_id: u8) -> usize {
        HEADER_SIZE + (symbol_id as usize * NUM_SOURCES as usize + source_id as usize) * 64
    }

    fn seq_entry(&self, symbol_id: u16, source_id: u8) -> &PriceSeqEntry {
        let offset = Self::slot_offset(symbol_id, source_id);
        unsafe { &*(self.seqs.as_ptr().add(offset) as *const PriceSeqEntry) }
    }

    fn data_entry(&self, symbol_id: u16, source_id: u8) -> &PriceDataEntry {
        let offset = Self::slot_offset(symbol_id, source_id);
        unsafe { &*(self.data.as_ptr().add(offset) as *const PriceDataEntry) }
    }

    fn data_entry_mut(&mut self, symbol_id: u16, source_id: u8) -> &mut PriceDataEntry {
        let offset = Self::slot_offset(symbol_id, source_id);
        unsafe { &mut *(self.data.as_mut_ptr().add(offset) as *mut PriceDataEntry) }
    }

    /// Write a price update for (symbol, source) under SeqLock protection.
    pub fn write(&mut self, symbol_id: u16, source_id: u8, snapshot: &PriceSnapshot) {
        let offset = Self::slot_offset(symbol_id, source_id);
        unsafe {
            let seq = &*(self.seqs.as_ptr().add(offset) as *const PriceSeqEntry);
            let data = &mut *(self.data.as_mut_ptr().add(offset) as *mut PriceDataEntry);
            seqlock::seqlock_write(seq, data, snapshot);
        }
    }

    /// Read a consistent price snapshot for (symbol, source).
    pub fn read(&self, symbol_id: u16, source_id: u8) -> Option<PriceSnapshot> {
        let seq = self.seq_entry(symbol_id, source_id);
        let data = self.data_entry(symbol_id, source_id);
        unsafe { seqlock::seqlock_read(seq, data) }
    }

    /// Read only the sequence number for staleness checks.
    pub fn read_seq(&self, symbol_id: u16, source_id: u8) -> u64 {
        let seq = self.seq_entry(symbol_id, source_id);
        seqlock::read_seq_only(seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_us() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64
    }

    #[test]
    fn test_price_store_create_and_readback() {
        let seqs_name = "test-seqs-basic";
        let data_name = "test-data-basic";

        // Cleanup
        let _ = mmap::remove_shm(seqs_name);
        let _ = mmap::remove_shm(data_name);

        let mut store = PriceStore::create(seqs_name, data_name, 100).unwrap();
        assert_eq!(store.num_symbols(), 100);

        let snap = PriceSnapshot {
            best_bid: 50000.0,
            best_ask: 50001.0,
            updated_at: now_us(),
        };

        store.write(0, 0, &snap);
        let result = store.read(0, 0).unwrap();
        assert!((result.best_bid - 50000.0).abs() < f64::EPSILON);
        assert!((result.best_ask - 50001.0).abs() < f64::EPSILON);

        // Unwritten slot returns zero/default
        let empty = store.read(1, 0);
        assert!(empty.is_some()); // seq=0 is even, so read succeeds
        let empty = empty.unwrap();
        assert!((empty.best_bid - 0.0).abs() < f64::EPSILON);

        // Cleanup
        mmap::remove_shm(seqs_name).unwrap();
        mmap::remove_shm(data_name).unwrap();
    }

    #[test]
    fn test_price_store_reopen() {
        let seqs_name = "test-seqs-reopen";
        let data_name = "test-data-reopen";
        let _ = mmap::remove_shm(seqs_name);
        let _ = mmap::remove_shm(data_name);

        {
            let mut store = PriceStore::create(seqs_name, data_name, 50).unwrap();
            store.write(
                10,
                3,
                &PriceSnapshot {
                    best_bid: 100.0,
                    best_ask: 101.0,
                    updated_at: 999,
                },
            );
        }

        // Reopen and read back
        let store = PriceStore::open(seqs_name, data_name).unwrap();
        assert_eq!(store.num_symbols(), 50);
        let snap = store.read(10, 3).unwrap();
        assert!((snap.best_bid - 100.0).abs() < f64::EPSILON);
        assert_eq!(snap.updated_at, 999);

        mmap::remove_shm(seqs_name).unwrap();
        mmap::remove_shm(data_name).unwrap();
    }
}
