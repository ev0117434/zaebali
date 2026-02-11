use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::types::{SourceId, NUM_SOURCES};

/// A single symbol record — stored in generated/symbols.bin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub symbol_id: u16,
    pub name: String,
    pub source_names: [Option<String>; NUM_SOURCES as usize],
    pub min_qty: [Option<f64>; NUM_SOURCES as usize],
    pub tick_size: [Option<f64>; NUM_SOURCES as usize],
}

/// Subscription entry for a source — symbol_id + exchange-specific name.
#[derive(Debug, Clone)]
pub struct SymbolSub {
    pub symbol_id: u16,
    pub exchange_name: String,
}

/// Global symbol table — loaded from generated/symbols.bin.
/// Provides fast lookups for hot path (ID-based) and warm path (name-based).
pub struct SymbolTable {
    pub records: Vec<SymbolRecord>,
    pub num_symbols: u16,
    /// Per-source lookup: exchange_symbol → symbol_id
    pub exchange_to_id: [HashMap<String, u16>; NUM_SOURCES as usize],
    /// symbol_id → normalized name
    pub id_to_name: Vec<String>,
}

impl SymbolTable {
    /// Load from generated/symbols.bin
    pub fn load(generated_dir: &Path) -> Result<Self> {
        let path = generated_dir.join("symbols.bin");
        let data = std::fs::read(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let records: Vec<SymbolRecord> = bincode::deserialize(&data)
            .with_context(|| format!("failed to deserialize {}", path.display()))?;

        let num_symbols = records.len() as u16;

        let mut exchange_to_id: [HashMap<String, u16>; NUM_SOURCES as usize] =
            std::array::from_fn(|_| HashMap::new());

        let mut id_to_name = Vec::with_capacity(records.len());

        for record in &records {
            id_to_name.push(record.name.clone());
            for source_idx in 0..NUM_SOURCES as usize {
                if let Some(ref exch_name) = record.source_names[source_idx] {
                    exchange_to_id[source_idx]
                        .insert(exch_name.clone(), record.symbol_id);
                }
            }
        }

        Ok(Self {
            records,
            num_symbols,
            exchange_to_id,
            id_to_name,
        })
    }

    /// Resolve exchange-specific symbol name to global symbol_id.
    pub fn resolve(&self, source: SourceId, exchange_symbol: &str) -> Option<u16> {
        self.exchange_to_id[source.index()].get(exchange_symbol).copied()
    }

    /// Get normalized name by symbol_id.
    pub fn name(&self, symbol_id: u16) -> &str {
        &self.id_to_name[symbol_id as usize]
    }

    pub fn num_symbols(&self) -> u16 {
        self.num_symbols
    }

    /// Build subscription list for a specific source — all symbols present on that source.
    pub fn subscription_list(&self, source: SourceId) -> Vec<SymbolSub> {
        let idx = source.index();
        self.records
            .iter()
            .filter_map(|rec| {
                rec.source_names[idx].as_ref().map(|name| SymbolSub {
                    symbol_id: rec.symbol_id,
                    exchange_name: name.clone(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_table_roundtrip() {
        let records = vec![
            SymbolRecord {
                symbol_id: 0,
                name: "BTC-USDT".to_string(),
                source_names: [
                    Some("BTCUSDT".to_string()),
                    Some("BTCUSDT".to_string()),
                    Some("BTCUSDT".to_string()),
                    Some("BTCUSDT".to_string()),
                    Some("BTCUSDT".to_string()),
                    Some("BTC_USDT".to_string()),
                    Some("BTC-USDT".to_string()),
                    Some("BTC-USDT-SWAP".to_string()),
                ],
                min_qty: [None; 8],
                tick_size: [None; 8],
            },
        ];

        // Serialize + deserialize
        let data = bincode::serialize(&records).unwrap();
        let decoded: Vec<SymbolRecord> = bincode::deserialize(&data).unwrap();
        assert_eq!(decoded[0].name, "BTC-USDT");
        assert_eq!(decoded[0].source_names[0], Some("BTCUSDT".to_string()));
    }
}
