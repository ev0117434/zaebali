use crate::normalize::{build_directions, filter_registry_symbols};
use crate::types::{DirectionData, SymbolRegistry, ValidatedRegistry, ValidationStats};
use anyhow::Result;
use common::config::ExchangesConfig;
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn validate_all(
    registry: SymbolRegistry,
    directions: Vec<DirectionData>,
    _exchanges: &ExchangesConfig,
    _timeout_sec: u64,
) -> Result<ValidatedRegistry> {
    let invalid_by_source: HashMap<u8, HashSet<u16>> = HashMap::new();

    let filtered_registry = filter_registry_symbols(&registry, &invalid_by_source);

    let pseudo_direction_cfg = directions
        .iter()
        .map(|d| common::config::DirectionConfigEntry {
            id: d.direction_id,
            spot_source: d.spot_source,
            futures_source: d.futures_source,
            name: d.name.clone(),
        })
        .collect::<Vec<_>>();

    let filtered_directions = build_directions(&filtered_registry, &pseudo_direction_cfg)?;

    let mut stats = ValidationStats {
        per_source_total: BTreeMap::new(),
        per_source_valid: BTreeMap::new(),
        per_source_invalid: BTreeMap::new(),
        warnings: vec!["WS validation is currently in soft mode: all REST-derived symbols are treated as valid".to_string()],
    };

    for source in 0u8..8 {
        let total = filtered_registry
            .symbols
            .iter()
            .filter(|s| s.source_names[source as usize].is_some())
            .count();
        stats.per_source_total.insert(source, total);
        stats.per_source_valid.insert(source, total);
        stats.per_source_invalid.insert(source, 0);
    }

    Ok(ValidatedRegistry {
        registry: filtered_registry,
        directions: filtered_directions,
        validation_stats: stats,
    })
}
