use crate::normalizer::SymbolRegistry;
use common::config::DirectionConfigEntry;
use common::directions::DirectionRecord;
use std::collections::HashSet;
use tracing::info;

/// Build direction lists from symbol registry and direction configs
/// Each direction is the intersection of symbols present on both spot_source and futures_source
pub fn build_directions(
    registry: &SymbolRegistry,
    direction_configs: &[DirectionConfigEntry],
) -> Vec<DirectionRecord> {
    let mut directions = Vec::new();

    for config in direction_configs {
        let spot_idx = config.spot_source as usize;
        let futures_idx = config.futures_source as usize;

        // Get symbols present on spot source
        let spot_symbols = &registry.source_symbols[spot_idx];

        // Get symbols present on futures source
        let futures_symbols = &registry.source_symbols[futures_idx];

        // Intersection: symbols present on BOTH sources
        let intersection: HashSet<u16> = spot_symbols.intersection(futures_symbols).copied().collect();

        // Convert to sorted Vec for deterministic output
        let mut symbols: Vec<u16> = intersection.into_iter().collect();
        symbols.sort_unstable();

        info!(
            "Direction {}: {} (spot={}, futures={}) → {} pairs",
            config.id,
            config.name,
            config.spot_source,
            config.futures_source,
            symbols.len()
        );

        directions.push(DirectionRecord {
            direction_id: config.id,
            spot_source: config.spot_source,
            futures_source: config.futures_source,
            name: config.name.clone(),
            symbols,
        });
    }

    directions
}

/// Remove invalid symbols from registry and recalculate directions
/// Called after WS validation
pub fn remove_invalid_symbols(
    registry: &mut SymbolRegistry,
    invalid_per_source: &[HashSet<u16>; 8],
) {
    // Remove from source_symbols sets
    for source_idx in 0..8 {
        let invalid = &invalid_per_source[source_idx];
        registry.source_symbols[source_idx].retain(|symbol_id| !invalid.contains(symbol_id));
    }

    // Optionally: remove symbols that are no longer present on ANY source
    // (though keeping them doesn't hurt - they just won't appear in any direction)
}

/// Recalculate direction lists after validation
pub fn recalculate_directions(
    registry: &SymbolRegistry,
    direction_configs: &[DirectionConfigEntry],
) -> Vec<DirectionRecord> {
    build_directions(registry, direction_configs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalizer::SymbolRegistry;
    use std::collections::HashMap;

    fn create_test_registry() -> SymbolRegistry {
        use common::symbols::SymbolRecord;
        use common::types::NUM_SOURCES;

        let mut source_symbols: [HashSet<u16>; NUM_SOURCES as usize] =
            std::array::from_fn(|_| HashSet::new());

        // Symbol 0 (BTC-USDT): on sources 0, 1, 6, 7 (Binance Spot/Fut, OKX Spot/Fut)
        source_symbols[0].insert(0); // Binance Spot
        source_symbols[1].insert(0); // Binance Futures
        source_symbols[6].insert(0); // OKX Spot
        source_symbols[7].insert(0); // OKX Futures

        // Symbol 1 (ETH-USDT): on sources 0, 1, 6 (Binance Spot/Fut, OKX Spot) - NOT on OKX Futures
        source_symbols[0].insert(1); // Binance Spot
        source_symbols[1].insert(1); // Binance Futures
        source_symbols[6].insert(1); // OKX Spot

        // Symbol 2 (SOL-USDT): only on sources 0, 6 (Binance Spot, OKX Spot)
        source_symbols[0].insert(2); // Binance Spot
        source_symbols[6].insert(2); // OKX Spot

        let symbols = vec![
            SymbolRecord {
                symbol_id: 0,
                name: "BTC-USDT".to_string(),
                source_names: std::array::from_fn(|_| None),
                min_qty: std::array::from_fn(|_| None),
                tick_size: std::array::from_fn(|_| None),
            },
            SymbolRecord {
                symbol_id: 1,
                name: "ETH-USDT".to_string(),
                source_names: std::array::from_fn(|_| None),
                min_qty: std::array::from_fn(|_| None),
                tick_size: std::array::from_fn(|_| None),
            },
            SymbolRecord {
                symbol_id: 2,
                name: "SOL-USDT".to_string(),
                source_names: std::array::from_fn(|_| None),
                min_qty: std::array::from_fn(|_| None),
                tick_size: std::array::from_fn(|_| None),
            },
        ];

        let mut name_to_id = HashMap::new();
        name_to_id.insert("BTC-USDT".to_string(), 0);
        name_to_id.insert("ETH-USDT".to_string(), 1);
        name_to_id.insert("SOL-USDT".to_string(), 2);

        SymbolRegistry {
            symbols,
            name_to_id,
            source_symbols,
        }
    }

    #[test]
    fn test_build_directions_intersection() {
        let registry = create_test_registry();

        let configs = vec![
            DirectionConfigEntry {
                id: 0,
                spot_source: 0,     // Binance Spot
                futures_source: 1,  // Binance Futures
                name: "binance_spot_binance_futures".to_string(),
            },
            DirectionConfigEntry {
                id: 1,
                spot_source: 6,     // OKX Spot
                futures_source: 7,  // OKX Futures
                name: "okx_spot_okx_futures".to_string(),
            },
        ];

        let directions = build_directions(&registry, &configs);

        // Direction 0: Binance Spot ∩ Binance Futures = {BTC, ETH} = {0, 1}
        assert_eq!(directions[0].direction_id, 0);
        assert_eq!(directions[0].symbols, vec![0, 1]);

        // Direction 1: OKX Spot ∩ OKX Futures = {BTC} = {0}
        // (ETH not on OKX Futures, SOL not on OKX Futures)
        assert_eq!(directions[1].direction_id, 1);
        assert_eq!(directions[1].symbols, vec![0]);
    }

    #[test]
    fn test_remove_invalid_symbols() {
        let mut registry = create_test_registry();

        let mut invalid_per_source: [HashSet<u16>; 8] = std::array::from_fn(|_| HashSet::new());

        // Mark symbol 1 (ETH) as invalid on Binance Futures
        invalid_per_source[1].insert(1);

        remove_invalid_symbols(&mut registry, &invalid_per_source);

        // Symbol 1 should be removed from source 1
        assert!(!registry.source_symbols[1].contains(&1));
        assert!(registry.source_symbols[0].contains(&1)); // Still on Binance Spot
    }

    #[test]
    fn test_recalculate_after_validation() {
        let mut registry = create_test_registry();

        // Mark symbol 1 (ETH) as invalid on Binance Futures
        let mut invalid_per_source: [HashSet<u16>; 8] = std::array::from_fn(|_| HashSet::new());
        invalid_per_source[1].insert(1);
        remove_invalid_symbols(&mut registry, &invalid_per_source);

        let configs = vec![DirectionConfigEntry {
            id: 0,
            spot_source: 0,     // Binance Spot
            futures_source: 1,  // Binance Futures
            name: "binance_spot_binance_futures".to_string(),
        }];

        let directions = recalculate_directions(&registry, &configs);

        // After removing ETH from Binance Futures, intersection should only contain BTC
        assert_eq!(directions[0].symbols, vec![0]);
    }
}
