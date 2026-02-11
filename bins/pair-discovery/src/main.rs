//! pair-discovery — Full pipeline: REST fetch → normalize → intersect → WS validate → generate.

use std::path::Path;

use anyhow::Result;
use tracing::{info, Level};

use common::config::{AppConfig, DirectionsConfig, ExchangesConfig};
use discovery::direction_builder;
use discovery::generator;
use discovery::normalizer::SymbolRegistry;
use discovery::rest_client;
use discovery::validator;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config/config.toml".to_string());

    info!("Loading configs...");
    let config = AppConfig::load(Path::new(&config_path))?;
    let exchanges = ExchangesConfig::load(Path::new("config/exchanges.toml"))?;
    let direction_defs = DirectionsConfig::load(Path::new("config/directions.toml"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    // STEP 1: Fetch instruments from all exchanges (parallel)
    info!("Fetching instruments from all exchanges...");
    let all_instruments = rest_client::fetch_all(
        &client,
        &exchanges.exchange,
        &config.discovery.quote_filter,
    ).await;

    for (i, source) in all_instruments.iter().enumerate() {
        if !source.is_empty() {
            info!("  source {}: {} instruments", i, source.len());
        }
    }

    // STEP 2: Normalize + build global symbol list
    info!("Building global symbol registry...");
    let mut registry = SymbolRegistry::build(&all_instruments);
    info!("Unique symbols: {}", registry.num_symbols());

    // STEP 3: Build directions (intersection)
    info!("Building directions...");
    let mut directions = direction_builder::build_directions(&registry, &direction_defs.direction);
    for d in &directions {
        info!("  {} ({}): {} pairs", d.name, d.direction_id, d.symbols.len());
    }

    // STEP 4: WS validation
    info!(
        "Starting WS validation ({} sec timeout)...",
        config.discovery.validation_timeout_sec
    );
    let results = validator::validate_all(
        &registry,
        &exchanges.exchange,
        config.discovery.validation_timeout_sec,
    ).await;

    validator::apply_validation(&mut registry, &mut directions, &results);

    info!("After validation:");
    for d in &directions {
        info!("  {} ({}): {} pairs", d.name, d.direction_id, d.symbols.len());
    }

    // STEP 5: Generate configs
    let output_dir = Path::new(&config.general.generated_dir);
    info!("Generating configs in {}...", output_dir.display());
    generator::generate(&registry, &directions, &results, output_dir)?;

    // Summary
    let total_pairs: usize = directions.iter().map(|d| d.symbols.len()).sum();
    info!(
        "Discovery complete: {} symbols, {} directions, {} total pair-directions",
        registry.num_symbols(),
        directions.len(),
        total_pairs
    );

    Ok(())
}
