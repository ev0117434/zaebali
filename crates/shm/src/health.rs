//! Health Table — 16 slots × 64 bytes in shared memory.
//!
//! Each process writes its slot periodically with heartbeat timestamp, status,
//! message count, error count, etc. Supervisor/CLI reads all slots.

use std::sync::atomic::{AtomicU64, AtomicU32, AtomicU8, Ordering};

use anyhow::Result;
use memmap2::MmapMut;

use crate::mmap;

const NUM_SLOTS: usize = 16;
const SLOT_SIZE: usize = 64;
const TOTAL_SIZE: usize = NUM_SLOTS * SLOT_SIZE;

/// Health slot — one per process. All fields are atomic for lock-free access.
#[repr(C, align(64))]
pub struct HealthSlot {
    /// Process status: 0=unknown, 1=starting, 2=running, 3=degraded, 4=stopped
    pub status: AtomicU8,
    pub _pad1: [u8; 7],
    /// Last heartbeat timestamp (microseconds since epoch)
    pub heartbeat_us: AtomicU64,
    /// Total messages processed
    pub msg_count: AtomicU64,
    /// Total errors
    pub error_count: AtomicU32,
    /// Active WS connections (for feeds)
    pub ws_connections: AtomicU8,
    pub _pad2: [u8; 3],
    /// Uptime in seconds
    pub uptime_sec: AtomicU32,
    pub _pad3: [u8; 24],
}

const _: () = {
    assert!(std::mem::size_of::<HealthSlot>() == 64);
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProcessStatus {
    Unknown = 0,
    Starting = 1,
    Running = 2,
    Degraded = 3,
    Stopped = 4,
}

impl ProcessStatus {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => ProcessStatus::Starting,
            2 => ProcessStatus::Running,
            3 => ProcessStatus::Degraded,
            4 => ProcessStatus::Stopped,
            _ => ProcessStatus::Unknown,
        }
    }
}

/// Snapshot of a health slot (non-atomic, for reading/display).
#[derive(Debug, Clone)]
pub struct HealthSnapshot {
    pub status: ProcessStatus,
    pub heartbeat_us: u64,
    pub msg_count: u64,
    pub error_count: u32,
    pub ws_connections: u8,
    pub uptime_sec: u32,
}

pub struct HealthTable {
    mmap: MmapMut,
}

impl HealthTable {
    pub fn create(shm_name: &str) -> Result<Self> {
        let mmap = mmap::create_shm(shm_name, TOTAL_SIZE)?;
        Ok(Self { mmap })
    }

    pub fn open(shm_name: &str) -> Result<Self> {
        let mmap = mmap::open_shm(shm_name, TOTAL_SIZE)?;
        Ok(Self { mmap })
    }

    fn slot(&self, slot_id: usize) -> &HealthSlot {
        assert!(slot_id < NUM_SLOTS);
        let offset = slot_id * SLOT_SIZE;
        unsafe { &*(self.mmap.as_ptr().add(offset) as *const HealthSlot) }
    }

    /// Update heartbeat for a slot.
    pub fn heartbeat(&self, slot_id: usize, timestamp_us: u64) {
        let s = self.slot(slot_id);
        s.heartbeat_us.store(timestamp_us, Ordering::Release);
    }

    /// Set process status.
    pub fn set_status(&self, slot_id: usize, status: ProcessStatus) {
        self.slot(slot_id)
            .status
            .store(status as u8, Ordering::Release);
    }

    /// Increment message counter.
    pub fn inc_msg_count(&self, slot_id: usize) {
        self.slot(slot_id)
            .msg_count
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Increment error counter.
    pub fn inc_error_count(&self, slot_id: usize) {
        self.slot(slot_id)
            .error_count
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Set WS connections count.
    pub fn set_ws_connections(&self, slot_id: usize, count: u8) {
        self.slot(slot_id)
            .ws_connections
            .store(count, Ordering::Relaxed);
    }

    /// Set uptime.
    pub fn set_uptime(&self, slot_id: usize, sec: u32) {
        self.slot(slot_id)
            .uptime_sec
            .store(sec, Ordering::Relaxed);
    }

    /// Read a snapshot of a slot.
    pub fn read(&self, slot_id: usize) -> HealthSnapshot {
        let s = self.slot(slot_id);
        HealthSnapshot {
            status: ProcessStatus::from_u8(s.status.load(Ordering::Acquire)),
            heartbeat_us: s.heartbeat_us.load(Ordering::Acquire),
            msg_count: s.msg_count.load(Ordering::Relaxed),
            error_count: s.error_count.load(Ordering::Relaxed),
            ws_connections: s.ws_connections.load(Ordering::Relaxed),
            uptime_sec: s.uptime_sec.load(Ordering::Relaxed),
        }
    }

    /// Read all slots.
    pub fn read_all(&self) -> Vec<HealthSnapshot> {
        (0..NUM_SLOTS).map(|i| self.read(i)).collect()
    }

    pub fn num_slots(&self) -> usize {
        NUM_SLOTS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_table() {
        let name = "test-health-basic";
        let _ = mmap::remove_shm(name);

        let ht = HealthTable::create(name).unwrap();

        ht.set_status(0, ProcessStatus::Running);
        ht.heartbeat(0, 1234567890);
        ht.inc_msg_count(0);
        ht.inc_msg_count(0);
        ht.inc_error_count(0);

        let snap = ht.read(0);
        assert_eq!(snap.status, ProcessStatus::Running);
        assert_eq!(snap.heartbeat_us, 1234567890);
        assert_eq!(snap.msg_count, 2);
        assert_eq!(snap.error_count, 1);

        // Slot 1 should be default
        let snap1 = ht.read(1);
        assert_eq!(snap1.status, ProcessStatus::Unknown);
        assert_eq!(snap1.msg_count, 0);

        mmap::remove_shm(name).unwrap();
    }
}
