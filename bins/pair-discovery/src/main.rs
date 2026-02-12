use anyhow::{Context, Result};
use common::config::{AppConfig, DirectionsConfig, ExchangesConfig};
use common::types::SourceId;
use discovery::{
    direction_builder, generator, normalizer, rest_client, validator,
};
use std::collections::HashSet;
use std::path::PathBuf;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("=== Pair Discovery Starting ===");

    let start = std::time::Instant::now();

    // 1. Load configs
    info!("Loading configuration files...");
    let app_config = AppConfig::load(&PathBuf::from("config/config.toml"))
        .context("Failed to load config.toml")?;

    let exchanges_config = ExchangesConfig::load(&PathBuf::from("config/exchanges.toml"))
        .context("Failed to load exchanges.toml")?;

    let directions_config = DirectionsConfig::load(&PathBuf::from("config/directions.toml"))
        .context("Failed to load directions.toml")?;

    info!(
        "Loaded configs: {} exchanges, {} directions",
        exchanges_config.exchange.len(),
        directions_config.direction.len()
    );

    // 2. Fetch instruments from all 8 sources (REST API with retry)
    info!("Fetching instruments from all exchanges (REST API)...");

    let fetch_results = fetch_all_instruments(&exchanges_config).await;

    // Check minimum sources (6/8)
    rest_client::check_minimum_sources(&fetch_results)?;

    // Extract successful results
    let mut all_sources = Vec::new();
    for result in fetch_results {
        match result {
            Ok((source_id, instruments)) => {
                info!(
                    "{:?}: fetched {} instruments",
                    source_id,
                    instruments.len()
                );
                all_sources.push((source_id, instruments));
            }
            Err(e) => {
                warn!("Source fetch failed (graceful degradation): {}", e);
            }
        }
    }

    // 3. Normalize and build global symbol list (ДЕТЕРМИНИРОВАННЫЙ!)
    info!("Normalizing symbols and building global list...");
    let mut registry =
        normalizer::build_global_list(&all_sources).context("Failed to build global list")?;

    info!(
        "Built global list: {} unique symbols",
        registry.symbols.len()
    );

    // 4. Build directions (intersections)
    info!("Building direction lists...");
    let mut directions =
        direction_builder::build_directions(&registry, &directions_config.direction);

    for dir in &directions {
        info!(
            "Direction {}: {} → {} pairs",
            dir.direction_id, dir.name, dir.symbols.len()
        );
    }

    // 5. WebSocket validation (with graceful degradation)
    info!("Starting WebSocket validation...");

    let ws_config = validator::WsValidationConfig {
        batch_timeout_secs: app_config.discovery.validation_timeout_sec,
        collect_duration_secs: 30,
        idle_timeout_secs: 10,
    };

    let ws_urls = build_ws_urls(&exchanges_config);
    let validation_results =
        validator::validate_all_sources(&registry, &ws_urls, &ws_config).await;

    // Check minimum validation (6/8)
    validator::check_minimum_validation(&validation_results)?;

    // Extract successful validation results and update registry
    let mut successful_validations = Vec::new();
    let mut invalid_per_source: [HashSet<u16>; 8] = std::array::from_fn(|_| HashSet::new());

    for result in validation_results {
        match result {
            Ok(validation) => {
                let valid_pct = if validation.total > 0 {
                    (validation.valid.len() as f64 / validation.total as f64) * 100.0
                } else {
                    0.0
                };

                info!(
                    "{:?}: {}/{} valid ({:.1}%)",
                    validation.source,
                    validation.valid.len(),
                    validation.total,
                    valid_pct
                );

                // Mark invalid symbols
                let source_idx = validation.source.index();
                for invalid_sym in &validation.invalid {
                    invalid_per_source[source_idx].insert(invalid_sym.symbol_id);
                }

                successful_validations.push(validation);
            }
            Err(e) => {
                warn!("Validation failed (graceful degradation): {}", e);
            }
        }
    }

    // 6. Remove invalid symbols and recalculate directions
    info!("Removing invalid symbols and recalculating directions...");
    direction_builder::remove_invalid_symbols(&mut registry, &invalid_per_source);
    directions = direction_builder::recalculate_directions(&registry, &directions_config.direction);

    info!(
        "After validation: {} symbols, {} directions",
        registry.symbols.len(),
        directions.len()
    );

    // 7. Generate configuration files (АТОМАРНАЯ запись!)
    info!("Generating configuration files...");
    let output_dir = PathBuf::from(&app_config.general.generated_dir);
    generator::generate_configs(&registry, &directions, &successful_validations, &output_dir)
        .context("Failed to generate configs")?;

    // 8. Summary
    let duration = start.elapsed();
    info!("=== Discovery Complete ===");
    info!("Duration: {:.1}s", duration.as_secs_f64());
    info!("Unique symbols: {}", registry.symbols.len());
    info!("Directions: {}", directions.len());
    info!(
        "Total pair-directions: {}",
        directions.iter().map(|d| d.symbols.len()).sum::<usize>()
    );
    info!("Output: {}", output_dir.display());

    Ok(())
}

/// Fetch instruments from all 8 sources in parallel
async fn fetch_all_instruments(
    exchanges_config: &ExchangesConfig,
) -> Vec<discovery::types::Result<(SourceId, Vec<discovery::types::RawInstrument>)>> {
    // Find exchange configs
    let binance = exchanges_config
        .exchange
        .iter()
        .find(|e| e.name == "binance")
        .expect("binance config not found");

    let bybit = exchanges_config
        .exchange
        .iter()
        .find(|e| e.name == "bybit")
        .expect("bybit config not found");

    let okx = exchanges_config
        .exchange
        .iter()
        .find(|e| e.name == "okx")
        .expect("okx config not found");

    let mexc = exchanges_config
        .exchange
        .iter()
        .find(|e| e.name == "mexc")
        .expect("mexc config not found");

    rest_client::fetch_all_sources(
        &binance.rest_spot,
        &binance.rest_futures,
        &binance.instruments_path_spot,
        &binance.instruments_path_futures,
        &bybit.rest_spot,
        &bybit.instruments_path_spot,
        &bybit.instruments_path_futures,
        &okx.rest_spot,
        &okx.instruments_path_spot,
        &okx.instruments_path_futures,
        &mexc.rest_spot,
        &mexc.rest_futures,
        &mexc.instruments_path_spot,
        &mexc.instruments_path_futures,
    )
    .await
}

/// Build WebSocket URLs for all 8 sources
fn build_ws_urls(exchanges_config: &ExchangesConfig) -> Vec<(SourceId, String)> {
    let binance = exchanges_config
        .exchange
        .iter()
        .find(|e| e.name == "binance")
        .expect("binance config not found");

    let bybit = exchanges_config
        .exchange
        .iter()
        .find(|e| e.name == "bybit")
        .expect("bybit config not found");

    let okx = exchanges_config
        .exchange
        .iter()
        .find(|e| e.name == "okx")
        .expect("okx config not found");

    let mexc = exchanges_config
        .exchange
        .iter()
        .find(|e| e.name == "mexc")
        .expect("mexc config not found");

    vec![
        (SourceId::BinanceSpot, binance.ws_spot.clone()),
        (SourceId::BinanceFutures, binance.ws_futures.clone()),
        (SourceId::BybitSpot, bybit.ws_spot.clone()),
        (SourceId::BybitFutures, bybit.ws_futures.clone()),
        (SourceId::MexcSpot, mexc.ws_spot.clone()),
        (SourceId::MexcFutures, mexc.ws_futures.clone()),
        (SourceId::OkxSpot, okx.ws_spot.clone()),
        (SourceId::OkxFutures, okx.ws_futures.clone()),
    ]
}
