//! Control Store — global flags for system control.
//!
//! 256 bytes in shared memory. All fields are atomic.
//! - global_pause: pause all processing
//! - kill_switch: emergency stop
//! - shutdown: graceful shutdown
//! - config_version: incremented by Discovery after generating new configs

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::Result;
use memmap2::MmapMut;

use crate::mmap;

const TOTAL_SIZE: usize = 256;

/// Control flags layout in shared memory.
#[repr(C)]
struct ControlLayout {
    global_pause: AtomicBool,
    kill_switch: AtomicBool,
    shutdown: AtomicBool,
    _pad1: [u8; 5],
    config_version: AtomicU64,
    _pad2: [u8; 240],
}

const _: () = {
    assert!(std::mem::size_of::<ControlLayout>() == TOTAL_SIZE);
};

pub struct ControlStore {
    mmap: MmapMut,
}

impl ControlStore {
    pub fn create(shm_name: &str) -> Result<Self> {
        let mmap = mmap::create_shm(shm_name, TOTAL_SIZE)?;
        Ok(Self { mmap })
    }

    pub fn open(shm_name: &str) -> Result<Self> {
        let mmap = mmap::open_shm(shm_name, TOTAL_SIZE)?;
        Ok(Self { mmap })
    }

    fn layout(&self) -> &ControlLayout {
        unsafe { &*(self.mmap.as_ptr() as *const ControlLayout) }
    }

    // --- Pause ---

    pub fn is_paused(&self) -> bool {
        self.layout().global_pause.load(Ordering::Acquire)
    }

    pub fn set_pause(&self, paused: bool) {
        self.layout()
            .global_pause
            .store(paused, Ordering::Release);
    }

    // --- Kill Switch ---

    pub fn is_killed(&self) -> bool {
        self.layout().kill_switch.load(Ordering::Acquire)
    }

    pub fn set_kill_switch(&self, killed: bool) {
        self.layout()
            .kill_switch
            .store(killed, Ordering::Release);
    }

    // --- Shutdown ---

    pub fn is_shutdown(&self) -> bool {
        self.layout().shutdown.load(Ordering::Acquire)
    }

    pub fn set_shutdown(&self, shutdown: bool) {
        self.layout().shutdown.store(shutdown, Ordering::Release);
    }

    // --- Config Version ---

    pub fn config_version(&self) -> u64 {
        self.layout().config_version.load(Ordering::Acquire)
    }

    pub fn set_config_version(&self, version: u64) {
        self.layout()
            .config_version
            .store(version, Ordering::Release);
    }

    pub fn increment_config_version(&self) -> u64 {
        self.layout()
            .config_version
            .fetch_add(1, Ordering::AcqRel)
            + 1
    }

    /// Check if any stop condition is active.
    pub fn should_stop(&self) -> bool {
        self.is_killed() || self.is_shutdown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_store() {
        let name = "test-control-basic";
        let _ = mmap::remove_shm(name);

        let ctrl = ControlStore::create(name).unwrap();

        // Defaults
        assert!(!ctrl.is_paused());
        assert!(!ctrl.is_killed());
        assert!(!ctrl.is_shutdown());
        assert_eq!(ctrl.config_version(), 0);
        assert!(!ctrl.should_stop());

        // Set flags
        ctrl.set_pause(true);
        assert!(ctrl.is_paused());

        ctrl.set_kill_switch(true);
        assert!(ctrl.is_killed());
        assert!(ctrl.should_stop());

        ctrl.set_kill_switch(false);
        assert!(!ctrl.should_stop());

        // Config version
        let v = ctrl.increment_config_version();
        assert_eq!(v, 1);
        assert_eq!(ctrl.config_version(), 1);

        mmap::remove_shm(name).unwrap();
    }

    #[test]
    fn test_control_store_reopen() {
        let name = "test-control-reopen";
        let _ = mmap::remove_shm(name);

        {
            let ctrl = ControlStore::create(name).unwrap();
            ctrl.set_pause(true);
            ctrl.set_config_version(42);
        }

        let ctrl = ControlStore::open(name).unwrap();
        assert!(ctrl.is_paused());
        assert_eq!(ctrl.config_version(), 42);

        mmap::remove_shm(name).unwrap();
    }
}
