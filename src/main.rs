mod cli_config;
mod state;
mod logging;
mod monitor;
mod parse;
mod podman;

use crate::cli_config::{Args, Config};
use crate::logging::setup_logging;
use crate::monitor::ContainerMonitor;

use anyhow::Result;
use clap::Parser;
use tracing::info;

// =============================================================================
// Main Application
// =============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let _guard = setup_logging(&args.log_dir, &args.log_level)?;

    info!("Starting Podman Container Monitor");
    info!(
        "Config: {}, Log dir: {}",
        args.config.display(),
        args.log_dir.display()
    );

    // Load configuration and start monitoring
    let config = Config::from_file(&args.config)?;
    info!(
        "Monitoring: {} compose files, check interval: {}s",
        config.compose_files.len(),
        config.check_interval_seconds
    );

    let mut monitor = ContainerMonitor::new(config, args.config);
    monitor.run().await
}
