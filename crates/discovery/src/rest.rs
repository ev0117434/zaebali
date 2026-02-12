use crate::source_from_exchange_market;
use crate::types::RawInstrument;
use anyhow::{anyhow, Context, Result};
use common::config::ExchangesConfig;
use common::types::SourceId;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;

pub fn fetch_all_sources(
    exchanges: &ExchangesConfig,
    quote_filter: &[String],
) -> Result<HashMap<SourceId, Vec<RawInstrument>>> {
    let mut out = HashMap::new();

    for ex in &exchanges.exchange {
        let spot_source =
            source_from_exchange_market(&ex.name, false).context("unknown spot source")?;
        let futures_source =
            source_from_exchange_market(&ex.name, true).context("unknown futures source")?;

        let spot = fetch_instruments(
            &ex.name,
            false,
            &ex.rest_spot,
            &ex.instruments_path_spot,
            quote_filter,
        )?;
        out.insert(spot_source, spot);
        let futures = fetch_instruments(
            &ex.name,
            true,
            &ex.rest_futures,
            &ex.instruments_path_futures,
            quote_filter,
        )?;
        out.insert(futures_source, futures);
    }

    Ok(out)
}

fn fetch_instruments(
    exchange: &str,
    is_futures: bool,
    base_url: &str,
    path: &str,
    quote_filter: &[String],
) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let output = Command::new("curl")
        .args(["-fsSL", &url])
        .output()
        .with_context(|| format!("failed to spawn curl for {url}"))?;
    if !output.status.success() {
        return Err(anyhow!("curl failed for {url}: status {}", output.status));
    }
    let v: Value = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("json parse failed: {url}"))?;

    parse_instruments(exchange, is_futures, v, quote_filter)
}

fn parse_instruments(
    exchange: &str,
    is_futures: bool,
    v: Value,
    quote_filter: &[String],
) -> Result<Vec<RawInstrument>> {
    match (exchange, is_futures) {
        ("binance", _) => parse_binance(v, is_futures, quote_filter),
        ("bybit", _) => parse_bybit(v, quote_filter),
        ("okx", _) => parse_okx(v, is_futures, quote_filter),
        ("mexc", _) => parse_mexc(v, is_futures, quote_filter),
        _ => Err(anyhow!("unsupported exchange: {exchange}")),
    }
}

fn parse_binance(
    v: Value,
    is_futures: bool,
    quote_filter: &[String],
) -> Result<Vec<RawInstrument>> {
    let arr = v["symbols"]
        .as_array()
        .context("binance: symbols is not array")?;
    let mut out = Vec::new();
    for item in arr {
        let status = item["status"].as_str().unwrap_or_default();
        if status != "TRADING" {
            continue;
        }
        if is_futures && item["contractType"].as_str().unwrap_or("PERPETUAL") != "PERPETUAL" {
            continue;
        }

        let quote = item["quoteAsset"].as_str().unwrap_or_default();
        if !quote_filter.iter().any(|q| q == quote) {
            continue;
        }
        let symbol = item["symbol"].as_str().unwrap_or_default();
        let base = item["baseAsset"].as_str().unwrap_or_default();

        let (mut tick_size, mut min_qty) = (None, None);
        if let Some(filters) = item["filters"].as_array() {
            for f in filters {
                match f["filterType"].as_str().unwrap_or_default() {
                    "PRICE_FILTER" => {
                        tick_size = f["tickSize"].as_str().and_then(|s| s.parse::<f64>().ok())
                    }
                    "LOT_SIZE" => {
                        min_qty = f["minQty"].as_str().and_then(|s| s.parse::<f64>().ok())
                    }
                    _ => {}
                }
            }
        }

        out.push(RawInstrument {
            exchange_symbol: symbol.to_string(),
            base_asset: base.to_string(),
            quote_asset: quote.to_string(),
            status: status.to_string(),
            min_qty,
            tick_size,
        });
    }
    Ok(out)
}

fn parse_bybit(v: Value, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let arr = v["result"]["list"]
        .as_array()
        .context("bybit: result.list is not array")?;
    let mut out = Vec::new();
    for item in arr {
        let status = item["status"].as_str().unwrap_or_default();
        if status != "Trading" {
            continue;
        }
        let quote = item["quoteCoin"].as_str().unwrap_or_default();
        if !quote_filter.iter().any(|q| q == quote) {
            continue;
        }

        out.push(RawInstrument {
            exchange_symbol: item["symbol"].as_str().unwrap_or_default().to_string(),
            base_asset: item["baseCoin"].as_str().unwrap_or_default().to_string(),
            quote_asset: quote.to_string(),
            status: status.to_string(),
            min_qty: item["lotSizeFilter"]["minOrderQty"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok()),
            tick_size: item["priceFilter"]["tickSize"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok()),
        });
    }
    Ok(out)
}

fn parse_okx(v: Value, is_futures: bool, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let arr = v["data"].as_array().context("okx: data is not array")?;
    let mut out = Vec::new();
    for item in arr {
        let state = item["state"].as_str().unwrap_or_default();
        if state != "live" {
            continue;
        }

        let (base, quote) = if is_futures {
            (
                item["ctValCcy"].as_str().unwrap_or_default(),
                item["settleCcy"].as_str().unwrap_or_default(),
            )
        } else {
            (
                item["baseCcy"].as_str().unwrap_or_default(),
                item["quoteCcy"].as_str().unwrap_or_default(),
            )
        };
        if !quote_filter.iter().any(|q| q == quote) {
            continue;
        }

        out.push(RawInstrument {
            exchange_symbol: item["instId"].as_str().unwrap_or_default().to_string(),
            base_asset: base.to_string(),
            quote_asset: quote.to_string(),
            status: state.to_string(),
            min_qty: item["minSz"].as_str().and_then(|s| s.parse::<f64>().ok()),
            tick_size: item["tickSz"].as_str().and_then(|s| s.parse::<f64>().ok()),
        });
    }
    Ok(out)
}

fn parse_mexc(v: Value, is_futures: bool, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let arr = if is_futures {
        v["data"]
            .as_array()
            .context("mexc futures: data is not array")?
    } else {
        v["symbols"]
            .as_array()
            .context("mexc spot: symbols is not array")?
    };
    let mut out = Vec::new();

    for item in arr {
        let (status_ok, symbol, base, quote) = if is_futures {
            (
                item["state"].as_i64().unwrap_or(-1) == 0,
                item["symbol"].as_str().unwrap_or_default(),
                item["baseCoin"].as_str().unwrap_or_default(),
                item["quoteCoin"].as_str().unwrap_or_default(),
            )
        } else {
            (
                item["status"].as_str().unwrap_or_default() == "1",
                item["symbol"].as_str().unwrap_or_default(),
                item["baseAsset"].as_str().unwrap_or_default(),
                item["quoteAsset"].as_str().unwrap_or_default(),
            )
        };
        if !status_ok || !quote_filter.iter().any(|q| q == quote) {
            continue;
        }

        out.push(RawInstrument {
            exchange_symbol: symbol.to_string(),
            base_asset: base.to_string(),
            quote_asset: quote.to_string(),
            status: if is_futures {
                "0".to_string()
            } else {
                "1".to_string()
            },
            min_qty: None,
            tick_size: None,
        });
    }
    Ok(out)
}
