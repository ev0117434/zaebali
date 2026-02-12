use crate::types::ValidatedRegistry;
use anyhow::{Context, Result};
use common::config::DirectionsConfig;
use common::directions::DirectionRecord;
use common::symbols::SymbolRecord;
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Serialize)]
struct Metadata {
    timestamp: String,
    num_symbols: usize,
    per_source_counts: Vec<(u8, usize)>,
    per_direction_counts: Vec<(u8, usize)>,
    validation_warnings: Vec<String>,
}

pub fn generate_configs(
    validated: &ValidatedRegistry,
    directions_cfg: &DirectionsConfig,
    output_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("create dir {}", output_dir.display()))?;

    let symbol_records: Vec<SymbolRecord> = validated
        .registry
        .symbols
        .iter()
        .map(|s| SymbolRecord {
            symbol_id: s.symbol_id,
            name: s.name.clone(),
            source_names: s.source_names.clone(),
            min_qty: s.min_qty,
            tick_size: s.tick_size,
        })
        .collect();
    fs::write(
        output_dir.join("symbols.bin"),
        bincode::serialize(&symbol_records)?,
    )?;

    let direction_records: Vec<DirectionRecord> = validated
        .directions
        .iter()
        .map(|d| DirectionRecord {
            direction_id: d.direction_id,
            spot_source: d.spot_source,
            futures_source: d.futures_source,
            name: d.name.clone(),
            symbols: d.symbols.clone(),
        })
        .collect();
    fs::write(
        output_dir.join("directions.bin"),
        bincode::serialize(&direction_records)?,
    )?;

    let metadata = Metadata {
        timestamp: format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        ),
        num_symbols: symbol_records.len(),
        per_source_counts: (0u8..8)
            .map(|source| {
                let c = symbol_records
                    .iter()
                    .filter(|s| s.source_names[source as usize].is_some())
                    .count();
                (source, c)
            })
            .collect(),
        per_direction_counts: validated
            .directions
            .iter()
            .map(|d| (d.direction_id, d.symbols.len()))
            .collect(),
        validation_warnings: validated.validation_stats.warnings.clone(),
    };
    fs::write(
        output_dir.join("metadata.json"),
        serde_json::to_vec_pretty(&metadata)?,
    )?;

    let symbols_txt = symbol_records
        .iter()
        .map(|s| {
            format!(
                "{}\t{}\t{}",
                s.symbol_id,
                s.name,
                s.source_names
                    .iter()
                    .map(|x| x.clone().unwrap_or_else(|| "-".into()))
                    .collect::<Vec<_>>()
                    .join("\t")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(output_dir.join("symbols.txt"), symbols_txt)?;

    let directions_txt = directions_cfg
        .direction
        .iter()
        .map(|cfg| {
            let c = validated
                .directions
                .iter()
                .find(|d| d.direction_id == cfg.id)
                .map(|d| d.symbols.len())
                .unwrap_or(0);
            format!("{}\t{}\t{} pairs", cfg.id, cfg.name, c)
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(output_dir.join("directions.txt"), directions_txt)?;

    let validation_report = (0u8..8)
        .map(|source| {
            let t = validated
                .validation_stats
                .per_source_total
                .get(&source)
                .copied()
                .unwrap_or(0);
            let v = validated
                .validation_stats
                .per_source_valid
                .get(&source)
                .copied()
                .unwrap_or(0);
            let i = validated
                .validation_stats
                .per_source_invalid
                .get(&source)
                .copied()
                .unwrap_or(0);
            format!("source_{source}: {t} total, {v} valid, {i} invalid")
        })
        .chain(validated.validation_stats.warnings.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(output_dir.join("validation_report.txt"), validation_report)?;

    Ok(())
}
