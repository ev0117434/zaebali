use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::types::{DirectionEntry, NUM_SOURCES};

/// A single direction record — stored in generated/directions.bin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionRecord {
    pub direction_id: u8,
    pub spot_source: u8,
    pub futures_source: u8,
    pub name: String,
    pub symbols: Vec<u16>,
}

/// Direction table — loaded from generated/directions.bin
pub struct DirectionTable {
    pub records: Vec<DirectionRecord>,
}

impl DirectionTable {
    pub fn load(generated_dir: &Path) -> Result<Self> {
        let path = generated_dir.join("directions.bin");
        let data = std::fs::read(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let records: Vec<DirectionRecord> = bincode::deserialize(&data)
            .with_context(|| format!("failed to deserialize {}", path.display()))?;
        Ok(Self { records })
    }
}

// === SourceSymbolIndex — flat array for Engine hot path ===

/// Per-(source, symbol) direction lookup.
/// Max 6 directions for any single (source, symbol) pair.
const MAX_DIRS_PER_SLOT: usize = 6;

#[derive(Debug, Clone, Copy, Default)]
pub struct SourceSymbolDirections {
    pub entries: [DirectionEntry; MAX_DIRS_PER_SLOT],
    pub count: u8,
}

impl SourceSymbolDirections {
    fn push(&mut self, entry: DirectionEntry) {
        if (self.count as usize) < MAX_DIRS_PER_SLOT {
            self.entries[self.count as usize] = entry;
            self.count += 1;
        }
    }
}

/// Flat lookup: index = source_id * num_symbols + symbol_id
/// Allows O(1) lookup of all directions involving a given (source, symbol) pair.
pub struct SourceSymbolIndex {
    lookup: Vec<SourceSymbolDirections>,
    num_symbols: u16,
}

impl SourceSymbolIndex {
    /// Build from DirectionTable.
    /// For each direction, for each symbol in that direction:
    ///   - Add entry at [spot_source][symbol_id] with counterpart = futures_source
    ///   - Add entry at [futures_source][symbol_id] with counterpart = spot_source
    pub fn build(directions: &DirectionTable, num_symbols: u16) -> Self {
        let total = NUM_SOURCES as usize * num_symbols as usize;
        let mut lookup = vec![SourceSymbolDirections::default(); total];

        for dir in &directions.records {
            for &symbol_id in &dir.symbols {
                // Spot side: counterpart is futures
                let spot_idx = dir.spot_source as usize * num_symbols as usize + symbol_id as usize;
                lookup[spot_idx].push(DirectionEntry {
                    direction_id: dir.direction_id,
                    counterpart_source: dir.futures_source,
                });

                // Futures side: counterpart is spot
                let fut_idx =
                    dir.futures_source as usize * num_symbols as usize + symbol_id as usize;
                lookup[fut_idx].push(DirectionEntry {
                    direction_id: dir.direction_id,
                    counterpart_source: dir.spot_source,
                });
            }
        }

        Self {
            lookup,
            num_symbols,
        }
    }

    /// O(1) lookup: all directions involving (source, symbol).
    pub fn get(&self, source: u8, symbol_id: u16) -> &SourceSymbolDirections {
        let idx = source as usize * self.num_symbols as usize + symbol_id as usize;
        &self.lookup[idx]
    }

    pub fn num_symbols(&self) -> u16 {
        self.num_symbols
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_symbol_index_build() {
        let directions = DirectionTable {
            records: vec![
                DirectionRecord {
                    direction_id: 0,
                    spot_source: 6, // OKX Spot
                    futures_source: 5, // MEXC Futures
                    name: "okx_spot_mexc_futures".to_string(),
                    symbols: vec![0, 1, 2],
                },
                DirectionRecord {
                    direction_id: 1,
                    spot_source: 6, // OKX Spot
                    futures_source: 3, // Bybit Futures
                    name: "okx_spot_bybit_futures".to_string(),
                    symbols: vec![0, 1],
                },
            ],
        };

        let index = SourceSymbolIndex::build(&directions, 10);

        // OKX Spot (6), symbol 0 should have 2 directions (dir 0 and dir 1)
        let dirs = index.get(6, 0);
        assert_eq!(dirs.count, 2);
        assert_eq!(dirs.entries[0].direction_id, 0);
        assert_eq!(dirs.entries[0].counterpart_source, 5);
        assert_eq!(dirs.entries[1].direction_id, 1);
        assert_eq!(dirs.entries[1].counterpart_source, 3);

        // MEXC Futures (5), symbol 0 should have 1 direction (dir 0)
        let dirs = index.get(5, 0);
        assert_eq!(dirs.count, 1);
        assert_eq!(dirs.entries[0].direction_id, 0);
        assert_eq!(dirs.entries[0].counterpart_source, 6);

        // Bybit Futures (3), symbol 2 should have 0 directions
        let dirs = index.get(3, 2);
        assert_eq!(dirs.count, 0);
    }

    #[test]
    fn test_direction_record_serde() {
        let records = vec![DirectionRecord {
            direction_id: 0,
            spot_source: 6,
            futures_source: 5,
            name: "okx_spot_mexc_futures".to_string(),
            symbols: vec![0, 1, 2, 3],
        }];

        let data = bincode::serialize(&records).unwrap();
        let decoded: Vec<DirectionRecord> = bincode::deserialize(&data).unwrap();
        assert_eq!(decoded[0].symbols.len(), 4);
        assert_eq!(decoded[0].name, "okx_spot_mexc_futures");
    }
}
