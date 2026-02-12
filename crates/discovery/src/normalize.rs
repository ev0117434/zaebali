use crate::types::{DirectionData, NormalizedPair, RawInstrument, RegistrySymbol, SymbolRegistry};
use anyhow::{ensure, Result};
use common::config::DirectionConfigEntry;
use common::types::{SourceId, MAX_SYMBOLS};
use std::collections::{HashMap, HashSet};

pub fn normalize(source: SourceId, raw: &RawInstrument) -> NormalizedPair {
    let normalized_name = format!(
        "{}-{}",
        raw.base_asset.to_uppercase(),
        raw.quote_asset.to_uppercase()
    );
    NormalizedPair {
        source,
        exchange_symbol: raw.exchange_symbol.clone(),
        normalized_name,
        min_qty: raw.min_qty,
        tick_size: raw.tick_size,
    }
}

pub fn normalize_all(
    raw_by_source: &HashMap<SourceId, Vec<RawInstrument>>,
) -> HashMap<SourceId, Vec<NormalizedPair>> {
    raw_by_source
        .iter()
        .map(|(source, raws)| {
            (
                *source,
                raws.iter().map(|r| normalize(*source, r)).collect(),
            )
        })
        .collect()
}

pub fn build_global_list(
    all_sources: &HashMap<SourceId, Vec<NormalizedPair>>,
) -> Result<SymbolRegistry> {
    let mut names: Vec<String> = all_sources
        .values()
        .flat_map(|pairs| pairs.iter().map(|p| p.normalized_name.clone()))
        .collect();
    names.sort();
    names.dedup();

    ensure!(
        names.len() <= MAX_SYMBOLS as usize,
        "too many symbols: {} > {}",
        names.len(),
        MAX_SYMBOLS
    );

    let mut symbols = Vec::with_capacity(names.len());
    let mut name_to_id = HashMap::new();
    for (i, name) in names.into_iter().enumerate() {
        let symbol_id = i as u16;
        name_to_id.insert(name.clone(), symbol_id);
        symbols.push(RegistrySymbol {
            symbol_id,
            name,
            source_names: Default::default(),
            min_qty: [None; 8],
            tick_size: [None; 8],
        });
    }

    let mut source_symbol_to_id: [HashMap<String, u16>; 8] =
        std::array::from_fn(|_| HashMap::new());

    for (source, pairs) in all_sources {
        for p in pairs {
            let Some(&id) = name_to_id.get(&p.normalized_name) else {
                continue;
            };
            let slot = &mut symbols[id as usize];
            slot.source_names[source.index()] = Some(p.exchange_symbol.clone());
            slot.min_qty[source.index()] = p.min_qty;
            slot.tick_size[source.index()] = p.tick_size;
            source_symbol_to_id[source.index()].insert(p.exchange_symbol.clone(), id);
        }
    }

    Ok(SymbolRegistry {
        symbols,
        source_symbol_to_id,
    })
}

pub fn build_directions(
    registry: &SymbolRegistry,
    direction_configs: &[DirectionConfigEntry],
) -> Result<Vec<DirectionData>> {
    let mut out = Vec::with_capacity(direction_configs.len());

    for d in direction_configs {
        let mut symbols = Vec::new();
        for s in &registry.symbols {
            if s.source_names[d.spot_source as usize].is_some()
                && s.source_names[d.futures_source as usize].is_some()
            {
                symbols.push(s.symbol_id);
            }
        }
        out.push(DirectionData {
            direction_id: d.id,
            spot_source: d.spot_source,
            futures_source: d.futures_source,
            name: d.name.clone(),
            symbols,
        });
    }

    Ok(out)
}

pub fn filter_registry_symbols(
    registry: &SymbolRegistry,
    invalid_by_source: &HashMap<u8, HashSet<u16>>,
) -> SymbolRegistry {
    let symbols = registry
        .symbols
        .iter()
        .cloned()
        .map(|mut s| {
            for source_idx in 0..8 {
                if invalid_by_source
                    .get(&(source_idx as u8))
                    .is_some_and(|invalid| invalid.contains(&s.symbol_id))
                {
                    s.source_names[source_idx] = None;
                    s.min_qty[source_idx] = None;
                    s.tick_size[source_idx] = None;
                }
            }
            s
        })
        .collect::<Vec<_>>();

    let mut source_symbol_to_id: [HashMap<String, u16>; 8] =
        std::array::from_fn(|_| HashMap::new());
    for s in &symbols {
        for i in 0..8 {
            if let Some(name) = &s.source_names[i] {
                source_symbol_to_id[i].insert(name.clone(), s.symbol_id);
            }
        }
    }

    SymbolRegistry {
        symbols,
        source_symbol_to_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_unifies_symbols() {
        let a = RawInstrument {
            exchange_symbol: "BTCUSDT".into(),
            base_asset: "BTC".into(),
            quote_asset: "usdt".into(),
            status: "TRADING".into(),
            min_qty: None,
            tick_size: None,
        };
        let b = RawInstrument {
            exchange_symbol: "BTC-USDT".into(),
            base_asset: "btc".into(),
            quote_asset: "USDT".into(),
            status: "live".into(),
            min_qty: None,
            tick_size: None,
        };
        assert_eq!(
            normalize(SourceId::BinanceSpot, &a).normalized_name,
            "BTC-USDT"
        );
        assert_eq!(normalize(SourceId::OkxSpot, &b).normalized_name, "BTC-USDT");
    }
}
