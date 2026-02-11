//! shm-init — Creates all shared memory segments.
//! Oneshot: runs once before all other processes.

use anyhow::Result;
use tracing::{info, Level};

use common::config::AppConfig;
use common::types::MAX_SYMBOLS;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/config.toml".to_string());

    let config = AppConfig::load(std::path::Path::new(&config_path))?;
    let g = &config.general;

    info!("Creating shared memory segments (MAX_SYMBOLS={})", MAX_SYMBOLS);

    // Price Store (seqs + data)
    let store = shm::price_store::PriceStore::create(&g.shm_seqs, &g.shm_data, MAX_SYMBOLS)?;
    info!(
        "Price Store: seqs={}, data={}, num_symbols={}",
        g.shm_seqs,
        g.shm_data,
        store.num_symbols()
    );

    // Update Bitmap
    shm::bitmap::UpdateBitmap::create(&g.shm_bitmap)?;
    info!("Bitmap: {}", g.shm_bitmap);

    // Event Ring Buffer
    shm::ring_buffer::RingBuffer::create(&g.shm_events)?;
    info!("Ring Buffer: {}", g.shm_events);

    // Health Table
    shm::health::HealthTable::create(&g.shm_health)?;
    info!("Health Table: {}", g.shm_health);

    // Control Store
    shm::control::ControlStore::create(&g.shm_control)?;
    info!("Control Store: {}", g.shm_control);

    info!("All shared memory segments created successfully");
    Ok(())
}
