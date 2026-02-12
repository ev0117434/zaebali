use crate::normalizer::SymbolRegistry;
use crate::types::Result;
use crate::validator::ValidationResult;
use common::directions::DirectionRecord;
use common::types::SourceId;
use serde::Serialize;
use std::fs;
use std::path::Path;
use tracing::{debug, info};

/// Metadata written to generated/metadata.json
#[derive(Debug, Serialize)]
pub struct Metadata {
    pub timestamp: String,
    pub num_symbols: usize,
    pub per_source_counts: PerSourceCounts,
    pub per_direction_counts: PerDirectionCounts,
    pub validation_stats: ValidationStatsAggregated,
}

#[derive(Debug, Serialize)]
pub struct PerSourceCounts {
    pub binance_spot: usize,
    pub binance_futures: usize,
    pub bybit_spot: usize,
    pub bybit_futures: usize,
    pub mexc_spot: usize,
    pub mexc_futures: usize,
    pub okx_spot: usize,
    pub okx_futures: usize,
}

#[derive(Debug, Serialize)]
pub struct PerDirectionCounts {
    #[serde(flatten)]
    pub counts: std::collections::HashMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct ValidationStatsAggregated {
    pub total_validated: usize,
    pub total_valid: usize,
    pub total_invalid: usize,
}

/// АТОМАРНАЯ запись файла через temp + fsync + rename
/// Гарантирует что читатели никогда не видят частично записанный файл
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let temp_path = path.with_extension("tmp");

    debug!("Writing to temporary file: {}", temp_path.display());

    // 1. Записываем во временный файл
    fs::write(&temp_path, data).map_err(|e| {
        crate::types::DiscoveryError::WriteError {
            path: temp_path.display().to_string(),
            source: e,
        }
    })?;

    // 2. fsync для гарантии записи на диск
    let file = fs::File::open(&temp_path).map_err(|e| {
        crate::types::DiscoveryError::WriteError {
            path: temp_path.display().to_string(),
            source: e,
        }
    })?;

    file.sync_all().map_err(|e| {
        crate::types::DiscoveryError::WriteError {
            path: temp_path.display().to_string(),
            source: e,
        }
    })?;

    drop(file);

    // 3. Атомарный rename (гарантируется ОС)
    fs::rename(&temp_path, path).map_err(|e| {
        crate::types::DiscoveryError::WriteError {
            path: path.display().to_string(),
            source: e,
        }
    })?;

    debug!("Atomically wrote: {}", path.display());

    Ok(())
}

/// Generate all configuration files
pub fn generate_configs(
    registry: &SymbolRegistry,
    directions: &[DirectionRecord],
    validation_results: &[ValidationResult],
    output_dir: &Path,
) -> Result<()> {
    info!("Generating configuration files in {}", output_dir.display());

    // Create output directory if it doesn't exist
    fs::create_dir_all(output_dir).map_err(|e| {
        crate::types::DiscoveryError::WriteError {
            path: output_dir.display().to_string(),
            source: e,
        }
    })?;

    // 1. symbols.bin (bincode, АТОМАРНАЯ запись)
    let symbols_data = bincode::serialize(&registry.symbols)
        .map_err(|e| crate::types::DiscoveryError::Other(e.into()))?;
    write_atomic(&output_dir.join("symbols.bin"), &symbols_data)?;
    info!("Wrote symbols.bin ({} symbols)", registry.symbols.len());

    // 2. directions.bin (bincode, АТОМАРНАЯ запись)
    let directions_data = bincode::serialize(&directions)
        .map_err(|e| crate::types::DiscoveryError::Other(e.into()))?;
    write_atomic(&output_dir.join("directions.bin"), &directions_data)?;
    info!("Wrote directions.bin ({} directions)", directions.len());

    // 3. metadata.json (АТОМАРНАЯ запись)
    let metadata = build_metadata(registry, directions, validation_results);
    let metadata_json = serde_json::to_string_pretty(&metadata)
        .map_err(|e| crate::types::DiscoveryError::Other(e.into()))?;
    write_atomic(
        &output_dir.join("metadata.json"),
        metadata_json.as_bytes(),
    )?;
    info!("Wrote metadata.json");

    // 4. Human-readable files (НЕ критично если частично записаны)
    generate_symbols_txt(registry, &output_dir.join("symbols.txt"))?;
    info!("Wrote symbols.txt");

    generate_directions_txt(directions, &output_dir.join("directions.txt"))?;
    info!("Wrote directions.txt");

    generate_validation_report(
        validation_results,
        &output_dir.join("validation_report.txt"),
    )?;
    info!("Wrote validation_report.txt");

    info!(
        "Configuration generation complete: {} symbols, {} directions",
        registry.symbols.len(),
        directions.len()
    );

    Ok(())
}

/// Build metadata from registry, directions, and validation results
fn build_metadata(
    registry: &SymbolRegistry,
    directions: &[DirectionRecord],
    validation_results: &[ValidationResult],
) -> Metadata {
    // Per-source counts
    let per_source = PerSourceCounts {
        binance_spot: registry.source_symbols[SourceId::BinanceSpot.index()].len(),
        binance_futures: registry.source_symbols[SourceId::BinanceFutures.index()].len(),
        bybit_spot: registry.source_symbols[SourceId::BybitSpot.index()].len(),
        bybit_futures: registry.source_symbols[SourceId::BybitFutures.index()].len(),
        mexc_spot: registry.source_symbols[SourceId::MexcSpot.index()].len(),
        mexc_futures: registry.source_symbols[SourceId::MexcFutures.index()].len(),
        okx_spot: registry.source_symbols[SourceId::OkxSpot.index()].len(),
        okx_futures: registry.source_symbols[SourceId::OkxFutures.index()].len(),
    };

    // Per-direction counts
    let mut dir_counts = std::collections::HashMap::new();
    for dir in directions {
        dir_counts.insert(
            format!("direction_{}", dir.direction_id),
            dir.symbols.len(),
        );
    }

    // Validation stats
    let mut total_validated = 0;
    let mut total_valid = 0;
    let mut total_invalid = 0;

    for result in validation_results {
        total_validated += result.total;
        total_valid += result.valid.len();
        total_invalid += result.invalid.len();
    }

    Metadata {
        timestamp: chrono::Utc::now().to_rfc3339(),
        num_symbols: registry.symbols.len(),
        per_source_counts: per_source,
        per_direction_counts: PerDirectionCounts { counts: dir_counts },
        validation_stats: ValidationStatsAggregated {
            total_validated,
            total_valid,
            total_invalid,
        },
    }
}

/// Generate human-readable symbols.txt
fn generate_symbols_txt(registry: &SymbolRegistry, path: &Path) -> Result<()> {
    let mut output = String::new();

    // Header
    output.push_str("symbol_id\tnormalized_name");
    for i in 0..8 {
        let source = SourceId::from_u8(i as u8).unwrap();
        output.push_str(&format!("\t{}", source.name()));
    }
    output.push('\n');

    // Rows
    for symbol in &registry.symbols {
        output.push_str(&format!("{}\t{}", symbol.symbol_id, symbol.name));
        for i in 0..8 {
            if let Some(ref name) = symbol.source_names[i] {
                output.push_str(&format!("\t{}", name));
            } else {
                output.push_str("\t-");
            }
        }
        output.push('\n');
    }

    fs::write(path, output).map_err(|e| crate::types::DiscoveryError::WriteError {
        path: path.display().to_string(),
        source: e,
    })?;

    Ok(())
}

/// Generate human-readable directions.txt
fn generate_directions_txt(directions: &[DirectionRecord], path: &Path) -> Result<()> {
    let mut output = String::new();

    output.push_str("direction_id\tname\tnum_pairs\n");

    for dir in directions {
        output.push_str(&format!(
            "{}\t{}\t{}\n",
            dir.direction_id,
            dir.name,
            dir.symbols.len()
        ));
    }

    fs::write(path, output).map_err(|e| crate::types::DiscoveryError::WriteError {
        path: path.display().to_string(),
        source: e,
    })?;

    Ok(())
}

/// Generate validation report
fn generate_validation_report(results: &[ValidationResult], path: &Path) -> Result<()> {
    let mut output = String::new();

    output.push_str("=== Validation Report ===\n\n");

    for result in results {
        let valid_pct = if result.total > 0 {
            (result.valid.len() as f64 / result.total as f64) * 100.0
        } else {
            0.0
        };

        output.push_str(&format!(
            "{}: {} total, {} valid, {} invalid ({:.1}%)\n",
            result.source.name(),
            result.total,
            result.valid.len(),
            result.invalid.len(),
            valid_pct
        ));

        if !result.invalid.is_empty() {
            output.push_str(&format!(
                "  Invalid symbols: {}\n",
                result
                    .invalid
                    .iter()
                    .take(10)
                    .map(|i| i.exchange_symbol.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            if result.invalid.len() > 10 {
                output.push_str(&format!("  ... and {} more\n", result.invalid.len() - 10));
            }
        }

        output.push('\n');
    }

    // Summary
    let total_validated: usize = results.iter().map(|r| r.total).sum();
    let total_valid: usize = results.iter().map(|r| r.valid.len()).sum();
    let total_invalid: usize = results.iter().map(|r| r.invalid.len()).sum();

    output.push_str(&format!(
        "Total: {} pairs validated, {} valid, {} invalid ({:.1}%)\n",
        total_validated,
        total_valid,
        total_invalid,
        if total_validated > 0 {
            (total_invalid as f64 / total_validated as f64) * 100.0
        } else {
            0.0
        }
    ));

    fs::write(path, output).map_err(|e| crate::types::DiscoveryError::WriteError {
        path: path.display().to_string(),
        source: e,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn test_write_atomic() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.bin");

        let data = b"test data";
        write_atomic(&file_path, data).unwrap();

        let read_data = fs::read(&file_path).unwrap();
        assert_eq!(read_data, data);

        // Temporary file should be cleaned up
        assert!(!file_path.with_extension("tmp").exists());
    }

    #[test]
    fn test_write_atomic_overwrites() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.bin");

        // First write
        write_atomic(&file_path, b"data1").unwrap();
        assert_eq!(fs::read(&file_path).unwrap(), b"data1");

        // Second write should overwrite
        write_atomic(&file_path, b"data2").unwrap();
        assert_eq!(fs::read(&file_path).unwrap(), b"data2");
    }
}
