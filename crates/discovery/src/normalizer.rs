use crate::types::{DiscoveryError, Exchange, Market, RawInstrument, Result};
use common::symbols::SymbolRecord;
use common::types::{SourceId, NUM_SOURCES};
use std::collections::{BTreeMap, HashMap, HashSet};
use tracing::{debug, info, warn};

/// Normalized symbol with per-source data
#[derive(Debug, Clone)]
pub struct NormalizedSymbol {
    pub normalized_name: String,
    pub exchange_symbol: String,
    pub base: String,
    pub quote: String,
    pub source: SourceId,
    pub min_qty: Option<f64>,
    pub max_qty: Option<f64>,
    pub tick_size: Option<f64>,
    pub min_notional: Option<f64>,
}

/// Symbol registry - the global list of all symbols across all sources
#[derive(Debug)]
pub struct SymbolRegistry {
    pub symbols: Vec<SymbolRecord>,
    pub name_to_id: HashMap<String, u16>,
    pub source_symbols: [HashSet<u16>; NUM_SOURCES as usize],
}

/// Normalize a symbol name from exchange-specific format to canonical format
pub fn normalize_symbol(
    exchange: Exchange,
    market: Market,
    raw: &RawInstrument,
) -> Result<NormalizedSymbol> {
    // СТРОГИЙ парсинг base/quote
    let (base, quote) = parse_base_quote(exchange, market, &raw.exchange_symbol, &raw.base_asset, &raw.quote_asset)?;

    // Uppercase normalization
    let base_norm = base.to_uppercase();
    let quote_norm = quote.to_uppercase();

    // ЖЁСТКОЕ равенство quote == "USDT" (НЕ substring!)
    if quote_norm != "USDT" {
        return Err(DiscoveryError::NormalizationError(format!(
            "Invalid quote asset: expected USDT, got {}",
            quote_norm
        )));
    }

    // Формат normalized_name: "BTC-USDT"
    let normalized_name = format!("{}-{}", base_norm, quote_norm);

    // Determine SourceId based on exchange + market
    let source = match (exchange, market) {
        (Exchange::Binance, Market::Spot) => SourceId::BinanceSpot,
        (Exchange::Binance, Market::Futures) => SourceId::BinanceFutures,
        (Exchange::Bybit, Market::Spot) => SourceId::BybitSpot,
        (Exchange::Bybit, Market::Futures) => SourceId::BybitFutures,
        (Exchange::MEXC, Market::Spot) => SourceId::MexcSpot,
        (Exchange::MEXC, Market::Futures) => SourceId::MexcFutures,
        (Exchange::OKX, Market::Spot) => SourceId::OkxSpot,
        (Exchange::OKX, Market::Futures) => SourceId::OkxFutures,
    };

    Ok(NormalizedSymbol {
        normalized_name,
        exchange_symbol: raw.exchange_symbol.clone(),
        base: base_norm,
        quote: quote_norm,
        source,
        min_qty: raw.min_qty,
        max_qty: raw.max_qty,
        tick_size: raw.tick_size,
        min_notional: raw.min_notional,
    })
}

/// СТРОГИЙ парсинг base/quote для каждой биржи
fn parse_base_quote(
    exchange: Exchange,
    market: Market,
    symbol: &str,
    base: &str,
    quote: &str,
) -> Result<(String, String)> {
    match exchange {
        Exchange::Binance => {
            // Binance: "BTCUSDT" для обоих рынков
            // Проверяем что symbol == base + quote
            let expected = format!("{}{}", base, quote);
            if symbol.to_uppercase() != expected.to_uppercase() {
                return Err(DiscoveryError::NormalizationError(format!(
                    "Binance symbol mismatch: {} != {}{}",
                    symbol, base, quote
                )));
            }
            Ok((base.to_string(), quote.to_string()))
        }
        Exchange::OKX => {
            // OKX: "BTC-USDT" для spot, "BTC-USDT-SWAP" для futures
            let parts: Vec<&str> = symbol.split('-').collect();
            match (market, parts.as_slice()) {
                (Market::Spot, [b, q]) => Ok((b.to_string(), q.to_string())),
                (Market::Futures, [b, q, "SWAP"]) => Ok((b.to_string(), q.to_string())),
                _ => Err(DiscoveryError::NormalizationError(format!(
                    "OKX {:?} invalid format: {}",
                    market, symbol
                ))),
            }
        }
        Exchange::Bybit => {
            // Bybit: "BTCUSDT" для обоих рынков
            Ok((base.to_string(), quote.to_string()))
        }
        Exchange::MEXC => {
            // MEXC: spot может быть "BTCUSDT", futures "BTC_USDT"
            if market == Market::Futures {
                let parts: Vec<&str> = symbol.split('_').collect();
                if parts.len() != 2 {
                    return Err(DiscoveryError::NormalizationError(format!(
                        "MEXC futures invalid format: {}",
                        symbol
                    )));
                }
                Ok((parts[0].to_string(), parts[1].to_string()))
            } else {
                // Spot
                Ok((base.to_string(), quote.to_string()))
            }
        }
    }
}

/// Build global symbol list from all sources
/// ДЕТЕРМИНИРОВАННЫЙ порядок через BTreeMap!
pub fn build_global_list(
    all_sources: &[(SourceId, Vec<RawInstrument>)],
) -> Result<SymbolRegistry> {
    // BTreeMap для ДЕТЕРМИНИРОВАННОГО порядка (НЕ HashMap!)
    let mut symbol_builders: BTreeMap<String, SymbolBuilder> = BTreeMap::new();

    info!("Building global symbol list from {} sources", all_sources.len());

    // Собираем все источники
    for (source_id, instruments) in all_sources {
        let exchange = source_to_exchange(*source_id);
        let market = source_to_market(*source_id);

        debug!(
            "Processing {} instruments from {:?}",
            instruments.len(),
            source_id
        );

        for raw in instruments {
            // Нормализуем
            let normalized = match normalize_symbol(exchange, market, raw) {
                Ok(n) => n,
                Err(e) => {
                    warn!(
                        "Failed to normalize {} from {:?}: {}",
                        raw.exchange_symbol, source_id, e
                    );
                    continue;
                }
            };

            // Добавляем в builder
            symbol_builders
                .entry(normalized.normalized_name.clone())
                .or_insert_with(|| SymbolBuilder::new(normalized.normalized_name.clone()))
                .add_source(*source_id, &normalized);
        }
    }

    // ДЕТЕРМИНИРОВАННОЕ присвоение symbol_id (BTreeMap итерируется в sorted order)
    let mut symbols = Vec::new();
    let mut name_to_id = HashMap::new();
    let mut source_symbols: [HashSet<u16>; NUM_SOURCES as usize] =
        std::array::from_fn(|_| HashSet::new());

    for (symbol_id, (name, builder)) in symbol_builders.into_iter().enumerate() {
        if symbol_id >= common::types::MAX_SYMBOLS as usize {
            warn!(
                "Reached MAX_SYMBOLS={}, truncating universe",
                common::types::MAX_SYMBOLS
            );
            break;
        }

        let symbol_id = symbol_id as u16;
        let record = builder.build(symbol_id);

        // Track which sources have this symbol
        for source_idx in 0..NUM_SOURCES as usize {
            if record.source_names[source_idx].is_some() {
                source_symbols[source_idx].insert(symbol_id);
            }
        }

        name_to_id.insert(name, symbol_id);
        symbols.push(record);
    }

    info!(
        "Built global list: {} unique symbols across {} sources",
        symbols.len(),
        all_sources.len()
    );

    Ok(SymbolRegistry {
        symbols,
        name_to_id,
        source_symbols,
    })
}

/// Helper to build a symbol record from multiple sources
#[derive(Debug)]
struct SymbolBuilder {
    normalized_name: String,
    exchange_symbols: [Option<String>; NUM_SOURCES as usize],
    min_qty: [Option<f64>; NUM_SOURCES as usize],
    max_qty: [Option<f64>; NUM_SOURCES as usize],
    tick_size: [Option<f64>; NUM_SOURCES as usize],
    min_notional: [Option<f64>; NUM_SOURCES as usize],
}

impl SymbolBuilder {
    fn new(normalized_name: String) -> Self {
        Self {
            normalized_name,
            exchange_symbols: std::array::from_fn(|_| None),
            min_qty: std::array::from_fn(|_| None),
            max_qty: std::array::from_fn(|_| None),
            tick_size: std::array::from_fn(|_| None),
            min_notional: std::array::from_fn(|_| None),
        }
    }

    fn add_source(&mut self, source: SourceId, normalized: &NormalizedSymbol) {
        let idx = source.index();
        self.exchange_symbols[idx] = Some(normalized.exchange_symbol.clone());
        self.min_qty[idx] = normalized.min_qty;
        self.max_qty[idx] = normalized.max_qty;
        self.tick_size[idx] = normalized.tick_size;
        self.min_notional[idx] = normalized.min_notional;
    }

    fn build(self, symbol_id: u16) -> SymbolRecord {
        SymbolRecord {
            symbol_id,
            name: self.normalized_name,
            source_names: self.exchange_symbols,
            min_qty: self.min_qty,
            tick_size: self.tick_size,
        }
    }
}

/// Map SourceId to Exchange
fn source_to_exchange(source: SourceId) -> Exchange {
    match source {
        SourceId::BinanceSpot | SourceId::BinanceFutures => Exchange::Binance,
        SourceId::BybitSpot | SourceId::BybitFutures => Exchange::Bybit,
        SourceId::MexcSpot | SourceId::MexcFutures => Exchange::MEXC,
        SourceId::OkxSpot | SourceId::OkxFutures => Exchange::OKX,
    }
}

/// Map SourceId to Market
fn source_to_market(source: SourceId) -> Market {
    if source.is_spot() {
        Market::Spot
    } else {
        Market::Futures
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::InstrumentStatus;

    #[test]
    fn test_normalize_binance_spot() {
        let raw = RawInstrument {
            exchange_symbol: "BTCUSDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            status: InstrumentStatus::Trading,
            min_qty: Some(0.00001),
            max_qty: None,
            tick_size: Some(0.01),
            min_notional: None,
        };

        let normalized = normalize_symbol(Exchange::Binance, Market::Spot, &raw).unwrap();
        assert_eq!(normalized.normalized_name, "BTC-USDT");
        assert_eq!(normalized.source, SourceId::BinanceSpot);
    }

    #[test]
    fn test_normalize_okx_swap() {
        let raw = RawInstrument {
            exchange_symbol: "BTC-USDT-SWAP".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            status: InstrumentStatus::Trading,
            min_qty: None,
            max_qty: None,
            tick_size: None,
            min_notional: None,
        };

        let normalized = normalize_symbol(Exchange::OKX, Market::Futures, &raw).unwrap();
        assert_eq!(normalized.normalized_name, "BTC-USDT");
        assert_eq!(normalized.source, SourceId::OkxFutures);
    }

    #[test]
    fn test_normalize_mexc_futures() {
        let raw = RawInstrument {
            exchange_symbol: "BTC_USDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            status: InstrumentStatus::Trading,
            min_qty: None,
            max_qty: None,
            tick_size: None,
            min_notional: None,
        };

        let normalized = normalize_symbol(Exchange::MEXC, Market::Futures, &raw).unwrap();
        assert_eq!(normalized.normalized_name, "BTC-USDT");
        assert_eq!(normalized.source, SourceId::MexcFutures);
    }

    #[test]
    fn test_invalid_quote() {
        let raw = RawInstrument {
            exchange_symbol: "BTCUSD".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USD".to_string(),
            status: InstrumentStatus::Trading,
            min_qty: None,
            max_qty: None,
            tick_size: None,
            min_notional: None,
        };

        let result = normalize_symbol(Exchange::Binance, Market::Spot, &raw);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_global_list_deterministic() {
        // Тест что symbol_id присваиваются детерминированно
        let instruments = vec![
            RawInstrument {
                exchange_symbol: "ETHUSDT".to_string(),
                base_asset: "ETH".to_string(),
                quote_asset: "USDT".to_string(),
                status: InstrumentStatus::Trading,
                min_qty: None,
                max_qty: None,
                tick_size: None,
                min_notional: None,
            },
            RawInstrument {
                exchange_symbol: "BTCUSDT".to_string(),
                base_asset: "BTC".to_string(),
                quote_asset: "USDT".to_string(),
                status: InstrumentStatus::Trading,
                min_qty: None,
                max_qty: None,
                tick_size: None,
                min_notional: None,
            },
        ];

        let sources = vec![(SourceId::BinanceSpot, instruments)];
        let registry = build_global_list(&sources).unwrap();

        // BTC должен идти перед ETH (алфавитный порядок)
        assert_eq!(registry.symbols[0].name, "BTC-USDT");
        assert_eq!(registry.symbols[1].name, "ETH-USDT");
        assert_eq!(registry.symbols[0].symbol_id, 0);
        assert_eq!(registry.symbols[1].symbol_id, 1);
    }

    #[test]
    fn test_per_source_parameters() {
        let btc_binance = RawInstrument {
            exchange_symbol: "BTCUSDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            status: InstrumentStatus::Trading,
            min_qty: Some(0.00001),
            max_qty: None,
            tick_size: Some(0.01),
            min_notional: Some(10.0),
        };

        let btc_okx = RawInstrument {
            exchange_symbol: "BTC-USDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            status: InstrumentStatus::Trading,
            min_qty: Some(0.0001),
            max_qty: None,
            tick_size: Some(0.1),
            min_notional: None,
        };

        let sources = vec![
            (SourceId::BinanceSpot, vec![btc_binance]),
            (SourceId::OkxSpot, vec![btc_okx]),
        ];

        let registry = build_global_list(&sources).unwrap();
        assert_eq!(registry.symbols.len(), 1);

        let btc = &registry.symbols[0];
        assert_eq!(btc.name, "BTC-USDT");

        // Binance parameters
        assert_eq!(btc.min_qty[SourceId::BinanceSpot.index()], Some(0.00001));
        assert_eq!(btc.tick_size[SourceId::BinanceSpot.index()], Some(0.01));

        // OKX parameters
        assert_eq!(btc.min_qty[SourceId::OkxSpot.index()], Some(0.0001));
        assert_eq!(btc.tick_size[SourceId::OkxSpot.index()], Some(0.1));
    }
}
