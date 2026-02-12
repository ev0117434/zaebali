use serde::{Deserialize, Serialize};

/// Raw instrument from exchange REST API
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawInstrument {
    pub exchange_symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub status: InstrumentStatus,
    // Per-source trading parameters
    pub min_qty: Option<f64>,
    pub max_qty: Option<f64>,
    pub tick_size: Option<f64>,
    pub min_notional: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstrumentStatus {
    Trading,
    Suspended,
    Delisted,
    PreLaunch,
}

/// Exchange identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exchange {
    Binance,
    Bybit,
    OKX,
    MEXC,
}

impl Exchange {
    pub fn name(&self) -> &'static str {
        match self {
            Exchange::Binance => "binance",
            Exchange::Bybit => "bybit",
            Exchange::OKX => "okx",
            Exchange::MEXC => "mexc",
        }
    }
}

/// Market type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Market {
    Spot,
    Futures,
}

impl Market {
    pub fn name(&self) -> &'static str {
        match self {
            Market::Spot => "spot",
            Market::Futures => "futures",
        }
    }
}


/// Error types for discovery module
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("Config not found: {path}")]
    ConfigNotFound { path: String },

    #[error("Invalid config: {reason}")]
    InvalidConfig { reason: String },

    #[error("Write error for {path}")]
    WriteError {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("REST fetch failed for {exchange:?}/{market:?}")]
    RestFailed {
        exchange: Exchange,
        market: Market,
        #[source]
        source: anyhow::Error,
    },

    #[error("Validation failed: {reason}")]
    ValidationFailed { reason: String },

    #[error("Insufficient sources: {successful}/{required} successful")]
    InsufficientSources { successful: usize, required: usize },

    #[error("Insufficient validation: {successful}/{required} successful")]
    InsufficientValidation { successful: usize, required: usize },

    #[error("Normalization error: {0}")]
    NormalizationError(String),

    #[error("HTTP error: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Sonic-rs parse error: {0}")]
    SonicError(#[from] sonic_rs::Error),

    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

impl DiscoveryError {
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::ConfigNotFound { .. }
                | Self::InvalidConfig { .. }
                | Self::WriteError { .. }
                | Self::InsufficientSources { .. }
                | Self::InsufficientValidation { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, DiscoveryError>;
