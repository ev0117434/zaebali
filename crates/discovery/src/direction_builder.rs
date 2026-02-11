//! Direction builder: intersects source pairs to create direction symbol lists.
//!
//! For each of the 12 directions (spot_source, futures_source), computes
//! the intersection of symbol_ids present on BOTH sources.

use common::config::DirectionConfigEntry;
use common::directions::DirectionRecord;

use crate::normalizer::SymbolRegistry;

/// Build direction records by intersecting source symbol sets.
pub fn build_directions(
    registry: &SymbolRegistry,
    direction_configs: &[DirectionConfigEntry],
) -> Vec<DirectionRecord> {
    direction_configs
        .iter()
        .map(|cfg| {
            let spot = cfg.spot_source as usize;
            let fut = cfg.futures_source as usize;

            let mut symbols: Vec<u16> = registry.source_symbols[spot]
                .intersection(&registry.source_symbols[fut])
                .copied()
                .collect();
            symbols.sort();

            DirectionRecord {
                direction_id: cfg.id,
                spot_source: cfg.spot_source,
                futures_source: cfg.futures_source,
                name: cfg.name.clone(),
                symbols,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest_client::RawInstrument;

    fn make_raw(source_id: u8, base: &str) -> RawInstrument {
        RawInstrument {
            source_id,
            exchange_symbol: format!("{}USDT", base),
            base_asset: base.to_string(),
            quote_asset: "USDT".to_string(),
            status: "TRADING".to_string(),
            min_qty: None,
            tick_size: None,
            min_notional: None,
        }
    }

    #[test]
    fn test_build_directions() {
        let mut sources: [Vec<RawInstrument>; 8] = std::array::from_fn(|_| Vec::new());

        // Source 0 (BinanceSpot): BTC, ETH, SOL
        sources[0] = vec![make_raw(0, "BTC"), make_raw(0, "ETH"), make_raw(0, "SOL")];
        // Source 1 (BinanceFutures): BTC, ETH
        sources[1] = vec![make_raw(1, "BTC"), make_raw(1, "ETH")];
        // Source 6 (OkxSpot): BTC, SOL, DOGE
        sources[6] = vec![make_raw(6, "BTC"), make_raw(6, "SOL"), make_raw(6, "DOGE")];

        let reg = SymbolRegistry::build(&sources);

        let configs = vec![
            DirectionConfigEntry {
                id: 0,
                spot_source: 0,
                futures_source: 1,
                name: "binance_spot_binance_futures".to_string(),
            },
            DirectionConfigEntry {
                id: 1,
                spot_source: 6,
                futures_source: 1,
                name: "okx_spot_binance_futures".to_string(),
            },
        ];

        let dirs = build_directions(&reg, &configs);

        // BinanceSpot ∩ BinanceFutures = BTC, ETH (2 pairs)
        assert_eq!(dirs[0].symbols.len(), 2);

        // OkxSpot ∩ BinanceFutures = BTC (only BTC is on both)
        assert_eq!(dirs[1].symbols.len(), 1);
    }
}
