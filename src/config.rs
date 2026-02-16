use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub scanner: ScannerConfig,
    pub risk: RiskConfig,
    pub kalshi: KalshiConfig,
}

#[derive(Debug, Deserialize)]
pub struct ScannerConfig {
    pub interval_secs: u64,
    #[serde(default)]
    pub series_filter: Vec<String>,
    #[serde(default = "default_scan_delay_ms")]
    pub scan_delay_ms: u64,
    #[serde(default = "default_min_brackets")]
    pub min_brackets: usize,
    #[serde(default = "default_max_brackets")]
    pub max_brackets: usize,
    #[serde(default = "default_series_cache_secs")]
    pub series_cache_secs: u64,
}

fn default_scan_delay_ms() -> u64 { 150 }
fn default_min_brackets() -> usize { 2 }
fn default_max_brackets() -> usize { 15 }
fn default_series_cache_secs() -> u64 { 300 }

#[derive(Debug, Deserialize)]
pub struct RiskConfig {
    pub min_net_profit_cents: u32,
    pub min_roi_pct: f64,
    pub position_size: u32,
    pub max_open_positions: u32,
}

#[derive(Debug, Deserialize)]
pub struct KalshiConfig {
    pub base_url: String,
    pub rsa_key_path: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();
        let content = std::fs::read_to_string("config.toml")
            .context("Failed to read config.toml")?;
        let config: Config = toml::from_str(&content)
            .context("Failed to parse config.toml")?;
        Ok(config)
    }
}

pub fn api_key_id() -> Result<String> {
    std::env::var("KALSHI_API_KEY_ID")
        .context("KALSHI_API_KEY_ID not set in environment or .env")
}

pub fn is_dry_run() -> bool {
    std::env::var("DRY_RUN")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false)
}
