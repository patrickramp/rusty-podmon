use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

// =============================================================================
// Container State Management
// =============================================================================

#[derive(Debug, Clone)]
pub struct ContainerState {
    pub compose_file: PathBuf,
    last_restart: Option<Instant>,
    pub restart_count: u32,
    pub consecutive_failures: u32,
}

impl ContainerState {
    pub fn new(compose_file: PathBuf) -> Self {
        Self {
            compose_file,
            last_restart: None,
            restart_count: 0,
            consecutive_failures: 0,
        }
    }

    pub fn backoff_duration(&self) -> Duration {
        let backoff_seconds = 2_u64.pow(self.consecutive_failures.min(6));
        Duration::from_secs(backoff_seconds)
    }

    pub fn is_in_backoff(&self) -> bool {
        self.last_restart
            .map(|time| time.elapsed() < self.backoff_duration())
            .unwrap_or(false)
    }

    pub fn record_success(&mut self) {
        self.restart_count += 1;
        self.last_restart = Some(Instant::now());
        self.consecutive_failures = 0;
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        if self.last_restart.is_none() {
            self.last_restart = Some(Instant::now());
        }
    }
}

#[derive(Debug)]
pub struct MonitorState {
    pub managed_containers: HashMap<String, ContainerState>,
    running_containers: HashSet<String>,
}

impl MonitorState {
    pub fn new() -> Self {
        Self {
            managed_containers: HashMap::new(),
            running_containers: HashSet::new(),
        }
    }

    pub fn update_running(&mut self, running: HashSet<String>) {
        self.running_containers = running;
    }

    pub fn running_managed_count(&self) -> usize {
        self.managed_containers
            .keys()
            .filter(|name| self.running_containers.contains(*name))
            .count()
    }

    pub fn clear_managed(&mut self) {
        self.managed_containers.clear();
    }

    pub fn add_container(&mut self, name: String, compose_file: PathBuf) {
        self.managed_containers
            .insert(name, ContainerState::new(compose_file));
    }

    pub fn is_running(&self, name: &str) -> bool {
        self.running_containers.contains(name)
    }
}
