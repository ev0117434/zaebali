use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Top-level application config — loaded from config/config.toml
#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub general: GeneralConfig,
    pub spread: SpreadConfig,
    pub tracker: TrackerConfig,
    pub ws: WsConfig,
    pub engine: EngineConfig,
    pub discovery: DiscoveryConfig,
    pub monitoring: MonitoringConfig,
}

#[derive(Debug, Deserialize)]
pub struct GeneralConfig {
    pub log_level: String,
    pub output_dir: String,
    pub generated_dir: String,
    pub shm_seqs: String,
    pub shm_data: String,
    pub shm_bitmap: String,
    pub shm_events: String,
    pub shm_health: String,
    pub shm_control: String,
}

#[derive(Debug, Deserialize)]
pub struct SpreadConfig {
    pub min_spread_threshold_pct: f64,
    pub staleness_max_ms: u64,
    pub converge_threshold_pct: f64,
}

#[derive(Debug, Deserialize)]
pub struct TrackerConfig {
    pub snapshot_interval_ms: u64,
    pub tracking_duration_hours: u64,
    pub delta_write_threshold_pct: f64,
    pub heartbeat_write_sec: u64,
    pub max_file_size_mb: u64,
}

#[derive(Debug, Deserialize)]
pub struct WsConfig {
    pub max_subscriptions_per_conn: usize,
    pub ping_interval_sec: u64,
    pub heartbeat_timeout_sec: u64,
    pub reconnect_base_ms: u64,
    pub reconnect_max_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct EngineConfig {
    pub notification_mode: String,
    pub eventfd_coalesce_us: u64,
}

#[derive(Debug, Deserialize)]
pub struct DiscoveryConfig {
    pub validation_timeout_sec: u64,
    pub quote_filter: Vec<String>,
    pub min_status: String,
    pub cron_interval_hours: u64,
}

#[derive(Debug, Deserialize)]
pub struct MonitoringConfig {
    pub prometheus_enabled: bool,
    pub stats_log_interval_sec: u64,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let config: AppConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse config: {}", path.display()))?;
        Ok(config)
    }
}

// === Exchange Config ===

#[derive(Debug, Deserialize)]
pub struct ExchangesConfig {
    pub exchange: Vec<ExchangeEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ExchangeEntry {
    pub name: String,
    pub rest_spot: String,
    pub rest_futures: String,
    pub ws_spot: String,
    pub ws_futures: String,
    pub max_ws_subscriptions: usize,
    pub instruments_path_spot: String,
    pub instruments_path_futures: String,
}

impl ExchangesConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read exchanges config: {}", path.display()))?;
        let config: ExchangesConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse exchanges config: {}", path.display()))?;
        Ok(config)
    }
}

// === Direction Config ===

#[derive(Debug, Deserialize)]
pub struct DirectionsConfig {
    pub direction: Vec<DirectionConfigEntry>,
}

#[derive(Debug, Deserialize)]
pub struct DirectionConfigEntry {
    pub id: u8,
    pub spot_source: u8,
    pub futures_source: u8,
    pub name: String,
}

impl DirectionsConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read directions config: {}", path.display()))?;
        let config: DirectionsConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse directions config: {}", path.display()))?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_config_deserialize() {
        let toml_str = r#"
[general]
log_level = "info"
output_dir = "output"
generated_dir = "generated"
shm_seqs = "spread-scanner-seqs"
shm_data = "spread-scanner-data"
shm_bitmap = "spread-scanner-bitmap"
shm_events = "spread-scanner-events"
shm_health = "spread-scanner-health"
shm_control = "spread-scanner-control"

[spread]
min_spread_threshold_pct = 0.3
staleness_max_ms = 5000
converge_threshold_pct = 0.05

[tracker]
snapshot_interval_ms = 200
tracking_duration_hours = 3
delta_write_threshold_pct = 0.01
heartbeat_write_sec = 60
max_file_size_mb = 100

[ws]
max_subscriptions_per_conn = 200
ping_interval_sec = 20
heartbeat_timeout_sec = 30
reconnect_base_ms = 100
reconnect_max_ms = 30000

[engine]
notification_mode = "eventfd"
eventfd_coalesce_us = 200

[discovery]
validation_timeout_sec = 30
quote_filter = ["USDT"]
min_status = "TRADING"
cron_interval_hours = 6

[monitoring]
prometheus_enabled = false
stats_log_interval_sec = 10
"#;
        let config: AppConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.spread.min_spread_threshold_pct, 0.3);
        assert_eq!(config.ws.max_subscriptions_per_conn, 200);
        assert_eq!(config.discovery.quote_filter, vec!["USDT"]);
    }
}
