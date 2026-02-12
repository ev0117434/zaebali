use anyhow::Result;
use common::config::{AppConfig, DirectionsConfig, ExchangesConfig};
use std::path::PathBuf;

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let mut config_path = PathBuf::from("config/config.toml");
    let mut output_dir = PathBuf::from("generated");

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" if i + 1 < args.len() => {
                config_path = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--output" if i + 1 < args.len() => {
                output_dir = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }

    let app = AppConfig::load(&config_path)?;
    let exchanges = ExchangesConfig::load(&PathBuf::from("config/exchanges.toml"))?;
    let directions = DirectionsConfig::load(&PathBuf::from("config/directions.toml"))?;

    let validated = discovery::run_discovery(&app, &exchanges, &directions, &output_dir)?;

    println!(
        "pair-discovery done: {} symbols, {} directions, output={}",
        validated.registry.symbols.len(),
        validated.directions.len(),
        output_dir.display()
    );

    Ok(())
}
