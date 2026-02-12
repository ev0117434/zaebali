mod generate;
mod normalize;
mod rest;
mod types;
mod validate;

use anyhow::{Context, Result};
use common::config::{AppConfig, DirectionsConfig, ExchangeEntry, ExchangesConfig};
use std::path::Path;

pub use crate::types::{
    DirectionData, NormalizedPair, RawInstrument, SymbolRegistry, ValidatedRegistry,
    ValidationStats,
};

pub fn run_discovery(
    app_config: &AppConfig,
    exchanges: &ExchangesConfig,
    directions: &DirectionsConfig,
    output_dir: &Path,
) -> Result<ValidatedRegistry> {
    let fetched = rest::fetch_all_sources(exchanges, &app_config.discovery.quote_filter)?;

    let all_normalized = normalize::normalize_all(&fetched);
    let registry = normalize::build_global_list(&all_normalized)?;
    let direction_data = normalize::build_directions(&registry, &directions.direction)?;

    let validated = validate::validate_all(
        registry,
        direction_data,
        exchanges,
        app_config.discovery.validation_timeout_sec,
    )?;

    generate::generate_configs(&validated, directions, output_dir).with_context(|| {
        format!(
            "failed to write generated files into {}",
            output_dir.display()
        )
    })?;

    Ok(validated)
}

pub fn source_from_exchange_market(
    exchange: &str,
    is_futures: bool,
) -> Option<common::types::SourceId> {
    use common::types::SourceId::*;
    match (exchange, is_futures) {
        ("binance", false) => Some(BinanceSpot),
        ("binance", true) => Some(BinanceFutures),
        ("bybit", false) => Some(BybitSpot),
        ("bybit", true) => Some(BybitFutures),
        ("mexc", false) => Some(MexcSpot),
        ("mexc", true) => Some(MexcFutures),
        ("okx", false) => Some(OkxSpot),
        ("okx", true) => Some(OkxFutures),
        _ => None,
    }
}

pub fn expected_sources(
    exchanges: &[ExchangeEntry],
) -> Result<Vec<(common::types::SourceId, String)>> {
    let mut out = Vec::with_capacity(exchanges.len() * 2);
    for ex in exchanges {
        let spot = source_from_exchange_market(&ex.name, false)
            .with_context(|| format!("unknown exchange {}", ex.name))?;
        let fut = source_from_exchange_market(&ex.name, true)
            .with_context(|| format!("unknown exchange {}", ex.name))?;
        out.push((spot, ex.name.clone()));
        out.push((fut, ex.name.clone()));
    }
    Ok(out)
}
