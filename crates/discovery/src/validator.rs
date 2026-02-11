//! WS validation: connect to each exchange's WebSocket, subscribe to all pairs,
//! wait for responses, and mark pairs that don't respond as invalid.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use common::config::ExchangeEntry;
use common::directions::DirectionRecord;

use crate::normalizer::SymbolRegistry;

#[derive(Debug)]
pub struct ValidationResult {
    pub source_id: u8,
    pub total: u16,
    pub valid: HashSet<u16>,
    pub invalid: Vec<InvalidPair>,
}

#[derive(Debug)]
pub struct InvalidPair {
    pub symbol_id: u16,
    pub exchange_name: String,
    pub reason: InvalidReason,
}

#[derive(Debug)]
pub enum InvalidReason {
    NoResponse,
    SubscribeError,
}

impl std::fmt::Display for InvalidReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidReason::NoResponse => write!(f, "NoResponse"),
            InvalidReason::SubscribeError => write!(f, "SubscribeError"),
        }
    }
}

struct SourceSub {
    symbol_id: u16,
    exchange_name: String,
}

/// Validate all 8 sources in parallel.
pub async fn validate_all(
    registry: &SymbolRegistry,
    exchanges: &[ExchangeEntry],
    timeout_sec: u64,
) -> Vec<ValidationResult> {
    let dur = Duration::from_secs(timeout_sec);

    let binance = exchanges.iter().find(|e| e.name == "binance");
    let bybit = exchanges.iter().find(|e| e.name == "bybit");
    let okx = exchanges.iter().find(|e| e.name == "okx");
    let mexc = exchanges.iter().find(|e| e.name == "mexc");

    let subs: [Vec<SourceSub>; 8] = std::array::from_fn(|src| {
        registry.source_symbols[src]
            .iter()
            .filter_map(|&sid| {
                registry.symbols[sid as usize].source_names[src]
                    .as_ref()
                    .map(|name| SourceSub {
                        symbol_id: sid,
                        exchange_name: name.clone(),
                    })
            })
            .collect()
    });

    let (r0, r1, r2, r3, r4, r5, r6, r7) = tokio::join!(
        validate_source_safe(0, &subs[0], binance.map(|e| e.ws_spot.as_str()), dur, subscribe_binance_spot),
        validate_source_safe(1, &subs[1], binance.map(|e| e.ws_futures.as_str()), dur, subscribe_binance_futures),
        validate_source_safe(2, &subs[2], bybit.map(|e| e.ws_spot.as_str()), dur, subscribe_bybit),
        validate_source_safe(3, &subs[3], bybit.map(|e| e.ws_futures.as_str()), dur, subscribe_bybit),
        validate_source_safe(4, &subs[4], mexc.map(|e| e.ws_spot.as_str()), dur, subscribe_mexc_spot),
        validate_source_safe(5, &subs[5], mexc.map(|e| e.ws_futures.as_str()), dur, subscribe_mexc_futures),
        validate_source_safe(6, &subs[6], okx.map(|e| e.ws_spot.as_str()), dur, subscribe_okx),
        validate_source_safe(7, &subs[7], okx.map(|e| e.ws_futures.as_str()), dur, subscribe_okx),
    );

    vec![r0, r1, r2, r3, r4, r5, r6, r7]
}

type SubscribeFn = fn(&[SourceSub], usize) -> Vec<String>;

async fn validate_source_safe(
    source_id: u8,
    subs: &[SourceSub],
    ws_url: Option<&str>,
    dur: Duration,
    subscribe_fn: SubscribeFn,
) -> ValidationResult {
    if subs.is_empty() || ws_url.is_none() {
        return ValidationResult {
            source_id,
            total: subs.len() as u16,
            valid: HashSet::new(),
            invalid: subs.iter().map(|s| InvalidPair {
                symbol_id: s.symbol_id,
                exchange_name: s.exchange_name.clone(),
                reason: InvalidReason::NoResponse,
            }).collect(),
        };
    }

    match validate_source(source_id, subs, ws_url.unwrap(), dur, subscribe_fn).await {
        Ok(r) => r,
        Err(e) => {
            warn!("Validation failed for source {}: {}", source_id, e);
            ValidationResult {
                source_id,
                total: subs.len() as u16,
                valid: subs.iter().map(|s| s.symbol_id).collect(),
                invalid: Vec::new(),
            }
        }
    }
}

async fn validate_source(
    source_id: u8,
    subs: &[SourceSub],
    ws_url: &str,
    dur: Duration,
    subscribe_fn: SubscribeFn,
) -> Result<ValidationResult> {
    let all_ids: HashSet<u16> = subs.iter().map(|s| s.symbol_id).collect();
    let total = subs.len() as u16;

    // For Binance, use combined stream URL with symbols baked in
    let (connect_url, need_subscribe) = if source_id == 0 || source_id == 1 {
        // Binance combined stream: wss://stream.binance.com:9443/stream?streams=...
        let streams = build_binance_stream_params(subs, source_id);
        if streams.is_empty() {
            return Ok(ValidationResult { source_id, total, valid: HashSet::new(), invalid: Vec::new() });
        }
        // Split into chunks of 200 streams per connection, use first chunk
        let chunk: Vec<&str> = streams.iter().take(200).map(|s| s.as_str()).collect();
        let url = format!("{}?streams={}", ws_url, chunk.join("/"));
        (url, false)
    } else {
        (ws_url.to_string(), true)
    };

    info!("Connecting to WS for source {} at {}...", source_id, &connect_url[..connect_url.len().min(100)]);

    let (ws_stream, _) = timeout(Duration::from_secs(10), connect_async(&connect_url))
        .await
        .context("WS connect timeout")?
        .context("WS connect failed")?;

    let (mut write, mut read) = ws_stream.split();

    // Send subscribe messages in batches
    if need_subscribe {
        let batch_size = 100;
        for chunk_start in (0..subs.len()).step_by(batch_size) {
            let msgs = subscribe_fn(subs, chunk_start);
            for msg in msgs {
                write.send(Message::Text(msg)).await?;
            }
            if chunk_start + batch_size < subs.len() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    // Collect responses
    let mut received: HashSet<u16> = HashSet::new();
    let name_to_id: std::collections::HashMap<String, u16> = subs.iter()
        .map(|s| (s.exchange_name.to_lowercase(), s.symbol_id))
        .collect();

    let start = tokio::time::Instant::now();
    loop {
        let remaining = dur.checked_sub(start.elapsed()).unwrap_or_default();
        if remaining.is_zero() {
            break;
        }

        match timeout(remaining, read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                // Try to extract symbol from the message
                if let Some(id) = extract_symbol_id(&text, source_id, &name_to_id) {
                    received.insert(id);
                }
            }
            Ok(Some(Ok(Message::Ping(data)))) => {
                let _ = write.send(Message::Pong(data)).await;
            }
            Ok(Some(Ok(_))) => {} // Binary, Pong, etc
            Ok(Some(Err(e))) => {
                debug!("WS error source {}: {}", source_id, e);
                break;
            }
            Ok(None) => break, // Stream closed
            Err(_) => break,   // Timeout
        }

        // Early exit if we've received all
        if received.len() >= all_ids.len() {
            break;
        }
    }

    let invalid: Vec<InvalidPair> = subs.iter()
        .filter(|s| !received.contains(&s.symbol_id))
        .map(|s| InvalidPair {
            symbol_id: s.symbol_id,
            exchange_name: s.exchange_name.clone(),
            reason: InvalidReason::NoResponse,
        })
        .collect();

    let valid = received;

    info!(
        "Source {}: {}/{} valid ({:.1}%), {} invalid",
        source_id,
        valid.len(),
        total,
        valid.len() as f64 / total.max(1) as f64 * 100.0,
        invalid.len()
    );

    Ok(ValidationResult { source_id, total, valid, invalid })
}

/// Extract symbol_id from a WS message based on exchange format.
fn extract_symbol_id(
    text: &str,
    source_id: u8,
    name_to_id: &std::collections::HashMap<String, u16>,
) -> Option<u16> {
    // Quick JSON parse to find the symbol field
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    let symbol = match source_id {
        // Binance spot/futures: {"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT",...}}
        0 | 1 => {
            v.get("data").and_then(|d| d.get("s")).and_then(|s| s.as_str())
                .map(|s| s.to_lowercase())
        }
        // Bybit: {"topic":"tickers.BTCUSDT","data":{...}}
        2 | 3 => {
            v.get("topic").and_then(|t| t.as_str())
                .and_then(|t| t.strip_prefix("tickers."))
                .map(|s| s.to_lowercase())
        }
        // MEXC spot: {"s":"BTCUSDT","d":{...}} or {"c":"spot@public.miniTicker.v3.api@BTCUSDT",...}
        4 => {
            v.get("s").and_then(|s| s.as_str()).map(|s| s.to_lowercase())
                .or_else(|| {
                    v.get("c").and_then(|c| c.as_str())
                        .and_then(|c| c.rsplit('@').next())
                        .map(|s| s.to_lowercase())
                })
                .or_else(|| {
                    v.get("d").and_then(|d| d.get("s")).and_then(|s| s.as_str())
                        .map(|s| s.to_lowercase())
                })
        }
        // MEXC futures: {"symbol":"BTC_USDT","data":{...}} or {"channel":"...", "symbol":"BTC_USDT"}
        5 => {
            v.get("symbol").and_then(|s| s.as_str()).map(|s| s.to_lowercase())
                .or_else(|| {
                    v.get("data").and_then(|d| d.get("symbol")).and_then(|s| s.as_str())
                        .map(|s| s.to_lowercase())
                })
        }
        // OKX: {"arg":{"channel":"tickers","instId":"BTC-USDT"},"data":[{...}]}
        6 | 7 => {
            v.get("arg").and_then(|a| a.get("instId")).and_then(|s| s.as_str())
                .map(|s| s.to_lowercase())
        }
        _ => None,
    };

    symbol.and_then(|s| name_to_id.get(&s).copied())
}

// === Subscribe message builders ===

fn build_binance_stream_params(subs: &[SourceSub], _source_id: u8) -> Vec<String> {
    subs.iter()
        .map(|s| format!("{}@bookTicker", s.exchange_name.to_lowercase()))
        .collect()
}

fn subscribe_binance_spot(_subs: &[SourceSub], _chunk_start: usize) -> Vec<String> {
    Vec::new() // Binance uses URL-based combined streams
}

fn subscribe_binance_futures(_subs: &[SourceSub], _chunk_start: usize) -> Vec<String> {
    Vec::new()
}

fn subscribe_bybit(subs: &[SourceSub], chunk_start: usize) -> Vec<String> {
    let batch_size = 100;
    let end = (chunk_start + batch_size).min(subs.len());
    let args: Vec<String> = subs[chunk_start..end]
        .iter()
        .map(|s| format!("tickers.{}", s.exchange_name))
        .collect();

    vec![serde_json::json!({
        "op": "subscribe",
        "args": args
    }).to_string()]
}

fn subscribe_mexc_spot(subs: &[SourceSub], chunk_start: usize) -> Vec<String> {
    let batch_size = 100;
    let end = (chunk_start + batch_size).min(subs.len());
    subs[chunk_start..end]
        .iter()
        .map(|s| serde_json::json!({
            "method": "SUBSCRIPTION",
            "params": [format!("spot@public.miniTicker.v3.api@{}", s.exchange_name)]
        }).to_string())
        .collect()
}

fn subscribe_mexc_futures(subs: &[SourceSub], chunk_start: usize) -> Vec<String> {
    let batch_size = 100;
    let end = (chunk_start + batch_size).min(subs.len());
    subs[chunk_start..end]
        .iter()
        .map(|s| serde_json::json!({
            "method": "sub.ticker",
            "param": { "symbol": s.exchange_name }
        }).to_string())
        .collect()
}

fn subscribe_okx(subs: &[SourceSub], chunk_start: usize) -> Vec<String> {
    let batch_size = 100;
    let end = (chunk_start + batch_size).min(subs.len());
    let args: Vec<serde_json::Value> = subs[chunk_start..end]
        .iter()
        .map(|s| serde_json::json!({
            "channel": "tickers",
            "instId": s.exchange_name
        }))
        .collect();

    vec![serde_json::json!({
        "op": "subscribe",
        "args": args
    }).to_string()]
}

/// Apply validation results: remove invalid symbols from registry and directions.
pub fn apply_validation(
    registry: &mut SymbolRegistry,
    directions: &mut Vec<DirectionRecord>,
    results: &[ValidationResult],
) {
    // Remove invalid symbol_ids from each source
    for result in results {
        let src = result.source_id as usize;
        for inv in &result.invalid {
            registry.source_symbols[src].remove(&inv.symbol_id);
        }
    }

    // Recompute direction symbol lists: only keep symbols valid on BOTH sources
    for dir in directions.iter_mut() {
        let spot = dir.spot_source as usize;
        let fut = dir.futures_source as usize;
        dir.symbols.retain(|sid| {
            registry.source_symbols[spot].contains(sid)
                && registry.source_symbols[fut].contains(sid)
        });
    }
}
