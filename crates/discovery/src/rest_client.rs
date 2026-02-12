use crate::types::{DiscoveryError, Exchange, InstrumentStatus, Market, RawInstrument, Result};
use common::types::SourceId;
use reqwest::Client;
use std::time::Duration;
use tracing::{debug, error, info, warn};

const MAX_RETRIES: u32 = 3;
const BASE_BACKOFF_MS: u64 = 100;
const REQUEST_TIMEOUT_SECS: u64 = 10;

/// Fetch instruments from a source with retry logic
pub async fn fetch_instruments_with_retry(
    client: &Client,
    exchange: Exchange,
    market: Market,
    base_url: &str,
    path: &str,
) -> Result<Vec<RawInstrument>> {
    let mut attempt = 0;

    loop {
        match fetch_instruments_once(client, exchange, market, base_url, path).await {
            Ok(instruments) => {
                info!(
                    "{:?} {}: fetched {} instruments",
                    exchange,
                    market.name(),
                    instruments.len()
                );
                return Ok(instruments);
            }
            Err(e) if attempt < MAX_RETRIES => {
                let backoff = Duration::from_millis(BASE_BACKOFF_MS * 2u64.pow(attempt));
                warn!(
                    "{:?} {} fetch failed (attempt {}/{}), retry after {:?}: {}",
                    exchange,
                    market.name(),
                    attempt + 1,
                    MAX_RETRIES,
                    backoff,
                    e
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
            Err(e) => {
                error!(
                    "{:?} {} fetch failed after {} retries: {}",
                    exchange,
                    market.name(),
                    MAX_RETRIES,
                    e
                );
                return Err(DiscoveryError::RestFailed {
                    exchange,
                    market,
                    source: e.into(),
                });
            }
        }
    }
}

async fn fetch_instruments_once(
    client: &Client,
    exchange: Exchange,
    market: Market,
    base_url: &str,
    path: &str,
) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", base_url, path);
    debug!("Fetching from {}", url);

    let response = client
        .get(&url)
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        return Err(DiscoveryError::Other(anyhow::anyhow!(
            "HTTP error: {}",
            status
        )));
    }

    let body = response.text().await?;

    match exchange {
        Exchange::Binance => parse_binance(&body, market),
        Exchange::Bybit => parse_bybit(&body, market),
        Exchange::OKX => parse_okx(&body, market),
        Exchange::MEXC => parse_mexc(&body, market),
    }
}

// === Binance ===

fn parse_binance(body: &str, market: Market) -> Result<Vec<RawInstrument>> {
    let value: serde_json::Value = serde_json::from_str(body)?;

    let symbols = value["symbols"]
        .as_array()
        .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing symbols array")))?;

    let mut instruments = Vec::new();

    for symbol in symbols {
        // Filter by status
        let status_str = symbol["status"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing status")))?;

        if status_str != "TRADING" {
            continue;
        }

        // For futures, also filter by contractType
        if market == Market::Futures {
            if let Some(contract_type) = symbol["contractType"].as_str() {
                if contract_type != "PERPETUAL" {
                    continue;
                }
            } else {
                continue;
            }
        }

        let exchange_symbol = symbol["symbol"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing symbol")))?
            .to_string();

        let base_asset = symbol["baseAsset"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing baseAsset")))?
            .to_string();

        let quote_asset = symbol["quoteAsset"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing quoteAsset")))?
            .to_string();

        // Only USDT pairs
        if quote_asset != "USDT" {
            continue;
        }

        // Extract filters
        let mut min_qty = None;
        let mut max_qty = None;
        let mut tick_size = None;
        let mut min_notional = None;

        if let Some(filters) = symbol["filters"].as_array() {
            for filter in filters {
                if let Some(filter_type) = filter["filterType"].as_str() {
                    match filter_type {
                        "LOT_SIZE" => {
                            if let Some(v) = filter["minQty"].as_str() {
                                min_qty = v.parse().ok();
                            }
                            if let Some(v) = filter["maxQty"].as_str() {
                                max_qty = v.parse().ok();
                            }
                        }
                        "PRICE_FILTER" => {
                            if let Some(v) = filter["tickSize"].as_str() {
                                tick_size = v.parse().ok();
                            }
                        }
                        "MIN_NOTIONAL" | "NOTIONAL" => {
                            if let Some(v) = filter["minNotional"].as_str() {
                                min_notional = v.parse().ok();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        instruments.push(RawInstrument {
            exchange_symbol,
            base_asset,
            quote_asset,
            status: InstrumentStatus::Trading,
            min_qty,
            max_qty,
            tick_size,
            min_notional,
        });
    }

    Ok(instruments)
}

// === Bybit ===

fn parse_bybit(body: &str, _market: Market) -> Result<Vec<RawInstrument>> {
    let value: serde_json::Value = serde_json::from_str(body)?;

    let list = value["result"]["list"]
        .as_array()
        .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing result.list")))?;

    let mut instruments = Vec::new();

    for item in list {
        let status_str = item["status"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing status")))?;

        if status_str != "Trading" {
            continue;
        }

        let exchange_symbol = item["symbol"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing symbol")))?
            .to_string();

        let base_asset = item["baseCoin"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing baseCoin")))?
            .to_string();

        let quote_asset = item["quoteCoin"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing quoteCoin")))?
            .to_string();

        // Only USDT pairs
        if quote_asset != "USDT" {
            continue;
        }

        // Extract trading parameters
        let tick_size = item["priceFilter"]["tickSize"]
            .as_str()
            .and_then(|s| s.parse().ok());

        let min_qty = item["lotSizeFilter"]["minOrderQty"]
            .as_str()
            .and_then(|s| s.parse().ok());

        let max_qty = item["lotSizeFilter"]["maxOrderQty"]
            .as_str()
            .and_then(|s| s.parse().ok());

        instruments.push(RawInstrument {
            exchange_symbol,
            base_asset,
            quote_asset,
            status: InstrumentStatus::Trading,
            min_qty,
            max_qty,
            tick_size,
            min_notional: None,
        });
    }

    Ok(instruments)
}

// === OKX ===

fn parse_okx(body: &str, market: Market) -> Result<Vec<RawInstrument>> {
    let value: serde_json::Value = serde_json::from_str(body)?;

    let data = value["data"]
        .as_array()
        .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing data array")))?;

    let mut instruments = Vec::new();

    for item in data {
        let state = item["state"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing state")))?;

        if state != "live" {
            continue;
        }

        let exchange_symbol = item["instId"]
            .as_str()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing instId")))?
            .to_string();

        let (base_asset, quote_asset) = if market == Market::Spot {
            let base = item["baseCcy"]
                .as_str()
                .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing baseCcy")))?
                .to_string();
            let quote = item["quoteCcy"]
                .as_str()
                .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing quoteCcy")))?
                .to_string();
            (base, quote)
        } else {
            // Futures: parse from instId (e.g., "BTC-USDT-SWAP")
            let parts: Vec<&str> = exchange_symbol.split('-').collect();
            if parts.len() < 2 {
                continue;
            }
            (parts[0].to_string(), parts[1].to_string())
        };

        // Only USDT pairs
        if quote_asset != "USDT" {
            continue;
        }

        let tick_size = item["tickSz"].as_str().and_then(|s| s.parse().ok());
        let min_qty = item["minSz"].as_str().and_then(|s| s.parse().ok());

        instruments.push(RawInstrument {
            exchange_symbol,
            base_asset,
            quote_asset,
            status: InstrumentStatus::Trading,
            min_qty,
            max_qty: None,
            tick_size,
            min_notional: None,
        });
    }

    Ok(instruments)
}

// === MEXC ===

fn parse_mexc(body: &str, market: Market) -> Result<Vec<RawInstrument>> {
    let value: serde_json::Value = serde_json::from_str(body)?;

    let mut instruments = Vec::new();

    if market == Market::Spot {
        let symbols = value["symbols"]
            .as_array()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing symbols array")))?;

        for symbol in symbols {
            let status_str = symbol["status"]
                .as_str()
                .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing status")))?;

            if status_str != "1" {
                continue;
            }

            let exchange_symbol = symbol["symbol"]
                .as_str()
                .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing symbol")))?
                .to_string();

            let base_asset = symbol["baseAsset"]
                .as_str()
                .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing baseAsset")))?
                .to_string();

            let quote_asset = symbol["quoteAsset"]
                .as_str()
                .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing quoteAsset")))?
                .to_string();

            if quote_asset != "USDT" {
                continue;
            }

            instruments.push(RawInstrument {
                exchange_symbol,
                base_asset,
                quote_asset,
                status: InstrumentStatus::Trading,
                min_qty: None,
                max_qty: None,
                tick_size: None,
                min_notional: None,
            });
        }
    } else {
        // Futures
        let data = value["data"]
            .as_array()
            .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing data array")))?;

        for item in data {
            let state = item["state"]
                .as_i64()
                .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing state")))?;

            if state != 0 {
                continue;
            }

            let exchange_symbol = item["symbol"]
                .as_str()
                .ok_or_else(|| DiscoveryError::Other(anyhow::anyhow!("Missing symbol")))?
                .to_string();

            // Parse base/quote from symbol (e.g., "BTC_USDT")
            let parts: Vec<&str> = exchange_symbol.split('_').collect();
            if parts.len() != 2 {
                continue;
            }

            let base_asset = parts[0].to_string();
            let quote_asset = parts[1].to_string();

            if quote_asset != "USDT" {
                continue;
            }

            instruments.push(RawInstrument {
                exchange_symbol,
                base_asset,
                quote_asset,
                status: InstrumentStatus::Trading,
                min_qty: None,
                max_qty: None,
                tick_size: None,
                min_notional: None,
            });
        }
    }

    Ok(instruments)
}

/// Fetch all instruments from all 8 sources in parallel
pub async fn fetch_all_sources(
    binance_rest_spot: &str,
    binance_rest_futures: &str,
    binance_path_spot: &str,
    binance_path_futures: &str,
    bybit_rest: &str,
    bybit_path_spot: &str,
    bybit_path_futures: &str,
    okx_rest: &str,
    okx_path_spot: &str,
    okx_path_futures: &str,
    mexc_rest_spot: &str,
    mexc_rest_futures: &str,
    mexc_path_spot: &str,
    mexc_path_futures: &str,
) -> Vec<Result<(SourceId, Vec<RawInstrument>)>> {
    let client = Client::new();

    let (r0, r1, r2, r3, r4, r5, r6, r7) = tokio::join!(
        fetch_instruments_with_retry(
            &client,
            Exchange::Binance,
            Market::Spot,
            binance_rest_spot,
            binance_path_spot
        ),
        fetch_instruments_with_retry(
            &client,
            Exchange::Binance,
            Market::Futures,
            binance_rest_futures,
            binance_path_futures
        ),
        fetch_instruments_with_retry(
            &client,
            Exchange::Bybit,
            Market::Spot,
            bybit_rest,
            bybit_path_spot
        ),
        fetch_instruments_with_retry(
            &client,
            Exchange::Bybit,
            Market::Futures,
            bybit_rest,
            bybit_path_futures
        ),
        fetch_instruments_with_retry(
            &client,
            Exchange::MEXC,
            Market::Spot,
            mexc_rest_spot,
            mexc_path_spot
        ),
        fetch_instruments_with_retry(
            &client,
            Exchange::MEXC,
            Market::Futures,
            mexc_rest_futures,
            mexc_path_futures
        ),
        fetch_instruments_with_retry(&client, Exchange::OKX, Market::Spot, okx_rest, okx_path_spot),
        fetch_instruments_with_retry(
            &client,
            Exchange::OKX,
            Market::Futures,
            okx_rest,
            okx_path_futures
        ),
    );

    vec![
        r0.map(|v| (SourceId::BinanceSpot, v)),
        r1.map(|v| (SourceId::BinanceFutures, v)),
        r2.map(|v| (SourceId::BybitSpot, v)),
        r3.map(|v| (SourceId::BybitFutures, v)),
        r4.map(|v| (SourceId::MexcSpot, v)),
        r5.map(|v| (SourceId::MexcFutures, v)),
        r6.map(|v| (SourceId::OkxSpot, v)),
        r7.map(|v| (SourceId::OkxFutures, v)),
    ]
}

/// Check if we have minimum required sources (6/8)
pub fn check_minimum_sources(
    results: &[Result<(SourceId, Vec<RawInstrument>)>],
) -> Result<()> {
    let successful = results
        .iter()
        .filter(|r: &&Result<(SourceId, Vec<RawInstrument>)>| r.is_ok())
        .count();
    const MIN_SOURCES: usize = 6;

    if successful < MIN_SOURCES {
        return Err(DiscoveryError::InsufficientSources {
            successful,
            required: MIN_SOURCES,
        });
    }

    info!(
        "Source fetch complete: {}/{} successful",
        successful,
        results.len()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_binance_spot() {
        let client = Client::new();
        let result = fetch_instruments_with_retry(
            &client,
            Exchange::Binance,
            Market::Spot,
            "https://api.binance.com",
            "/api/v3/exchangeInfo",
        )
        .await;

        match &result {
            Ok(instruments) => {
                println!("Fetched {} instruments", instruments.len());
                assert!(instruments.len() > 400); // Should have 400+ pairs
                assert!(instruments.iter().any(|i| i.exchange_symbol == "BTCUSDT"));
            }
            Err(e) => {
                println!("Error: {:?}", e);
                panic!("Fetch failed: {}", e);
            }
        }
    }

    #[tokio::test]
    async fn test_fetch_bybit_spot() {
        let client = Client::new();
        let result = fetch_instruments_with_retry(
            &client,
            Exchange::Bybit,
            Market::Spot,
            "https://api.bybit.com",
            "/v5/market/instruments-info?category=spot",
        )
        .await;

        assert!(result.is_ok());
        let instruments = result.unwrap();
        assert!(instruments.len() > 300);
        assert!(instruments.iter().any(|i| i.exchange_symbol == "BTCUSDT"));
    }

    #[tokio::test]
    async fn test_fetch_okx_spot() {
        let client = Client::new();
        let result = fetch_instruments_with_retry(
            &client,
            Exchange::OKX,
            Market::Spot,
            "https://www.okx.com",
            "/api/v5/public/instruments?instType=SPOT",
        )
        .await;

        assert!(result.is_ok());
        let instruments = result.unwrap();
        assert!(instruments.len() > 200);
        assert!(instruments.iter().any(|i| i.exchange_symbol == "BTC-USDT"));
    }

    #[tokio::test]
    async fn test_fetch_mexc_spot() {
        let client = Client::new();
        let result = fetch_instruments_with_retry(
            &client,
            Exchange::MEXC,
            Market::Spot,
            "https://api.mexc.com",
            "/api/v3/exchangeInfo",
        )
        .await;

        assert!(result.is_ok());
        let instruments = result.unwrap();
        assert!(instruments.len() > 500);
        assert!(instruments.iter().any(|i| i.exchange_symbol == "BTCUSDT"));
    }
}
