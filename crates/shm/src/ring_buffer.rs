//! SPSC Ring Buffer — 64K entries of 64-byte Events.
//!
//! Single-producer (Engine), single-consumer (Tracker).
//! Producer and consumer state are on separate cache lines to avoid false sharing.
//!
//! Layout:
//!   - Header: producer_seq (padded 64B) + consumer_seq (padded 64B) = 128B
//!   - Entries: CAPACITY * 64B = 4 MB

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use memmap2::MmapMut;

use common::types::Event;

use crate::mmap;

/// Number of event slots. Must be power of 2.
const CAPACITY: usize = 64 * 1024; // 65536
const MASK: usize = CAPACITY - 1;

const HEADER_SIZE: usize = 128; // 64B producer + 64B consumer (padded)
const ENTRIES_SIZE: usize = CAPACITY * Event::SIZE;
const TOTAL_SIZE: usize = HEADER_SIZE + ENTRIES_SIZE;

/// Ring buffer header — padded producer/consumer on separate cache lines.
#[repr(C, align(64))]
struct ProducerState {
    seq: AtomicU64,
    _pad: [u8; 56],
}

#[repr(C, align(64))]
struct ConsumerState {
    seq: AtomicU64,
    _pad: [u8; 56],
}

pub struct RingBuffer {
    mmap: MmapMut,
}

impl RingBuffer {
    pub fn create(shm_name: &str) -> Result<Self> {
        let mmap = mmap::create_shm(shm_name, TOTAL_SIZE)?;
        Ok(Self { mmap })
    }

    pub fn open(shm_name: &str) -> Result<Self> {
        let mmap = mmap::open_shm(shm_name, TOTAL_SIZE)?;
        Ok(Self { mmap })
    }

    fn producer(&self) -> &ProducerState {
        unsafe { &*(self.mmap.as_ptr() as *const ProducerState) }
    }

    fn consumer(&self) -> &ConsumerState {
        unsafe { &*(self.mmap.as_ptr().add(64) as *const ConsumerState) }
    }

    fn entry_ptr(&self, index: usize) -> *const Event {
        let offset = HEADER_SIZE + (index & MASK) * Event::SIZE;
        unsafe { self.mmap.as_ptr().add(offset) as *const Event }
    }

    fn entry_mut_ptr(&mut self, index: usize) -> *mut Event {
        let offset = HEADER_SIZE + (index & MASK) * Event::SIZE;
        unsafe { self.mmap.as_mut_ptr().add(offset) as *mut Event }
    }

    /// Push an event (producer side). Returns false if buffer is full.
    pub fn push(&mut self, event: &Event) -> bool {
        let prod_seq = self.producer().seq.load(Ordering::Relaxed);
        let cons_seq = self.consumer().seq.load(Ordering::Acquire);

        // Check if full
        if prod_seq - cons_seq >= CAPACITY as u64 {
            return false;
        }

        let ptr = self.entry_mut_ptr(prod_seq as usize);
        unsafe {
            std::ptr::write(ptr, *event);
        }

        // Release: make event visible before advancing producer
        self.producer().seq.store(prod_seq + 1, Ordering::Release);
        true
    }

    /// Pop an event (consumer side). Returns None if buffer is empty.
    pub fn pop(&mut self) -> Option<Event> {
        let cons_seq = self.consumer().seq.load(Ordering::Relaxed);
        let prod_seq = self.producer().seq.load(Ordering::Acquire);

        if cons_seq >= prod_seq {
            return None;
        }

        let ptr = self.entry_ptr(cons_seq as usize);
        let event = unsafe { std::ptr::read(ptr) };

        // Release: advance consumer after reading
        self.consumer().seq.store(cons_seq + 1, Ordering::Release);
        Some(event)
    }

    /// Number of pending events.
    pub fn len(&self) -> usize {
        let prod = self.producer().seq.load(Ordering::Acquire);
        let cons = self.consumer().seq.load(Ordering::Acquire);
        (prod - cons) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        CAPACITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::types::{EventHeader, EventType};

    fn make_event(seq: u64) -> Event {
        Event {
            header: EventHeader {
                timestamp: seq * 100,
                sequence: seq,
                event_type: EventType::SpreadSignal as u16,
                source_proc: 0,
                _reserved: 0,
                payload_len: 0,
                _reserved2: [0; 2],
            },
            payload: [0u8; 40],
        }
    }

    #[test]
    fn test_ring_buffer_push_pop() {
        let name = "test-ringbuf-basic";
        let _ = mmap::remove_shm(name);

        let mut rb = RingBuffer::create(name).unwrap();
        assert!(rb.is_empty());

        // Push 3 events
        assert!(rb.push(&make_event(1)));
        assert!(rb.push(&make_event(2)));
        assert!(rb.push(&make_event(3)));
        assert_eq!(rb.len(), 3);

        // Pop them back
        let e1 = rb.pop().unwrap();
        assert_eq!(e1.header.sequence, 1);
        let e2 = rb.pop().unwrap();
        assert_eq!(e2.header.sequence, 2);
        let e3 = rb.pop().unwrap();
        assert_eq!(e3.header.sequence, 3);

        assert!(rb.pop().is_none());
        assert!(rb.is_empty());

        mmap::remove_shm(name).unwrap();
    }

    #[test]
    fn test_ring_buffer_wrap_around() {
        let name = "test-ringbuf-wrap";
        let _ = mmap::remove_shm(name);

        let mut rb = RingBuffer::create(name).unwrap();

        // Push and pop many events to test wrapping
        for i in 0..1000u64 {
            assert!(rb.push(&make_event(i)));
            let e = rb.pop().unwrap();
            assert_eq!(e.header.sequence, i);
        }

        mmap::remove_shm(name).unwrap();
    }

    #[test]
    fn test_ring_buffer_reopen() {
        let name = "test-ringbuf-reopen";
        let _ = mmap::remove_shm(name);

        {
            let mut rb = RingBuffer::create(name).unwrap();
            rb.push(&make_event(42));
            rb.push(&make_event(43));
        }

        // Reopen and read
        let mut rb = RingBuffer::open(name).unwrap();
        assert_eq!(rb.len(), 2);
        let e = rb.pop().unwrap();
        assert_eq!(e.header.sequence, 42);

        mmap::remove_shm(name).unwrap();
    }
}
