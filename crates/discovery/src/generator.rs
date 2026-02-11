//! Config generator: writes binary and human-readable output files.

use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

use common::directions::DirectionRecord;
use common::types::SourceId;

use crate::normalizer::SymbolRegistry;
use crate::validator::ValidationResult;

pub fn generate(
    registry: &SymbolRegistry,
    directions: &[DirectionRecord],
    validation_results: &[ValidationResult],
    output_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("creating output dir: {}", output_dir.display()))?;

    // 1. symbols.bin
    let symbols_bin = bincode::serialize(&registry.symbols)
        .context("serializing symbols")?;
    std::fs::write(output_dir.join("symbols.bin"), &symbols_bin)
        .context("writing symbols.bin")?;
    info!("symbols.bin: {} bytes ({} symbols)", symbols_bin.len(), registry.symbols.len());

    // 2. directions.bin
    let directions_bin = bincode::serialize(&directions)
        .context("serializing directions")?;
    std::fs::write(output_dir.join("directions.bin"), &directions_bin)
        .context("writing directions.bin")?;
    info!("directions.bin: {} bytes ({} directions)", directions_bin.len(), directions.len());

    // 3. metadata.json
    let metadata = build_metadata(registry, directions, validation_results);
    let metadata_json = serde_json::to_string_pretty(&metadata)
        .context("serializing metadata")?;
    std::fs::write(output_dir.join("metadata.json"), &metadata_json)
        .context("writing metadata.json")?;

    // 4. symbols.txt — human-readable
    let mut symbols_txt = String::new();
    for rec in &registry.symbols {
        let sources: Vec<String> = rec.source_names.iter()
            .map(|s| s.as_deref().unwrap_or("-").to_string())
            .collect();
        symbols_txt.push_str(&format!("{}\t{}\t{}\n", rec.symbol_id, rec.name, sources.join("\t")));
    }
    std::fs::write(output_dir.join("symbols.txt"), &symbols_txt)
        .context("writing symbols.txt")?;

    // 5. directions.txt — human-readable
    let mut directions_txt = String::new();
    for dir in directions {
        directions_txt.push_str(&format!(
            "{}\t{}\t{} pairs\n",
            dir.direction_id, dir.name, dir.symbols.len()
        ));
    }
    std::fs::write(output_dir.join("directions.txt"), &directions_txt)
        .context("writing directions.txt")?;

    // 6. validation_report.txt
    let mut report = String::new();
    for res in validation_results {
        let name = SourceId::from_u8(res.source_id)
            .map(|s| s.name())
            .unwrap_or("unknown");
        let pct = if res.total > 0 {
            res.valid.len() as f64 / res.total as f64 * 100.0
        } else {
            0.0
        };
        report.push_str(&format!(
            "{}: {} total, {} valid ({:.1}%)\n",
            name, res.total, res.valid.len(), pct
        ));
        if !res.invalid.is_empty() {
            let names: Vec<String> = res.invalid.iter()
                .take(20) // Limit output
                .map(|i| format!("{} ({})", i.exchange_name, i.reason))
                .collect();
            report.push_str(&format!("  invalid: {}", names.join(", ")));
            if res.invalid.len() > 20 {
                report.push_str(&format!(" ... and {} more", res.invalid.len() - 20));
            }
            report.push('\n');
        }
    }
    std::fs::write(output_dir.join("validation_report.txt"), &report)
        .context("writing validation_report.txt")?;

    info!("Generated all configs in {}", output_dir.display());
    Ok(())
}

fn build_metadata(
    registry: &SymbolRegistry,
    directions: &[DirectionRecord],
    validation_results: &[ValidationResult],
) -> serde_json::Value {
    let mut sources = serde_json::Map::new();
    for res in validation_results {
        let name = SourceId::from_u8(res.source_id)
            .map(|s| s.name())
            .unwrap_or("unknown");
        sources.insert(name.to_string(), serde_json::json!({
            "total": res.total,
            "valid": res.valid.len(),
            "invalid": res.invalid.len(),
        }));
    }

    let mut dirs = serde_json::Map::new();
    for dir in directions {
        dirs.insert(dir.name.clone(), serde_json::json!({
            "direction_id": dir.direction_id,
            "pairs": dir.symbols.len(),
        }));
    }

    serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "num_symbols": registry.symbols.len(),
        "sources": sources,
        "directions": dirs,
        "total_pair_directions": directions.iter().map(|d| d.symbols.len()).sum::<usize>(),
    })
}
