//! Normalization: exchange-specific symbols → global symbol registry.
//!
//! Takes RawInstruments from all 8 sources, deduplicates by normalized name
//! "{BASE}-{QUOTE}", assigns symbol_id, and builds per-source mappings.

use std::collections::{HashMap, HashSet};

use common::symbols::SymbolRecord;
use common::types::NUM_SOURCES;

use crate::rest_client::RawInstrument;

/// Normalize base+quote into canonical form: "BTC-USDT"
pub fn normalize_name(base: &str, quote: &str) -> String {
    format!("{}-{}", base.to_uppercase(), quote.to_uppercase())
}

/// Registry of all symbols across all sources.
pub struct SymbolRegistry {
    pub symbols: Vec<SymbolRecord>,
    pub name_to_id: HashMap<String, u16>,
    /// Which symbol_ids are present on each source
    pub source_symbols: [HashSet<u16>; NUM_SOURCES as usize],
}

impl SymbolRegistry {
    /// Build global symbol registry from all source instruments.
    pub fn build(all_sources: &[Vec<RawInstrument>; 8]) -> Self {
        let mut symbols: Vec<SymbolRecord> = Vec::new();
        let mut name_to_id: HashMap<String, u16> = HashMap::new();
        let mut source_symbols: [HashSet<u16>; 8] = std::array::from_fn(|_| HashSet::new());

        for source_instruments in all_sources.iter() {
            for inst in source_instruments {
                let name = normalize_name(&inst.base_asset, &inst.quote_asset);
                let src = inst.source_id as usize;

                let symbol_id = if let Some(&id) = name_to_id.get(&name) {
                    id
                } else {
                    let id = symbols.len() as u16;
                    name_to_id.insert(name.clone(), id);
                    symbols.push(SymbolRecord {
                        symbol_id: id,
                        name: name.clone(),
                        source_names: std::array::from_fn(|_| None),
                        min_qty: [None; 8],
                        tick_size: [None; 8],
                    });
                    id
                };

                let rec = &mut symbols[symbol_id as usize];
                rec.source_names[src] = Some(inst.exchange_symbol.clone());
                if inst.min_qty.is_some() {
                    rec.min_qty[src] = inst.min_qty;
                }
                if inst.tick_size.is_some() {
                    rec.tick_size[src] = inst.tick_size;
                }

                source_symbols[src].insert(symbol_id);
            }
        }

        Self {
            symbols,
            name_to_id,
            source_symbols,
        }
    }

    pub fn num_symbols(&self) -> u16 {
        self.symbols.len() as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_raw(source_id: u8, symbol: &str, base: &str, quote: &str) -> RawInstrument {
        RawInstrument {
            source_id,
            exchange_symbol: symbol.to_string(),
            base_asset: base.to_string(),
            quote_asset: quote.to_string(),
            status: "TRADING".to_string(),
            min_qty: None,
            tick_size: None,
            min_notional: None,
        }
    }

    #[test]
    fn test_normalize_name() {
        assert_eq!(normalize_name("btc", "usdt"), "BTC-USDT");
        assert_eq!(normalize_name("BTC", "USDT"), "BTC-USDT");
    }

    #[test]
    fn test_build_registry() {
        let mut sources: [Vec<RawInstrument>; 8] = std::array::from_fn(|_| Vec::new());

        // Binance spot
        sources[0].push(make_raw(0, "BTCUSDT", "BTC", "USDT"));
        sources[0].push(make_raw(0, "ETHUSDT", "ETH", "USDT"));

        // Binance futures
        sources[1].push(make_raw(1, "BTCUSDT", "BTC", "USDT"));

        // OKX spot
        sources[6].push(make_raw(6, "BTC-USDT", "BTC", "USDT"));
        sources[6].push(make_raw(6, "SOL-USDT", "SOL", "USDT"));

        let reg = SymbolRegistry::build(&sources);

        // 3 unique symbols: BTC-USDT, ETH-USDT, SOL-USDT
        assert_eq!(reg.num_symbols(), 3);

        // BTC-USDT should be on sources 0, 1, 6
        let btc_id = reg.name_to_id["BTC-USDT"];
        assert!(reg.source_symbols[0].contains(&btc_id));
        assert!(reg.source_symbols[1].contains(&btc_id));
        assert!(reg.source_symbols[6].contains(&btc_id));
        assert!(!reg.source_symbols[2].contains(&btc_id));

        // Exchange names are preserved
        assert_eq!(reg.symbols[btc_id as usize].source_names[0], Some("BTCUSDT".to_string()));
        assert_eq!(reg.symbols[btc_id as usize].source_names[6], Some("BTC-USDT".to_string()));

        // SOL only on OKX
        let sol_id = reg.name_to_id["SOL-USDT"];
        assert!(reg.source_symbols[6].contains(&sol_id));
        assert!(!reg.source_symbols[0].contains(&sol_id));
    }
}
