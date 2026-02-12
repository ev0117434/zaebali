use common::types::SourceId;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
pub struct RawInstrument {
    pub exchange_symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub status: String,
    pub min_qty: Option<f64>,
    pub tick_size: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct NormalizedPair {
    pub source: SourceId,
    pub exchange_symbol: String,
    pub normalized_name: String,
    pub min_qty: Option<f64>,
    pub tick_size: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct RegistrySymbol {
    pub symbol_id: u16,
    pub name: String,
    pub source_names: [Option<String>; 8],
    pub min_qty: [Option<f64>; 8],
    pub tick_size: [Option<f64>; 8],
}

#[derive(Debug, Clone)]
pub struct SymbolRegistry {
    pub symbols: Vec<RegistrySymbol>,
    pub source_symbol_to_id: [HashMap<String, u16>; 8],
}

#[derive(Debug, Clone)]
pub struct DirectionData {
    pub direction_id: u8,
    pub spot_source: u8,
    pub futures_source: u8,
    pub name: String,
    pub symbols: Vec<u16>,
}

#[derive(Debug, Clone, Default)]
pub struct ValidationStats {
    pub per_source_total: BTreeMap<u8, usize>,
    pub per_source_valid: BTreeMap<u8, usize>,
    pub per_source_invalid: BTreeMap<u8, usize>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedRegistry {
    pub registry: SymbolRegistry,
    pub directions: Vec<DirectionData>,
    pub validation_stats: ValidationStats,
}
