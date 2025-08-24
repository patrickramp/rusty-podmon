use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use serde_yml::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio::time::{Instant, interval, sleep};
use tracing::{debug, error, info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// CLI arguments for the Podman container monitor
#[derive(Parser)]
#[command(name = "rusty-podmon")]
#[command(about = "A monitor for Podman containers managed via compose files")]
struct Args {
    /// Path to the configuration file
    #[arg(short, long, default_value = "monitor.toml")]
    config: PathBuf,

    /// Directory for log files (creates if doesn't exist)
    #[arg(short, long, default_value = "logs")]
    log_dir: PathBuf,

    /// Log level filter
    #[arg(short = 'v', long, default_value = "info")]
    log_level: String,
}

/// Application configuration loaded from TOML
#[derive(Debug, Deserialize)]
struct Config {
    /// List of docker-compose.yml file paths to monitor
    compose_files: Vec<String>,
    /// Interval between container health checks (seconds)
    #[serde(default = "default_check_interval")]
    check_interval_seconds: u64,
    /// Interval for status summary logs (seconds)
    #[serde(default = "default_status_interval")]
    status_interval_seconds: u64,
    /// Maximum consecutive failures before extended backoff
    #[serde(default = "default_max_failures")]
    max_consecutive_failures: u32,
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

/// Information about a managed container
#[derive(Debug, Clone)]
struct ContainerInfo {
    compose_file: PathBuf,
    last_restart: Option<Instant>,
    restart_count: u32,
    consecutive_failures: u32,
}

/// ContainerInfo methods
impl ContainerInfo {
    fn new(compose_file: PathBuf) -> Self {
        Self {
            compose_file,
            last_restart: None,
            restart_count: 0,
            consecutive_failures: 0,
        }
    }

    /// Calculate exponential backoff duration based on consecutive failures
    fn backoff_duration(&self) -> Duration {
        let backoff_seconds = 2_u64.pow(self.consecutive_failures.min(6));
        Duration::from_secs(backoff_seconds)
    }

    /// Check if container is in backoff period
    fn in_backoff(&self) -> bool {
        if let Some(last_restart) = self.last_restart {
            last_restart.elapsed() < self.backoff_duration()
        } else {
            false
        }
    }

    /// Record successful restart
    fn record_restart(&mut self) {
        self.restart_count += 1;
        self.last_restart = Some(Instant::now());
        self.consecutive_failures = 0;
    }

    /// Record failed restart attempt
    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.last_restart.is_none() {
            self.last_restart = Some(Instant::now());
        }
    }
}

/// Main application state managing container monitoring
#[derive(Debug)]
struct MonitorState {
    containers: HashMap<String, ContainerInfo>,
    running_containers: HashSet<String>,
}

/// MonitorState methods
impl MonitorState {
    fn new() -> Self {
        Self {
            containers: HashMap::new(),
            running_containers: HashSet::new(),
        }
    }

    /// Update the set of currently running containers
    fn update_running_containers(&mut self, running: HashSet<String>) {
        self.running_containers = running;
    }

    /// Get count of managed containers that are currently running
    fn running_managed_count(&self) -> usize {
        self.containers
            .keys()
            .filter(|name| self.running_containers.contains(*name))
            .count()
    }

    /// Clear all container information (used during rediscovery)
    fn clear_containers(&mut self) {
        self.containers.clear();
    }

    /// Add a new container to be managed
    fn add_container(&mut self, name: String, compose_file: PathBuf) {
        self.containers
            .insert(name.clone(), ContainerInfo::new(compose_file));
    }

    /// Get mutable reference to container info if it exists
    fn get_container_mut(&mut self, name: &str) -> Option<&mut ContainerInfo> {
        self.containers.get_mut(name)
    }
}

/// Podman command execution wrapper
struct PodmanClient;

impl PodmanClient {
    /// Get list of currently running container names
    fn get_running_containers() -> Result<HashSet<String>> {
        let output = Command::new("podman")
            .args(["ps", "--format", "{{.Names}}"])
            .output()
            .context("Failed to execute 'podman ps'")?;

        let stdout =
            String::from_utf8(output.stdout).context("Invalid UTF-8 in podman command output")?;

        if !output.status.success() {
            return Err(anyhow::anyhow!("podman ps failed: {}", stdout));
        }

        Ok(stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_string())
            .collect())
    }

    /// Restart podman container using podman-compose
    fn start_container_service(compose_file: &Path, service_name: &str) -> Result<()> {
        let compose_dir = compose_file
            .parent()
            .context("Invalid compose file path - no parent directory")?;

        debug!(
            "Executing: podman-compose down && podman-compose up -d in {} because {} is missing",
            compose_dir.display(),
            service_name
        );

        let output = Command::new("podman-compose")
            .current_dir(compose_dir)
            .args(["down"])
            .output()
            .context("Failed to execute 'podman-compose down'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("podman-compose down failed: {}", stderr));
        }

        let output = Command::new("podman-compose")
            .current_dir(compose_dir)
            .args(["up", "-d", service_name])
            .output()
            .context("Failed to execute 'podman-compose up'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("podman-compose up failed: {}", stderr));
        }

        Ok(())
    }
}

/// Configuration and compose file management
struct ConfigManager;

impl ConfigManager {
    /// Load configuration from TOML file
    fn load_config(config_path: &Path) -> Result<Config> {
        let config_content = fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

        toml::from_str(&config_content)
            .with_context(|| format!("Failed to parse config file: {}", config_path.display()))
    }

    /// Parse docker-compose YAML file and extract container names
    fn parse_compose_file(file_path: &Path) -> Result<Vec<String>> {
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read compose file: {}", file_path.display()))?;

        let yaml: Value = serde_yml::from_str(&content)
            .with_context(|| format!("Failed to parse YAML: {}", file_path.display()))?;

        let mut container_names = Vec::new();

        if let Some(services) = yaml.get("services").and_then(|s| s.as_mapping()) {
            for (service_name, service_config) in services {
                let service_name_str = service_name
                    .as_str()
                    .context("Service name is not a valid string")?;

                // Check restart policy - skip if set to "no"
                let restart_policy = service_config
                    .get("restart")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unless-stopped"); // Default Docker behavior
                if restart_policy == "no" {
                    debug!("Skipping {} - restart policy is 'no'", service_name_str);
                    continue;
                }

                // Use explicit container_name if specified, otherwise use podman-compose default
                let container_name = service_config
                    .get("container_name")
                    .and_then(|name| name.as_str())
                    .map(|name| name.to_string())
                    .unwrap_or_else(|| {
                        parse_default_name(file_path, service_name_str).unwrap_or({
                            warn!("Failed to generate container name for {}", service_name_str);
                            debug!(
                                "Unable to generate container name for {}, in {}",
                                service_name_str,
                                file_path.display()
                            );
                            service_name_str.to_string()
                        })
                    });

                container_names.push(container_name);
            }
        }
        Ok(container_names)
    }
}

// Helper function to generate default container name
fn parse_default_name(file_path: &Path, service_name: &str) -> Option<String> {
    let dir_name = file_path.parent()?.file_name()?.to_str()?.to_lowercase();
    format!("{}_{}_1", dir_name, service_name).into()
}

/// Container monitoring and management logic
struct ContainerMonitor;

impl ContainerMonitor {
    /// Discover all containers from configured compose files
    async fn discover_containers(config: &Config, state: &mut MonitorState) -> Result<()> {
        info!(
            "Discovering containers from {} compose files",
            config.compose_files.len()
        );

        state.clear_containers();

        for compose_path in &config.compose_files {
            let path = PathBuf::from(compose_path);

            if !path.exists() {
                warn!("Compose file not found: {}", compose_path);
                continue;
            }

            match ConfigManager::parse_compose_file(&path) {
                Ok(container_names) => {
                    debug!(
                        "Found {} containers in {}",
                        container_names.len(),
                        compose_path
                    );

                    for name in container_names {
                        state.add_container(name, path.clone());
                    }
                }
                Err(e) => {
                    error!("Failed to parse compose file {}: {:#}", compose_path, e);
                }
            }
        }

        info!("Discovered {} containers total", state.containers.len());
        Ok(())
    }

    /// Check container states and restart any that are down
    async fn check_and_restart_containers(
        state: &mut MonitorState,
        max_failures: u32,
    ) -> Result<()> {
        debug!("Checking container states");

        // Update current running container state
        let running = PodmanClient::get_running_containers()
            .context("Failed to get running containers list")?;
        state.update_running_containers(running);

        // Process each managed container
        let container_names: Vec<String> = state.containers.keys().cloned().collect();

        for container_name in container_names {
            if state.running_containers.contains(&container_name) {
                continue; // Container is running, skip
            }

            // Check if we should skip due to too many failures
            let should_skip = {
                let container = state.containers.get(&container_name).unwrap();
                container.consecutive_failures >= max_failures || container.in_backoff()
            };

            if should_skip {
                let container = state.containers.get(&container_name).unwrap();
                debug!(
                    "Skipping {} - failures: {}/{}, backoff: {}s",
                    container_name,
                    container.consecutive_failures,
                    max_failures,
                    container.backoff_duration().as_secs()
                );
                continue;
            }

            // Attempt to restart the container
            warn!("Container {} is down, attempting restart", container_name);

            let container_info = state.containers.get(&container_name).unwrap().clone();

            match PodmanClient::start_container_service(
                &container_info.compose_file,
                &container_name,
            ) {
                Ok(()) => {
                    // Brief wait for container to initialize
                    sleep(Duration::from_secs(3)).await;

                    // Verify the restart was successful
                    let running = PodmanClient::get_running_containers()?;

                    if running.contains(&container_name) {
                        info!("Successfully restarted container: {}", container_name);
                        state
                            .get_container_mut(&container_name)
                            .unwrap()
                            .record_restart();
                    } else {
                        error!("Container {} failed to start properly", container_name);
                        state
                            .get_container_mut(&container_name)
                            .unwrap()
                            .record_failure();
                    }
                }
                Err(e) => {
                    error!("Failed to restart container {}: {:#}", container_name, e);
                    state
                        .get_container_mut(&container_name)
                        .unwrap()
                        .record_failure();
                }
            }
        }

        Ok(())
    }

    /// Perform initial startup recovery for all containers
    async fn startup_recovery(state: &mut MonitorState, max_failures: u32) -> Result<()> {
        info!("Performing startup container recovery");

        // Single pass through all containers
        Self::check_and_restart_containers(state, max_failures).await?;

        info!("Startup recovery completed");
        Ok(())
    }

    /// Print periodic status summary
    fn print_status_summary(state: &MonitorState) {
        let total = state.containers.len();
        let running = state.running_managed_count();

        info!("Status: {}/{} managed containers running", running, total);

        // Log containers with restart history
        for (name, info) in &state.containers {
            if info.restart_count > 0 || info.consecutive_failures > 0 {
                info!(
                    "Container {} - restarts: {}, consecutive failures: {}",
                    name, info.restart_count, info.consecutive_failures
                );
            }
        }
    }
}

/// Initialize logging with file rotation
fn setup_logging(log_dir: &Path, log_level: &str) -> Result<WorkerGuard> {
    // Create log directory if it doesn't exist
    fs::create_dir_all(log_dir)
        .with_context(|| format!("Failed to create log directory: {}", log_dir.display()))?;

    // Set up file appender with daily rotation
    let file_appender = RollingFileAppender::new(Rotation::DAILY, log_dir, "rusty-podmon.log");

    let (non_blocking_appender, guard) = tracing_appender::non_blocking(file_appender);

    // Initialize tracing subscriber with both console and file output
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
                .with_ansi(false), // No ANSI codes in log files
        )
        .init();

    Ok(guard)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging (keep guard alive for program duration)
    let _guard = setup_logging(&args.log_dir, &args.log_level)?;

    info!("Starting Podman Container Monitor");
    info!(
        "Config: {}, Log dir: {}",
        args.config.display(),
        args.log_dir.display()
    );

    // Load configuration
    let config = ConfigManager::load_config(&args.config)?;
    info!(
        "Monitoring: {} compose files, on {}s check interval",
        config.compose_files.len(),
        config.check_interval_seconds
    );

    let mut state = MonitorState::new();

    // Initial container discovery
    ContainerMonitor::discover_containers(&config, &mut state).await?;

    // Perform startup recovery
    ContainerMonitor::startup_recovery(&mut state, config.max_consecutive_failures).await?;

    // Set up monitoring intervals
    let mut check_interval = interval(Duration::from_secs(config.check_interval_seconds));
    let mut status_interval = interval(Duration::from_secs(config.status_interval_seconds));

    info!(
        "Entering monitoring loop (check: {}s, status: {}s)",
        config.check_interval_seconds, config.status_interval_seconds
    );

    // Main monitoring loop
    loop {
        tokio::select! {
            _ = check_interval.tick() => {
                if let Err(e) = ContainerMonitor::check_and_restart_containers(
                    &mut state,
                    config.max_consecutive_failures
                ).await {
                    error!("Container check cycle failed: {:#}", e);
                }
            }
            _ = status_interval.tick() => {
                ContainerMonitor::discover_containers(&config, &mut state).await?;
                ContainerMonitor::print_status_summary(&state);
            }
        }
    }
}
