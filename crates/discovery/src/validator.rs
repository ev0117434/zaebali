use crate::normalizer::SymbolRegistry;
use crate::types::{DiscoveryError, Result};
use common::symbols::SymbolRecord;
use common::types::SourceId;
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// Configuration for WS validation
#[derive(Debug, Clone)]
pub struct WsValidationConfig {
    pub batch_timeout_secs: u64,      // Max time for entire batch (default: 90)
    pub collect_duration_secs: u64,   // How long to collect data (default: 30)
    pub idle_timeout_secs: u64,       // Max time without new updates (default: 10)
}

impl Default for WsValidationConfig {
    fn default() -> Self {
        Self {
            batch_timeout_secs: 90,
            collect_duration_secs: 30,
            idle_timeout_secs: 10,
        }
    }
}

/// Validation result for a single source
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub source: SourceId,
    pub total: usize,
    pub valid: HashSet<u16>,
    pub invalid: Vec<InvalidSymbol>,
    pub stats: ValidationStats,
}

#[derive(Debug, Clone)]
pub struct InvalidSymbol {
    pub symbol_id: u16,
    pub exchange_symbol: String,
    pub reason: InvalidReason,
}

#[derive(Debug, Clone)]
pub enum InvalidReason {
    NoData,
    InvalidPrices,
    SubscribeError,
}

#[derive(Debug, Clone, Default)]
pub struct ValidationStats {
    pub total: usize,
    pub received: usize,
    pub valid: usize,
    pub no_data: usize,
    pub invalid_prices: usize,
    pub failed_batches: usize,
    pub duration_secs: u64,
}

/// Get batch size per exchange (based on connection limits)
fn get_batch_size(source: SourceId) -> usize {
    match source {
        SourceId::BinanceSpot | SourceId::BinanceFutures => 200,
        SourceId::OkxSpot | SourceId::OkxFutures => 100,
        SourceId::BybitSpot | SourceId::BybitFutures => 50,
        SourceId::MexcSpot | SourceId::MexcFutures => 30,
    }
}

/// Validate all symbols for a given source via WebSocket
/// This is an MVP version - simplified validation without full feed parser reuse
pub async fn validate_source(
    source: SourceId,
    symbols: &[SymbolRecord],
    ws_url: &str,
    config: &WsValidationConfig,
) -> Result<ValidationResult> {
    let batch_size = get_batch_size(source);
    let symbol_map = build_symbol_map(source, symbols);

    if symbol_map.is_empty() {
        info!("{:?}: no symbols to validate", source);
        return Ok(ValidationResult {
            source,
            total: 0,
            valid: HashSet::new(),
            invalid: Vec::new(),
            stats: ValidationStats::default(),
        });
    }

    let symbols_list: Vec<_> = symbol_map.keys().cloned().collect();
    let batches: Vec<_> = symbols_list.chunks(batch_size).collect();

    let mut all_valid = HashSet::new();
    let mut all_invalid = Vec::new();
    let mut stats = ValidationStats {
        total: symbol_map.len(),
        ..Default::default()
    };

    info!(
        "{:?}: validating {} symbols in {} batches (batch_size={})",
        source,
        symbol_map.len(),
        batches.len(),
        batch_size
    );

    for (batch_idx, batch) in batches.iter().enumerate() {
        info!(
            "{:?}: batch {}/{} ({} symbols)",
            source,
            batch_idx + 1,
            batches.len(),
            batch.len()
        );

        match validate_batch_with_timeout(source, *batch, &symbol_map, ws_url, config).await {
            Ok(result) => {
                all_valid.extend(result.valid);
                all_invalid.extend(result.invalid);
                stats.received += result.stats.received;
                stats.valid += result.stats.valid;
                stats.no_data += result.stats.no_data;
                stats.invalid_prices += result.stats.invalid_prices;
            }
            Err(e) => {
                error!("{:?}: batch {} failed: {}", source, batch_idx + 1, e);
                stats.failed_batches += 1;

                // Mark all symbols in this batch as invalid
                for exchange_symbol in *batch {
                    if let Some(&symbol_id) = symbol_map.get(exchange_symbol) {
                        all_invalid.push(InvalidSymbol {
                            symbol_id,
                            exchange_symbol: exchange_symbol.to_string(),
                            reason: InvalidReason::NoData,
                        });
                    }
                }
            }
        }

        // Rate limiting between batches
        if batch_idx < batches.len() - 1 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    info!(
        "{:?}: validation complete - {}/{} valid ({:.1}%)",
        source,
        all_valid.len(),
        symbol_map.len(),
        (all_valid.len() as f64 / symbol_map.len() as f64) * 100.0
    );

    Ok(ValidationResult {
        source,
        total: symbol_map.len(),
        valid: all_valid,
        invalid: all_invalid,
        stats,
    })
}

/// Build map: exchange_symbol → symbol_id for a given source
fn build_symbol_map(source: SourceId, symbols: &[SymbolRecord]) -> HashMap<String, u16> {
    let idx = source.index();
    symbols
        .iter()
        .filter_map(|rec| {
            rec.source_names[idx]
                .as_ref()
                .map(|name| (name.clone(), rec.symbol_id))
        })
        .collect()
}

/// Validate a single batch with overall timeout
async fn validate_batch_with_timeout(
    source: SourceId,
    batch: &[String],
    symbol_map: &HashMap<String, u16>,
    ws_url: &str,
    config: &WsValidationConfig,
) -> Result<BatchValidationResult> {
    match tokio::time::timeout(
        Duration::from_secs(config.batch_timeout_secs),
        validate_batch_inner(source, batch, symbol_map, ws_url, config),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(DiscoveryError::ValidationFailed {
            reason: format!("{:?} batch timeout after {}s", source, config.batch_timeout_secs),
        }),
    }
}

#[derive(Debug)]
struct BatchValidationResult {
    valid: HashSet<u16>,
    invalid: Vec<InvalidSymbol>,
    stats: ValidationStats,
}

/// Validate a single batch (inner implementation)
/// MVP version: just check that we receive ANY data for the symbol
async fn validate_batch_inner(
    source: SourceId,
    batch: &[String],
    symbol_map: &HashMap<String, u16>,
    ws_url: &str,
    config: &WsValidationConfig,
) -> Result<BatchValidationResult> {
    let start = Instant::now();

    // Connect to WS
    debug!("{:?}: connecting to {}", source, ws_url);
    let (ws_stream, _) = connect_async(ws_url)
        .await
        .map_err(|e| DiscoveryError::ValidationFailed {
            reason: format!("WS connection failed: {}", e),
        })?;

    let (mut write, mut read) = ws_stream.split();

    // Build subscription message
    let sub_msg = build_subscription_message(source, batch)?;
    debug!("{:?}: subscribing to {} symbols", source, batch.len());

    write
        .send(Message::Text(sub_msg))
        .await
        .map_err(|e| DiscoveryError::ValidationFailed {
            reason: format!("Failed to send subscription: {}", e),
        })?;

    // Collect updates
    let mut received_symbols = HashSet::new();
    let mut last_update = Instant::now();
    let collect_duration = Duration::from_secs(config.collect_duration_secs);
    let idle_timeout = Duration::from_secs(config.idle_timeout_secs);

    loop {
        // Check timeouts
        if start.elapsed() > collect_duration {
            debug!("{:?}: collect duration reached", source);
            break;
        }

        if last_update.elapsed() > idle_timeout {
            debug!("{:?}: idle timeout reached", source);
            break;
        }

        // Read message with timeout
        match tokio::time::timeout(Duration::from_secs(1), read.next()).await {
            Ok(Some(Ok(msg))) => {
                if let Ok(symbol) = parse_symbol_from_message(source, &msg, symbol_map) {
                    if let Some(symbol_id) = symbol {
                        received_symbols.insert(symbol_id);
                        last_update = Instant::now();

                        // Early exit if all symbols received
                        if received_symbols.len() == batch.len() {
                            info!("{:?}: all {} symbols validated", source, batch.len());
                            break;
                        }
                    }
                }
            }
            Ok(Some(Err(e))) => {
                warn!("{:?}: WebSocket error: {}", source, e);
            }
            Ok(None) => {
                warn!("{:?}: connection closed", source);
                break;
            }
            Err(_) => {
                // Timeout on read - this is normal, continue
            }
        }
    }

    // Determine valid/invalid symbols
    let mut valid = HashSet::new();
    let mut invalid = Vec::new();

    for exchange_symbol in batch {
        if let Some(&symbol_id) = symbol_map.get(exchange_symbol) {
            if received_symbols.contains(&symbol_id) {
                valid.insert(symbol_id);
            } else {
                invalid.push(InvalidSymbol {
                    symbol_id,
                    exchange_symbol: exchange_symbol.to_string(),
                    reason: InvalidReason::NoData,
                });
            }
        }
    }

    let stats = ValidationStats {
        total: batch.len(),
        received: received_symbols.len(),
        valid: valid.len(),
        no_data: invalid.len(),
        invalid_prices: 0,
        failed_batches: 0,
        duration_secs: start.elapsed().as_secs(),
    };

    Ok(BatchValidationResult {
        valid,
        invalid,
        stats,
    })
}

/// Build subscription message for a given source
fn build_subscription_message(source: SourceId, symbols: &[String]) -> Result<String> {
    match source {
        SourceId::BinanceSpot | SourceId::BinanceFutures => {
            // Binance: {"method":"SUBSCRIBE","params":["btcusdt@bookTicker"],"id":1}
            let streams: Vec<String> = symbols
                .iter()
                .map(|s| format!("{}@bookTicker", s.to_lowercase()))
                .collect();
            let msg = serde_json::json!({
                "method": "SUBSCRIBE",
                "params": streams,
                "id": 1
            });
            Ok(msg.to_string())
        }
        SourceId::BybitSpot | SourceId::BybitFutures => {
            // Bybit: {"op":"subscribe","args":["tickers.BTCUSDT"]}
            let args: Vec<String> = symbols.iter().map(|s| format!("tickers.{}", s)).collect();
            let msg = serde_json::json!({
                "op": "subscribe",
                "args": args
            });
            Ok(msg.to_string())
        }
        SourceId::OkxSpot | SourceId::OkxFutures => {
            // OKX: {"op":"subscribe","args":[{"channel":"tickers","instId":"BTC-USDT"}]}
            let args: Vec<serde_json::Value> = symbols
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "channel": "tickers",
                        "instId": s
                    })
                })
                .collect();
            let msg = serde_json::json!({
                "op": "subscribe",
                "args": args
            });
            Ok(msg.to_string())
        }
        SourceId::MexcSpot | SourceId::MexcFutures => {
            // MEXC: {"method":"SUBSCRIPTION","params":["spot@public.bookTicker.v3.api@BTCUSDT"]}
            let params: Vec<String> = symbols
                .iter()
                .map(|s| format!("spot@public.bookTicker.v3.api@{}", s.to_uppercase()))
                .collect();
            let msg = serde_json::json!({
                "method": "SUBSCRIPTION",
                "params": params
            });
            Ok(msg.to_string())
        }
    }
}

/// Parse symbol from WebSocket message (MVP: just detect if it's about a symbol we care about)
fn parse_symbol_from_message(
    source: SourceId,
    msg: &Message,
    symbol_map: &HashMap<String, u16>,
) -> Result<Option<u16>> {
    let text = match msg {
        Message::Text(t) => t,
        _ => return Ok(None),
    };

    // Try to parse as JSON
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    // Extract symbol based on exchange format
    let symbol_str = match source {
        SourceId::BinanceSpot | SourceId::BinanceFutures => {
            // {"s":"BTCUSDT","b":"...","a":"..."}
            value["s"].as_str()
        }
        SourceId::BybitSpot | SourceId::BybitFutures => {
            // {"topic":"tickers.BTCUSDT","data":{"symbol":"BTCUSDT",...}}
            value["data"]["symbol"].as_str()
        }
        SourceId::OkxSpot | SourceId::OkxFutures => {
            // {"arg":{"channel":"tickers","instId":"BTC-USDT"},"data":[...]}
            value["arg"]["instId"].as_str()
        }
        SourceId::MexcSpot | SourceId::MexcFutures => {
            // MEXC format varies, try both
            value["s"]
                .as_str()
                .or_else(|| value["symbol"].as_str())
        }
    };

    if let Some(symbol) = symbol_str {
        Ok(symbol_map.get(symbol).copied())
    } else {
        Ok(None)
    }
}

/// Validate all sources in parallel
pub async fn validate_all_sources(
    registry: &SymbolRegistry,
    ws_urls: &[(SourceId, String)],
    config: &WsValidationConfig,
) -> Vec<Result<ValidationResult>> {
    let mut tasks = Vec::new();

    for (source, ws_url) in ws_urls {
        let source = *source;
        let ws_url = ws_url.clone();
        let symbols = registry.symbols.clone();
        let config = config.clone();

        let task = tokio::spawn(async move {
            validate_source(source, &symbols, &ws_url, &config).await
        });

        tasks.push(task);
    }

    let mut results = Vec::new();
    for task in tasks {
        match task.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(Err(DiscoveryError::ValidationFailed {
                reason: format!("Task failed: {}", e),
            })),
        }
    }

    results
}

/// Check if we have minimum required valid sources (6/8)
pub fn check_minimum_validation(results: &[Result<ValidationResult>]) -> Result<()> {
    let successful = results.iter().filter(|r| r.is_ok()).count();
    const MIN_SOURCES: usize = 6;

    if successful < MIN_SOURCES {
        return Err(DiscoveryError::InsufficientValidation {
            successful,
            required: MIN_SOURCES,
        });
    }

    info!(
        "Validation complete: {}/{} sources successful",
        successful,
        results.len()
    );

    Ok(())
}
