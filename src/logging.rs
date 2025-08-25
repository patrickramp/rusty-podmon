use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

// =============================================================================
// Logging Setup
// =============================================================================

pub fn setup_logging(log_dir: &Path, log_level: &str) -> Result<WorkerGuard> {
    fs::create_dir_all(log_dir)
        .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;

    let file_appender = RollingFileAppender::new(Rotation::DAILY, log_dir, "rusty-podmon.log");
    let (non_blocking_appender, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(EnvFilter::new(format!("rusty_podmon={}", log_level)))
        .with(
            fmt::Layer::new()
                .with_writer(std::io::stdout)
                .with_target(false)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false),
        )
        .with(
            fmt::Layer::new()
                .with_writer(non_blocking_appender)
                .with_target(false)
                .with_thread_ids(false)
                .with_file(false)
                .with_line_number(false)
                .with_ansi(false),
        )
        .init();

    Ok(guard)
}
