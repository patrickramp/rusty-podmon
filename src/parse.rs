use anyhow::{Context, Result};
use serde_yml::Value;
use std::fs;
use std::path::Path;
use tracing::debug;

// =============================================================================
// Compose File Parser
// =============================================================================

#[derive(Debug)]
pub struct ContainerSpec {
    pub name: String,
}

pub struct ComposeParser;

impl ComposeParser {
    pub fn parse_containers(file_path: &Path) -> Result<Vec<ContainerSpec>> {
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read compose file: {}", file_path.display()))?;

        let yaml: Value = serde_yml::from_str(&content)
            .with_context(|| format!("Failed to parse YAML: {}", file_path.display()))?;

        let mut containers = Vec::new();

        if let Some(services) = yaml.get("services").and_then(|s| s.as_mapping()) {
            for (service_name, service_config) in services {
                let service_name_str = service_name
                    .as_str()
                    .context("Service name is not a valid string")?;

                // Skip services with restart: "no"
                let restart_policy = service_config
                    .get("restart")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unless-stopped");

                if restart_policy == "no" {
                    debug!("Skipping {} - restart policy is 'no'", service_name_str);
                    continue;
                }

                let container_name = service_config
                    .get("container_name")
                    .and_then(|name| name.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| {
                        Self::generate_default_name(file_path, service_name_str)
                            .unwrap_or_else(|| service_name_str.to_string())
                    });

                containers.push(ContainerSpec {
                    name: container_name,
                });
            }
        }

        Ok(containers)
    }

    fn generate_default_name(file_path: &Path, service_name: &str) -> Option<String> {
        let dir_name = file_path.parent()?.file_name()?.to_str()?.to_lowercase();
        Some(format!("{}_{}_1", dir_name, service_name))
    }
}
