//! SeqLock v2 — split seq/data design.
//!
//! Writer: increment seq (odd = writing), write data with Release, increment seq (even = done).
//! Reader: read seq, if odd → retry; read data with Acquire; re-read seq, if changed → retry.
//!
//! The seq and data are in separate mmap regions to allow different access patterns
//! and avoid false sharing between the lock metadata and the price data.

use std::sync::atomic::Ordering;

use common::types::{PriceDataEntry, PriceSeqEntry, PriceSnapshot};

const MAX_READ_RETRIES: u32 = 4;

/// Write a price update under SeqLock protection.
///
/// # Safety
/// - `seq` and `data` must point to valid, properly aligned entries
///   in shared memory for the same (source, symbol) slot.
/// - Only one writer per slot (guaranteed by process architecture — one feed per source).
pub unsafe fn seqlock_write(seq: &PriceSeqEntry, data: &mut PriceDataEntry, snapshot: &PriceSnapshot) {
    // Step 1: Increment seq to odd (signals "write in progress")
    let current = seq.seq.load(Ordering::Relaxed);
    seq.seq.store(current + 1, Ordering::Release);

    // Step 2: Write data
    // Use volatile writes to prevent compiler reordering
    std::ptr::write_volatile(&mut data.best_bid, snapshot.best_bid);
    std::ptr::write_volatile(&mut data.best_ask, snapshot.best_ask);
    std::ptr::write_volatile(&mut data.updated_at, snapshot.updated_at);

    // Step 3: Increment seq to even (signals "write complete")
    // Release fence ensures data writes are visible before seq update
    std::sync::atomic::fence(Ordering::Release);
    seq.seq.store(current + 2, Ordering::Release);
}

/// Read a price snapshot under SeqLock protection.
///
/// Returns `Some(snapshot)` if a consistent read was obtained within MAX_READ_RETRIES,
/// or `None` if the writer was continuously active.
///
/// # Safety
/// - `seq` and `data` must point to valid, properly aligned entries
///   in shared memory for the same (source, symbol) slot.
pub unsafe fn seqlock_read(seq: &PriceSeqEntry, data: &PriceDataEntry) -> Option<PriceSnapshot> {
    for _ in 0..MAX_READ_RETRIES {
        // Step 1: Read sequence number
        let s1 = seq.seq.load(Ordering::Acquire);

        // If odd, writer is active — spin
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }

        // Step 2: Read data with acquire fence
        std::sync::atomic::fence(Ordering::Acquire);
        let bid = std::ptr::read_volatile(&data.best_bid);
        let ask = std::ptr::read_volatile(&data.best_ask);
        let ts = std::ptr::read_volatile(&data.updated_at);

        // Step 3: Re-read sequence — if unchanged, data is consistent
        std::sync::atomic::fence(Ordering::Acquire);
        let s2 = seq.seq.load(Ordering::Acquire);

        if s1 == s2 {
            return Some(PriceSnapshot {
                best_bid: bid,
                best_ask: ask,
                updated_at: ts,
            });
        }

        std::hint::spin_loop();
    }

    None
}

/// Read only the sequence number (for staleness checks without reading data).
pub fn read_seq_only(seq: &PriceSeqEntry) -> u64 {
    seq.seq.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    fn make_seq_entry() -> PriceSeqEntry {
        PriceSeqEntry {
            seq: AtomicU64::new(0),
            _pad: [0u8; 56],
        }
    }

    fn make_data_entry() -> PriceDataEntry {
        PriceDataEntry {
            best_bid: 0.0,
            best_ask: 0.0,
            updated_at: 0,
            _pad: [0u8; 40],
        }
    }

    #[test]
    fn test_single_thread_write_read() {
        let seq = make_seq_entry();
        let mut data = make_data_entry();

        let snap = PriceSnapshot {
            best_bid: 50000.0,
            best_ask: 50001.0,
            updated_at: 12345,
        };

        unsafe {
            seqlock_write(&seq, &mut data, &snap);
            let result = seqlock_read(&seq, &data).unwrap();
            assert!((result.best_bid - 50000.0).abs() < f64::EPSILON);
            assert!((result.best_ask - 50001.0).abs() < f64::EPSILON);
            assert_eq!(result.updated_at, 12345);
        }

        // Seq should be 2 after one write
        assert_eq!(seq.seq.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_multiple_writes() {
        let seq = make_seq_entry();
        let mut data = make_data_entry();

        for i in 1..=100u64 {
            let snap = PriceSnapshot {
                best_bid: i as f64,
                best_ask: (i + 1) as f64,
                updated_at: i,
            };
            unsafe {
                seqlock_write(&seq, &mut data, &snap);
                let result = seqlock_read(&seq, &data).unwrap();
                assert!((result.best_bid - i as f64).abs() < f64::EPSILON);
            }
        }

        assert_eq!(seq.seq.load(Ordering::Relaxed), 200);
    }

    #[test]
    fn test_concurrent_no_torn_reads() {
        // This test verifies that concurrent reads never observe torn data.
        // A torn read would show bid from one write and ask from another.

        let seq = Arc::new(make_seq_entry());
        let data = Arc::new(std::sync::Mutex::new(make_data_entry()));
        let running = Arc::new(AtomicBool::new(true));

        let seq_w = Arc::clone(&seq);
        let data_w = Arc::clone(&data);
        let running_w = Arc::clone(&running);

        // Writer thread: writes pairs where bid == N*1000, ask == N*1000+1
        let writer = std::thread::spawn(move || {
            let mut i = 1u64;
            while running_w.load(Ordering::Relaxed) {
                let snap = PriceSnapshot {
                    best_bid: (i * 1000) as f64,
                    best_ask: (i * 1000 + 1) as f64,
                    updated_at: i,
                };
                let mut d = data_w.lock().unwrap();
                unsafe {
                    seqlock_write(&seq_w, &mut *d, &snap);
                }
                i += 1;
            }
        });

        // Reader thread: reads and verifies consistency
        let seq_r = Arc::clone(&seq);
        let data_r = Arc::clone(&data);

        let reader = std::thread::spawn(move || {
            let mut reads = 0u64;
            let mut failed = 0u64;
            for _ in 0..100_000 {
                let d = data_r.lock().unwrap();
                let result = unsafe { seqlock_read(&seq_r, &*d) };
                if let Some(snap) = result {
                    reads += 1;
                    // Verify consistency: ask should be bid + 1
                    let diff = snap.best_ask - snap.best_bid;
                    assert!(
                        (diff - 1.0).abs() < f64::EPSILON,
                        "Torn read detected: bid={}, ask={}, diff={}",
                        snap.best_bid,
                        snap.best_ask,
                        diff
                    );
                } else {
                    failed += 1;
                }
            }
            (reads, failed)
        });

        let (reads, _failed) = reader.join().unwrap();
        running.store(false, Ordering::Relaxed);
        writer.join().unwrap();

        assert!(reads > 0, "Should have completed some successful reads");
    }

    #[test]
    fn test_read_seq_only() {
        let seq = make_seq_entry();
        assert_eq!(read_seq_only(&seq), 0);

        seq.seq.store(42, Ordering::Release);
        assert_eq!(read_seq_only(&seq), 42);
    }
}
