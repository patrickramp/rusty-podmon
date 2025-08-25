use crate::cli_config::Config;
use crate::state::{ContainerState, MonitorState};
use crate::parse::ComposeParser;
use crate::podman::PodmanClient;

use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::{interval, sleep};
use tracing::{debug, error, info, warn};

// =============================================================================
// Main Monitor Logic
// =============================================================================

pub struct ContainerMonitor {
    config: Config,
    config_path: PathBuf,
    state: MonitorState,
}

impl ContainerMonitor {
    pub fn new(config: Config, config_path: PathBuf) -> Self {
        Self {
            config,
            config_path,
            state: MonitorState::new(),
        }
    }

    async fn discover_containers(&mut self) -> Result<()> {
        info!(
            "Discovering containers from {} compose files",
            self.config.compose_files.len()
        );

        self.state.clear_managed();

        for compose_path_str in &self.config.compose_files {
            let compose_path = PathBuf::from(compose_path_str);

            if !compose_path.exists() {
                warn!("Compose file not found: {}", compose_path_str);
                continue;
            }

            match ComposeParser::parse_containers(&compose_path) {
                Ok(containers) => {
                    debug!(
                        "Found {} containers in {}",
                        containers.len(),
                        compose_path_str
                    );

                    for container_spec in containers {
                        self.state
                            .add_container(container_spec.name, compose_path.clone());
                    }
                }
                Err(e) => {
                    error!("Failed to parse compose file {}: {:#}", compose_path_str, e);
                }
            }
        }

        info!(
            "Discovered {} containers total",
            self.state.managed_containers.len()
        );
        Ok(())
    }

    fn should_restart_container(
        &self,
        container_name: &str,
        container_state: &ContainerState,
    ) -> bool {
        if container_state.consecutive_failures >= self.config.max_consecutive_failures {
            debug!(
                "Skipping {} - too many failures: {}/{}",
                container_name,
                container_state.consecutive_failures,
                self.config.max_consecutive_failures
            );
            return false;
        }

        if container_state.is_in_backoff() {
            debug!(
                "Skipping {} - in backoff: {}s remaining",
                container_name,
                container_state.backoff_duration().as_secs()
            );
            return false;
        }

        true
    }

    async fn check_and_restart_containers(&mut self) -> Result<()> {
        debug!("Checking container states");

        // Always reload config to check for changes (removed/added compose files)
        match Config::from_file(&self.config_path) {
            Ok(new_config) => {
                if new_config.compose_files != self.config.compose_files {
                    info!("Configuration changed, rediscovering containers");
                    self.config = new_config;
                    self.discover_containers().await?;
                    return Ok(()); // Skip this check cycle after rediscovery
                }
            }
            Err(e) => {
                warn!("Failed to reload config: {:#}", e);
            }
        }

        if self.state.managed_containers.is_empty() {
            debug!("No containers to check");
            return Ok(());
        }

        // Update running container state
        let running = PodmanClient::get_running_containers().map_err(|e| {
            error!("Failed to get running containers: {:#}", e);
            e
        })?;

        self.state.update_running(running);

        // Find containers that need restart
        let containers_to_restart: Vec<(String, ContainerState)> = self
            .state
            .managed_containers
            .iter()
            .filter(|(name, state)| {
                !self.state.is_running(name) && self.should_restart_container(name, state)
            })
            .map(|(name, state)| (name.clone(), state.clone()))
            .collect();

        // Process each container that needs restart
        for (container_name, container_state) in containers_to_restart {
            warn!("Container {} is down, attempting restart", container_name);

            match PodmanClient::restart_compose_service(&container_state.compose_file) {
                Ok(()) => {
                    // Wait for container to initialize
                    sleep(Duration::from_secs(10)).await;

                    // Verify restart success
                    if let Ok(running) = PodmanClient::get_running_containers() {
                        if running.contains(&container_name) {
                            info!("Successfully restarted container: {}", container_name);
                            if let Some(state) =
                                self.state.managed_containers.get_mut(&container_name)
                            {
                                state.record_success();
                            }
                        } else {
                            error!("Container {} failed to start after restart", container_name);
                            if let Some(state) =
                                self.state.managed_containers.get_mut(&container_name)
                            {
                                state.record_failure();
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to restart container {}: {:#}", container_name, e);
                    if let Some(state) = self.state.managed_containers.get_mut(&container_name) {
                        state.record_failure();
                    }
                }
            }
        }

        Ok(())
    }

    fn print_status(&self) {
        let total = self.state.managed_containers.len();
        let running = self.state.running_managed_count();

        info!("Status: {}/{} managed containers running", running, total);

        // Log containers with restart history
        for (name, state) in &self.state.managed_containers {
            if state.restart_count > 0 || state.consecutive_failures > 0 {
                info!(
                    "Container {} - restarts: {}, consecutive failures: {}",
                    name, state.restart_count, state.consecutive_failures
                );
            }
        }
    }

    async fn startup_recovery(&mut self) -> Result<()> {
        info!("Performing startup container recovery");
        self.check_and_restart_containers().await?;
        info!("Startup recovery completed");
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        // Initial setup
        self.discover_containers().await?;
        self.startup_recovery().await?;

        // Set up monitoring intervals
        let mut check_interval = interval(Duration::from_secs(self.config.check_interval_seconds));
        let mut status_interval =
            interval(Duration::from_secs(self.config.status_interval_seconds));

        info!(
            "Entering monitoring loop (check: {}s, status: {}s)",
            self.config.check_interval_seconds, self.config.status_interval_seconds
        );

        // Main monitoring loop
        loop {
            tokio::select! {
                _ = check_interval.tick() => {
                    if let Err(e) = self.check_and_restart_containers().await {
                        error!("Container check cycle failed: {:#}", e);
                    }
                }
                _ = status_interval.tick() => {
                    self.print_status();
                }
            }
        }
    }
}
