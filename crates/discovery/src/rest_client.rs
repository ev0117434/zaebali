//! REST clients for fetching instrument lists from all 4 exchanges.
//!
//! Each exchange has separate spot and futures endpoints with different
//! JSON response formats. We normalize everything into RawInstrument.

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{info, warn};

use common::config::ExchangeEntry;

/// Raw instrument as fetched from an exchange REST API.
#[derive(Debug, Clone)]
pub struct RawInstrument {
    pub source_id: u8,
    pub exchange_symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub status: String,
    pub min_qty: Option<f64>,
    pub tick_size: Option<f64>,
    pub min_notional: Option<f64>,
}

const MAX_RETRIES: u32 = 3;
const RETRY_DELAY_MS: u64 = 1000;

async fn fetch_with_retry(client: &reqwest::Client, url: &str) -> Result<String> {
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return resp.text().await.context("reading response body");
                }
                let body = resp.text().await.unwrap_or_default();
                last_err = Some(anyhow::anyhow!("HTTP {} from {}: {}", status, url, body));
            }
            Err(e) => {
                last_err = Some(e.into());
            }
        }
        if attempt < MAX_RETRIES - 1 {
            warn!("Retry {}/{} for {}", attempt + 1, MAX_RETRIES, url);
            tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS * (attempt as u64 + 1))).await;
        }
    }
    Err(last_err.unwrap())
}

// ============================================================
// Binance
// ============================================================

#[derive(Deserialize)]
struct BinanceExchangeInfo {
    symbols: Vec<BinanceSymbol>,
}

#[derive(Deserialize)]
struct BinanceSymbol {
    symbol: String,
    #[serde(alias = "baseAsset")]
    base_asset: String,
    #[serde(alias = "quoteAsset")]
    quote_asset: String,
    status: String,
    #[serde(alias = "contractType", default)]
    contract_type: Option<String>,
    filters: Option<Vec<serde_json::Value>>,
}

fn parse_binance_filters(filters: &Option<Vec<serde_json::Value>>) -> (Option<f64>, Option<f64>, Option<f64>) {
    let mut min_qty = None;
    let mut tick_size = None;
    let mut min_notional = None;

    if let Some(filters) = filters {
        for f in filters {
            match f.get("filterType").and_then(|v| v.as_str()) {
                Some("PRICE_FILTER") => {
                    tick_size = f.get("tickSize").and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
                }
                Some("LOT_SIZE") => {
                    min_qty = f.get("minQty").and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
                }
                Some("MIN_NOTIONAL" | "NOTIONAL") => {
                    min_notional = f.get("minNotional").and_then(|v| v.as_str()).and_then(|s| s.parse().ok());
                }
                _ => {}
            }
        }
    }
    (min_qty, tick_size, min_notional)
}

pub async fn fetch_binance_spot(client: &reqwest::Client, exchange: &ExchangeEntry, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", exchange.rest_spot, exchange.instruments_path_spot);
    let body = fetch_with_retry(client, &url).await?;
    let info: BinanceExchangeInfo = serde_json::from_str(&body).context("parsing binance spot")?;

    let instruments: Vec<RawInstrument> = info.symbols.into_iter()
        .filter(|s| s.status == "TRADING")
        .filter(|s| quote_filter.iter().any(|q| s.quote_asset.eq_ignore_ascii_case(q)))
        .map(|s| {
            let (min_qty, tick_size, min_notional) = parse_binance_filters(&s.filters);
            RawInstrument {
                source_id: 0,
                exchange_symbol: s.symbol,
                base_asset: s.base_asset,
                quote_asset: s.quote_asset,
                status: s.status,
                min_qty,
                tick_size,
                min_notional,
            }
        })
        .collect();

    info!("binance_spot: {} instruments", instruments.len());
    Ok(instruments)
}

pub async fn fetch_binance_futures(client: &reqwest::Client, exchange: &ExchangeEntry, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", exchange.rest_futures, exchange.instruments_path_futures);
    let body = fetch_with_retry(client, &url).await?;
    let info: BinanceExchangeInfo = serde_json::from_str(&body).context("parsing binance futures")?;

    let instruments: Vec<RawInstrument> = info.symbols.into_iter()
        .filter(|s| s.status == "TRADING")
        .filter(|s| s.contract_type.as_deref() == Some("PERPETUAL"))
        .filter(|s| quote_filter.iter().any(|q| s.quote_asset.eq_ignore_ascii_case(q)))
        .map(|s| {
            let (min_qty, tick_size, min_notional) = parse_binance_filters(&s.filters);
            RawInstrument {
                source_id: 1,
                exchange_symbol: s.symbol,
                base_asset: s.base_asset,
                quote_asset: s.quote_asset,
                status: s.status,
                min_qty,
                tick_size,
                min_notional,
            }
        })
        .collect();

    info!("binance_futures: {} instruments", instruments.len());
    Ok(instruments)
}

// ============================================================
// Bybit
// ============================================================

#[derive(Deserialize)]
struct BybitResponse {
    result: BybitResult,
}

#[derive(Deserialize)]
struct BybitResult {
    list: Vec<BybitInstrument>,
}

#[derive(Deserialize)]
struct BybitInstrument {
    symbol: String,
    #[serde(alias = "baseCoin")]
    base_coin: String,
    #[serde(alias = "quoteCoin")]
    quote_coin: String,
    status: String,
    #[serde(alias = "contractType", default)]
    contract_type: Option<String>,
    #[serde(alias = "lotSizeFilter", default)]
    lot_size_filter: Option<BybitLotFilter>,
    #[serde(alias = "priceFilter", default)]
    price_filter: Option<BybitPriceFilter>,
}

#[derive(Deserialize)]
struct BybitLotFilter {
    #[serde(alias = "minOrderQty", default)]
    min_order_qty: Option<String>,
}

#[derive(Deserialize)]
struct BybitPriceFilter {
    #[serde(alias = "tickSize", default)]
    tick_size: Option<String>,
}

pub async fn fetch_bybit_spot(client: &reqwest::Client, exchange: &ExchangeEntry, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", exchange.rest_spot, exchange.instruments_path_spot);
    let body = fetch_with_retry(client, &url).await?;
    let resp: BybitResponse = serde_json::from_str(&body).context("parsing bybit spot")?;

    let instruments: Vec<RawInstrument> = resp.result.list.into_iter()
        .filter(|s| s.status == "Trading")
        .filter(|s| quote_filter.iter().any(|q| s.quote_coin.eq_ignore_ascii_case(q)))
        .map(|s| RawInstrument {
            source_id: 2,
            exchange_symbol: s.symbol,
            base_asset: s.base_coin,
            quote_asset: s.quote_coin,
            status: s.status,
            min_qty: s.lot_size_filter.and_then(|f| f.min_order_qty.and_then(|v| v.parse().ok())),
            tick_size: s.price_filter.and_then(|f| f.tick_size.and_then(|v| v.parse().ok())),
            min_notional: None,
        })
        .collect();

    info!("bybit_spot: {} instruments", instruments.len());
    Ok(instruments)
}

pub async fn fetch_bybit_futures(client: &reqwest::Client, exchange: &ExchangeEntry, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", exchange.rest_futures, exchange.instruments_path_futures);
    let body = fetch_with_retry(client, &url).await?;
    let resp: BybitResponse = serde_json::from_str(&body).context("parsing bybit futures")?;

    let instruments: Vec<RawInstrument> = resp.result.list.into_iter()
        .filter(|s| s.status == "Trading")
        .filter(|s| s.contract_type.as_deref() == Some("LinearPerpetual"))
        .filter(|s| quote_filter.iter().any(|q| s.quote_coin.eq_ignore_ascii_case(q)))
        .map(|s| RawInstrument {
            source_id: 3,
            exchange_symbol: s.symbol,
            base_asset: s.base_coin,
            quote_asset: s.quote_coin,
            status: s.status,
            min_qty: s.lot_size_filter.and_then(|f| f.min_order_qty.and_then(|v| v.parse().ok())),
            tick_size: s.price_filter.and_then(|f| f.tick_size.and_then(|v| v.parse().ok())),
            min_notional: None,
        })
        .collect();

    info!("bybit_futures: {} instruments", instruments.len());
    Ok(instruments)
}

// ============================================================
// OKX
// ============================================================

#[derive(Deserialize)]
struct OkxResponse {
    data: Vec<OkxInstrument>,
}

#[derive(Deserialize)]
struct OkxInstrument {
    #[serde(alias = "instId")]
    inst_id: String,
    #[serde(alias = "baseCcy", default)]
    base_ccy: Option<String>,
    #[serde(alias = "quoteCcy", default)]
    quote_ccy: Option<String>,
    #[serde(alias = "ctValCcy", default)]
    ct_val_ccy: Option<String>,
    #[serde(alias = "settleCcy", default)]
    settle_ccy: Option<String>,
    state: String,
    #[serde(alias = "lotSz", default)]
    lot_sz: Option<String>,
    #[serde(alias = "tickSz", default)]
    tick_sz: Option<String>,
    #[serde(alias = "minSz", default)]
    min_sz: Option<String>,
}

pub async fn fetch_okx_spot(client: &reqwest::Client, exchange: &ExchangeEntry, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", exchange.rest_spot, exchange.instruments_path_spot);
    let body = fetch_with_retry(client, &url).await?;
    let resp: OkxResponse = serde_json::from_str(&body).context("parsing okx spot")?;

    let instruments: Vec<RawInstrument> = resp.data.into_iter()
        .filter(|s| s.state == "live")
        .filter(|s| {
            s.quote_ccy.as_ref().map_or(false, |q| quote_filter.iter().any(|f| q.eq_ignore_ascii_case(f)))
        })
        .map(|s| RawInstrument {
            source_id: 6,
            exchange_symbol: s.inst_id.clone(),
            base_asset: s.base_ccy.unwrap_or_default(),
            quote_asset: s.quote_ccy.unwrap_or_default(),
            status: s.state,
            min_qty: s.min_sz.and_then(|v| v.parse().ok()),
            tick_size: s.tick_sz.and_then(|v| v.parse().ok()),
            min_notional: None,
        })
        .collect();

    info!("okx_spot: {} instruments", instruments.len());
    Ok(instruments)
}

pub async fn fetch_okx_futures(client: &reqwest::Client, exchange: &ExchangeEntry, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", exchange.rest_futures, exchange.instruments_path_futures);
    let body = fetch_with_retry(client, &url).await?;
    let resp: OkxResponse = serde_json::from_str(&body).context("parsing okx futures")?;

    let instruments: Vec<RawInstrument> = resp.data.into_iter()
        .filter(|s| s.state == "live")
        .filter(|s| {
            s.settle_ccy.as_ref().map_or(false, |c| quote_filter.iter().any(|f| c.eq_ignore_ascii_case(f)))
        })
        .map(|s| {
            // OKX SWAP: instId="BTC-USDT-SWAP", ctValCcy="BTC", settleCcy="USDT"
            let base = s.ct_val_ccy.unwrap_or_default();
            let quote = s.settle_ccy.clone().unwrap_or_default();
            RawInstrument {
                source_id: 7,
                exchange_symbol: s.inst_id,
                base_asset: base,
                quote_asset: quote,
                status: s.state,
                min_qty: s.min_sz.and_then(|v| v.parse().ok()),
                tick_size: s.tick_sz.and_then(|v| v.parse().ok()),
                min_notional: None,
            }
        })
        .collect();

    info!("okx_futures: {} instruments", instruments.len());
    Ok(instruments)
}

// ============================================================
// MEXC
// ============================================================

// MEXC Spot uses same format as Binance Spot
pub async fn fetch_mexc_spot(client: &reqwest::Client, exchange: &ExchangeEntry, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", exchange.rest_spot, exchange.instruments_path_spot);
    let body = fetch_with_retry(client, &url).await?;
    let info: BinanceExchangeInfo = serde_json::from_str(&body).context("parsing mexc spot")?;

    // MEXC uses status "1" for enabled in some versions, "ENABLED" in others
    let instruments: Vec<RawInstrument> = info.symbols.into_iter()
        .filter(|s| s.status == "1" || s.status == "ENABLED" || s.status == "TRADING")
        .filter(|s| quote_filter.iter().any(|q| s.quote_asset.eq_ignore_ascii_case(q)))
        .map(|s| {
            let (min_qty, tick_size, min_notional) = parse_binance_filters(&s.filters);
            RawInstrument {
                source_id: 4,
                exchange_symbol: s.symbol,
                base_asset: s.base_asset,
                quote_asset: s.quote_asset,
                status: s.status,
                min_qty,
                tick_size,
                min_notional,
            }
        })
        .collect();

    info!("mexc_spot: {} instruments", instruments.len());
    Ok(instruments)
}

#[derive(Deserialize)]
struct MexcFuturesResponse {
    data: Vec<MexcFuturesInstrument>,
}

#[derive(Deserialize)]
struct MexcFuturesInstrument {
    symbol: String,
    #[serde(alias = "baseCoin")]
    base_coin: String,
    #[serde(alias = "quoteCoin")]
    quote_coin: String,
    state: u8, // 0 = enabled
    #[serde(alias = "minVol", default)]
    min_vol: Option<f64>,
    #[serde(alias = "priceUnit", default)]
    price_unit: Option<f64>,
}

pub async fn fetch_mexc_futures(client: &reqwest::Client, exchange: &ExchangeEntry, quote_filter: &[String]) -> Result<Vec<RawInstrument>> {
    let url = format!("{}{}", exchange.rest_futures, exchange.instruments_path_futures);
    let body = fetch_with_retry(client, &url).await?;

    // MEXC futures wraps in {"success":true,"code":0,"data":[...]}
    let resp: MexcFuturesResponse = serde_json::from_str(&body).context("parsing mexc futures")?;

    let instruments: Vec<RawInstrument> = resp.data.into_iter()
        .filter(|s| s.state == 0) // 0 = enabled
        .filter(|s| quote_filter.iter().any(|q| s.quote_coin.eq_ignore_ascii_case(q)))
        .map(|s| RawInstrument {
            source_id: 5,
            exchange_symbol: s.symbol,
            base_asset: s.base_coin,
            quote_asset: s.quote_coin,
            status: "TRADING".to_string(),
            min_qty: s.min_vol.map(|v| v as f64),
            tick_size: s.price_unit.map(|v| v as f64),
            min_notional: None,
        })
        .collect();

    info!("mexc_futures: {} instruments", instruments.len());
    Ok(instruments)
}

// ============================================================
// Fetch All
// ============================================================

async fn safe_fetch<F>(f: F, name: &str) -> Vec<RawInstrument>
where
    F: std::future::Future<Output = Result<Vec<RawInstrument>>>,
{
    match f.await {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to fetch {}: {}", name, e);
            Vec::new()
        }
    }
}

pub async fn fetch_all(
    client: &reqwest::Client,
    exchanges: &[ExchangeEntry],
    quote_filter: &[String],
) -> [Vec<RawInstrument>; 8] {
    let binance = exchanges.iter().find(|e| e.name == "binance");
    let bybit = exchanges.iter().find(|e| e.name == "bybit");
    let okx = exchanges.iter().find(|e| e.name == "okx");
    let mexc = exchanges.iter().find(|e| e.name == "mexc");

    // Use a dummy ExchangeEntry for missing exchanges (will produce empty results)
    let dummy = ExchangeEntry {
        name: String::new(),
        rest_spot: String::new(),
        rest_futures: String::new(),
        ws_spot: String::new(),
        ws_futures: String::new(),
        max_ws_subscriptions: 0,
        instruments_path_spot: String::new(),
        instruments_path_futures: String::new(),
    };

    let b = binance.unwrap_or(&dummy);
    let by = bybit.unwrap_or(&dummy);
    let o = okx.unwrap_or(&dummy);
    let m = mexc.unwrap_or(&dummy);

    let (r0, r1, r2, r3, r4, r5, r6, r7) = tokio::join!(
        safe_fetch(fetch_binance_spot(client, b, quote_filter), "binance_spot"),
        safe_fetch(fetch_binance_futures(client, b, quote_filter), "binance_futures"),
        safe_fetch(fetch_bybit_spot(client, by, quote_filter), "bybit_spot"),
        safe_fetch(fetch_bybit_futures(client, by, quote_filter), "bybit_futures"),
        safe_fetch(fetch_mexc_spot(client, m, quote_filter), "mexc_spot"),
        safe_fetch(fetch_mexc_futures(client, m, quote_filter), "mexc_futures"),
        safe_fetch(fetch_okx_spot(client, o, quote_filter), "okx_spot"),
        safe_fetch(fetch_okx_futures(client, o, quote_filter), "okx_futures"),
    );

    [r0, r1, r2, r3, r4, r5, r6, r7]
}
