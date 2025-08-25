use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use tracing::debug;

// =============================================================================
// External Command Interface
// =============================================================================

pub struct PodmanClient;

impl PodmanClient {
    pub fn get_running_containers() -> Result<HashSet<String>> {
        let output = Command::new("podman")
            .args(["ps", "--format", "{{.Names}}"])
            .output()
            .context("Failed to execute 'podman ps'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("podman ps failed: {}", stderr));
        }

        let stdout =
            String::from_utf8(output.stdout).context("Invalid UTF-8 in podman command output")?;

        Ok(stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_string())
            .collect())
    }

    pub fn restart_compose_service(compose_file: &Path) -> Result<()> {
        let compose_dir = compose_file
            .parent()
            .context("Failed to get parent directory of compose file")?;

        debug!("Restarting compose services in {}", compose_dir.display());

        // Stop services
        let output = Command::new("podman-compose")
            .current_dir(compose_dir)
            .args(["down"])
            .output()
            .context("Failed to execute 'podman-compose down'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("podman-compose down failed: {}", stderr));
        }

        // Start services
        let output = Command::new("podman-compose")
            .current_dir(compose_dir)
            .args(["up", "-d"])
            .output()
            .context("Failed to execute 'podman-compose up'")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("podman-compose up failed: {}", stderr));
        }

        Ok(())
    }
}
