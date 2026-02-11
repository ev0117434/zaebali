//! WS validation: connect to each exchange's WebSocket, subscribe to all pairs,
//! receive actual bid/ask prices, validate bid > 0 && ask > 0 && bid <= ask.
//!
//! Key design decisions:
//! - Each exchange has its own parse logic that extracts (symbol, bid, ask)
//! - name_to_id mapping is ALWAYS built from exchange_name as-is (not lowercased)
//!   because different exchanges use different casing in WS responses
//! - MEXC futures uses protobuf in recent versions → skip WS, trust REST
//! - Binance uses combined stream URL (symbols in URL, no subscribe messages)
//! - Bybit/OKX use batch subscribe
//! - MEXC spot uses per-symbol subscribe with bookTicker channel (has bid/ask)

use std::collections::{HashMap, HashSet};
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

// ── Public types ──

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
    ZeroBid,
    BidAboveAsk,
    ProtobufSkipped,
}

impl std::fmt::Display for InvalidReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidReason::NoResponse => write!(f, "NoResponse"),
            InvalidReason::ZeroBid => write!(f, "ZeroBid"),
            InvalidReason::BidAboveAsk => write!(f, "BidAboveAsk"),
            InvalidReason::ProtobufSkipped => write!(f, "ProtobufSkipped"),
        }
    }
}

// ── Internal types ──

struct SourceSub {
    symbol_id: u16,
    exchange_name: String,
}

// ── Entry point ──

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

    // Build subscription lists: symbol_id → exchange_name (as-is from REST)
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
        validate_binance(0, &subs[0], binance.map(|e| e.ws_spot.as_str()), dur),
        validate_binance(1, &subs[1], binance.map(|e| e.ws_futures.as_str()), dur),
        validate_bybit(2, &subs[2], bybit.map(|e| e.ws_spot.as_str()), dur),
        validate_bybit(3, &subs[3], bybit.map(|e| e.ws_futures.as_str()), dur),
        validate_mexc_spot(4, &subs[4], mexc.map(|e| e.ws_spot.as_str()), dur),
        skip_mexc_futures(5, &subs[5]),
        validate_okx(6, &subs[6], okx.map(|e| e.ws_spot.as_str()), dur),
        validate_okx(7, &subs[7], okx.map(|e| e.ws_futures.as_str()), dur),
    );

    vec![r0, r1, r2, r3, r4, r5, r6, r7]
}

// ══════════════════════════════════════════════════════════════
// MEXC Futures — protobuf, skip WS validation, trust REST
// ══════════════════════════════════════════════════════════════

async fn skip_mexc_futures(source_id: u8, subs: &[SourceSub]) -> ValidationResult {
    info!(
        "Source {} (mexc_futures): skipping WS validation (protobuf), trusting REST ({} pairs)",
        source_id,
        subs.len()
    );
    ValidationResult {
        source_id,
        total: subs.len() as u16,
        valid: subs.iter().map(|s| s.symbol_id).collect(),
        invalid: Vec::new(),
    }
}

// ══════════════════════════════════════════════════════════════
// Binance (spot + futures) — combined stream URL, bookTicker
// ══════════════════════════════════════════════════════════════
//
// URL: wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/ethusdt@bookTicker/...
// Response: {"stream":"btcusdt@bookTicker","data":{"s":"BTCUSDT","b":"96500.00","B":"1.5","a":"96501.00","A":"0.8"}}
//
// Mapping: name_to_id key = "BTCUSDT" (uppercase, as from REST)
// Extract: data.s = "BTCUSDT" → lookup directly
// Bid: data.b (string→f64), Ask: data.a (string→f64)

async fn validate_binance(
    source_id: u8,
    subs: &[SourceSub],
    ws_url: Option<&str>,
    dur: Duration,
) -> ValidationResult {
    if subs.is_empty() || ws_url.is_none() {
        return empty_result(source_id, subs);
    }
    let ws_url = ws_url.unwrap();

    // name_to_id: "BTCUSDT" → symbol_id (as-is from REST, uppercase)
    let name_to_id: HashMap<String, u16> = subs.iter()
        .map(|s| (s.exchange_name.clone(), s.symbol_id))
        .collect();

    // Build combined stream URL with all symbols
    let streams: Vec<String> = subs.iter()
        .map(|s| format!("{}@bookTicker", s.exchange_name.to_lowercase()))
        .collect();

    // Binance limit: ~200 streams per connection. Use multiple connections if needed.
    let mut all_received: HashSet<u16> = HashSet::new();

    for chunk in streams.chunks(200) {
        let url = format!("{}?streams={}", ws_url, chunk.join("/"));
        match validate_ws_loop(source_id, &url, &name_to_id, subs.len(), dur, false, &|_| Vec::new(), parse_binance).await {
            Ok(received) => all_received.extend(received),
            Err(e) => warn!("Source {} WS chunk error: {}", source_id, e),
        }
    }

    build_result(source_id, subs, all_received)
}

fn parse_binance(text: &str, name_to_id: &HashMap<String, u16>) -> Option<u16> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let data = v.get("data")?;

    let symbol = data.get("s")?.as_str()?; // "BTCUSDT"
    let bid: f64 = data.get("b")?.as_str()?.parse().ok()?;
    let ask: f64 = data.get("a")?.as_str()?.parse().ok()?;

    if bid > 0.0 && ask > 0.0 && bid <= ask {
        name_to_id.get(symbol).copied()
    } else {
        None
    }
}

// ══════════════════════════════════════════════════════════════
// Bybit (spot + futures) — batch subscribe, tickers channel
// ══════════════════════════════════════════════════════════════
//
// Subscribe: {"op":"subscribe","args":["tickers.BTCUSDT","tickers.ETHUSDT",...]}
// Response:  {"topic":"tickers.BTCUSDT","type":"snapshot","data":{"symbol":"BTCUSDT","bid1Price":"96500","ask1Price":"96501",...}}
//
// Mapping: name_to_id key = "BTCUSDT" (as from REST)
// Extract: data.symbol = "BTCUSDT" → lookup directly
// Bid: data.bid1Price, Ask: data.ask1Price

async fn validate_bybit(
    source_id: u8,
    subs: &[SourceSub],
    ws_url: Option<&str>,
    dur: Duration,
) -> ValidationResult {
    if subs.is_empty() || ws_url.is_none() {
        return empty_result(source_id, subs);
    }
    let ws_url = ws_url.unwrap();

    let name_to_id: HashMap<String, u16> = subs.iter()
        .map(|s| (s.exchange_name.clone(), s.symbol_id))
        .collect();

    let subscribe_fn = |subs_slice: &[SourceSub]| -> Vec<String> {
        // Batch subscribe: up to 10 args per message (Bybit limit)
        let mut msgs = Vec::new();
        for chunk in subs_slice.chunks(10) {
            let args: Vec<String> = chunk.iter()
                .map(|s| format!("tickers.{}", s.exchange_name))
                .collect();
            msgs.push(serde_json::json!({
                "op": "subscribe",
                "args": args
            }).to_string());
        }
        msgs
    };

    match validate_ws_loop(source_id, ws_url, &name_to_id, subs.len(), dur, true, &subscribe_fn, parse_bybit).await {
        Ok(received) => build_result(source_id, subs, received),
        Err(e) => {
            warn!("Source {} validation failed: {}", source_id, e);
            trust_rest_result(source_id, subs)
        }
    }
}

fn parse_bybit(text: &str, name_to_id: &HashMap<String, u16>) -> Option<u16> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let data = v.get("data")?;

    // data.symbol = "BTCUSDT"
    let symbol = data.get("symbol")?.as_str()?;
    let bid: f64 = data.get("bid1Price")?.as_str()?.parse().ok()?;
    let ask: f64 = data.get("ask1Price")?.as_str()?.parse().ok()?;

    if bid > 0.0 && ask > 0.0 && bid <= ask {
        name_to_id.get(symbol).copied()
    } else {
        None
    }
}

// ══════════════════════════════════════════════════════════════
// OKX (spot + futures) — batch subscribe, tickers channel
// ══════════════════════════════════════════════════════════════
//
// Subscribe: {"op":"subscribe","args":[{"channel":"tickers","instId":"BTC-USDT"},..]}
// Response:  {"arg":{"channel":"tickers","instId":"BTC-USDT"},"data":[{"bidPx":"96500","askPx":"96501",...}]}
//
// Mapping: name_to_id key = "BTC-USDT" (spot) or "BTC-USDT-SWAP" (futures), as from REST
// Extract: arg.instId → lookup directly
// Bid: data[0].bidPx, Ask: data[0].askPx

async fn validate_okx(
    source_id: u8,
    subs: &[SourceSub],
    ws_url: Option<&str>,
    dur: Duration,
) -> ValidationResult {
    if subs.is_empty() || ws_url.is_none() {
        return empty_result(source_id, subs);
    }
    let ws_url = ws_url.unwrap();

    let name_to_id: HashMap<String, u16> = subs.iter()
        .map(|s| (s.exchange_name.clone(), s.symbol_id))
        .collect();

    let subscribe_fn = |subs_slice: &[SourceSub]| -> Vec<String> {
        let mut msgs = Vec::new();
        // OKX allows multiple args per subscribe, batch by 100
        for chunk in subs_slice.chunks(100) {
            let args: Vec<serde_json::Value> = chunk.iter()
                .map(|s| serde_json::json!({
                    "channel": "tickers",
                    "instId": s.exchange_name
                }))
                .collect();
            msgs.push(serde_json::json!({
                "op": "subscribe",
                "args": args
            }).to_string());
        }
        msgs
    };

    match validate_ws_loop(source_id, ws_url, &name_to_id, subs.len(), dur, true, &subscribe_fn, parse_okx).await {
        Ok(received) => build_result(source_id, subs, received),
        Err(e) => {
            warn!("Source {} validation failed: {}", source_id, e);
            trust_rest_result(source_id, subs)
        }
    }
}

fn parse_okx(text: &str, name_to_id: &HashMap<String, u16>) -> Option<u16> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    let inst_id = v.get("arg")?.get("instId")?.as_str()?; // "BTC-USDT" or "BTC-USDT-SWAP"
    let data = v.get("data")?.as_array()?.first()?;

    let bid: f64 = data.get("bidPx")?.as_str()?.parse().ok()?;
    let ask: f64 = data.get("askPx")?.as_str()?.parse().ok()?;

    if bid > 0.0 && ask > 0.0 && bid <= ask {
        name_to_id.get(inst_id).copied()
    } else {
        None
    }
}

// ══════════════════════════════════════════════════════════════
// MEXC Spot — per-symbol subscribe, bookTicker channel
// ══════════════════════════════════════════════════════════════
//
// Subscribe: {"method":"SUBSCRIPTION","params":["spot@public.bookTicker.v3.api@BTCUSDT"]}
// Response:  {"c":"spot@public.bookTicker.v3.api@BTCUSDT","d":{"a":"96501","b":"96500","A":"0.5","B":"1.2"},"s":"BTCUSDT","t":1234}
//
// Mapping: name_to_id key = "BTCUSDT" (as from REST)
// Extract: field "s" = "BTCUSDT" → lookup directly
// Bid: d.b, Ask: d.a

async fn validate_mexc_spot(
    source_id: u8,
    subs: &[SourceSub],
    ws_url: Option<&str>,
    dur: Duration,
) -> ValidationResult {
    if subs.is_empty() || ws_url.is_none() {
        return empty_result(source_id, subs);
    }
    let ws_url = ws_url.unwrap();

    let name_to_id: HashMap<String, u16> = subs.iter()
        .map(|s| (s.exchange_name.clone(), s.symbol_id))
        .collect();

    let subscribe_fn = |subs_slice: &[SourceSub]| -> Vec<String> {
        // MEXC spot: one subscribe message per symbol
        subs_slice.iter()
            .map(|s| serde_json::json!({
                "method": "SUBSCRIPTION",
                "params": [format!("spot@public.bookTicker.v3.api@{}", s.exchange_name)]
            }).to_string())
            .collect()
    };

    match validate_ws_loop(source_id, ws_url, &name_to_id, subs.len(), dur, true, &subscribe_fn, parse_mexc_spot).await {
        Ok(received) => build_result(source_id, subs, received),
        Err(e) => {
            warn!("Source {} validation failed: {}", source_id, e);
            trust_rest_result(source_id, subs)
        }
    }
}

fn parse_mexc_spot(text: &str, name_to_id: &HashMap<String, u16>) -> Option<u16> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    // Primary: "s" field has symbol directly
    let symbol = v.get("s")?.as_str()?; // "BTCUSDT"
    let d = v.get("d")?;

    let bid: f64 = d.get("b")?.as_str()?.parse().ok()?;
    let ask: f64 = d.get("a")?.as_str()?.parse().ok()?;

    if bid > 0.0 && ask > 0.0 && bid <= ask {
        name_to_id.get(symbol).copied()
    } else {
        None
    }
}

// ══════════════════════════════════════════════════════════════
// Generic WS validation loop
// ══════════════════════════════════════════════════════════════

async fn validate_ws_loop(
    source_id: u8,
    ws_url: &str,
    name_to_id: &HashMap<String, u16>,
    total_count: usize,
    dur: Duration,
    need_subscribe: bool,
    subscribe_fn: &dyn Fn(&[SourceSub]) -> Vec<String>,
    parse_fn: fn(&str, &HashMap<String, u16>) -> Option<u16>,
) -> Result<HashSet<u16>> {
    info!(
        "Source {}: connecting to {}...",
        source_id,
        &ws_url[..ws_url.len().min(80)]
    );

    let (ws_stream, _) = timeout(Duration::from_secs(15), connect_async(ws_url))
        .await
        .context("WS connect timeout")?
        .context("WS connect failed")?;

    let (mut write, mut read) = ws_stream.split();

    // Subscribe in batches with delay between batches
    if need_subscribe {
        // Reconstruct SourceSub from name_to_id for subscribe_fn
        let subs_for_fn: Vec<SourceSub> = name_to_id.iter()
            .map(|(name, &id)| SourceSub {
                symbol_id: id,
                exchange_name: name.clone(),
            })
            .collect();

        let msgs = subscribe_fn(&subs_for_fn);
        for (i, msg) in msgs.iter().enumerate() {
            write.send(Message::Text(msg.clone())).await?;
            // Pace subscribe messages: pause every 50 messages
            if i > 0 && i % 50 == 0 {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        info!("Source {}: sent {} subscribe messages", source_id, msgs.len());
    }

    // Collect valid symbols
    let mut received: HashSet<u16> = HashSet::new();
    let start = tokio::time::Instant::now();

    loop {
        let remaining = dur.checked_sub(start.elapsed()).unwrap_or_default();
        if remaining.is_zero() {
            break;
        }

        match timeout(remaining, read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Some(id) = parse_fn(&text, name_to_id) {
                    received.insert(id);
                }
            }
            Ok(Some(Ok(Message::Ping(data)))) => {
                let _ = write.send(Message::Pong(data)).await;
            }
            Ok(Some(Ok(Message::Binary(_)))) => {
                // Some exchanges send binary (e.g. compressed), skip
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(e))) => {
                debug!("Source {} WS error: {}", source_id, e);
                break;
            }
            Ok(None) => {
                debug!("Source {} WS stream closed", source_id);
                break;
            }
            Err(_) => break, // timeout
        }

        // Early exit if we've validated all
        if received.len() >= total_count {
            info!("Source {}: all {} symbols validated, exiting early", source_id, total_count);
            break;
        }
    }

    let elapsed = start.elapsed();
    info!(
        "Source {}: {}/{} valid ({:.1}%) in {:.1}s",
        source_id,
        received.len(),
        total_count,
        received.len() as f64 / total_count.max(1) as f64 * 100.0,
        elapsed.as_secs_f64()
    );

    Ok(received)
}

// ══════════════════════════════════════════════════════════════
// Helpers
// ══════════════════════════════════════════════════════════════

fn empty_result(source_id: u8, subs: &[SourceSub]) -> ValidationResult {
    ValidationResult {
        source_id,
        total: subs.len() as u16,
        valid: HashSet::new(),
        invalid: subs.iter().map(|s| InvalidPair {
            symbol_id: s.symbol_id,
            exchange_name: s.exchange_name.clone(),
            reason: InvalidReason::NoResponse,
        }).collect(),
    }
}

fn build_result(source_id: u8, subs: &[SourceSub], received: HashSet<u16>) -> ValidationResult {
    let invalid: Vec<InvalidPair> = subs.iter()
        .filter(|s| !received.contains(&s.symbol_id))
        .map(|s| InvalidPair {
            symbol_id: s.symbol_id,
            exchange_name: s.exchange_name.clone(),
            reason: InvalidReason::NoResponse,
        })
        .collect();

    ValidationResult {
        source_id,
        total: subs.len() as u16,
        valid: received,
        invalid,
    }
}

/// When WS validation completely fails, trust REST data (mark all valid).
fn trust_rest_result(source_id: u8, subs: &[SourceSub]) -> ValidationResult {
    warn!("Source {}: WS failed, trusting REST ({} pairs)", source_id, subs.len());
    ValidationResult {
        source_id,
        total: subs.len() as u16,
        valid: subs.iter().map(|s| s.symbol_id).collect(),
        invalid: Vec::new(),
    }
}

/// Apply validation results: remove invalid symbols from registry and directions.
pub fn apply_validation(
    registry: &mut SymbolRegistry,
    directions: &mut Vec<DirectionRecord>,
    results: &[ValidationResult],
) {
    for result in results {
        let src = result.source_id as usize;
        for inv in &result.invalid {
            registry.source_symbols[src].remove(&inv.symbol_id);
        }
    }

    for dir in directions.iter_mut() {
        let spot = dir.spot_source as usize;
        let fut = dir.futures_source as usize;
        dir.symbols.retain(|sid| {
            registry.source_symbols[spot].contains(sid)
                && registry.source_symbols[fut].contains(sid)
        });
    }
}
