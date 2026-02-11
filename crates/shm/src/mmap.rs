//! Shared memory helpers — create and open POSIX shared memory via /dev/shm.

use anyhow::{Context, Result};
use memmap2::MmapMut;
use std::fs::OpenOptions;
use std::path::PathBuf;

/// Path in /dev/shm for a named shared memory segment.
fn shm_path(name: &str) -> PathBuf {
    PathBuf::from("/dev/shm").join(name)
}

/// Create a new shared memory segment, truncating if it exists.
/// Initializes to zeros.
pub fn create_shm(name: &str, size: usize) -> Result<MmapMut> {
    let path = shm_path(name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .with_context(|| format!("failed to create shm: {}", path.display()))?;

    file.set_len(size as u64)
        .with_context(|| format!("failed to set shm size: {}", path.display()))?;

    // Safety: we just created the file and will manage concurrent access via SeqLock/atomics
    let mmap = unsafe { MmapMut::map_mut(&file)? };
    Ok(mmap)
}

/// Open an existing shared memory segment.
pub fn open_shm(name: &str, expected_size: usize) -> Result<MmapMut> {
    let path = shm_path(name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("failed to open shm: {}", path.display()))?;

    let actual_size = file.metadata()?.len() as usize;
    anyhow::ensure!(
        actual_size >= expected_size,
        "shm {} too small: expected {}, got {}",
        name,
        expected_size,
        actual_size
    );

    let mmap = unsafe { MmapMut::map_mut(&file)? };
    Ok(mmap)
}

/// Remove a shared memory segment.
pub fn remove_shm(name: &str) -> Result<()> {
    let path = shm_path(name);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove shm: {}", path.display()))?;
    }
    Ok(())
}
