use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

// =============================================================================
// Configuration and CLI
// =============================================================================

#[derive(Parser)]
#[command(name = "rusty-podmon")]
#[command(about = "A monitor for Podman containers managed via compose files")]
pub struct Args {
    #[arg(short, long, default_value = "monitor.toml")]
    pub config: PathBuf,

    #[arg(short, long, default_value = "logs")]
    pub log_dir: PathBuf,

    #[arg(short = 'v', long, default_value = "info")]
    pub log_level: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub compose_files: Vec<String>,
    #[serde(default = "default_check_interval")]
    pub check_interval_seconds: u64,
    #[serde(default = "default_status_interval")]
    pub status_interval_seconds: u64,
    #[serde(default = "default_max_failures")]
    pub max_consecutive_failures: u32,
}

const fn default_check_interval() -> u64 {
    30
}
const fn default_status_interval() -> u64 {
    300
}
const fn default_max_failures() -> u32 {
    5
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))
    }
}
